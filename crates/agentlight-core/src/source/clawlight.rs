//! clawlight `state.json` file adapter.
//!
//! This is a direct lift of the parsing rules in [`crate::state`] plus the
//! `notify` + poll-backstop watcher that used to live in the Tauri shell. It
//! reads without locking, applies the 24h staleness reap, and writes through
//! the existing atomic `clear_session` / `clear_done` helpers.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use notify::Watcher;

use crate::config::SourceKind;
use crate::session::{char_prefix, harness_badge};
use crate::source::{
    Capabilities, Session, SessionKey, SourceCommand, SourceEvent, SourceHealth, SourceId,
    SourceSink, SourceSnapshot, StateSource,
};
use crate::state::{self, reap_stale, HookState, Load, Result, STALE_AFTER_HOURS};

/// Default id for the local clawlight file source.
pub const CLAWLIGHT_SOURCE_ID: &str = "local";

/// Reads and watches a clawlight `state.json`.
pub struct ClawlightFileSource {
    id: SourceId,
    path: PathBuf,
    poll_ms: u64,
    revision: AtomicU64,
    sinks: Arc<Mutex<Vec<SourceSink>>>,
    watcher: Mutex<Option<WatcherHandle>>,
}

struct WatcherHandle {
    stop: Arc<AtomicBool>,
}

impl ClawlightFileSource {
    pub fn new(path: PathBuf, poll_ms: u64) -> Self {
        Self {
            id: SourceId::new(CLAWLIGHT_SOURCE_ID),
            path,
            poll_ms,
            revision: AtomicU64::new(0),
            sinks: Arc::new(Mutex::new(Vec::new())),
            watcher: Mutex::new(None),
        }
    }

    /// Build the source for the file, poll interval, and path resolved from a
    /// config. Config changes are handled by replacing the whole source.
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self::new(config.state_file(), config.poll_ms)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn start_watching(&self) {
        let mut watcher = self.watcher.lock().unwrap();
        if watcher.is_some() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let sinks = self.sinks.clone();
        let path = self.path.clone();
        let poll = self.poll_ms.max(250);
        let thread_stop = stop.clone();
        std::thread::spawn(move || watch_loop(path, poll, thread_stop, sinks));
        *watcher = Some(WatcherHandle { stop });
    }
}

impl Drop for ClawlightFileSource {
    fn drop(&mut self) {
        if let Ok(mut watcher) = self.watcher.lock() {
            if let Some(handle) = watcher.take() {
                handle.stop.store(true, Ordering::SeqCst);
            }
        }
    }
}

impl StateSource for ClawlightFileSource {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            remove_session: true,
            clear_done: true,
            push_events: false,
        }
    }

    fn snapshot(&self, now: DateTime<Utc>) -> SourceSnapshot {
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let capabilities = self.capabilities();
        let (health, sessions) = match state::load_state(&self.path) {
            Load::Loaded(mut state) => {
                reap_stale(&mut state, now, STALE_AFTER_HOURS);
                (SourceHealth::Ready, build_sessions(&self.id, &state))
            }
            Load::Missing => (SourceHealth::Missing, Vec::new()),
            Load::Unreadable(e) => (SourceHealth::Unreadable(e.to_string()), Vec::new()),
        };
        SourceSnapshot {
            source: self.id.clone(),
            kind: SourceKind::File,
            label: self.path.to_string_lossy().to_string(),
            revision,
            observed_at: now,
            health,
            sessions,
            capabilities,
        }
    }

    fn subscribe(&self, sink: SourceSink) {
        self.sinks.lock().unwrap().push(sink);
        self.start_watching();
    }

    fn command(&self, command: SourceCommand) -> Result<()> {
        match command {
            SourceCommand::RemoveSession(key) => {
                state::clear_session(&self.path, &key.session_id)?;
            }
            SourceCommand::ClearDone => {
                state::clear_done(&self.path)?;
            }
        }
        Ok(())
    }
}

