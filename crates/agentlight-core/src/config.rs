//! AgentLight's own preferences.
//!
//! Kept separate from clawlight's state so we never write into the file
//! clawlight owns. Stored as JSON next to the OS config dir:
//! `%APPDATA%\AgentLight\config.json` on Windows.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::state::{self, Error, Result};

/// Default bind address for the opt-in embedded hub. Loopback; LAN exposure is
/// an explicit opt-in.
pub const DEFAULT_SERVER_BIND: &str = "127.0.0.1:8787";

/// Default remote hub address. Matches `agentlight-server`'s loopback default.
pub const DEFAULT_HUB_URL: &str = "http://127.0.0.1:8787";

/// Where the desktop reads session state from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// A local clawlight `state.json`. Default.
    #[default]
    File,
    /// A remote AgentLight hub over HTTP.
    Hub,
    /// Events pushed to an AgentLight hub rather than read from a file.
    Push,
}

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

/// What transition fires an attention alarm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AlarmTrigger {
    /// A session enters `needs_help`. Default.
    #[default]
    NeedsHelp,
    /// A session becomes `done`.
    Done,
    /// Any status change after the session is first seen.
    AnyStatus,
}

/// Collapsed (mini) window layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CollapseStyle {
    /// One aggregate traffic light. Default.
    #[default]
    Single,
    /// Three independent lights in a row: red, orange, green.
    Triple,
    /// Three independent lights stacked, traffic-light style.
    TripleVertical,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Absolute path to clawlight's `state.json`. `None` uses the default.
    pub state_path: Option<String>,
    /// Keep the window above other windows. Settled default is on.
    pub always_on_top: bool,
    /// Aggregate rule for idle sessions.
    pub yellow_mode: YellowMode,
    /// Layout of the collapsed window.
    pub collapse_style: CollapseStyle,
    /// Custom color for the red (needs help / aggregate red) collapsed light.
    /// Any CSS color; `None` keeps the built-in palette.
    pub mini_red: Option<String>,
    /// Custom color for the orange (idle / aggregate orange) collapsed light.
    pub mini_orange: Option<String>,
    /// Custom color for the green (working / aggregate green) collapsed light.
    pub mini_green: Option<String>,
    /// Show the text labels beside the collapsed lights.
    pub mini_show_labels: bool,
    /// Persisted width of the collapsed window, in logical pixels. `None` uses
    /// the fixed size for the selected style.
    pub mini_width: Option<f64>,
    /// Persisted height of the collapsed window, in logical pixels.
    pub mini_height: Option<f64>,
    /// Periodically re-assert always-on-top so the widget stays above
    /// borderless full-screen apps that push it behind. Opt-in, Windows-only.
    pub topmost_reassert: bool,
    /// Fallback poll interval, in milliseconds, for the file watcher.
    pub poll_ms: u64,
    /// Show every `done` session instead of only the newest few.
    pub show_done: bool,
    /// Where session state comes from: `file` (default) or `hub`.
    pub source_kind: SourceKind,
    /// Base URL of the remote hub, e.g. `http://127.0.0.1:8787`. Used only when
    /// [`source_kind`](Self::source_kind) is [`SourceKind::Hub`].
    pub hub_url: String,
    /// Bearer token sent to the remote hub, if one is required.
    ///
    /// This is a **client credential**: the hub validates it, so unlike the
    /// server's admin token it must be sent verbatim and cannot be stored as a
    /// hash. It therefore stays plaintext in `config.json`; protect that file
    /// as you would any secret. It is never logged.
    pub hub_token: Option<String>,
    /// Fire a desktop notification when a session hits the notification trigger.
    pub notifications: bool,
    /// Which transition fires a desktop notification. Same choices as
    /// [`alarms`](Self::alarm_trigger).
    pub notification_trigger: AlarmTrigger,
    /// Play an attention alarm (sound) when a session needs attention.
    pub alarms_enabled: bool,
    /// Which transition fires an alarm.
    pub alarm_trigger: AlarmTrigger,
    /// Custom sound file for alarms. `None` uses the system/default sound.
    pub alarm_sound: Option<String>,
    /// Launch AgentLight at login. Off by default.
    pub start_at_login: bool,
    /// Serve the engine over HTTP on the LAN. Off by default.
    pub server_enabled: bool,
    /// Address the embedded server binds. Defaults to
    /// [`DEFAULT_SERVER_BIND`] (`127.0.0.1:8787`).
    pub server_bind: String,
    /// SHA-256 hex of the admin bearer token for the embedded server. `None`
    /// disables admin auth. The plaintext is never persisted.
    pub server_token_hash: Option<String>,
    /// Legacy plaintext admin token. Read-only: [`Config::sanitized`] hashes it
    /// into [`server_token_hash`](Self::server_token_hash) and clears it, and it
    /// is never serialized back to disk.
    #[serde(default, skip_serializing)]
    pub server_token: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            state_path: None,
            always_on_top: true,
            yellow_mode: YellowMode::AnyInactive,
            collapse_style: CollapseStyle::Single,
            mini_red: None,
            mini_orange: None,
            mini_green: None,
            mini_show_labels: false,
            mini_width: None,
            mini_height: None,
            topmost_reassert: false,
            poll_ms: 1500,
            show_done: false,
            source_kind: SourceKind::File,
            hub_url: DEFAULT_HUB_URL.to_string(),
            hub_token: None,
            notifications: false,
            notification_trigger: AlarmTrigger::NeedsHelp,
            alarms_enabled: false,
            alarm_trigger: AlarmTrigger::NeedsHelp,
            alarm_sound: None,
            start_at_login: false,
            server_enabled: false,
            server_bind: DEFAULT_SERVER_BIND.to_string(),
            server_token_hash: None,
            server_token: None,
        }
    }
}

