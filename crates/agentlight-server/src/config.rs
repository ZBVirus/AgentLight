//! Server configuration: where to bind, whether to require a token, and what
//! to read.

use std::net::SocketAddr;
use std::path::PathBuf;

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
    /// SHA-256 hex of the static admin bearer token. `None` disables admin auth
    /// (loopback only). When set it also authorizes device management. The
    /// plaintext is never stored or accepted here.
    pub admin_token_hash: Option<String>,
    /// Where paired-device records live. `None` keeps them in memory only.
    pub devices_path: Option<PathBuf>,
    /// State source and display policy.
    pub core: Config,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: DEFAULT_BIND
                .parse()
                .expect("DEFAULT_BIND is a valid socket address"),
            admin_token_hash: None,
            devices_path: None,
            core: Config::default(),
        }
    }
}

impl ServerConfig {
    /// `admin_token_hash` is the SHA-256 hex of the admin token, not the
    /// plaintext. Use [`crate::hash_token`] to hash a user-supplied secret.
    pub fn new(bind: SocketAddr, admin_token_hash: Option<String>, core: Config) -> Self {
        Self {
            bind,
            admin_token_hash,
            devices_path: None,
            core,
        }
    }

    /// Point the server at a specific clawlight `state.json`.
    pub fn with_state_path(mut self, path: impl Into<String>) -> Self {
        self.core.state_path = Some(path.into());
        self
    }

    /// Persist paired devices at `path`.
    pub fn with_devices_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.devices_path = Some(path.into());
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
        assert!(config.admin_token_hash.is_none());
        assert!(config.devices_path.is_none());
        assert!(config.core.state_path.is_none());
    }

    #[test]
    fn with_devices_path_sets_the_store() {
        let config = ServerConfig::default().with_devices_path("/tmp/devices.json");
        assert_eq!(
            config.devices_path.as_deref(),
            Some(std::path::Path::new("/tmp/devices.json"))
        );
    }

    #[test]
    fn with_state_path_sets_the_core_path() {
        let config = ServerConfig::default().with_state_path("/tmp/state.json");
        assert_eq!(config.core.state_path.as_deref(), Some("/tmp/state.json"));
    }
}
