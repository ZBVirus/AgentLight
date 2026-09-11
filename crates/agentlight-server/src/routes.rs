//! The HTTP handlers. Async wrappers only: every state computation goes
//! through `spawn_blocking` into the synchronous engine.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use agentlight_core::{SessionKey, Snapshot, SourceCommand, SourceId, Update};

use crate::app::AppState;
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

/// `GET /healthz` — no auth.
pub async fn healthz(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        schema_version: SCHEMA_VERSION,
        capabilities: CapabilityResponse {
            events: vec!["sse"],
            commands: vec!["remove_session", "clear_done"],
            auth: if state.token().is_some() {
                "bearer"
            } else {
                "none"
            },
        },
    })
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
        let command = match request {
            CommandRequest::RemoveSession { source, session_id } => {
                let source = source
                    .map(SourceId::new)
                    .or_else(|| default_source.clone())
                    .ok_or_else(|| {
                        ApiError::bad_request(
                            "remove_session requires a source and none is configured",
                        )
                    })?;
                SourceCommand::RemoveSession(SessionKey::new(source, session_id))
            }
            CommandRequest::ClearDone => SourceCommand::ClearDone,
        };
        engine.dispatch(command).map_err(ApiError::from)?;
        Ok(engine.refresh().revision)
    })
    .await
    .map_err(|e| ApiError::internal(format!("command task failed: {e}")))??;

    Ok(Json(CommandResponse { revision }))
}
