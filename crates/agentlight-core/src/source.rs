//! The source seam.
//!
//! A [`StateSource`] translates one backing store (a clawlight `state.json`, a
//! remote hub, an in-memory fixture) into the normalized [`Session`] model.
//! The engine owns merge, retention, aggregation, and notification policy;
//! adapters only read and translate. This module is synchronous and
//! runtime-free: adapters may spawn a plain `std::thread`, never an async
//! runtime.
//!
//! clawlight's own types stay inside [`clawlight`]; only the normalized model
//! crosses the seam.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::SourceKind;
use crate::state::{Result, Status};

pub mod clawlight;

/// A callback a source calls when its revision changes.
pub type SourceSink = Arc<dyn Fn(SourceEvent) + Send + Sync>;

/// Stable identity for a source: `"local"`, `"laptop-docker"`, ...
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceId(String);

impl SourceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for SourceId {
    fn from(id: &str) -> Self {
        Self(id.to_string())
    }
}

impl From<String> for SourceId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identity of a session, unique across every source.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionKey {
    pub source: SourceId,
    pub session_id: String,
}

impl SessionKey {
    pub fn new(source: SourceId, session_id: impl Into<String>) -> Self {
        Self {
            source,
            session_id: session_id.into(),
        }
    }
}

/// A display-ready session, normalized across sources. `status` reuses the
/// clawlight [`Status`] semantics; `name`, `badge`, and `last_updated` already
/// follow the display rules from [`crate::session`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Session {
    pub key: SessionKey,
    pub name: String,
    pub status: Status,
    /// Project path, if the source reported one.
    pub project: Option<String>,
    pub harness: Option<String>,
    /// Two-character harness badge, already derived.
    pub badge: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
    pub is_done: bool,
    /// The raw `last_updated` string, echoed verbatim so the wire output stays
    /// byte-identical to the pre-seam snapshot.
    pub last_updated: String,
}

impl Session {
    pub fn new(key: SessionKey, name: impl Into<String>, status: Status) -> Self {
        Self {
            key,
            name: name.into(),
            status,
            project: None,
            harness: None,
            badge: None,
            updated_at: None,
            is_done: status == Status::Done,
            last_updated: String::new(),
        }
    }

    pub fn with_project(mut self, project: impl Into<String>) -> Self {
        self.project = Some(project.into());
        self
    }

    pub fn with_harness(mut self, harness: impl Into<String>) -> Self {
        let harness = harness.into();
        self.badge = Some(crate::session::harness_badge(&harness));
        self.harness = Some(harness);
        self
    }

    pub fn with_updated_at(mut self, at: DateTime<Utc>) -> Self {
        self.last_updated = at.to_rfc3339();
        self.updated_at = Some(at);
        self
    }
}

/// Whether a source could be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceHealth {
    Ready,
    Missing,
    Unreadable(String),
}

impl SourceHealth {
    pub fn is_ready(&self) -> bool {
        matches!(self, SourceHealth::Ready)
    }

    pub fn is_missing(&self) -> bool {
        matches!(self, SourceHealth::Missing)
    }
}

/// What a source can do. The engine and UI gate commands on these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub remove_session: bool,
    pub clear_done: bool,
    pub push_events: bool,
}

impl Capabilities {
    pub const NONE: Self = Self {
        remove_session: false,
        clear_done: false,
        push_events: false,
    };
}

/// One source's view at a point in time. Sessions are post-staleness but
/// pre-retention: the engine owns global retention and counts.
#[derive(Debug, Clone, Serialize)]
pub struct SourceSnapshot {
    pub source: SourceId,
    /// Backing-store kind, so the engine can pick source-aware status wording.
    pub kind: SourceKind,
    /// Human-readable location (a file path or a hub URL) for status text.
    pub label: String,
    pub revision: u64,
    pub observed_at: DateTime<Utc>,
    pub health: SourceHealth,
    pub sessions: Vec<Session>,
    pub capabilities: Capabilities,
}

/// A change signal from a source. Deliberately coarse; the engine re-reads the
/// source on every signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceEvent {
    Changed,
}

/// A mutating request routed to a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceCommand {
    RemoveSession(SessionKey),
    ClearDone,
}

/// The adapter contract. Implementations must be cheap to snapshot and safe to
/// drive from several shells.
pub trait StateSource: Send + Sync {
    fn id(&self) -> SourceId;
    /// The kind of backing store. Defaults to [`SourceKind::File`]; remote
    /// adapters override it so status text can say "hub" instead of "state
    /// file".
    fn kind(&self) -> SourceKind {
        SourceKind::File
    }
    /// Human-readable location shown in status text: a path for a file source,
    /// a base URL for a hub. Defaults to the source id.
    fn label(&self) -> String {
        self.id().as_str().to_string()
    }
    fn capabilities(&self) -> Capabilities;
    /// Full current view. Cheap and idempotent.
    fn snapshot(&self, now: DateTime<Utc>) -> SourceSnapshot;
    /// Register a callback for revision changes. The engine decides how to
    /// debounce and merge.
    fn subscribe(&self, sink: SourceSink);
    /// Optional commands, gated by `capabilities`.
    fn command(&self, command: SourceCommand) -> Result<()>;
}

