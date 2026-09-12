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

use std::net::SocketAddr;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::watch;

mod app;
mod auth;
pub mod config;
mod devices;
mod error;
mod routes;

pub use app::{app, build_engine, router, AppState};
pub use config::ServerConfig;
pub use devices::{DeviceInfo, DeviceStore, PairInfo};
pub use error::ApiError;

/// Wire protocol version. Bump only for breaking changes; additive changes
/// stay within a version and clients ignore unknown fields.
pub const SCHEMA_VERSION: u32 = 1;

/// How long a shutdown waits for in-flight connections to drain before the
/// runtime force-closes them. Long-lived SSE clients never drain on their own.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// A running embedded server.
///
/// The desktop is not async, so [`start`] owns a multi-thread Tokio runtime on
/// a background thread. Dropping the handle stops the server; call
/// [`shutdown`](Self::shutdown) to stop it explicitly and wait for the thread.
pub struct ServerHandle {
    addr: SocketAddr,
    state: Arc<AppState>,
    shutdown: Option<watch::Sender<bool>>,
    thread: Option<JoinHandle<()>>,
}

impl ServerHandle {
    /// The actual bound address. With a `:0` bind this is the OS-chosen port.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The pairing code and expiry the desktop should display.
    pub fn pair_info(&self) -> PairInfo {
        self.state.pair_info()
    }

    /// Rotate the pairing code, invalidating the previous one.
    pub fn regenerate_pairing(&self) -> PairInfo {
        self.state.regenerate_pairing()
    }

    /// Every paired device, without secret material.
    pub fn devices(&self) -> Vec<DeviceInfo> {
        self.state.list_devices()
    }

    /// Revoke a device. Returns `true` if it existed.
    pub fn revoke_device(&self, id: &str) -> bool {
        self.state.revoke_device(id)
    }

    /// Ask the server to stop and wait for its thread to exit.
    pub fn shutdown(&mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(true);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Start the server against an existing engine on a background thread.
///
/// Binds `config.bind` (port 0 picks a free port) and reports the real address
/// through [`ServerHandle::addr`]. The returned handle must be kept alive for
/// the server to keep running.
pub fn start(
    engine: agentlight_core::Engine,
    config: ServerConfig,
) -> std::io::Result<ServerHandle> {
    let token_required = config.token.is_some();
    // Build state on this thread and share it with the server thread so the
    // handle can reach the live pairing code and device list.
    let state = Arc::new(AppState::with_devices(
        engine,
        config.token.clone(),
        DeviceStore::load(config.devices_path.clone()),
    ));
    let server_state = state.clone();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (addr_tx, addr_rx) = mpsc::channel::<std::io::Result<SocketAddr>>();

    let thread = std::thread::Builder::new()
        .name("agentlight-server".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = addr_tx.send(Err(error));
                    return;
                }
            };

            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::bind(config.bind).await {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = addr_tx.send(Err(error));
                        return;
                    }
                };
                let bound = match listener.local_addr() {
                    Ok(addr) => addr,
                    Err(error) => {
                        let _ = addr_tx.send(Err(error));
                        return;
                    }
                };
                let _ = addr_tx.send(Ok(bound));
                tracing::info!(
                    address = %bound,
                    token = if token_required { "required" } else { "disabled" },
                    "agentlight-server listening"
                );

                // Graceful shutdown stops accepting and lets short requests
                // finish. `wait_for_shutdown` races it so a long-lived SSE
                // client cannot keep the drain (and the thread) alive forever;
                // `runtime.shutdown_timeout` then forces remaining tasks down.
                let graceful = {
                    let mut rx = shutdown_rx.clone();
                    async move {
                        let _ = rx.changed().await;
                    }
                };
                let serve = axum::serve(listener, router((*server_state).clone()))
                    .with_graceful_shutdown(graceful);
                tokio::select! {
                    _ = serve => {}
                    _ = wait_for_shutdown(shutdown_rx) => {}
                }
            });

            runtime.shutdown_timeout(SHUTDOWN_GRACE);
        })?;

    let addr = addr_rx
        .recv()
        .map_err(|_| std::io::Error::other("server thread exited before binding"))??;

    Ok(ServerHandle {
        addr,
        state,
        shutdown: Some(shutdown_tx),
        thread: Some(thread),
    })
}

async fn wait_for_shutdown(mut rx: watch::Receiver<bool>) {
    let _ = rx.changed().await;
}

/// Build the router, bind it, and serve until the process exits. The pairing
/// code is logged so a headless host can be paired without a display.
pub async fn serve(config: ServerConfig) -> std::io::Result<()> {
    let state = AppState::from_config(&config);
    let pair = state.pair_info();
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(
        address = %listener.local_addr()?,
        token = if config.token.is_some() { "required" } else { "disabled" },
        "agentlight-server listening"
    );
    tracing::info!(
        pairing_code = %pair.code,
        expires_at = %pair.expires_at,
        "pair a device with this code"
    );
    axum::serve(listener, router(state)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentlight_core::{Config, Engine};
    use std::io::{Read, Write};
    use std::net::TcpStream;

    fn get(addr: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).expect("connect");
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).unwrap();
        body
    }

    #[test]
    fn embed_start_binds_port_zero_and_shuts_down() {
        let engine = Engine::new(Config::default());
        let config = ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: None,
            ..ServerConfig::default()
        };
        let mut handle = start(engine, config).unwrap();
        assert_eq!(handle.addr().ip().to_string(), "127.0.0.1");
        assert_ne!(handle.addr().port(), 0);

        let health = get(handle.addr(), "/healthz");
        assert!(health.contains("\"status\":\"ok\""), "{health}");

        let client = get(handle.addr(), "/");
        assert!(client.contains("<!doctype html>"), "{client}");

        let pair = handle.pair_info();
        assert_eq!(pair.code.chars().count(), 8);
        assert!(handle.devices().is_empty());

        handle.shutdown();
    }
}
