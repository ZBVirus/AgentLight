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
    /// The most recent snapshot the background thread fetched. Reads serve this
    /// so a render never blocks on the network; the thread refreshes it.
    cache: Arc<Mutex<Option<CacheEntry>>>,
}

/// The display-ready slice of a hub snapshot, cached for non-blocking reads.
#[derive(Clone)]
struct CacheEntry {
    health: SourceHealth,
    sessions: Vec<Session>,
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
            cache: Arc::new(Mutex::new(None)),
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
        let cache = self.cache.clone();
        let id = self.id.clone();
        let poll_ms = self.poll_ms;
        std::thread::Builder::new()
            .name("agentlight-hub-stream".to_string())
            .spawn(move || stream_loop(client, id, poll_ms, thread_stop, sinks, cache))
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
            bulk_clear_done: true,
            push_events: false,
        }
    }

    fn snapshot(&self, now: DateTime<Utc>) -> SourceSnapshot {
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let capabilities = self.capabilities();
        // Serve the background thread's latest fetch so a render never blocks on
        // the network. With no background thread (a direct caller or a test),
        // fetch synchronously as before.
        let running = self.poller.lock().unwrap().is_some();
        let entry = if running {
            self.cache.lock().unwrap().clone()
        } else {
            Some(fetch_entry(&self.client, &self.id))
        };
        let (health, sessions) = match entry {
            Some(entry) => (entry.health, entry.sessions),
            // Subscribed, but the first fetch has not landed yet.
            None => (SourceHealth::Missing, Vec::new()),
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
        let reply = match command {
            SourceCommand::RemoveSession(key) => {
                // A key from another source is not ours to route.
                if key.source != self.id {
                    return Ok(());
                }
                self.client
                    .remove_session(None, &key.session_id)
                    .map_err(remote_error)?
            }
            SourceCommand::ClearDone => self.client.clear_done().map_err(remote_error)?,
        };
        // A new-enough hub returns the refreshed snapshot with the reply, so
        // one round trip replaces POST + GET — which is what makes Remove and
        // Clear done feel immediate on a slow or remote link. An older hub
        // omits it and the cache is fetched as before.
        match reply.snapshot {
            Some(snapshot) => {
                *self.cache.lock().unwrap() = Some(entry_from_snapshot(&self.id, &snapshot))
            }
            None => refresh_cache(&self.client, &self.id, &self.cache),
        }
        emit(&self.sinks);
        Ok(())
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
        subagent: row.is_subagent,
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
    id: SourceId,
    poll_ms: u64,
    stop: Arc<AtomicBool>,
    sinks: Arc<Mutex<Vec<SourceSink>>>,
    cache: Arc<Mutex<Option<CacheEntry>>>,
) {
    let reconnect_delay = poll_ms.max(MIN_POLL_MS);
    let mut last: Option<String> = None;
    // Prime the cache once before the first connect so an unreachable hub
    // reports its error promptly without the UI ever blocking on it.
    poll_once(&client, &id, &mut last, &sinks, &cache);
    while !stop.load(Ordering::SeqCst) {
        match client.events_stream() {
            Ok(response) => {
                // Refresh and signal on every connect so a subscriber renders
                // (and a reconnect is visible) before the first frame.
                refresh_cache(&client, &id, &cache);
                emit(&sinks);
                let reader = std::io::BufReader::new(response);
                crate::sse::read_frames(reader, &stop, &mut |event, data| match event {
                    "update" => {
                        // The frame carries the full refreshed snapshot, so use
                        // it directly instead of a follow-up GET; a payload we
                        // cannot parse (an older hub) falls back to a fetch.
                        match serde_json::from_str::<UpdateFrame>(data) {
                            Ok(frame) => {
                                *cache.lock().unwrap() =
                                    Some(entry_from_snapshot(&id, &frame.snapshot));
                                emit(&sinks);
                            }
                            Err(_) => {
                                refresh_cache(&client, &id, &cache);
                                emit(&sinks);
                            }
                        }
                    }
                    "lagged" => {
                        // The stream fell behind and this frame has no state:
                        // re-fetch so nothing is missed.
                        refresh_cache(&client, &id, &cache);
                        emit(&sinks);
                    }
                    _ => {}
                });
            }
            Err(_) => poll_once(&client, &id, &mut last, &sinks, &cache),
        }
        if !stop.load(Ordering::SeqCst) {
            sleep_interruptible(&stop, reconnect_delay);
        }
    }
}

/// Fetch the authoritative snapshot into the cache without signaling.
fn refresh_cache(client: &HubClient, id: &SourceId, cache: &Mutex<Option<CacheEntry>>) {
    *cache.lock().unwrap() = Some(fetch_entry(client, id));
}

/// Fetch the authoritative snapshot, refresh the cache, and emit only when its
/// signature changes (matching the pre-SSE behavior).
fn poll_once(
    client: &HubClient,
    id: &SourceId,
    last: &mut Option<String>,
    sinks: &Mutex<Vec<SourceSink>>,
    cache: &Mutex<Option<CacheEntry>>,
) {
    let entry = fetch_entry(client, id);
    let current = cache_signature(&entry);
    *cache.lock().unwrap() = Some(entry);
    if Some(&current) != last.as_ref() {
        *last = Some(current);
        emit(sinks);
    }
}

/// Fetch one snapshot from the hub into a cache entry, mapping an error to an
/// unreadable health rather than losing it.
fn fetch_entry(client: &HubClient, id: &SourceId) -> CacheEntry {
    match client.snapshot() {
        Ok(hub) => entry_from_snapshot(id, &hub),
        Err(error) => CacheEntry {
            health: SourceHealth::Unreadable(error.to_string()),
            sessions: Vec::new(),
        },
    }
}

/// The minimal slice of an SSE `update` frame the source needs: the full wire
/// snapshot. Other `Update` fields (`notifications`, `alarms`) are ignored.
#[derive(serde::Deserialize)]
struct UpdateFrame {
    snapshot: Snapshot,
}

/// A hub snapshot rendered into the cache: source health plus normalized rows.
fn entry_from_snapshot(id: &SourceId, hub: &Snapshot) -> CacheEntry {
    CacheEntry {
        health: health_of(hub),
        sessions: hub.sessions.iter().map(|row| normalize(id, row)).collect(),
    }
}

fn cache_signature(entry: &CacheEntry) -> String {
    let health = format!("{:?}", entry.health);
    let sessions = serde_json::to_string(&entry.sessions).unwrap_or_default();
    format!("{health}|{sessions}")
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
