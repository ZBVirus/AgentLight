//! The policy engine.
//!
//! One or more [`StateSource`]s feed normalized sessions; the engine merges
//! them, applies retention, computes the aggregate and counts, and emits
//! revisioned [`Update`]s plus notification edges. It is synchronous and
//! runtime-free, so any shell (Tauri, server, CLI) can drive the same policy.
//!
//! Notification edge detection, source lifecycle, and merge policy move here
//! from the Tauri shell; the shell only delivers [`Update`]s.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::config::Config;
use crate::session::{display_session, DisplaySession, DONE_RETENTION};
use crate::snapshot::{Counts, Snapshot};
use crate::source::{Session, SessionKey, SourceCommand, SourceId, SourceSnapshot, StateSource};
use crate::state::{aggregate_statuses, Error, Result, Status};

/// A callback the engine calls for every published update.
pub type UpdateSink = Arc<dyn Fn(Update) + Send + Sync>;

/// How urgent a notification is. Additive; the desktop shell does not use it
/// yet, but transports (push, SSE) will.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

/// An edge-triggered notification: a session entering `needs_help`.
#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    pub key: SessionKey,
    pub title: String,
    pub body: String,
    pub urgency: Urgency,
}

/// A full state update: the wire snapshot, the notifications that fired with
/// it, and the engine's monotonic revision.
#[derive(Debug, Clone, Serialize)]
pub struct Update {
    pub revision: u64,
    pub snapshot: Snapshot,
    pub notifications: Vec<Notification>,
}

