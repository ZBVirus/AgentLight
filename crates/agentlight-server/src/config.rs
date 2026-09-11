//! Server configuration: where to bind, whether to require a token, and what
//! to read.

use std::net::SocketAddr;

use agentlight_core::Config;

/// Default loopback address. LAN exposure is an explicit opt-in.
pub const DEFAULT_BIND: &str = "127.0.0.1:8787";

/// Everything the server needs to run.
///
/// [`core`](Self::core) is the same [`agentlight_core::Config`] the desktop
/// shell uses, so `state_path`, `poll_ms`, `show_done`, and `yellow_mode` keep
/// their meaning. The server adds only transport concerns.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to listen on. Defaults to `127.0.0.1:8787`.
    pub bind: SocketAddr,
    /// Static bearer token. `None` disables auth (loopback only).
    pub token: Option<String>,
    /// State source and display policy.
    pub core: Config,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: DEFAULT_BIND
                .parse()
                .expect("DEFAULT_BIND is a valid socket address"),
            token: None,
            core: Config::default(),
        }
    }
}

impl ServerConfig {
    pub fn new(bind: SocketAddr, token: Option<String>, core: Config) -> Self {
        Self { bind, token, core }
    }

    /// Point the server at a specific clawlight `state.json`.
    pub fn with_state_path(mut self, path: impl Into<String>) -> Self {
        self.core.state_path = Some(path.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_loopback_without_a_token() {
        let config = ServerConfig::default();
        assert_eq!(config.bind.to_string(), DEFAULT_BIND);
        assert!(config.token.is_none());
        assert!(config.core.state_path.is_none());
    }

    #[test]
    fn with_state_path_sets_the_core_path() {
        let config = ServerConfig::default().with_state_path("/tmp/state.json");
        assert_eq!(config.core.state_path.as_deref(), Some("/tmp/state.json"));
    }
}