/// In-memory source for deterministic tests and demos. Holds no capabilities
/// by default.
pub struct FixtureSource {
    id: SourceId,
    kind: SourceKind,
    capabilities: Capabilities,
    health: Mutex<SourceHealth>,
    sessions: Mutex<Vec<Session>>,
    sinks: Mutex<Vec<SourceSink>>,
    revision: AtomicU64,
}

impl FixtureSource {
    pub fn new(id: impl Into<SourceId>) -> Self {
        Self {
            id: id.into(),
            kind: SourceKind::File,
            capabilities: Capabilities::NONE,
            health: Mutex::new(SourceHealth::Ready),
            sessions: Mutex::new(Vec::new()),
            sinks: Mutex::new(Vec::new()),
            revision: AtomicU64::new(0),
        }
    }

    pub fn with_sessions(mut self, sessions: Vec<Session>) -> Self {
        self.sessions = Mutex::new(sessions);
        self
    }

    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Override the backing-store kind, for tests that exercise hub wording.
    pub fn with_kind(mut self, kind: SourceKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn set_sessions(&self, sessions: Vec<Session>) {
        *self.sessions.lock().unwrap() = sessions;
        self.emit(SourceEvent::Changed);
    }

    pub fn set_health(&self, health: SourceHealth) {
        *self.health.lock().unwrap() = health;
        self.emit(SourceEvent::Changed);
    }

    pub fn emit(&self, event: SourceEvent) {
        let sinks = self.sinks.lock().unwrap().clone();
        for sink in sinks {
            sink(event);
        }
    }
}

impl StateSource for FixtureSource {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn kind(&self) -> SourceKind {
        self.kind
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn snapshot(&self, now: DateTime<Utc>) -> SourceSnapshot {
        SourceSnapshot {
            source: self.id.clone(),
            kind: self.kind,
            label: self.id.as_str().to_string(),
            revision: self.revision.fetch_add(1, Ordering::SeqCst) + 1,
            observed_at: now,
            health: self.health.lock().unwrap().clone(),
            sessions: self.sessions.lock().unwrap().clone(),
            capabilities: self.capabilities,
        }
    }

    fn subscribe(&self, sink: SourceSink) {
        self.sinks.lock().unwrap().push(sink);
    }

    fn command(&self, command: SourceCommand) -> Result<()> {
        match command {
            SourceCommand::RemoveSession(key) => {
                self.sessions.lock().unwrap().retain(|s| s.key != key);
            }
            SourceCommand::ClearDone => {
                self.sessions.lock().unwrap().retain(|s| !s.is_done);
            }
        }
        self.emit(SourceEvent::Changed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Status;

    #[test]
    fn fixture_reports_its_sessions_and_bumps_revision() {
        let source = FixtureSource::new("fixture").with_sessions(vec![Session::new(
            SessionKey::new(SourceId::new("fixture"), "a"),
            "A",
            Status::Active,
        )]);
        let now = Utc::now();
        let first = source.snapshot(now);
        let second = source.snapshot(now);
        assert_eq!(first.source, SourceId::new("fixture"));
        assert_eq!(first.kind, SourceKind::File);
        assert_eq!(first.label, "fixture");
        assert_eq!(first.health, SourceHealth::Ready);
        assert_eq!(first.sessions.len(), 1);
        assert_eq!(first.capabilities, Capabilities::NONE);
        assert!(second.revision > first.revision);
    }

    #[test]
    fn fixture_kind_and_label_follow_the_source() {
        let source = FixtureSource::new("remote").with_kind(SourceKind::Hub);
        assert_eq!(StateSource::kind(&source), SourceKind::Hub);
        assert_eq!(StateSource::label(&source), "remote");
        assert_eq!(source.snapshot(Utc::now()).kind, SourceKind::Hub);

        let fallback = FixtureSource::new("default");
        assert_eq!(StateSource::kind(&fallback), SourceKind::File);
        assert_eq!(StateSource::label(&fallback), "default");
    }

    #[test]
    fn fixture_command_mutates_and_notifies() {
        let hello = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hello.clone();
        let source = FixtureSource::new("fixture")
            .with_capabilities(Capabilities {
                remove_session: true,
                clear_done: true,
                push_events: true,
            })
            .with_sessions(vec![
                Session::new(
                    SessionKey::new(SourceId::new("fixture"), "a"),
                    "A",
                    Status::Done,
                ),
                Session::new(
                    SessionKey::new(SourceId::new("fixture"), "b"),
                    "B",
                    Status::Active,
                ),
            ]);
        source.subscribe(Arc::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        source.command(SourceCommand::ClearDone).unwrap();
        assert_eq!(source.snapshot(Utc::now()).sessions.len(), 1);
        assert_eq!(hello.load(Ordering::SeqCst), 1);
    }
}
