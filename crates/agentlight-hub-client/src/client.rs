//! Blocking HTTP/1.1 client for the AgentLight hub protocol.
//!
//! The wire contract lives in `docs/protocol.md`. Reads are a single
//! `GET /api/v1/snapshot`; mutations are a tagged `POST /api/v1/commands`. The
//! payload is the canonical `agentlight_core::Snapshot`, unchanged.
//!
//! Transport is plaintext only. `minreq` is built with default features off so
//! no TLS stack is linked; an `https://` `base_url` is not supported in this
//! version. Use the LAN or a VPN.

use serde::{Deserialize, Serialize};

use agentlight_core::Snapshot;
use agentlight_source_events::SessionEvent;

/// Default source id for a hub-backed source.
pub const DEFAULT_HUB_ID: &str = "hub";
/// Default hub poll interval, in milliseconds.
pub const DEFAULT_POLL_MS: u64 = 1500;
/// Default hub address: the loopback default `agentlight-server` binds.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8787";
/// Per-request timeout. The client is blocking, so a call cannot hang forever.
const REQUEST_TIMEOUT_SECS: u64 = 10;

/// Connection settings for a single hub.
///
/// `id` is the local [`SourceId`](agentlight_core::SourceId) this hub's
/// sessions are keyed under; `base_url` is the scheme/host/port with an
/// optional path prefix (a trailing slash is tolerated). `token` is sent as
/// `Authorization: Bearer <token>` when set, matching the hub's admin/device
/// token auth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubConfig {
    pub id: String,
    pub base_url: String,
    pub token: Option<String>,
    pub poll_ms: u64,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            id: DEFAULT_HUB_ID.to_string(),
            base_url: DEFAULT_BASE_URL.to_string(),
            token: None,
            poll_ms: DEFAULT_POLL_MS,
        }
    }
}

impl HubConfig {
    /// Config for `base_url` with the other fields at their defaults.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            ..Self::default()
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub fn with_poll_ms(mut self, poll_ms: u64) -> Self {
        self.poll_ms = poll_ms;
        self
    }
}

/// Local error type for hub traffic. The state crate's `Error` has no HTTP
/// variant, so callers that need one (the [`StateSource`](agentlight_core::StateSource)
/// adapter) stringify this at the seam.
#[derive(Debug)]
pub enum HubError {
    /// The connection failed, timed out, or produced an unreadable body.
    Transport(String),
    /// The hub answered with a non-2xx status.
    Http { status: u16, body: String },
    /// A 2xx body did not match the wire schema.
    Decode(serde_json::Error),
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HubError::Transport(message) => write!(f, "hub transport error: {message}"),
            HubError::Http { status, body } => {
                write!(f, "hub returned HTTP {status}: {body}")
            }
            HubError::Decode(error) => write!(f, "hub response decode error: {error}"),
        }
    }
}

impl std::error::Error for HubError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HubError::Decode(error) => Some(error),
            _ => None,
        }
    }
}

impl From<minreq::Error> for HubError {
    fn from(error: minreq::Error) -> Self {
        HubError::Transport(error.to_string())
    }
}

impl From<serde_json::Error> for HubError {
    fn from(error: serde_json::Error) -> Self {
        HubError::Decode(error)
    }
}

/// Tagged command body, serialized exactly as `docs/protocol.md` defines it.
#[derive(Debug, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum CommandBody<'a> {
    RemoveSession {
        #[serde(skip_serializing_if = "Option::is_none")]
        source: Option<&'a str>,
        session_id: &'a str,
    },
    ClearDone,
}

#[derive(Debug, Deserialize)]
struct CommandResponse {
    revision: u64,
}

/// Body for `POST /api/v1/ingest`: the canonical pushed-event shape. `mode` is
/// omitted for the default upsert so the body is unchanged from before.
#[derive(Debug, Serialize)]
struct IngestBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'a str>,
    events: &'a [SessionEvent],
}

#[derive(Debug, Deserialize)]
struct IngestResponse {
    accepted: usize,
}

