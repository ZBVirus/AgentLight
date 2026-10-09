//! The in-memory push source.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use agentlight_core::session::char_prefix;
use agentlight_core::{
    harness_badge, Capabilities, Result, Session, SessionKey, SourceCommand, SourceEvent,
    SourceHealth, SourceId, SourceKind, SourceSink, SourceSnapshot, StateSource, Status,
};

use crate::event::SessionEvent;

/// Default id for the event push source.
pub const EVENTS_SOURCE_ID: &str = "events";

/// How many removed-session tombstones are remembered. A producer heartbeat
/// re-reports its live set, so a session the user removed must not be
/// resurrected by the next `mode:"snapshot"` batch; genuinely new activity
/// (an upsert) clears the tombstone. Bounded so the durable store cannot grow
/// without limit.
const MAX_REMOVED: usize = 512;

/// The capabilities of a push source: it can be cleared and pruned locally, and
/// it accepts pushed events.
const EVENT_CAPABILITIES: Capabilities = Capabilities {
    remove_session: true,
    clear_done: true,
    push_events: true,
};

/// A [`StateSource`] fed by pushed [`SessionEvent`]s rather than a file.
///
/// Events are upserted by `session_id`. Health is `Missing` until the first
/// event arrives, so the UI shows "Waiting for agent events" even though there
/// is no file to read. When a store path is configured the live set is
/// persisted on every mutation and restored at startup, and an optional stale
/// window marks the source unreadable when the producer stops reporting.
pub struct EventPushSource {
    id: SourceId,
    sessions: Mutex<HashMap<String, Session>>,
    /// session_id -> producer, so a `snapshot` prunes only its own producer's
    /// absent sessions instead of the global live set.
    producers: Mutex<HashMap<String, String>>,
    /// Removed session ids, oldest first. A producer `snapshot` skips them so
    /// the next heartbeat cannot resurrect a session the user dismissed from
    /// the UI; an explicit upsert (real activity) clears the tombstone.
    removed: Mutex<Vec<String>>,
    sinks: Mutex<Vec<SourceSink>>,
    revision: AtomicU64,
    seen: AtomicBool,
    store: Option<PathBuf>,
    last_ingest: Mutex<Option<DateTime<Utc>>>,
    stale_after: Option<Duration>,
}

/// On-disk shape of the push source's durable store.
#[derive(Debug, Serialize, Deserialize)]
struct Store {
    version: u32,
    #[serde(default)]
    last_ingest: Option<String>,
    #[serde(default)]
    events: Vec<SessionEvent>,
    /// Removed-session tombstones, oldest first.
    #[serde(default)]
    removed: Vec<String>,
}

impl EventPushSource {
    pub fn new(id: impl Into<SourceId>) -> Self {
        Self::with_store(id, None)
    }

    /// Build a source that loads `path` when present and persists every
    /// mutation there. Restored rows are re-normalized from their stored
    /// [`SessionEvent`]s; the source reports ready once any are loaded.
    pub fn with_store(id: impl Into<SourceId>, path: Option<PathBuf>) -> Self {
        let source = Self {
            id: id.into(),
            sessions: Mutex::new(HashMap::new()),
            producers: Mutex::new(HashMap::new()),
            removed: Mutex::new(Vec::new()),
            sinks: Mutex::new(Vec::new()),
            revision: AtomicU64::new(0),
            seen: AtomicBool::new(false),
            store: path,
            last_ingest: Mutex::new(None),
            stale_after: None,
        };
        source.load_store();
        source
    }

    /// Mark the source stale when the producer has not reported within
    /// `stale_after`. `None` disables the check.
    pub fn with_stale_after(mut self, stale_after: Option<Duration>) -> Self {
        self.stale_after = stale_after;
        self
    }

