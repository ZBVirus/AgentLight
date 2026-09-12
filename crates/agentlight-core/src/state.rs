//! Parsing and writing of clawlight's `state.json`.
//!
//! This is a host-side reader of the clawlight state contract. It is a
//! faithful port of clawlight v0.13.0 (`src/state.rs`), with one deliberate
//! difference: the host cannot check `terminal.owner_pid`, because that PID is
//! namespaced to the container clawlight runs in. We therefore only apply the
//! 24h staleness backstop, never a process-liveness reap. See
//! `docs/state-format.md`.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Config, YellowMode};

/// A session older than this (without an owner-PID check) is treated as done.
pub const STALE_AFTER_HOURS: i64 = 24;

/// Core error type. Keeps `agentlight-core` free of a heavyweight error crate
/// while still giving callers a typed reason.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// The state file exists but could not be understood. Callers must not
    /// write on this, or they would wipe every other session's status.
    Unreadable(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Json(e) => write!(f, "{e}"),
            Error::Unreadable(m) => write!(f, "unreadable state file: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Session status as written by clawlight. JSON is snake_case; the human label
/// differs (`needs_help` renders as "needs help").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Inactive,
    NeedsHelp,
    Done,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Inactive => "inactive",
            Status::NeedsHelp => "needs_help",
            Status::Done => "done",
        }
    }

    /// The label clawlight's UI uses.
    pub fn label(self) -> &'static str {
        match self {
            Status::Active => "working",
            Status::Inactive => "paused",
            Status::NeedsHelp => "needs help",
            Status::Done => "done",
        }
    }

    /// Sort weight: most urgent first.
    pub fn urgency(self) -> u8 {
        match self {
            Status::NeedsHelp => 0,
            Status::Active => 1,
            Status::Inactive => 2,
            Status::Done => 3,
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Status {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "active" => Ok(Status::Active),
            "inactive" => Ok(Status::Inactive),
            "needs_help" => Ok(Status::NeedsHelp),
            "done" => Ok(Status::Done),
            _ => Err(()),
        }
    }
}

/// One session entry. Only the fields AgentLight displays are lifted out of the
/// JSON; everything else is preserved verbatim by [`clear_session`], which
/// operates on the raw document rather than this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatus {
    pub status: Status,
    pub last_updated: Option<String>,
    pub project_path: Option<String>,
    pub notification_type: Option<String>,
    pub name: Option<String>,
    pub harness: Option<String>,
}

impl SessionStatus {
    fn from_json(value: &Value) -> Option<Self> {
        let status = value
            .get("status")
            .and_then(Value::as_str)
            .and_then(|s| Status::from_str(s).ok())?;
        Some(Self {
            status,
            last_updated: str_field(value, "last_updated"),
            project_path: str_field(value, "project_path"),
            notification_type: str_field(value, "notification_type"),
            name: str_field(value, "name"),
            harness: str_field(value, "harness"),
        })
    }

    /// Epoch/`DateTime` of `last_updated`, if it parses.
    pub fn last_updated_at(&self) -> Option<DateTime<Utc>> {
        self.last_updated
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
    }
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookState {
    pub sessions: HashMap<String, SessionStatus>,
}

impl HookState {
    /// Parse permissively: a session whose `status` is missing or unknown is
    /// skipped, never fatal. A document that is not JSON at all is an error.
    pub fn from_json_str(contents: &str) -> Result<Self> {
        let root: Value = serde_json::from_str(contents)?;
        let mut sessions = HashMap::new();
        if let Some(map) = root.get("sessions").and_then(Value::as_object) {
            for (id, value) in map {
                if let Some(session) = SessionStatus::from_json(value) {
                    sessions.insert(id.clone(), session);
                }
            }
        }
        Ok(Self { sessions })
    }
}

/// Outcome of reading the state file, so the UI can distinguish "waiting for
/// clawlight" from "the file is broken".
#[derive(Debug)]
pub enum Load {
    Loaded(HookState),
    Missing,
    Unreadable(Error),
}

/// Read and normalize the state file. Does not lock: clawlight writes with an
/// atomic temp-file + rename, so a reader always sees a complete snapshot.
pub fn load_state(state_path: &Path) -> Load {
    if !state_path.exists() {
        return Load::Missing;
    }
    match std::fs::read_to_string(state_path) {
        Ok(contents) => match HookState::from_json_str(&contents) {
            Ok(state) => Load::Loaded(state),
            Err(e) => Load::Unreadable(e),
        },
        Err(e) => Load::Unreadable(Error::Io(e)),
    }
}

/// Downgrade stale sessions to `done`, in memory only.
///
/// clawlight also reaps sessions whose `owner_pid` is dead. AgentLight cannot:
/// that PID is namespaced to the container. So we apply only the 24h backstop.
pub fn reap_stale(state: &mut HookState, now: DateTime<Utc>, stale_hours: i64) {
    for session in state.sessions.values_mut() {
        if session.status == Status::Done {
            continue;
        }
        if let Some(ts) = session.last_updated_at() {
            if now.signed_duration_since(ts).num_hours() >= stale_hours {
                session.status = Status::Done;
            }
        }
    }
}

/// Aggregate health across all live sessions. Matches clawlight's
/// `state::aggregate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aggregate {
    Red,
    Yellow,
    Green,
    None,
}