/// Owns sources and policy. Cheap to clone through an `Arc`.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    sources: Mutex<Vec<Arc<dyn StateSource>>>,
    config: Mutex<Config>,
    revision: AtomicU64,
    previous: Mutex<HashMap<SessionKey, Status>>,
    subscribers: Mutex<Vec<UpdateSink>>,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        Self {
            inner: Arc::new(Inner {
                sources: Mutex::new(Vec::new()),
                config: Mutex::new(config),
                revision: AtomicU64::new(0),
                previous: Mutex::new(HashMap::new()),
                subscribers: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Register a source and start forwarding its changes.
    pub fn add_source(&self, source: Arc<dyn StateSource>) {
        self.inner.add_source(source);
    }

    /// Replace every source with the same id (used on a config change).
    pub fn replace_source(&self, source: Arc<dyn StateSource>) {
        self.inner.replace_source(source);
    }

    pub fn set_config(&self, config: Config) {
        *self.inner.config.lock().unwrap() = config;
    }

    pub fn config(&self) -> Config {
        self.inner.config.lock().unwrap().clone()
    }

    /// Current wire snapshot at an injected clock.
    pub fn snapshot(&self, now: DateTime<Utc>) -> Snapshot {
        self.inner.snapshot(now)
    }

    pub fn snapshot_now(&self) -> Snapshot {
        self.snapshot(Utc::now())
    }

    /// Counts over every source session, before retention.
    pub fn counts_now(&self) -> Counts {
        self.inner.counts(Utc::now())
    }

    /// Whether a key is present in any source (pre-retention).
    pub fn has_session(&self, key: &SessionKey) -> bool {
        self.inner.has_session(key, Utc::now())
    }

    pub fn subscribe(&self, sink: UpdateSink) {
        self.inner.subscribers.lock().unwrap().push(sink);
    }

    /// Route a mutating command to the source that owns the target.
    pub fn dispatch(&self, command: SourceCommand) -> Result<()> {
        self.inner.dispatch(command)
    }

    /// Recompute and publish an update now. Used after an explicit command so
    /// the frontend sees the result without waiting for the watcher.
    pub fn refresh(&self) -> Update {
        self.inner.publish(Utc::now())
    }

    pub fn source_id(&self) -> Option<SourceId> {
        self.inner.sources.lock().unwrap().first().map(|s| s.id())
    }
}

impl Inner {
    fn add_source(self: &Arc<Self>, source: Arc<dyn StateSource>) {
        self.sources.lock().unwrap().push(source.clone());
        let weak = Arc::downgrade(self);
        source.subscribe(Arc::new(move |_event| {
            if let Some(inner) = weak.upgrade() {
                inner.publish(Utc::now());
            }
        }));
    }

    fn replace_source(self: &Arc<Self>, source: Arc<dyn StateSource>) {
        {
            let mut sources = self.sources.lock().unwrap();
            sources.retain(|existing| existing.id() != source.id());
            sources.push(source.clone());
        }
        let weak = Arc::downgrade(self);
        source.subscribe(Arc::new(move |_event| {
            if let Some(inner) = weak.upgrade() {
                inner.publish(Utc::now());
            }
        }));
    }

    fn dispatch(&self, command: SourceCommand) -> Result<()> {
        let sources = self.sources.lock().unwrap().clone();
        let source = match &command {
            SourceCommand::RemoveSession(key) => sources.iter().find(|s| s.id() == key.source),
            SourceCommand::ClearDone => sources.first(),
        };
        match source {
            Some(source) => source.command(command),
            None => Err(Error::Unreadable(
                "no source can handle the command".to_string(),
            )),
        }
    }

    fn source_snapshots(&self, now: DateTime<Utc>) -> Vec<SourceSnapshot> {
        let sources = self.sources.lock().unwrap().clone();
        sources.iter().map(|source| source.snapshot(now)).collect()
    }

    fn models(&self, now: DateTime<Utc>) -> Vec<Session> {
        let mut models: Vec<Session> = self
            .source_snapshots(now)
            .into_iter()
            .flat_map(|snapshot| snapshot.sessions)
            .collect();
        models.sort_by(compare_sessions);
        models
    }

    fn counts(&self, now: DateTime<Utc>) -> Counts {
        count_models(&self.models(now))
    }

    fn has_session(&self, key: &SessionKey, now: DateTime<Utc>) -> bool {
        self.models(now).iter().any(|model| &model.key == key)
    }

    fn snapshot(&self, now: DateTime<Utc>) -> Snapshot {
        let sources = self.source_snapshots(now);
        self.render(&sources, now)
    }

    fn publish(&self, now: DateTime<Utc>) -> Update {
        let sources = self.source_snapshots(now);
        let snapshot = self.render(&sources, now);
        let notifications = self.notification_edge(&sources);
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let update = Update {
            revision,
            snapshot,
            notifications,
        };
        let subscribers = self.subscribers.lock().unwrap().clone();
        for sink in subscribers {
            sink(update.clone());
        }
        update
    }

    fn render(&self, sources: &[SourceSnapshot], now: DateTime<Utc>) -> Snapshot {
        let config = self.config.lock().unwrap().clone();

        let ok = !sources.is_empty() && sources.iter().all(|s| s.health.is_ready());
        let exists = sources.iter().any(|s| !s.health.is_missing());
        let error = if ok {
            None
        } else if let Some(source) = sources.iter().find(|s| !s.health.is_ready()) {
            match &source.health {
                crate::source::SourceHealth::Unreadable(reason) => {
                    Some(format!("Could not read state file: {reason}"))
                }
                _ => Some("Waiting for clawlight state file".to_string()),
            }
        } else {
            Some("Waiting for clawlight state file".to_string())
        };

        let mut sessions: Vec<Session> = sources
            .iter()
            .flat_map(|source| source.sessions.iter().cloned())
            .collect();
        sessions.sort_by(compare_sessions);

        let counts = count_models(&sessions);
        let aggregate = aggregate_statuses(sessions.iter().map(|s| s.status), config.yellow_mode)
            .as_str()
            .to_string();

        let mut rows: Vec<DisplaySession> = sessions.iter().map(display_session).collect();
        apply_retention(&mut rows, config.show_done);

        Snapshot {
            ok,
            error,
            state_path: config.state_file().to_string_lossy().to_string(),
            exists,
            aggregate,
            counts,
            sessions: rows,
            yellow_mode: yellow_mode_str(config.yellow_mode),
            generated_at: now.to_rfc3339(),
        }
    }

    fn notification_edge(&self, sources: &[SourceSnapshot]) -> Vec<Notification> {
        if !self.config.lock().unwrap().notifications {
            return Vec::new();
        }
        let mut previous = self.previous.lock().unwrap();
        let models: Vec<&Session> = sources
            .iter()
            .flat_map(|source| source.sessions.iter())
            .collect();

        let mut notifications = Vec::new();
        for model in &models {
            if model.status == Status::NeedsHelp
                && previous.get(&model.key) != Some(&Status::NeedsHelp)
            {
                notifications.push(Notification {
                    key: model.key.clone(),
                    title: "AgentLight".to_string(),
                    body: format!("\"{}\" needs help", model.name),
                    urgency: Urgency::Critical,
                });
            }
        }

        previous.clear();
        for model in &models {
            previous.insert(model.key.clone(), model.status);
        }
        notifications
    }
}

fn compare_sessions(a: &Session, b: &Session) -> std::cmp::Ordering {
    a.status
        .urgency()
        .cmp(&b.status.urgency())
        .then_with(|| b.updated_at.cmp(&a.updated_at))
}

fn count_models(models: &[Session]) -> Counts {
    let mut counts = Counts::default();
    for model in models {
        counts.total += 1;
        match model.status {
            Status::NeedsHelp => counts.needs_help += 1,
            Status::Active => counts.active += 1,
            Status::Inactive => counts.inactive += 1,
            Status::Done => counts.done += 1,
        }
    }
    counts
}

fn apply_retention(rows: &mut Vec<DisplaySession>, show_done: bool) {
    if show_done {
        return;
    }
    let mut done_seen = 0usize;
    rows.retain(|row| {
        if row.is_done {
            done_seen += 1;
            done_seen <= DONE_RETENTION
        } else {
            true
        }
    });
}

fn yellow_mode_str(mode: crate::config::YellowMode) -> String {
    match mode {
        crate::config::YellowMode::AnyInactive => "any_inactive".to_string(),
        crate::config::YellowMode::ActiveWins => "active_wins".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::FixtureSource;
    use chrono::TimeZone;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, hour, 0, 0).unwrap()
    }

    fn frame(source: &str, id: &str, status: Status, hour: u32) -> Session {
        Session::new(
            SessionKey::new(SourceId::new(source), id),
            format!("Session {id}"),
            status,
        )
        .with_updated_at(at(hour))
    }

    #[test]
    fn merges_fixture_sources_and_aggregates() {
        let engine = Engine::new(Config::default());
        engine.add_source(Arc::new(
            FixtureSource::new("a").with_sessions(vec![frame("a", "1", Status::Active, 1)]),
        ));
        engine.add_source(Arc::new(
            FixtureSource::new("b").with_sessions(vec![frame("b", "1", Status::NeedsHelp, 2)]),
        ));

        let snapshot = engine.snapshot(at(3));
        assert!(snapshot.ok);
        assert_eq!(snapshot.counts.total, 2);
        assert_eq!(snapshot.counts.needs_help, 1);
        assert_eq!(snapshot.counts.active, 1);
        assert_eq!(snapshot.aggregate, "red");
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(snapshot.sessions[0].status, Status::NeedsHelp);
        assert_eq!(snapshot.sessions[0].session_id, "1");
    }

    #[test]
    fn revision_is_monotonic() {
        let engine = Engine::new(Config::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        engine.subscribe(Arc::new(move |update: Update| {
            captured.lock().unwrap().push(update.revision);
        }));
        let source = Arc::new(FixtureSource::new("a"));
        engine.add_source(source.clone());

        let first = engine.refresh().revision;
        source.set_sessions(vec![frame("a", "1", Status::Active, 1)]);
        let third = engine.refresh().revision;

        assert!(third > first);
        let seen = seen.lock().unwrap().clone();
        assert!(seen.windows(2).all(|pair| pair[1] > pair[0]));
    }

    #[test]
    fn notification_fires_once_when_entering_needs_help() {
        let config = Config {
            notifications: true,
            ..Config::default()
        };
        let engine = Engine::new(config);
        let source = Arc::new(FixtureSource::new("a").with_sessions(vec![frame(
            "a",
            "1",
            Status::Active,
            1,
        )]));
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let captured = notifications.clone();
        engine.subscribe(Arc::new(move |update: Update| {
            captured.lock().unwrap().extend(update.notifications);
        }));
        engine.add_source(source.clone());
        engine.refresh();

        source.set_sessions(vec![frame("a", "1", Status::NeedsHelp, 2)]);
        {
            let notifications = notifications.lock().unwrap();
            assert_eq!(notifications.len(), 1);
            assert_eq!(notifications[0].body, "\"Session 1\" needs help");
            assert_eq!(notifications[0].key.session_id, "1");
        }

        source.set_sessions(vec![frame("a", "1", Status::NeedsHelp, 3)]);
        assert_eq!(notifications.lock().unwrap().len(), 1);
    }

    #[test]
    fn notifications_are_suppressed_when_disabled() {
        let engine = Engine::new(Config::default());
        let source = Arc::new(FixtureSource::new("a").with_sessions(vec![frame(
            "a",
            "1",
            Status::NeedsHelp,
            1,
        )]));
        engine.add_source(source);
        assert!(engine.refresh().notifications.is_empty());
    }

    #[test]
    fn retention_keeps_the_newest_five_done() {
        let engine = Engine::new(Config::default());
        let mut sessions: Vec<Session> = (1..=8)
            .map(|i| frame("a", &format!("d{i}"), Status::Done, i))
            .collect();
        sessions.push(frame("a", "live", Status::Active, 1));
        engine.add_source(Arc::new(FixtureSource::new("a").with_sessions(sessions)));

        let snapshot = engine.snapshot(at(12));
        assert_eq!(snapshot.counts.done, 8);
        assert_eq!(
            snapshot.sessions.iter().filter(|s| s.is_done).count(),
            DONE_RETENTION
        );
        assert_eq!(snapshot.sessions.iter().filter(|s| !s.is_done).count(), 1);
    }

    #[test]
    fn replace_source_swaps_the_backing_data() {
        let engine = Engine::new(Config::default());
        engine.add_source(Arc::new(
            FixtureSource::new("a").with_sessions(vec![frame("a", "1", Status::Active, 1)]),
        ));
        assert_eq!(engine.snapshot(at(2)).counts.total, 1);

        engine.replace_source(Arc::new(FixtureSource::new("a").with_sessions(vec![
            frame("a", "2", Status::NeedsHelp, 2),
            frame("a", "3", Status::Active, 2),
        ])));
        let snapshot = engine.snapshot(at(3));
        assert_eq!(snapshot.counts.total, 2);
        assert_eq!(snapshot.counts.needs_help, 1);
        assert!(engine.has_session(&SessionKey::new(SourceId::new("a"), "2")));
        assert!(!engine.has_session(&SessionKey::new(SourceId::new("a"), "1")));
        assert_eq!(engine.counts_now().total, 2);
    }

    #[test]
    fn clawlight_adapter_snapshot_matches_the_legacy_builder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"sessions":{
                "a":{"status":"needs_help","name":"A","last_updated":"2026-01-01T01:00:00Z"},
                "b":{"status":"active","name":"B","harness":"opencode","project_path":"/work/agentlight"},
                "c":{"status":"inactive","name":"C"},
                "d":{"status":"done","name":"D"}
            }}"#,
        )
        .unwrap();
        let config = Config {
            state_path: Some(path.to_string_lossy().to_string()),
            ..Config::default()
        };
        let now = at(2);

        let engine = Engine::new(config.clone());
        engine.add_source(Arc::new(
            crate::source::clawlight::ClawlightFileSource::new(path.clone(), 1500),
        ));
        let from_engine = engine.snapshot(now);
        let legacy = crate::snapshot::build_snapshot_at(&path, &config, now);

        assert_eq!(from_engine.ok, legacy.ok);
        assert_eq!(from_engine.exists, legacy.exists);
        assert_eq!(from_engine.error, legacy.error);
        assert_eq!(from_engine.aggregate, legacy.aggregate);
        assert_eq!(from_engine.counts, legacy.counts);
        assert_eq!(from_engine.sessions, legacy.sessions);
        assert_eq!(from_engine.yellow_mode, legacy.yellow_mode);
    }
}
