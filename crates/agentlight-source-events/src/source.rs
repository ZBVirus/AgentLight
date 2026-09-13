//! The in-memory push source.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, Utc};

use agentlight_core::session::char_prefix;
use agentlight_core::{
    harness_badge, Capabilities, Result, Session, SessionKey, SourceCommand, SourceEvent,
    SourceHealth, SourceId, SourceKind, SourceSink, SourceSnapshot, StateSource, Status,
};

use crate::event::SessionEvent;

/// Default id for the event push source.
pub const EVENTS_SOURCE_ID: &str = "events";

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
/// is no file to read.
pub struct EventPushSource {
    id: SourceId,
    sessions: Mutex<HashMap<String, Session>>,
    sinks: Mutex<Vec<SourceSink>>,
    revision: AtomicU64,
    seen: AtomicBool,
}

impl EventPushSource {
    pub fn new(id: impl Into<SourceId>) -> Self {
        Self {
            id: id.into(),
            sessions: Mutex::new(HashMap::new()),
            sinks: Mutex::new(Vec::new()),
            revision: AtomicU64::new(0),
            seen: AtomicBool::new(false),
        }
    }

    /// Upsert a batch of events. Returns the number accepted (the batch
    /// length); emits [`SourceEvent::Changed`] once if anything actually
    /// changed.
    pub fn apply(&self, events: &[SessionEvent]) -> usize {
        let mut changed = false;
        {
            let mut sessions = self.sessions.lock().unwrap();
            for event in events {
                let next = normalize(&self.id, event);
                match sessions.get(&event.session_id) {
                    Some(previous) if *previous == next => {}
                    _ => {
                        sessions.insert(event.session_id.clone(), next);
                        changed = true;
                    }
                }
            }
        }
        if !events.is_empty() {
            self.seen.store(true, Ordering::SeqCst);
        }
        if changed {
            self.emit(SourceEvent::Changed);
        }
        events.len()
    }

    /// Remove one session. Returns whether it existed.
    pub fn remove(&self, session_id: &str) -> bool {
        let removed = self.sessions.lock().unwrap().remove(session_id).is_some();
        if removed {
            self.emit(SourceEvent::Changed);
        }
        removed
    }

    /// Remove every `done` session. Returns the number removed.
    pub fn clear_done(&self) -> usize {
        let mut sessions = self.sessions.lock().unwrap();
        let before = sessions.len();
        sessions.retain(|_, session| !session.is_done);
        let removed = before - sessions.len();
        drop(sessions);
        if removed > 0 {
            self.emit(SourceEvent::Changed);
        }
        removed
    }

    fn emit(&self, event: SourceEvent) {
        let sinks = self.sinks.lock().unwrap().clone();
        for sink in sinks {
            sink(event);
        }
    }

    fn health(&self) -> SourceHealth {
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
            health: self.health(),
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
    session
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
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
}
