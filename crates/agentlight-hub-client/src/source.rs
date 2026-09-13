//! [`StateSource`] adapter over a remote hub.
//!
//! The hub owns retention, so a remote view can report fewer sessions than the
//! host's raw `state.json` would (e.g. `done` rows already capped). That is
//! acceptable for a remote view; the engine still applies its own limits
//! locally.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};

use agentlight_core::state::{Error, Result};
use agentlight_core::{
    Capabilities, DisplaySession, Session, SessionKey, Snapshot, SourceCommand, SourceEvent,
    SourceHealth, SourceId, SourceKind, SourceSink, SourceSnapshot, StateSource,
};

use crate::client::{HubClient, HubConfig};

/// Poll interval floor, so a misconfigured `poll_ms = 0` cannot spin a core.
const MIN_POLL_MS: u64 = 50;
/// Granularity of the interruptible sleep, so dropping the source stops its
/// thread promptly even when `poll_ms` is large.
const SLEEP_STEP: Duration = Duration::from_millis(50);

/// A [`StateSource`] backed by a running AgentLight hub.
pub struct HubSource {
    id: SourceId,
    client: HubClient,
    poll_ms: u64,
    revision: AtomicU64,
    sinks: Arc<Mutex<Vec<SourceSink>>>,
    poller: Mutex<Option<PollHandle>>,
}

struct PollHandle {
    stop: Arc<AtomicBool>,
}

impl HubSource {
    /// Build a source for `config`. The hub is probed lazily on the first
    /// [`snapshot`](StateSource::snapshot) or [`subscribe`](StateSource::subscribe).
    pub fn new(config: HubConfig) -> Self {
        Self {
            id: SourceId::new(config.id.clone()),
            poll_ms: config.poll_ms.max(MIN_POLL_MS),
            client: HubClient::new(config),
            revision: AtomicU64::new(0),
            sinks: Arc::new(Mutex::new(Vec::new())),
            poller: Mutex::new(None),
        }
    }

    /// The underlying client, for callers that need raw hub access.
    pub fn client(&self) -> &HubClient {
        &self.client
    }

    fn start_polling(&self) {
        let mut poller = self.poller.lock().unwrap();
        if poller.is_some() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let client = self.client.clone();
        let sinks = self.sinks.clone();
        let poll_ms = self.poll_ms;
        std::thread::Builder::new()
            .name("agentlight-hub-stream".to_string())
            .spawn(move || stream_loop(client, poll_ms, thread_stop, sinks))
            .ok();
        *poller = Some(PollHandle { stop });
    }
}

impl Drop for HubSource {
    fn drop(&mut self) {
        if let Ok(mut poller) = self.poller.lock() {
            if let Some(handle) = poller.take() {
                handle.stop.store(true, Ordering::SeqCst);
            }
        }
    }
}

impl StateSource for HubSource {
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
        let (health, sessions) = match self.client.snapshot() {
            Ok(hub) => {
                let health = health_of(&hub);
                let sessions = hub
                    .sessions
                    .iter()
                    .map(|row| normalize(&self.id, row))
                    .collect();
                (health, sessions)
            }
            Err(error) => (SourceHealth::Unreadable(error.to_string()), Vec::new()),
        };
        SourceSnapshot {
            source: self.id.clone(),
            kind: SourceKind::Hub,
            label: self.client.config().base_url.clone(),
            revision,
            observed_at: now,
            health,
            sessions,
            capabilities,
        }
    }

    fn subscribe(&self, sink: SourceSink) {
        self.sinks.lock().unwrap().push(sink);
        self.start_polling();
    }

    fn command(&self, command: SourceCommand) -> Result<()> {
        match command {
            SourceCommand::RemoveSession(key) => {
                // A key from another source is not ours to route.
                if key.source != self.id {
                    return Ok(());
                }
                self.client
                    .remove_session(None, &key.session_id)
                    .map(|_| ())
                    .map_err(remote_error)
            }
            SourceCommand::ClearDone => self.client.clear_done().map(|_| ()).map_err(remote_error),
        }
    }
}