    /// Upsert a batch of events. Returns the number accepted (the batch
    /// length); emits [`SourceEvent::Changed`] once if anything actually
    /// changed.
    pub fn apply(&self, events: &[SessionEvent]) -> usize {
        *self.last_ingest.lock().unwrap() = Some(Utc::now());
        let mut changed = false;
        // An explicit upsert is real activity: it may bring back a session the
        // user removed, so the tombstone is cleared before the upsert.
        for event in events {
            self.forget_removed(&event.session_id);
        }
        {
            let mut sessions = self.sessions.lock().unwrap();
            let mut producers = self.producers.lock().unwrap();
            for event in events {
                let next = normalize(&self.id, event);
                match sessions.get(&event.session_id) {
                    Some(previous) if *previous == next => {}
                    _ => {
                        sessions.insert(event.session_id.clone(), next);
                        changed = true;
                    }
                }
                match event.producer.as_deref() {
                    Some(producer) => {
                        producers.insert(event.session_id.clone(), producer.to_string());
                    }
                    None => {
                        producers.remove(&event.session_id);
                    }
                }
            }
        }
        if !events.is_empty() {
            self.seen.store(true, Ordering::SeqCst);
        }
        self.persist();
        if changed {
            self.emit(SourceEvent::Changed);
        }
        events.len()
    }

    /// Replace the producer's live set: upsert the batch, then prune every
    /// session whose `session_id` is absent from it **and** that belongs to the
    /// batch's producer. A batch with no `producer` on any event only upserts;
    /// it never prunes (the old global prune let one producer wipe another's
    /// sessions). Returns the batch length; emits [`SourceEvent::Changed`] once
    /// if anything actually changed.
    pub fn apply_snapshot(&self, events: &[SessionEvent]) -> usize {
        let accepted = events.len();
        *self.last_ingest.lock().unwrap() = Some(Utc::now());
        // A heartbeat snapshot must not resurrect sessions the user removed.
        // Filter tombstones out of the batch before it can upsert anything;
        // a prune still runs against the rest of the producer's live set. The
        // producer is read from the *original* batch so an all-tombstoned
        // heartbeat cannot degrade into a legacy global prune.
        let batch_producer = events
            .iter()
            .find_map(|event| event.producer.as_deref())
            .map(str::to_string);
        let events: Vec<&SessionEvent> = events
            .iter()
            .filter(|event| !self.is_removed(&event.session_id))
            .collect();
        let mut changed = false;
        {
            let mut sessions = self.sessions.lock().unwrap();
            let mut producers = self.producers.lock().unwrap();
            let keep: HashSet<&str> = events.iter().map(|e| e.session_id.as_str()).collect();
            let before = sessions.len();
            sessions.retain(|id, _| {
                if keep.contains(id.as_str()) {
                    return true;
                }
                match batch_producer.as_deref() {
                    // No producer: upsert-only, nothing is pruned.
                    None => true,
                    // Scoped snapshot: prune only the batch producer's sessions.
                    Some(producer) => producers.get(id).map(String::as_str) != Some(producer),
                }
            });
            if sessions.len() != before {
                changed = true;
            }
            producers.retain(|id, _| sessions.contains_key(id));
            for event in events {
                let next = normalize(&self.id, event);
                match sessions.get(&event.session_id) {
                    Some(previous) if *previous == next => {}
                    _ => {
                        sessions.insert(event.session_id.clone(), next);
                        changed = true;
                    }
                }
                match event.producer.as_deref() {
                    Some(producer) => {
                        producers.insert(event.session_id.clone(), producer.to_string());
                    }
                    None => {
                        producers.remove(&event.session_id);
                    }
                }
            }
        }
        if accepted > 0 {
            self.seen.store(true, Ordering::SeqCst);
        }
        self.persist();
        if changed {
            self.emit(SourceEvent::Changed);
        }
        accepted
    }

    /// Remove one session. Returns whether it existed.
    pub fn remove(&self, session_id: &str) -> bool {
        let removed = self.sessions.lock().unwrap().remove(session_id).is_some();
        self.producers.lock().unwrap().remove(session_id);
        if removed {
            self.remember_removed(session_id);
            self.persist();
            self.emit(SourceEvent::Changed);
        }
        removed
    }