/// SHA-256 hex of a token. Identical to the server's implementation so a config
/// migrated here compares equal to one the server hashed.
pub fn hash_token(token: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(token.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

impl Config {
    /// Normalize values loaded from disk so a hand-edited file can't produce a
    /// pathological watcher interval.
    pub fn sanitized(mut self) -> Self {
        self.poll_ms = self.poll_ms.clamp(250, 60_000);
        for color in [
            &mut self.mini_red,
            &mut self.mini_orange,
            &mut self.mini_green,
        ] {
            *color = color
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
        self.mini_width = self
            .mini_width
            .filter(|width| width.is_finite())
            .map(|width| width.clamp(40.0, 2000.0));
        self.mini_height = self
            .mini_height
            .filter(|height| height.is_finite())
            .map(|height| height.clamp(24.0, 2000.0));
        if let Some(path) = &self.state_path {
            if path.trim().is_empty() {
                self.state_path = None;
            }
        }
        self.hub_url = self.hub_url.trim().to_string();
        if self.hub_url.is_empty() {
            self.hub_url = DEFAULT_HUB_URL.to_string();
        }
        // The hub token is a client credential and cannot be hashed, so it is
        // kept plaintext; only trim it and drop an empty value.
        self.hub_token = self
            .hub_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        self.alarm_sound = self
            .alarm_sound
            .as_deref()
            .map(str::trim)
            .filter(|sound| !sound.is_empty())
            .map(str::to_string);
        self.server_bind = self.server_bind.trim().to_string();
        if self.server_bind.is_empty() {
            self.server_bind = DEFAULT_SERVER_BIND.to_string();
        }
        // Migrate a legacy plaintext token to its hash, then drop the plaintext
        // so it cannot be serialized back to disk.
        if let Some(token) = self.server_token.take() {
            let token = token.trim();
            if !token.is_empty() {
                self.server_token_hash = Some(hash_token(token));
            }
        }
        self.server_token_hash = self
            .server_token_hash
            .as_deref()
            .map(str::trim)
            .filter(|hash| !hash.is_empty())
            .map(str::to_string);
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
        assert_eq!(c.collapse_style, CollapseStyle::Single);
        assert_eq!(c.poll_ms, 1500);
        assert!(c.state_path.is_none());
        assert_eq!(
            c.source_kind,
            SourceKind::File,
            "the file source is default"
        );
        assert_eq!(c.hub_url, DEFAULT_HUB_URL);
        assert!(c.hub_token.is_none());
        assert!(!c.server_enabled, "the embedded hub is off by default");
        assert_eq!(c.server_bind, DEFAULT_SERVER_BIND);
        assert!(c.server_token_hash.is_none());
        assert!(c.server_token.is_none());
    }

    #[test]
    fn server_fields_parse_and_sanitize() {
        let c: Config = serde_json::from_str(
            r#"{"server_enabled":true,"server_bind":"  0.0.0.0:9000  ","server_token":"  s3cret  "}"#,
        )
        .unwrap();
        assert!(c.server_enabled);
        assert_eq!(c.server_bind.trim(), "0.0.0.0:9000");
        assert_eq!(c.server_token.as_deref(), Some("  s3cret  "));
        let c = c.sanitized();
        assert_eq!(c.server_bind, "0.0.0.0:9000");
        assert_eq!(c.server_token_hash, Some(hash_token("s3cret")));
        assert!(c.server_token.is_none(), "legacy plaintext is dropped");

        let c = Config {
            server_bind: "   ".to_string(),
            server_token: Some("   ".to_string()),
            ..Config::default()
        }
        .sanitized();
        assert_eq!(c.server_bind, DEFAULT_SERVER_BIND);
        assert!(c.server_token.is_none());
        assert!(c.server_token_hash.is_none());
    }

    #[test]
    fn server_enabled_defaults_off_and_true_roundtrips() {
        assert!(
            !Config::default().server_enabled,
            "serving to other devices is off unless turned on"
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AgentLight").join("config.json");
        let on = Config {
            server_enabled: true,
            ..Config::default()
        };
        save(&path, &on).unwrap();
        assert!(
            load(&path).server_enabled,
            "an opt-in must survive a save/load round trip"
        );

        let off = Config {
            server_enabled: false,
            ..Config::default()
        };
        save(&path, &off).unwrap();
        assert!(
            !load(&path).server_enabled,
            "an opt-out must survive a save/load round trip"
        );
    }

    #[test]
    fn source_fields_parse_and_sanitize() {
        let c: Config = serde_json::from_str(
            r#"{"source_kind":"hub","hub_url":"  http://10.0.0.5:9999  ","hub_token":"  s3cret  "}"#,
        )
        .unwrap();
        assert_eq!(c.source_kind, SourceKind::Hub);
        assert_eq!(c.hub_url, "  http://10.0.0.5:9999  ");
        assert_eq!(c.hub_token.as_deref(), Some("  s3cret  "));
        let c = c.sanitized();
        assert_eq!(c.source_kind, SourceKind::Hub);
        assert_eq!(c.hub_url, "http://10.0.0.5:9999");
        assert_eq!(c.hub_token.as_deref(), Some("s3cret"));

        let c = Config {
            hub_url: "   ".to_string(),
            hub_token: Some("   ".to_string()),
            ..Config::default()
        }
        .sanitized();
        assert_eq!(c.hub_url, DEFAULT_HUB_URL);
        assert!(c.hub_token.is_none());
    }

    #[test]
    fn hub_token_roundtrips_as_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AgentLight").join("config.json");
        let c = Config {
            source_kind: SourceKind::Hub,
            hub_url: "http://hub.local:8787".to_string(),
            hub_token: Some("client-secret".to_string()),
            ..Config::default()
        };
        save(&path, &c).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("client-secret"),
            "a client credential cannot be hashed and is stored plaintext"
        );
        assert_eq!(load(&path), c);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let c: Config = serde_json::from_str(
            r#"{"source_kind":"hub","hub_url":"http://h:1","future_field":42}"#,
        )
        .unwrap();
        assert_eq!(c.source_kind, SourceKind::Hub);
        assert_eq!(c.hub_url, "http://h:1");
        assert!(c.always_on_top);
    }

    #[test]
    fn legacy_plaintext_token_is_hashed_and_never_serialized() {
        let c: Config = serde_json::from_str(r#"{"server_token":"hunter2"}"#).unwrap();
        assert_eq!(c.server_token.as_deref(), Some("hunter2"));
        let c = c.sanitized();
        assert_eq!(c.server_token_hash, Some(hash_token("hunter2")));
        assert!(c.server_token.is_none());

        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("hunter2"), "plaintext must not be written");
        assert!(json.contains(&hash_token("hunter2")));
    }

    #[test]
    fn load_migrates_legacy_plaintext_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AgentLight").join("config.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"server_enabled":true,"server_token":"hunter2"}"#).unwrap();

        let config = load(&path);
        assert_eq!(config.server_token_hash, Some(hash_token("hunter2")));
        assert!(config.server_token.is_none());

        save(&path, &config).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("hunter2"), "plaintext must not remain: {raw}");
        assert!(raw.contains(&hash_token("hunter2")));
    }

    #[test]
    fn mini_customization_parses_sanitizes_and_clamps() {
        let c: Config = serde_json::from_str(
            r#"{"mini_red":"  #ff0000  ","mini_green":"   ","mini_show_labels":true,"mini_width":10.0,"mini_height":99999.0}"#,
        )
        .unwrap();
        assert_eq!(c.mini_red.as_deref(), Some("  #ff0000  "));
        let c = c.sanitized();
        assert_eq!(c.mini_red.as_deref(), Some("#ff0000"));
        assert!(
            c.mini_green.is_none(),
            "a blank color falls back to built-in"
        );
        assert!(c.mini_show_labels);
        assert_eq!(c.mini_width, Some(40.0), "a too-small width clamps up");
        assert_eq!(c.mini_height, Some(2000.0), "a huge height clamps down");

        let defaults = Config::default();
        assert!(
            !defaults.mini_show_labels,
            "labels are hidden in the collapsed view by default"
        );
        assert!(defaults.mini_width.is_none());
        assert!(!defaults.topmost_reassert);
    }

    #[test]
    fn alarms_parse_and_default() {
        let defaults = Config::default();
        assert!(!defaults.alarms_enabled);
        assert_eq!(defaults.alarm_trigger, AlarmTrigger::NeedsHelp);
        assert!(defaults.alarm_sound.is_none());
        assert_eq!(defaults.notification_trigger, AlarmTrigger::NeedsHelp);

        let c: Config = serde_json::from_str(r#"{"notification_trigger":"done"}"#).unwrap();
        assert_eq!(c.notification_trigger, AlarmTrigger::Done);

        let c: Config = serde_json::from_str(
            r#"{"alarms_enabled":true,"alarm_trigger":"any_status","alarm_sound":"  C:/ding.wav  "}"#,
        )
        .unwrap();
        assert!(c.alarms_enabled);
        assert_eq!(c.alarm_trigger, AlarmTrigger::AnyStatus);
        let c = c.sanitized();
        assert_eq!(c.alarm_sound.as_deref(), Some("C:/ding.wav"));

        let c = Config {
            alarm_sound: Some("   ".to_string()),
            ..Config::default()
        }
        .sanitized();
        assert!(c.alarm_sound.is_none());
    }

    #[test]
    fn collapse_style_parses_and_defaults() {
        assert_eq!(Config::default().collapse_style, CollapseStyle::Single);
        let c: Config = serde_json::from_str(r#"{"collapse_style":"triple"}"#).unwrap();
        assert_eq!(c.collapse_style, CollapseStyle::Triple);
        let c: Config = serde_json::from_str(r#"{"collapse_style":"triple_vertical"}"#).unwrap();
        assert_eq!(c.collapse_style, CollapseStyle::TripleVertical);
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