/// Map the hub snapshot's `ok`/`exists` pair to source health exactly as the
/// local file adapter would: a reachable-but-unreadable hub is `Unreadable`,
/// a hub with no state is `Missing`.
fn health_of(hub: &Snapshot) -> SourceHealth {
    if hub.ok {
        SourceHealth::Ready
    } else if hub.exists {
        SourceHealth::Unreadable(
            hub.error
                .clone()
                .unwrap_or_else(|| "hub reported an unreadable state".to_string()),
        )
    } else {
        SourceHealth::Missing
    }
}

/// A hub row is already display-ready (`DisplaySession`); lift it into the
/// normalized [`Session`] model, keyed under this source.
fn normalize(source: &SourceId, row: &DisplaySession) -> Session {
    Session {
        key: SessionKey::new(source.clone(), row.session_id.clone()),
        name: row.name.clone(),
        status: row.status,
        project: (!row.project_path.is_empty()).then(|| row.project_path.clone()),
        harness: row.harness.clone(),
        badge: row.harness_badge.clone(),
        updated_at: parse_timestamp(&row.last_updated),
        is_done: row.is_done,
        last_updated: row.last_updated.clone(),
        url: row.url.clone(),
    }
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn remote_error(error: crate::client::HubError) -> Error {
    Error::Io(std::io::Error::other(error.to_string()))
}

/// Follow the hub's SSE stream forever, emitting [`SourceEvent::Changed`] on
/// every `update`/`lagged` frame. A successful connect also emits once, so a
/// subscriber can render before the first frame arrives. If the stream cannot
/// be established, fall back to one [`poll_once`] so the source still makes
/// progress. Reconnects after `poll_ms` (clamped by [`MIN_POLL_MS`]). Plain
/// `std::thread`, no async runtime.
fn stream_loop(
    client: HubClient,
    poll_ms: u64,
    stop: Arc<AtomicBool>,
    sinks: Arc<Mutex<Vec<SourceSink>>>,
) {
    let reconnect_delay = poll_ms.max(MIN_POLL_MS);
    let mut last: Option<String> = None;
    while !stop.load(Ordering::SeqCst) {
        match client.events_stream() {
            Ok(response) => {
                emit(&sinks);
                let reader = std::io::BufReader::new(response);
                crate::sse::read_frames(reader, &stop, &mut |event| {
                    if matches!(event, "update" | "lagged") {
                        emit(&sinks);
                    }
                });
            }
            Err(_) => poll_once(&client, &mut last, &sinks),
        }
        if !stop.load(Ordering::SeqCst) {
            sleep_interruptible(&stop, reconnect_delay);
        }
    }
}

/// One fallback poll: fetch the authoritative snapshot and emit only when its
/// signature changes (matching the pre-SSE behavior).
fn poll_once(client: &HubClient, last: &mut Option<String>, sinks: &Mutex<Vec<SourceSink>>) {
    let signature = client.snapshot().ok().map(|snapshot| signature(&snapshot));
    if signature != *last {
        *last = signature;
        emit(sinks);
    }
}

fn signature(snapshot: &Snapshot) -> String {
    serde_json::to_string(snapshot).unwrap_or_default()
}

fn emit(sinks: &Mutex<Vec<SourceSink>>) {
    let sinks = sinks.lock().unwrap().clone();
    for sink in sinks {
        sink(SourceEvent::Changed);
    }
}

fn sleep_interruptible(stop: &AtomicBool, poll_ms: u64) {
    let mut slept = 0;
    while slept < poll_ms {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let step = SLEEP_STEP.min(Duration::from_millis(poll_ms - slept));
        std::thread::sleep(step);
        slept += step.as_millis() as u64;
    }
}
