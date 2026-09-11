//! Standalone `agentlight-server` binary.
//!
//! Configuration is environment-only so the process is container-friendly:
//! `AGENTLIGHT_BIND` (`127.0.0.1:8787`), `AGENTLIGHT_TOKEN` (unset disables
//! auth), `AGENTLIGHT_STATE_PATH` (clawlight `state.json`), and
//! `AGENTLIGHT_POLL_MS` (`1500`). No config file is read.

use agentlight_core::Config;
use agentlight_server::{serve, ServerConfig};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    init_tracing();
    serve(server_config()).await
}

fn server_config() -> ServerConfig {
    let bind = env_parse("AGENTLIGHT_BIND").unwrap_or_else(|| ServerConfig::default().bind);
    let token = env_non_empty("AGENTLIGHT_TOKEN");

    let mut core = Config::default();
    if let Some(path) = env_non_empty("AGENTLIGHT_STATE_PATH") {
        core.state_path = Some(path);
    }
    if let Some(poll_ms) = env_parse("AGENTLIGHT_POLL_MS") {
        core.poll_ms = poll_ms;
    }
    // The hub exists to carry notification edges to remote clients, so turn
    // them on unless the engine's default is deliberately changed.
    core.notifications = true;

    ServerConfig::new(bind, token, core.sanitized())
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_parse<T: std::str::FromStr>(key: &str) -> Option<T> {
    env_non_empty(key).and_then(|value| value.parse().ok())
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
