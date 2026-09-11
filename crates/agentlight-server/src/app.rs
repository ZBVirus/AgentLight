//! Engine wiring and the router.

use std::sync::Arc;

use axum::routing::{get, post};
use axum::{middleware, Router};
use tokio::sync::broadcast;
use tower_http::trace::TraceLayer;

use agentlight_core::{ClawlightFileSource, Engine, Update, UpdateSink};

use crate::auth;
use crate::config::ServerConfig;
use crate::routes;

/// How many updates a slow SSE client may fall behind before it is told to
/// re-fetch the snapshot.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Shared handler state: the synchronous engine, the fan-out channel, and the
/// auth setting.
#[derive(Clone)]
pub struct AppState {
    engine: Engine,
    sender: broadcast::Sender<Update>,
    token: Option<String>,
}

impl AppState {
    /// Wrap an engine and start forwarding its updates to connected clients.
    ///
    /// `Engine::subscribe` runs the sink on the watcher's `std::thread`;
    /// `broadcast::Sender::send` is synchronous and non-blocking, so it is safe
    /// to call there.
    pub fn new(engine: Engine, token: Option<String>) -> Self {
        let (sender, _receiver) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let sink_sender = sender.clone();
        let sink: UpdateSink = Arc::new(move |update| {
            let _ = sink_sender.send(update);
        });
        engine.subscribe(sink);
        Self {
            engine,
            sender,
            token,
        }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Update> {
        self.sender.subscribe()
    }
}

/// Build an engine with one [`ClawlightFileSource`]. Registration starts the
/// source's watcher thread.
pub fn build_engine(config: &ServerConfig) -> Engine {
    let engine = Engine::new(config.core.clone());
    engine.add_source(Arc::new(ClawlightFileSource::from_config(&config.core)));
    engine
}

/// Build the router from an existing state. Useful for tests and for embedding.
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/snapshot", get(routes::snapshot))
        .route("/events", get(routes::events))
        .route("/commands", post(routes::commands))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ));

    Router::new()
        .route("/healthz", get(routes::healthz))
        .nest("/api/v1", api)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Build the default router for a config, constructing the engine and source.
pub fn app(config: &ServerConfig) -> Router {
    router(AppState::new(build_engine(config), config.token.clone()))
}