impl Aggregate {
    pub fn as_str(self) -> &'static str {
        match self {
            Aggregate::Red => "red",
            Aggregate::Yellow => "orange",
            Aggregate::Green => "green",
            Aggregate::None => "gray",
        }
    }
}

/// Aggregate a stream of statuses. Shared by the clawlight [`HookState`] path
/// and the normalized engine path so the rule lives in exactly one place.
pub fn aggregate_statuses(
    statuses: impl IntoIterator<Item = Status>,
    yellow_mode: YellowMode,
) -> Aggregate {
    let mut needs_help = 0usize;
    let mut active = 0usize;
    let mut inactive = 0usize;
    for status in statuses {
        match status {
            Status::NeedsHelp => needs_help += 1,
            Status::Active => active += 1,
            Status::Inactive => inactive += 1,
            Status::Done => {}
        }
    }
    if needs_help > 0 {
        return Aggregate::Red;
    }
    let any_inactive_wins = matches!(yellow_mode, YellowMode::AnyInactive);
    if inactive > 0 && (any_inactive_wins || active == 0) {
        Aggregate::Yellow
    } else if active > 0 {
        Aggregate::Green
    } else {
        Aggregate::None
    }
}

pub fn aggregate(state: &HookState, yellow_mode: YellowMode) -> Aggregate {
    aggregate_statuses(
        state.sessions.values().map(|session| session.status),
        yellow_mode,
    )
}

/// Default state path, in resolution order: `AGENTLIGHT_STATE_FILE`, then the
/// config's `state_path`, then `~/.claude/clawlight/state.json`.
pub fn default_state_path() -> PathBuf {
    if let Some(path) = env_state_path() {
        return path;
    }
    dirs::home_dir()
        .unwrap_or_default()
        .join(".claude")
        .join("clawlight")
        .join("state.json")
}

fn env_state_path() -> Option<PathBuf> {
    std::env::var_os("AGENTLIGHT_STATE_FILE")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Resolve the effective state path for a config.
pub fn resolve_state_path(config: &Config) -> PathBuf {
    if let Some(path) = env_state_path() {
        return path;
    }
    if let Some(raw) = config.state_path.as_deref() {
        if !raw.trim().is_empty() {
            return PathBuf::from(raw);
        }
    }
    default_state_path()
}

/// Best-effort exclusive lock on `.state.lock` beside `state.json`, guarding a
/// read-modify-write span against concurrent clawlight writers. The lock
/// releases when the returned `File` drops. Returns `None` if it cannot be
/// taken (bad directory, or an OS/filesystem that does not support the lock) —
/// callers proceed unlocked, exactly like clawlight.
pub fn acquire_state_lock(state_path: &Path) -> Option<File> {
    let dir = state_path.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    let lock_path = dir.join(".state.lock");
    let file = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    file.lock_exclusive().ok()?;
    Some(file)
}

/// Atomic write: serialize to a sibling temp file, then rename onto the target.
/// On Windows `std::fs::rename` uses `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`,
/// so this overwrites atomically, same as clawlight. The temp name is unique per
/// process and call so concurrent writers never collide.
pub fn write_state_atomic(state_path: &Path, contents: &str) -> Result<()> {
    let dir = state_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| Error::Unreadable("state path has no parent directory".to_string()))?;
    std::fs::create_dir_all(dir)?;
    let tmp_path = dir.join(format!(".state.{}.{}.tmp", std::process::id(), next_seq()));
    std::fs::write(&tmp_path, contents)?;
    std::fs::rename(&tmp_path, state_path)?;
    Ok(())
}

static SEQ: AtomicU64 = AtomicU64::new(0);

fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