fn build_sessions(source: &SourceId, state: &HookState) -> Vec<Session> {
    state
        .sessions
        .iter()
        .map(|(id, session)| {
            let name = session
                .name
                .clone()
                .unwrap_or_else(|| format!("Session {}", char_prefix(id, 8)));
            let project_path = session.project_path.clone().unwrap_or_default();
            let project = if project_path.is_empty() {
                None
            } else {
                Some(project_path)
            };
            let harness = session
                .harness
                .as_ref()
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty());
            Session {
                key: SessionKey::new(source.clone(), id.clone()),
                name,
                status: session.status,
                badge: harness.as_deref().map(harness_badge),
                harness,
                project,
                updated_at: session.last_updated_at(),
                is_done: session.status == crate::state::Status::Done,
                last_updated: session.last_updated.clone().unwrap_or_default(),
                url: None,
            }
        })
        .collect()
}

/// `notify` gives instant updates where the filesystem supports it; a
/// `recv_timeout` poll is the backstop for network/bind-mount filesystems that
/// emit no events at all.
fn watch_loop(
    path: PathBuf,
    poll_ms: u64,
    stop: Arc<AtomicBool>,
    sinks: Arc<Mutex<Vec<SourceSink>>>,
) {
    let directory = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |_| {
        let _ = tx.send(());
    })
    .ok();
    if let Some(watcher) = watcher.as_mut() {
        let _ = watcher.watch(&directory, notify::RecursiveMode::NonRecursive);
    }

    let mut last = file_signature(&path);
    emit(&sinks);

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let _ = rx.recv_timeout(Duration::from_millis(poll_ms));
        let signature = file_signature(&path);
        if signature != last {
            last = signature;
            emit(&sinks);
        }
    }
}

fn emit(sinks: &Mutex<Vec<SourceSink>>) {
    let sinks = sinks.lock().unwrap().clone();
    for sink in sinks {
        sink(SourceEvent::Changed);
    }
}

fn file_signature(path: &Path) -> Option<(u64, u128)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((metadata.len(), modified))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-11T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn snapshot_reads_and_normalizes_a_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"sessions":{
                "a":{"status":"needs_help","name":"Fix auth","project_path":"/work/agentlight","harness":"opencode","last_updated":"2026-09-11T11:00:00Z"},
                "b":{"status":"done","name":"Old"}
            }}"#,
        )
        .unwrap();
        let source = ClawlightFileSource::new(path, 1500);
        let snapshot = source.snapshot(now());
        assert_eq!(snapshot.kind, SourceKind::File);
        assert!(snapshot.label.ends_with("state.json"));
        assert_eq!(snapshot.health, SourceHealth::Ready);
        assert_eq!(snapshot.sessions.len(), 2);
        let a = snapshot
            .sessions
            .iter()
            .find(|s| s.key.session_id == "a")
            .unwrap();
        assert_eq!(a.name, "Fix auth");
        assert_eq!(a.project.as_deref(), Some("/work/agentlight"));
        assert_eq!(a.harness.as_deref(), Some("opencode"));
        assert_eq!(a.badge.as_deref(), Some("op"));
        assert_eq!(a.updated_at, Some(now() - chrono::Duration::hours(1)));
        assert!(!a.is_done);
        let b = snapshot
            .sessions
            .iter()
            .find(|s| s.key.session_id == "b")
            .unwrap();
        assert!(b.is_done);
    }

    #[test]
    fn missing_and_unreadable_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let missing = ClawlightFileSource::new(dir.path().join("nope.json"), 1500);
        assert_eq!(missing.snapshot(now()).health, SourceHealth::Missing);

        let path = dir.path().join("broken.json");
        std::fs::write(&path, "{ not json").unwrap();
        let source = ClawlightFileSource::new(path, 1500);
        match source.snapshot(now()).health {
            SourceHealth::Unreadable(_) => {}
            other => panic!("expected unreadable, got {other:?}"),
        }
    }

    #[test]
    fn command_removes_sessions_from_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"sessions":{"a":{"status":"done"},"b":{"status":"active"}}}"#,
        )
        .unwrap();
        let source = ClawlightFileSource::new(path.clone(), 1500);
        source.command(SourceCommand::ClearDone).unwrap();
        let snapshot = source.snapshot(now());
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].key.session_id, "b");

        source
            .command(SourceCommand::RemoveSession(SessionKey::new(
                SourceId::new(CLAWLIGHT_SOURCE_ID),
                "b",
            )))
            .unwrap();
        assert!(source.snapshot(now()).sessions.is_empty());
    }

    #[test]
    fn from_config_resolves_the_state_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let config = Config {
            state_path: Some(path.to_string_lossy().to_string()),
            ..Config::default()
        };
        assert_eq!(ClawlightFileSource::from_config(&config).path(), path);
    }
}
