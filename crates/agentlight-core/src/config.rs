//! AgentLight's own preferences.
//!
//! Kept separate from clawlight's state so we never write into the file
//! clawlight owns. Stored as JSON next to the OS config dir:
//! `%APPDATA%\AgentLight\config.json` on Windows.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::state::{self, Error, Result};

/// How an idle (`inactive`) session colors the aggregate when others still
/// work. Mirrors clawlight's `YellowMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum YellowMode {
    /// Any idle session turns the light orange. Default.
    #[default]
    AnyInactive,
    /// Working wins: orange only when every live session is idle.
    ActiveWins,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Absolute path to clawlight's `state.json`. `None` uses the default.
    pub state_path: Option<String>,
    /// Keep the window above other windows. Settled default is on.
    pub always_on_top: bool,
    /// Aggregate rule for idle sessions.
    pub yellow_mode: YellowMode,
    /// Fallback poll interval, in milliseconds, for the file watcher.
    pub poll_ms: u64,
    /// Show every `done` session instead of only the newest few.
    pub show_done: bool,
    /// Fire a desktop notification when a session needs help.
    pub notifications: bool,
    /// Launch AgentLight at login. Off by default.
    pub start_at_login: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            state_path: None,
            always_on_top: true,
            yellow_mode: YellowMode::AnyInactive,
            poll_ms: 1500,
            show_done: false,
            notifications: false,
            start_at_login: false,
        }
    }
}

impl Config {
    /// Normalize values loaded from disk so a hand-edited file can't produce a
    /// pathological watcher interval.
    pub fn sanitized(mut self) -> Self {
        self.poll_ms = self.poll_ms.clamp(250, 60_000);
        if let Some(path) = &self.state_path {
            if path.trim().is_empty() {
                self.state_path = None;
            }
        }
        self
    }

    pub fn state_file(&self) -> PathBuf {
        state::resolve_state_path(self)
    }
}

/// Default config file location: `<config_dir>/AgentLight/config.json`.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("AgentLight")
        .join("config.json")
}

/// Load config, falling back to defaults when it is missing or unreadable.
pub fn load(path: &Path) -> Config {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str::<Config>(&c).ok())
        .unwrap_or_default()
        .sanitized()
}

/// Persist config atomically (temp file + rename), creating the directory.
pub fn save(path: &Path, config: &Config) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| Error::Unreadable("config path has no parent directory".to_string()))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".config.{}.tmp", std::process::id()));
    let serialized = serde_json::to_string_pretty(config)?;
    std::fs::write(&tmp, serialized)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_settled_decisions() {
        let c = Config::default();
        assert!(c.always_on_top, "topmost is on by default");
        assert!(!c.start_at_login, "autostart is OFF by default");
        assert!(!c.notifications, "notifications are off by default");
        assert!(!c.show_done);
        assert_eq!(c.yellow_mode, YellowMode::AnyInactive);
        assert_eq!(c.poll_ms, 1500);
        assert!(c.state_path.is_none());
    }

    #[test]
    fn partial_config_uses_defaults() {
        let c: Config = serde_json::from_str(r#"{"show_done":true}"#).unwrap();
        assert!(c.show_done);
        assert!(c.always_on_top);
        assert!(!c.start_at_login);
        assert_eq!(c.yellow_mode, YellowMode::AnyInactive);
    }

    #[test]
    fn old_or_unknown_config_still_loads() {
        let c: Config =
            serde_json::from_str(r#"{"led_enabled":true,"state_path":"C:/x/state.json"}"#).unwrap();
        assert_eq!(c.state_path.as_deref(), Some("C:/x/state.json"));
        assert!(c.always_on_top);
    }

    #[test]
    fn roundtrips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AgentLight").join("config.json");
        let c = Config {
            state_path: Some("C:/Users/me/clawlight/state.json".into()),
            show_done: true,
            yellow_mode: YellowMode::ActiveWins,
            ..Config::default()
        };
        save(&path, &c).unwrap();
        assert_eq!(load(&path), c);
    }

    #[test]
    fn missing_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(&dir.path().join("nope.json")), Config::default());
    }

    #[test]
    fn poll_interval_is_clamped() {
        let c: Config = serde_json::from_str(r#"{"poll_ms":1}"#).unwrap();
        assert_eq!(c.sanitized().poll_ms, 250);
        let c: Config = serde_json::from_str(r#"{"poll_ms":9999999}"#).unwrap();
        assert_eq!(c.sanitized().poll_ms, 60_000);
    }
}