    /// Remove every `done` session. Returns the number removed.
    pub fn clear_done(&self) -> usize {
        let mut sessions = self.sessions.lock().unwrap();
        let before = sessions.len();
        let removed_ids: Vec<String> = sessions
            .iter()
            .filter(|(_, session)| session.is_done)
            .map(|(id, _)| id.clone())
            .collect();
        sessions.retain(|_, session| !session.is_done);
        let removed = before - sessions.len();
        self.producers
            .lock()
            .unwrap()
            .retain(|id, _| sessions.contains_key(id));
        drop(sessions);
        if removed > 0 {
            for id in &removed_ids {
                self.remember_removed(id);
            }
            self.persist();
            self.emit(SourceEvent::Changed);
        }
        removed
    }

    /// Whether a session was removed from the UI and must not be resurrected
    /// by a producer snapshot.
    fn is_removed(&self, session_id: &str) -> bool {
        self.removed
            .lock()
            .unwrap()
            .iter()
            .any(|id| id == session_id)
    }

    /// Forget a tombstone: real activity for the session brings it back.
    fn forget_removed(&self, session_id: &str) {
        let mut removed = self.removed.lock().unwrap();
        if let Some(position) = removed.iter().position(|id| id == session_id) {
            removed.remove(position);
        }
    }

    /// Record a removal tombstone, newest last and bounded by [`MAX_REMOVED`].
    fn remember_removed(&self, session_id: &str) {
        let mut removed = self.removed.lock().unwrap();
        if removed.iter().any(|id| id == session_id) {
            return;
        }
        removed.push(session_id.to_string());
        if removed.len() > MAX_REMOVED {
            let excess = removed.len() - MAX_REMOVED;
            removed.drain(..excess);
        }
    }

    fn emit(&self, event: SourceEvent) {
        let sinks = self.sinks.lock().unwrap().clone();
        for sink in sinks {
            sink(event);
        }
    }

    /// Load the durable store when one exists. A missing or unreadable file is
    /// ignored: the source just starts empty, as before.
    fn load_store(&self) {
        let Some(path) = self.store.as_deref() else {
            return;
        };
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let Ok(store) = serde_json::from_slice::<Store>(&bytes) else {
            return;
        };

        let mut sessions = HashMap::new();
        let mut producers = HashMap::new();
        for event in &store.events {
            sessions.insert(event.session_id.clone(), normalize(&self.id, event));
            if let Some(producer) = event.producer.as_deref() {
                producers.insert(event.session_id.clone(), producer.to_string());
            }
        }
        if !sessions.is_empty() {
            self.seen.store(true, Ordering::SeqCst);
        }
        *self.sessions.lock().unwrap() = sessions;
        *self.producers.lock().unwrap() = producers;
        let mut removed = store.removed;
        if removed.len() > MAX_REMOVED {
            let excess = removed.len() - MAX_REMOVED;
            removed.drain(..excess);
        }
        *self.removed.lock().unwrap() = removed;
        if let Some(last) = store.last_ingest.as_deref().and_then(parse_timestamp) {
            *self.last_ingest.lock().unwrap() = Some(last);
        }
    }

    /// Write the live set through a same-directory temp file and atomic rename.
    /// Failures are ignored: persistence is best-effort and never blocks a read.
    fn persist(&self) {
        let Some(path) = self.store.as_deref() else {
            return;
        };
        let events = {
            let sessions = self.sessions.lock().unwrap();
            let producers = self.producers.lock().unwrap();
            let mut events: Vec<SessionEvent> = sessions
                .values()
                .map(|session| {
                    session_to_event(session, producers.get(&session.key.session_id).cloned())
                })
                .collect();
            events.sort_by(|a, b| a.session_id.cmp(&b.session_id));
            events
        };
        let removed = self.removed.lock().unwrap().clone();
        let store = Store {
            version: 1,
            last_ingest: self
                .last_ingest
                .lock()
                .unwrap()
                .map(|last| last.to_rfc3339()),
            events,
            removed,
        };
        let Ok(payload) = serde_json::to_vec(&store) else {
            return;
        };
        let _ = write_atomic(path, &payload);
    }

    fn health(&self, now: DateTime<Utc>) -> SourceHealth {
        if let Some(window) = self.stale_after {
            if let Some(last) = *self.last_ingest.lock().unwrap() {
                let window = chrono::Duration::from_std(window).unwrap_or(chrono::Duration::MAX);
                if now.signed_duration_since(last) > window {
                    return SourceHealth::Unreadable(format!(
                        "producer has not reported since {}",
                        last.to_rfc3339()
                    ));
                }
            }
        }
        if self.seen.load(Ordering::SeqCst) {
            SourceHealth::Ready
        } else {
            SourceHealth::Missing
        }
    }
}

