//! Engine wiring and the router.

use std::sync::Arc;
use std::time::Duration;

use axum::routing::{delete, get, post};
use axum::{middleware, Router};
use tokio::sync::broadcast;
use tower_http::trace::TraceLayer;

use agentlight_core::{ClawlightFileSource, Engine, SourceKind, Update, UpdateSink};
use agentlight_source_events::{EventPushSource, EVENTS_SOURCE_ID};

use crate::auth;
use crate::config::ServerConfig;
use crate::devices::{DeviceInfo, DeviceStore, PairInfo};
use crate::routes;

/// How many updates a slow SSE client may fall behind before it is told to
/// re-fetch the snapshot.
const EVENT_CHANNEL_CAPACITY: usize = 64;

/// Shared handler state: the synchronous engine, the fan-out channel, the auth
/// setting, and the paired-device store.
///
/// Cloning is cheap and shares every field, so a [`crate::ServerHandle`] can
/// hold one copy and reach the live server.
#[derive(Clone)]
pub struct AppState {
    engine: Engine,
    sender: broadcast::Sender<Update>,
    admin_token_hash: Option<String>,
    devices: Arc<DeviceStore>,
    /// The push source when the server runs in events mode, so the ingest route
    /// can reach it. `None` in file mode.
    events: Option<Arc<EventPushSource>>,
}

impl AppState {
    /// Wrap an engine and start forwarding its updates to connected clients.
    ///
    /// `Engine::subscribe` runs the sink on the watcher's `std::thread`;
    /// `broadcast::Sender::send` is synchronous and non-blocking, so it is safe
    /// to call there. Devices are kept in memory; use [`with_devices`] to
    /// persist them.
    ///
    /// [`with_devices`]: Self::with_devices
    pub fn new(engine: Engine, admin_token_hash: Option<String>) -> Self {
        Self::with_devices(engine, admin_token_hash, DeviceStore::in_memory())
    }

    /// Wrap an engine with an explicit device store. `admin_token_hash` is a
    /// SHA-256 hex digest, never a plaintext token.
    pub fn with_devices(
        engine: Engine,
        admin_token_hash: Option<String>,
        devices: DeviceStore,
    ) -> Self {
        let (sender, _receiver) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let sink_sender = sender.clone();
        let sink: UpdateSink = Arc::new(move |update| {
            let _ = sink_sender.send(update);
        });
        engine.subscribe(sink);
        Self {
            engine,
            sender,
            admin_token_hash,
            devices: Arc::new(devices),
            events: None,
        }
    }

    /// Attach the push source an events-mode engine was built with.
    pub fn with_events(mut self, events: Option<Arc<EventPushSource>>) -> Self {
        self.events = events;
        self
    }

    /// Build the engine, source, and device store a config describes.
    pub fn from_config(config: &ServerConfig) -> Self {
        let (engine, events) = build_engine_with_source(config);
        Self::with_devices(
            engine,
            config.admin_token_hash.clone(),
            DeviceStore::load(config.devices_path.clone()),
        )
        .with_events(events)
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The push source when this server is in events mode.
    pub fn events(&self) -> Option<&Arc<EventPushSource>> {
        self.events.as_ref()
    }

    /// The admin token's SHA-256 hex, if one is configured.
    pub fn admin_token_hash(&self) -> Option<&str> {
        self.admin_token_hash.as_deref()
    }

    /// The paired-device store.
    pub fn devices(&self) -> &DeviceStore {
        &self.devices
    }

    /// Whether any credential exists to check. With neither an admin token nor
    /// a paired device, the loopback default stays open, matching the previous
    /// "auth disabled" behavior.
    pub fn auth_required(&self) -> bool {
        self.admin_token_hash.is_some() || self.devices.has_devices()
    }

    /// Pairing code and expiry for the desktop to display.
    pub fn pair_info(&self) -> PairInfo {
        self.devices.pair_info()
    }

    /// Rotate the pairing code.
    pub fn regenerate_pairing(&self) -> PairInfo {
        self.devices.regenerate()
    }

    /// Every paired device, without secret material.
    pub fn list_devices(&self) -> Vec<DeviceInfo> {
        self.devices.list()
    }

    /// Revoke a device, immediately invalidating its token.
    pub fn revoke_device(&self, id: &str) -> bool {
        self.devices.revoke(id)
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Update> {
        self.sender.subscribe()
    }
}

/// Build an engine with one file [`ClawlightFileSource`]. Registration starts
/// the source's watcher thread.
pub fn build_engine(config: &ServerConfig) -> Engine {
    build_engine_with_source(config).0
}

/// Build the engine for `config`, returning the push source when events mode is
/// selected so the caller can keep it for the ingest route.
pub fn build_engine_with_source(config: &ServerConfig) -> (Engine, Option<Arc<EventPushSource>>) {
    let engine = Engine::new(config.core.clone());
    if config.source_kind == SourceKind::Push {
        let mut events = EventPushSource::with_store(EVENTS_SOURCE_ID, config.events_path.clone());
        if config.heartbeat_ms > 0 {
            events = events.with_stale_after(Some(Duration::from_millis(
                config.heartbeat_ms.saturating_mul(4),
            )));
        }
        let events = Arc::new(events);
        engine.add_source(events.clone());
        (engine, Some(events))
    } else {
        engine.add_source(Arc::new(ClawlightFileSource::from_config(&config.core)));
        (engine, None)
    }
}

/// Build the router from an existing state. Useful for tests and for embedding.
///
/// `/pair` is intentionally outside the auth layer; the pairing code is the
/// credential. `/devices` is admin-only.
pub fn router(state: AppState) -> Router {
    let authed = Router::new()
        .route("/snapshot", get(routes::snapshot))
        .route("/events", get(routes::events))
        .route("/commands", post(routes::commands))
        .route("/ingest", post(routes::ingest))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ));

    let admin = Router::new()
        .route("/devices", get(routes::list_devices))
        .route("/devices/{id}", delete(routes::revoke_device))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_admin,
        ));

    let api = Router::new()
        .route("/pair", post(routes::pair))
        .merge(authed)
        .merge(admin);

    Router::new()
        .route("/", get(routes::client))
        .route("/healthz", get(routes::healthz))
        .nest("/api/v1", api)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Build the default router for a config, constructing the engine and source.
pub fn app(config: &ServerConfig) -> Router {
    router(AppState::from_config(config))
}