/// Blocking HTTP client for one hub. Cheap to clone and safe to share.
#[derive(Debug, Clone)]
pub struct HubClient {
    config: HubConfig,
}

impl HubClient {
    pub fn new(config: HubConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &HubConfig {
        &self.config
    }

    /// `base_url` without a trailing slash, so endpoint joins are stable.
    fn base(&self) -> &str {
        self.config.base_url.trim_end_matches('/')
    }

    fn authorize(&self, request: minreq::Request) -> minreq::Request {
        match &self.config.token {
            Some(token) => request.with_header("Authorization", format!("Bearer {token}")),
            None => request,
        }
    }

    /// `GET /api/v1/snapshot` — the canonical hub snapshot.
    pub fn snapshot(&self) -> Result<Snapshot, HubError> {
        let url = format!("{}/api/v1/snapshot", self.base());
        let response = self
            .authorize(minreq::get(url))
            .with_timeout(REQUEST_TIMEOUT_SECS)
            .send()?;
        ensure_success(&response)?;
        let body = response.as_str()?;
        serde_json::from_str(body).map_err(HubError::Decode)
    }

    /// `POST /api/v1/commands` with a `remove_session` body. Returns the
    /// revision produced by the hub's follow-up refresh.
    pub fn remove_session(&self, source: Option<&str>, session_id: &str) -> Result<u64, HubError> {
        self.post_command(&CommandBody::RemoveSession { source, session_id })
    }

    /// `POST /api/v1/commands` with a `clear_done` body. Returns the revision
    /// produced by the hub's follow-up refresh.
    pub fn clear_done(&self) -> Result<u64, HubError> {
        self.post_command(&CommandBody::ClearDone)
    }

    /// `POST /api/v1/ingest` — upsert a batch of pushed [`SessionEvent`]s into a
    /// hub running in events mode. Returns the hub's `accepted` count (the batch
    /// length). Only meaningful against an events-mode hub; a file-mode hub
    /// answers `400 bad_request`.
    pub fn ingest(&self, events: &[SessionEvent]) -> Result<usize, HubError> {
        self.post_ingest(events, None)
    }

    /// `POST /api/v1/ingest` with `"mode":"snapshot"` — the batch is the
    /// producer's authoritative live set, so the hub prunes push-source
    /// sessions absent from it. Returns the hub's `accepted` count.
    pub fn ingest_snapshot(&self, events: &[SessionEvent]) -> Result<usize, HubError> {
        self.post_ingest(events, Some("snapshot"))
    }

    fn post_ingest(&self, events: &[SessionEvent], mode: Option<&str>) -> Result<usize, HubError> {
        let url = format!("{}/api/v1/ingest", self.base());
        let payload = serde_json::to_string(&IngestBody { mode, events })?;
        let response = self
            .authorize(minreq::post(url))
            .with_header("Content-Type", "application/json")
            .with_body(payload)
            .with_timeout(REQUEST_TIMEOUT_SECS)
            .send()?;
        ensure_success(&response)?;
        let body = response.as_str()?;
        let parsed: IngestResponse = serde_json::from_str(body)?;
        Ok(parsed.accepted)
    }

    fn post_command(&self, body: &CommandBody<'_>) -> Result<u64, HubError> {
        let url = format!("{}/api/v1/commands", self.base());
        let payload = serde_json::to_string(body)?;
        let response = self
            .authorize(minreq::post(url))
            .with_header("Content-Type", "application/json")
            .with_body(payload)
            .with_timeout(REQUEST_TIMEOUT_SECS)
            .send()?;
        ensure_success(&response)?;
        let body = response.as_str()?;
        let parsed: CommandResponse = serde_json::from_str(body)?;
        Ok(parsed.revision)
    }
}

fn ensure_success(response: &minreq::Response) -> Result<(), HubError> {
    if (200..300).contains(&response.status_code) {
        return Ok(());
    }
    let body = response
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|_| "<non-utf8 body>".to_string());
    Err(HubError::Http {
        status: response.status_code,
        body,
    })
}