/// Remove one session from the state file, mirroring clawlight's TUI `x`.
///
/// Operates on the raw JSON document so every other field — including ones this
/// crate does not know about — is preserved byte-for-byte in value. Never writes
/// if the read failed to parse. Returns `true` if a key was removed.
pub fn clear_session(state_path: &Path, session_id: &str) -> Result<bool> {
    if !state_path.exists() {
        return Ok(false);
    }
    let _lock = acquire_state_lock(state_path);
    let contents = std::fs::read_to_string(state_path)?;
    let mut root: Value =
        serde_json::from_str(&contents).map_err(|e| Error::Unreadable(format!("{e}")))?;
    let removed = match root.get_mut("sessions").and_then(Value::as_object_mut) {
        Some(sessions) => sessions.remove(session_id).is_some(),
        None => false,
    };
    if removed {
        let serialized = serde_json::to_string(&root)?;
        write_state_atomic(state_path, &serialized)?;
    }
    Ok(removed)
}

/// Remove every `done` session. Returns the number removed.
pub fn clear_done(state_path: &Path) -> Result<usize> {
    if !state_path.exists() {
        return Ok(0);
    }
    let _lock = acquire_state_lock(state_path);
    let contents = std::fs::read_to_string(state_path)?;
    let mut root: Value =
        serde_json::from_str(&contents).map_err(|e| Error::Unreadable(format!("{e}")))?;
    let mut removed = 0usize;
    if let Some(sessions) = root.get_mut("sessions").and_then(Value::as_object_mut) {
        let done: Vec<String> = sessions
            .iter()
            .filter(|(_, v)| v.get("status").and_then(Value::as_str) == Some("done"))
            .map(|(k, _)| k.clone())
            .collect();
        for key in done {
            sessions.remove(&key);
            removed += 1;
        }
    }
    if removed > 0 {
        let serialized = serde_json::to_string(&root)?;
        write_state_atomic(state_path, &serialized)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn state_with(statuses: &[Status]) -> HookState {
        let mut state = HookState::default();
        for (i, status) in statuses.iter().enumerate() {
            state.sessions.insert(
                format!("s{i}"),
                SessionStatus {
                    status: *status,
                    last_updated: None,
                    project_path: None,
                    notification_type: None,
                    name: None,
                    harness: None,
                },
            );
        }
        state
    }

    #[test]
    fn any_inactive_shows_yellow_over_active() {
        let mixed = state_with(&[Status::Active, Status::Inactive]);
        assert_eq!(
            aggregate(&mixed, YellowMode::AnyInactive),
            Aggregate::Yellow
        );
    }

    #[test]
    fn active_wins_stays_green_while_anything_works() {
        let mixed = state_with(&[Status::Active, Status::Inactive]);
        assert_eq!(aggregate(&mixed, YellowMode::ActiveWins), Aggregate::Green);

        let idle_only = state_with(&[Status::Inactive, Status::Done]);
        assert_eq!(
            aggregate(&idle_only, YellowMode::ActiveWins),
            Aggregate::Yellow
        );
        assert_eq!(
            aggregate(&idle_only, YellowMode::AnyInactive),
            Aggregate::Yellow
        );
    }

    #[test]
    fn needs_help_is_red_in_both_modes() {
        let help = state_with(&[Status::Active, Status::Inactive, Status::NeedsHelp]);
        assert_eq!(aggregate(&help, YellowMode::AnyInactive), Aggregate::Red);
        assert_eq!(aggregate(&help, YellowMode::ActiveWins), Aggregate::Red);
    }

    #[test]
    fn done_only_is_none() {
        let done = state_with(&[Status::Done]);
        assert_eq!(aggregate(&done, YellowMode::AnyInactive), Aggregate::None);
        assert_eq!(aggregate(&done, YellowMode::ActiveWins), Aggregate::None);
    }

    #[test]
    fn empty_is_none() {
        assert_eq!(
            aggregate(&HookState::default(), YellowMode::AnyInactive),
            Aggregate::None
        );
    }

    #[test]
    fn parses_the_documented_shape() {
        let json = r#"{
            "sessions": {
                "ses_a": {
                    "status": "needs_help",
                    "last_updated": "2026-09-11T09:01:41Z",
                    "project_path": "/work/thing",
                    "notification_type": "permission",
                    "name": "Fix flaky auth test",
                    "terminal": {"term_program": "vscode", "owner_pid": 42},
                    "harness": "opencode",
                    "future_field": {"nested": true}
                }
            }
        }"#;
        let state = HookState::from_json_str(json).unwrap();
        assert_eq!(state.sessions.len(), 1);
        let s = &state.sessions["ses_a"];
        assert_eq!(s.status, Status::NeedsHelp);
        assert_eq!(s.name.as_deref(), Some("Fix flaky auth test"));
        assert_eq!(s.harness.as_deref(), Some("opencode"));
        assert_eq!(s.notification_type.as_deref(), Some("permission"));
    }

    #[test]
    fn skips_malformed_sessions_without_failing_the_file() {
        let json = r#"{"sessions": {
            "good": {"status": "active"},
            "missing_status": {"name": "x"},
            "unknown_status": {"status": "exploded"},
            "not_an_object": 7
        }}"#;
        let state = HookState::from_json_str(json).unwrap();
        assert_eq!(state.sessions.len(), 1);
        assert!(state.sessions.contains_key("good"));
    }

    #[test]
    fn non_json_is_an_error() {
        assert!(HookState::from_json_str("not json").is_err());
    }

    #[test]
    fn stale_sessions_are_reaped_but_only_past_the_window() {
        let now = Utc::now();
        let mut state = HookState::default();
        state.sessions.insert(
            "old".into(),
            SessionStatus {
                status: Status::NeedsHelp,
                last_updated: Some((now - Duration::hours(STALE_AFTER_HOURS + 1)).to_rfc3339()),
                project_path: None,
                notification_type: None,
                name: None,
                harness: None,
            },
        );
        state.sessions.insert(
            "fresh".into(),
            SessionStatus {
                status: Status::NeedsHelp,
                last_updated: Some(now.to_rfc3339()),
                project_path: None,
                notification_type: None,
                name: None,
                harness: None,
            },
        );
        reap_stale(&mut state, now, STALE_AFTER_HOURS);
        assert_eq!(state.sessions["old"].status, Status::Done);
        assert_eq!(state.sessions["fresh"].status, Status::NeedsHelp);
    }

    #[test]
    fn done_is_never_reaped_or_reflagged() {
        let now = Utc::now();
        let mut state = HookState::default();
        state.sessions.insert(
            "d".into(),
            SessionStatus {
                status: Status::Done,
                last_updated: Some((now - Duration::hours(STALE_AFTER_HOURS + 5)).to_rfc3339()),
                project_path: None,
                notification_type: None,
                name: None,
                harness: None,
            },
        );
        reap_stale(&mut state, now, STALE_AFTER_HOURS);
        assert_eq!(state.sessions["d"].status, Status::Done);
    }

    #[test]
    fn missing_file_loads_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            load_state(&dir.path().join("nope.json")),
            Load::Missing
        ));
    }

    #[test]
    fn clear_session_preserves_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let json = r#"{"version":7,"sessions":{
            "keep":{"status":"active","name":"Keep","future":{"a":1}},
            "drop":{"status":"done","name":"Drop"}
        },"other_top":{"x":true}}"#;
        std::fs::write(&path, json).unwrap();

        assert!(clear_session(&path, "drop").unwrap());

        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["version"], 7);
        assert_eq!(after["other_top"]["x"], true);
        assert!(after["sessions"].get("drop").is_none());
        assert_eq!(after["sessions"]["keep"]["future"]["a"], 1);
        assert_eq!(after["sessions"]["keep"]["name"], "Keep");
    }

    #[test]
    fn clear_session_is_a_noop_for_unknown_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"sessions":{"a":{"status":"active"}}}"#).unwrap();
        assert!(!clear_session(&path, "zzz").unwrap());
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(after["sessions"].get("a").is_some());
    }

    #[test]
    fn clear_session_never_writes_on_unparseable_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let broken = "{ this is not json";
        std::fs::write(&path, broken).unwrap();
        assert!(clear_session(&path, "a").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    }

    #[test]
    fn clear_done_removes_only_done() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"sessions":{"a":{"status":"done"},"b":{"status":"active"},"c":{"status":"done"}}}"#,
        )
        .unwrap();
        assert_eq!(clear_done(&path).unwrap(), 2);
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(after["sessions"].get("a").is_none());
        assert!(after["sessions"].get("c").is_none());
        assert_eq!(after["sessions"]["b"]["status"], "active");
    }

    #[test]
    fn status_strings_match_the_contract() {
        assert_eq!(Status::Active.as_str(), "active");
        assert_eq!(Status::Inactive.as_str(), "inactive");
        assert_eq!(Status::NeedsHelp.as_str(), "needs_help");
        assert_eq!(Status::Done.as_str(), "done");
        assert_eq!(Status::NeedsHelp.label(), "needs help");
        assert_eq!(Aggregate::Yellow.as_str(), "orange");
        assert_eq!(Aggregate::None.as_str(), "gray");
    }
}
