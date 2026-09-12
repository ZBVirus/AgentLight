//! The HTTP handlers. Async wrappers only: every state computation goes
//! through `spawn_blocking` into the synchronous engine.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::Html;
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use agentlight_core::{SessionKey, Snapshot, SourceCommand, SourceId, Update};

use crate::app::AppState;
use crate::devices::DeviceInfo;
use crate::error::ApiError;
use crate::SCHEMA_VERSION;

/// `GET /healthz` — liveness plus the negotiation data a client needs before
/// it authenticates.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub schema_version: u32,
    pub capabilities: CapabilityResponse,
}

#[derive(Debug, Serialize)]
pub struct CapabilityResponse {
    pub events: Vec<&'static str>,
    pub commands: Vec<&'static str>,
    pub auth: &'static str,
}

/// The self-contained, framework-free browser client served at `GET /`.
const CLIENT_HTML: &str = include_str!("client.html");

/// `GET /` — the built-in web client. Same-origin with the API, so no CORS.
pub async fn client() -> Html<&'static str> {
    Html(CLIENT_HTML)
}

/// `GET /healthz` — no auth.
pub async fn healthz(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        schema_version: SCHEMA_VERSION,
        capabilities: CapabilityResponse {
            events: vec!["sse"],
            commands: vec!["remove_session", "clear_done"],
            auth: if state.auth_required() {
                "bearer"
            } else {
                "none"
            },
        },
    })
}

/// Body for `POST /api/v1/pair`.
#[derive(Debug, Deserialize)]
pub struct PairRequest {
    pub code: String,
    #[serde(default)]
    pub device_name: String,
}

#[derive(Debug, Serialize)]
pub struct PairResponse {
    pub device_id: String,
    pub token: String,
}

/// `POST /api/v1/pair` — no auth. Exchange a valid, unexpired pairing code for
/// a per-device token. The plaintext token is returned exactly once.
pub async fn pair(
    State(state): State<AppState>,
    payload: Result<Json<PairRequest>, JsonRejection>,
) -> Result<Json<PairResponse>, ApiError> {
    let Json(request) =
        payload.map_err(|rejection| ApiError::bad_request(rejection.body_text()))?;
    let (device, token) = state
        .devices()
        .pair(&request.code, &request.device_name)
        .ok_or_else(|| ApiError::unauthorized("invalid or expired pairing code"))?;
    Ok(Json(PairResponse {
        device_id: device.id,
        token,
    }))
}

/// `GET /api/v1/devices` — admin-only list of paired devices. Never exposes a
/// token or its hash.
pub async fn list_devices(State(state): State<AppState>) -> Json<Vec<DeviceInfo>> {
    Json(state.list_devices())
}

/// `DELETE /api/v1/devices/{id}` — admin-only revoke. The token stops working
/// on the next request.
pub async fn revoke_device(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if state.revoke_device(&id) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("unknown device"))
    }
}

/// `GET /api/v1/snapshot` — the current [`Snapshot`]. The read runs on the
/// blocking pool so a slow filesystem cannot stall the runtime.
pub async fn snapshot(State(state): State<AppState>) -> Result<Json<Snapshot>, ApiError> {
    let engine = state.engine().clone();
    let snapshot = tokio::task::spawn_blocking(move || engine.snapshot_now())
        .await
        .map_err(|e| ApiError::internal(format!("snapshot task failed: {e}")))?;
    Ok(Json(snapshot))
}

/// `GET /api/v1/events` — Server-Sent Events carrying every [`Update`].
pub async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>> + Send + 'static> {
    let stream = BroadcastStream::new(state.subscribe()).filter_map(|result| match result {
        Ok(update) => Some(Ok(update_event(update))),
        Err(BroadcastStreamRecvError::Lagged(skipped)) => Some(Ok(Event::default()
            .event("lagged")
            .data(skipped.to_string()))),
    });
    Sse::new(stream).keep_alive(KeepAlive::default().interval(Duration::from_secs(15)))
}

fn update_event(update: Update) -> Event {
    match Event::default().event("update").json_data(&update) {
        Ok(event) => event,
        Err(_) => Event::default()
            .event("error")
            .data("failed to serialize update"),
    }
}

/// A command as accepted on the wire. `source` is optional for
/// `remove_session`; it defaults to the engine's first source.
#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum CommandRequest {
    RemoveSession {
        #[serde(default)]
        source: Option<String>,
        session_id: String,
    },
    ClearDone,
}

#[derive(Debug, Serialize)]
pub struct CommandResponse {
    pub revision: u64,
}

/// `POST /api/v1/commands` — route a mutation to its source and report the
/// revision produced by the follow-up refresh.
pub async fn commands(
    State(state): State<AppState>,
    payload: Result<Json<CommandRequest>, JsonRejection>,
) -> Result<Json<CommandResponse>, ApiError> {
    let Json(request) =
        payload.map_err(|rejection| ApiError::bad_request(rejection.body_text()))?;
    let engine = state.engine().clone();
    let default_source = engine.source_id();

    let revision = tokio::task::spawn_blocking(move || -> Result<u64, ApiError> {
        match request {
            CommandRequest::RemoveSession { source, session_id } => {
                let source = source
                    .map(SourceId::new)
                    .or_else(|| default_source.clone())
                    .ok_or_else(|| {
                        ApiError::bad_request(
                            "remove_session requires a source and none is configured",
                        )
                    })?;
                engine
                    .dispatch(SourceCommand::RemoveSession(SessionKey::new(
                        source, session_id,
                    )))
                    .map_err(ApiError::from)?;
            }
            CommandRequest::ClearDone => {
                engine.clear_done().map_err(ApiError::from)?;
            }
        }
        Ok(engine.refresh().revision)
    })
    .await
    .map_err(|e| ApiError::internal(format!("command task failed: {e}")))??;

    Ok(Json(CommandResponse { revision }))
}