impl StateSource for EventPushSource {
    fn id(&self) -> SourceId {
        self.id.clone()
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Push
    }

    fn capabilities(&self) -> Capabilities {
        EVENT_CAPABILITIES
    }

    fn snapshot(&self, now: DateTime<Utc>) -> SourceSnapshot {
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let mut sessions: Vec<Session> = self.sessions.lock().unwrap().values().cloned().collect();
        sessions.sort_by(|a, b| a.key.session_id.cmp(&b.key.session_id));
        SourceSnapshot {
            source: self.id.clone(),
            kind: SourceKind::Push,
            label: self.id.as_str().to_string(),
            revision,
            observed_at: now,
            health: self.health(now),
            sessions,
            capabilities: EVENT_CAPABILITIES,
        }
    }

    fn subscribe(&self, sink: SourceSink) {
        self.sinks.lock().unwrap().push(sink);
    }

    fn command(&self, command: SourceCommand) -> Result<()> {
        match command {
            SourceCommand::RemoveSession(key) => {
                self.remove(&key.session_id);
            }
            SourceCommand::ClearDone => {
                self.clear_done();
            }
        }
        Ok(())
    }
}

fn normalize(source: &SourceId, event: &SessionEvent) -> Session {
    let name = event
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Session {}", char_prefix(&event.session_id, 8)));

    let mut session = Session::new(
        SessionKey::new(source.clone(), event.session_id.clone()),
        name,
        event.status,
    );

    if let Some(project) = event
        .project_path
        .as_deref()
        .map(str::trim)
        .filter(|project| !project.is_empty())
    {
        session = session.with_project(project);
    }
    if let Some(harness) = event
        .harness
        .as_deref()
        .map(str::trim)
        .filter(|harness| !harness.is_empty())
    {
        session.harness = Some(harness.to_string());
        session.badge = Some(harness_badge(harness));
    }
    if let Some(at) = event.last_updated.as_deref().and_then(parse_timestamp) {
        session = session.with_updated_at(at);
    }
    session.last_updated = event.last_updated.clone().unwrap_or_default();
    session.is_done = event.status == Status::Done;
    if let Some(url) = event.url.as_deref() {
        session = session.with_url(url);
    }
    session
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Rebuild the wire event a normalized session came from, for persistence.
fn session_to_event(session: &Session, producer: Option<String>) -> SessionEvent {
    SessionEvent {
        session_id: session.key.session_id.clone(),
        status: session.status,
        name: Some(session.name.clone()),
        project_path: session.project.clone(),
        harness: session.harness.clone(),
        last_updated: (!session.last_updated.is_empty()).then(|| session.last_updated.clone()),
        url: session.url.clone(),
        producer,
    }
}

/// Write `bytes` to `path` via a same-directory temp file and atomic rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "push-state".to_string());
    let tmp = path.with_file_name(format!("{file_name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
    }

    fn event(id: &str, status: Status) -> SessionEvent {
        SessionEvent {
            session_id: id.to_string(),
            status,
            name: Some(format!("Session {id}")),
            project_path: None,
            harness: None,
            last_updated: None,
            url: None,
            producer: None,
        }
    }

    fn producer_event(producer: &str, id: &str, status: Status) -> SessionEvent {
        SessionEvent {
            producer: Some(producer.to_string()),
            ..event(id, status)
        }
    }

    fn counting_sink() -> (Arc<AtomicUsize>, SourceSink) {
        let count = Arc::new(AtomicUsize::new(0));
        let captured = count.clone();
        let sink: SourceSink = Arc::new(move |_| {
            captured.fetch_add(1, Ordering::SeqCst);
        });
        (count, sink)
    }

    #[test]
    fn apply_upserts_and_fires_once() {
        let source = EventPushSource::new(EVENTS_SOURCE_ID);
        let (count, sink) = counting_sink();
        source.subscribe(sink);

        let applied = source.apply(&[event("a", Status::Active), event("b", Status::NeedsHelp)]);
        assert_eq!(applied, 2);
        assert_eq!(count.load(Ordering::SeqCst), 1);

        let snapshot = source.snapshot(at());
        assert_eq!(snapshot.kind, SourceKind::Push);
        assert_eq!(snapshot.health, SourceHealth::Ready);
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(snapshot.capabilities, EVENT_CAPABILITIES);
        let a = snapshot
            .sessions
            .iter()
            .find(|s| s.key.session_id == "a")
            .unwrap();
        assert_eq!(a.name, "Session a");
        assert!(!a.is_done);
    }

    #[test]
    fn status_transitions_update_in_place() {
        let source = EventPushSource::new(EVENTS_SOURCE_ID);
        source.apply(&[event("a", Status::Active)]);
        source.apply(&[event("a", Status::NeedsHelp)]);

        let snapshot = source.snapshot(at());
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].status, Status::NeedsHelp);
    }

    #[test]
    fn identical_apply_does_not_refire() {
        let source = EventPushSource::new(EVENTS_SOURCE_ID);
        source.apply(&[event("a", Status::Active)]);
        let (count, sink) = counting_sink();
        source.subscribe(sink);

        assert_eq!(source.apply(&[event("a", Status::Active)]), 1);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn remove_and_clear_done_work() {
        let source = EventPushSource::new(EVENTS_SOURCE_ID);
        source.apply(&[
            event("a", Status::Done),
            event("b", Status::Active),
            event("c", Status::Done),
        ]);

        assert!(!source.remove("missing"));
        assert!(source.remove("b"));
        assert_eq!(source.snapshot(at()).sessions.len(), 2);

        assert_eq!(source.clear_done(), 2);
        assert!(source.snapshot(at()).sessions.is_empty());
    }

    #[test]
    fn health_goes_missing_to_ready_on_the_first_event() {
        let source = EventPushSource::new(EVENTS_SOURCE_ID);
        assert_eq!(source.snapshot(at()).health, SourceHealth::Missing);

        source.apply(&[event("a", Status::Active)]);
        assert_eq!(source.snapshot(at()).health, SourceHealth::Ready);
    }

    #[test]
    fn default_id_is_events() {
        assert_eq!(
            StateSource::id(&EventPushSource::new(EVENTS_SOURCE_ID)),
            SourceId::new("events")
        );
        assert_eq!(
            StateSource::kind(&EventPushSource::new(EVENTS_SOURCE_ID)),
            SourceKind::Push
        );
    }

    #[test]
    fn harness_and_timestamp_are_normalized() {
        let source = EventPushSource::new(EVENTS_SOURCE_ID);
        source.apply(&[SessionEvent {
            session_id: "a".to_string(),
            status: Status::Active,
            name: None,
            project_path: Some("/work/agentlight".to_string()),
            harness: Some("opencode".to_string()),
            last_updated: Some("2026-01-01T01:00:00Z".to_string()),
            url: None,
            producer: None,
        }]);

        let snapshot = source.snapshot(at());
        let a = &snapshot.sessions[0];
        assert_eq!(a.name, "Session a");
        assert_eq!(a.project.as_deref(), Some("/work/agentlight"));
        assert_eq!(a.harness.as_deref(), Some("opencode"));
        assert_eq!(a.badge.as_deref(), Some("op"));
        assert_eq!(a.updated_at, Some(at() + chrono::Duration::hours(1)));
        assert_eq!(a.last_updated, "2026-01-01T01:00:00Z");
    }

    #[test]
    fn store_round_trips_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("push-state.json");

        let first = EventPushSource::with_store(EVENTS_SOURCE_ID, Some(path.clone()));
        first.apply(&[
            SessionEvent {
                session_id: "a".to_string(),
                status: Status::NeedsHelp,
                name: Some("Fix auth".to_string()),
                project_path: Some("/work/agentlight".to_string()),
                harness: Some("opencode".to_string()),
                last_updated: Some("2026-01-01T01:00:00Z".to_string()),
                url: None,
                producer: Some("p1".to_string()),
            },
            event("b", Status::Done),
        ]);
        let expected = first.snapshot(at()).sessions;

        let restored = EventPushSource::with_store(EVENTS_SOURCE_ID, Some(path));
        let snapshot = restored.snapshot(at());
        assert_eq!(snapshot.health, SourceHealth::Ready);
        assert_eq!(snapshot.sessions, expected);
        assert_eq!(snapshot.sessions.len(), 2);
    }

    #[test]
    fn apply_snapshot_prunes_absent_ids() {
        let source = EventPushSource::new(EVENTS_SOURCE_ID);
        source.apply(&[
            producer_event("p1", "a", Status::Active),
            producer_event("p1", "b", Status::Active),
        ]);
        let (count, sink) = counting_sink();
        source.subscribe(sink);

        assert_eq!(
            source.apply_snapshot(&[
                producer_event("p1", "b", Status::Active),
                producer_event("p1", "c", Status::Active),
            ]),
            2
        );
        let ids: Vec<String> = source
            .snapshot(at())
            .sessions
            .iter()
            .map(|session| session.key.session_id.clone())
            .collect();
        assert_eq!(ids, vec!["b".to_string(), "c".to_string()]);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn apply_snapshot_scopes_to_producer() {
        let source = EventPushSource::new(EVENTS_SOURCE_ID);
        source.apply(&[
            producer_event("p1", "a", Status::Active),
            producer_event("p1", "b", Status::Active),
            producer_event("p2", "c", Status::Active),
        ]);

        // A snapshot from p1 prunes its own absent session (b) but never p2's c.
        assert_eq!(
            source.apply_snapshot(&[producer_event("p1", "a", Status::Active)]),
            1
        );
        let ids: Vec<String> = source
            .snapshot(at())
            .sessions
            .iter()
            .map(|session| session.key.session_id.clone())
            .collect();
        assert_eq!(ids, vec!["a".to_string(), "c".to_string()]);

        // A snapshot with no producer only upserts; it never prunes.
        assert_eq!(source.apply_snapshot(&[event("z", Status::Active)]), 1);
        let ids: Vec<String> = source
            .snapshot(at())
            .sessions
            .iter()
            .map(|session| session.key.session_id.clone())
            .collect();
        assert_eq!(ids, vec!["a".to_string(), "c".to_string(), "z".to_string()]);
    }

    #[test]
    fn apply_snapshot_does_not_resurrect_a_removed_session() {
        let source = EventPushSource::new(EVENTS_SOURCE_ID);
        source.apply(&[
            producer_event("p1", "a", Status::Active),
            producer_event("p1", "b", Status::Active),
        ]);

        assert!(source.remove("a"));
        assert_eq!(source.snapshot(at()).sessions.len(), 1);

        // The producer's authoritative live set still lists "a"; a heartbeat
        // must not bring it back.
        assert_eq!(
            source.apply_snapshot(&[
                producer_event("p1", "a", Status::Active),
                producer_event("p1", "b", Status::Active),
            ]),
            2
        );
        let ids: Vec<String> = source
            .snapshot(at())
            .sessions
            .iter()
            .map(|session| session.key.session_id.clone())
            .collect();
        assert_eq!(ids, vec!["b".to_string()]);
    }

    #[test]
    fn upsert_after_removal_brings_the_session_back() {
        let source = EventPushSource::new(EVENTS_SOURCE_ID);
        source.apply(&[producer_event("p1", "a", Status::Active)]);
        assert!(source.remove("a"));

        // A real activity event is not a heartbeat: it clears the tombstone.
        source.apply(&[producer_event("p1", "a", Status::NeedsHelp)]);
        let sessions = source.snapshot(at()).sessions;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].status, Status::NeedsHelp);

        // ...and a later snapshot is free to keep the session.
        source.apply_snapshot(&[producer_event("p1", "a", Status::Inactive)]);
        assert_eq!(source.snapshot(at()).sessions.len(), 1);
    }

    #[test]
    fn snapshot_with_only_removed_sessions_never_prunes_other_producers() {
        let source = EventPushSource::new(EVENTS_SOURCE_ID);
        source.apply(&[
            producer_event("p1", "a", Status::Active),
            producer_event("p2", "c", Status::Active),
        ]);
        source.remove("a");

        // p1's heartbeat now carries only the removed session. The filtered
        // batch is empty; p2's session must survive.
        source.apply_snapshot(&[producer_event("p1", "a", Status::Active)]);
        let ids: Vec<String> = source
            .snapshot(at())
            .sessions
            .iter()
            .map(|session| session.key.session_id.clone())
            .collect();
        assert_eq!(ids, vec!["c".to_string()]);
    }

    #[test]
    fn clear_done_tombstones_the_cleared_sessions() {
        let source = EventPushSource::new(EVENTS_SOURCE_ID);
        source.apply(&[
            producer_event("p1", "done1", Status::Done),
            producer_event("p1", "live", Status::Active),
        ]);
        assert_eq!(source.clear_done(), 1);

        // The next heartbeat still lists the cleared done session.
        source.apply_snapshot(&[
            producer_event("p1", "done1", Status::Done),
            producer_event("p1", "live", Status::Active),
        ]);
        let ids: Vec<String> = source
            .snapshot(at())
            .sessions
            .iter()
            .map(|session| session.key.session_id.clone())
            .collect();
        assert_eq!(ids, vec!["live".to_string()]);
    }

    #[test]
    fn store_round_trip_keeps_removed_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("push-state.json");

        let first = EventPushSource::with_store(EVENTS_SOURCE_ID, Some(path.clone()));
        first.apply(&[producer_event("p1", "a", Status::Active)]);
        first.remove("a");

        let restored = EventPushSource::with_store(EVENTS_SOURCE_ID, Some(path));
        assert_eq!(restored.snapshot(at()).sessions.len(), 0);
        restored.apply_snapshot(&[producer_event("p1", "a", Status::Active)]);
        assert_eq!(restored.snapshot(at()).sessions.len(), 0);
    }

    #[test]
    fn store_round_trip_keeps_producer_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("push-state.json");

        let first = EventPushSource::with_store(EVENTS_SOURCE_ID, Some(path.clone()));
        first.apply(&[
            producer_event("p1", "a", Status::Active),
            producer_event("p2", "c", Status::Active),
        ]);

        let restored = EventPushSource::with_store(EVENTS_SOURCE_ID, Some(path));
        // p1's absent session is pruned on p1's snapshot, but p2's survives,
        // so the producer mapping survived the reload with the sessions.
        restored.apply_snapshot(&[producer_event("p1", "x", Status::Active)]);
        let ids: Vec<String> = restored
            .snapshot(at())
            .sessions
            .iter()
            .map(|session| session.key.session_id.clone())
            .collect();
        assert_eq!(ids, vec!["c".to_string(), "x".to_string()]);
    }

    #[test]
    fn stale_health_past_the_window_still_returns_sessions() {
        let source =
            EventPushSource::new(EVENTS_SOURCE_ID).with_stale_after(Some(Duration::from_secs(60)));
        source.apply(&[event("a", Status::Active)]);
        assert_eq!(source.snapshot(Utc::now()).health, SourceHealth::Ready);

        let later = Utc::now() + chrono::Duration::seconds(61);
        let snapshot = source.snapshot(later);
        match &snapshot.health {
            SourceHealth::Unreadable(message) => {
                assert!(
                    message.contains("producer has not reported since"),
                    "{message}"
                );
            }
            other => panic!("expected unreadable, got {other:?}"),
        }
        assert_eq!(snapshot.sessions.len(), 1);
    }

    #[test]
    fn last_ingest_updates_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("push-state.json");
        let source = EventPushSource::with_store(EVENTS_SOURCE_ID, Some(path.clone()));
        assert!(source.last_ingest.lock().unwrap().is_none());

        source.apply(&[event("a", Status::Active)]);
        let first = source.last_ingest.lock().unwrap().expect("stamped");
        std::thread::sleep(Duration::from_millis(5));
        source.apply(&[event("b", Status::Active)]);
        let second = source.last_ingest.lock().unwrap().expect("restamped");
        assert!(second > first);

        let reloaded = EventPushSource::with_store(EVENTS_SOURCE_ID, Some(path));
        assert_eq!(reloaded.last_ingest.lock().unwrap().as_ref(), Some(&second));
    }
}
