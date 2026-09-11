//! Optional HTTP/SSE surface over the AgentLight engine.
//!
//! `agentlight-core` stays synchronous and runtime-free; this crate is the
//! async edge. It owns a [`agentlight_core::Engine`] backed by one
//! `ClawlightFileSource`, bridges engine updates into a
//! `tokio::sync::broadcast` channel, and exposes them over HTTP. See
//! [`docs/protocol.md`](../../../docs/protocol.md) for the wire contract.
//!
//! The surface is deliberately loopback-oriented: bind to `127.0.0.1:8787` by
//! default, and require a static bearer token on `/api/v1/*` when one is
//! configured.

#![forbid(unsafe_code)]

mod app;
mod auth;
pub mod config;
mod error;
mod routes;

pub use app::{app, build_engine, router, AppState};
pub use config::ServerConfig;
pub use error::ApiError;

/// Wire protocol version. Bump only for breaking changes; additive changes
/// stay within a version and clients ignore unknown fields.
pub const SCHEMA_VERSION: u32 = 1;

/// Build the router, bind it, and serve until the process exits.
pub async fn serve(config: ServerConfig) -> std::io::Result<()> {
    let app = app(&config);
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(
        address = %listener.local_addr()?,
        token = if config.token.is_some() { "required" } else { "disabled" },
        "agentlight-server listening"
    );
    axum::serve(listener, app).await
}
