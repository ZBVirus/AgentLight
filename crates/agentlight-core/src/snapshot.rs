//! The JSON payload rendered by the frontend. Building it lives here (not in
//! the Tauri layer) so it is unit-testable on any host.

use chrono::Utc;
use serde::Serialize;

use crate::config::Config;
use crate::session::{load_sessions, DisplaySession, DONE_RETENTION};
use crate::state::{self, aggregate, reap_stale, HookState, Load, STALE_AFTER_HOURS};

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct Counts {
    pub needs_help: usize,
    pub active: usize,
    pub inactive: usize,
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    /// Whether the state file was read and parsed. `false` means missing or
    /// unreadable; the UI shows a waiting/error state instead of stale data.
    pub ok: bool,
    pub error: Option<String>,
    pub state_path: String,
    pub exists: bool,
    /// `red` | `orange` | `green` | `gray`.
    pub aggregate: String,
    pub counts: Counts,
    pub sessions: Vec<DisplaySession>,
    /// Echoed so the UI can render the active mode without a second call.
    pub yellow_mode: String,
    pub generated_at: String,
}

fn count(state: &HookState) -> Counts {
    let mut counts = Counts::default();
    for session in state.sessions.values() {
        counts.total += 1;
        match session.status {
            state::Status::NeedsHelp => counts.needs_help += 1,
            state::Status::Active => counts.active += 1,
            state::Status::Inactive => counts.inactive += 1,
            state::Status::Done => counts.done += 1,
        }
    }
    counts
}

/// Build the snapshot for a config. `now` is injected for deterministic tests.
pub fn build_snapshot_at(
    state_path: &std::path::Path,
    config: &Config,
    now: chrono::DateTime<Utc>,
) -> Snapshot {
    let mut snap = Snapshot {
        ok: false,
        error: None,
        state_path: state_path.to_string_lossy().to_string(),
        exists: state_path.exists(),
        aggregate: "gray".to_string(),
        counts: Counts::default(),
        sessions: Vec::new(),
        yellow_mode: match config.yellow_mode {
            crate::config::YellowMode::AnyInactive => "any_inactive".to_string(),
            crate::config::YellowMode::ActiveWins => "active_wins".to_string(),
        },
        generated_at: now.to_rfc3339(),
    };

    match state::load_state(state_path) {
        Load::Loaded(mut state) => {
            reap_stale(&mut state, now, STALE_AFTER_HOURS);
            snap.counts = count(&state);
            snap.aggregate = aggregate(&state, config.yellow_mode).as_str().to_string();
            snap.sessions = load_sessions(&state, config.show_done, DONE_RETENTION);
            snap.ok = true;
        }
        Load::Missing => {
            snap.error = Some("Waiting for clawlight state file".to_string());
        }
        Load::Unreadable(e) => {
            snap.error = Some(format!("Could not read state file: {e}"));
        }
    }

    snap
}

pub fn build_snapshot(state_path: &std::path::Path, config: &Config) -> Snapshot {
    build_snapshot_at(state_path, config, Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::YellowMode;
    use chrono::Duration;

    fn write_state(dir: &std::path::Path, json: &str) -> std::path::PathBuf {
        let path = dir.join("state.json");
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn missing_file_is_not_ok_but_not_an_error_to_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let snap = build_snapshot(&path, &Config::default());
        assert!(!snap.ok);
        assert!(!snap.exists);
        assert_eq!(snap.aggregate, "gray");
        assert_eq!(snap.counts.total, 0);
        assert!(snap.error.is_some());
    }

    #[test]
    fn aggregates_and_counts_a_mixed_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_state(
            dir.path(),
            r#"{"sessions":{
                "a":{"status":"needs_help","name":"A"},
                "b":{"status":"active","name":"B","harness":"opencode"},
                "c":{"status":"inactive","name":"C"},
                "d":{"status":"done","name":"D"}
            }}"#,
        );
        let snap = build_snapshot(&path, &Config::default());
        assert!(snap.ok);
        assert_eq!(snap.aggregate, "red");
        assert_eq!(
            snap.counts,
            Counts {
                needs_help: 1,
                active: 1,
                inactive: 1,
                done: 1,
                total: 4
            }
        );
        assert_eq!(snap.sessions.len(), 4);
        assert_eq!(snap.sessions[0].status, crate::state::Status::NeedsHelp);
    }

    #[test]
    fn active_wins_keeps_green_with_an_idle_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_state(
            dir.path(),
            r#"{"sessions":{"a":{"status":"active"},"b":{"status":"inactive"}}}"#,
        );
        let cfg = Config {
            yellow_mode: YellowMode::ActiveWins,
            ..Config::default()
        };
        assert_eq!(build_snapshot(&path, &cfg).aggregate, "green");
        assert_eq!(
            build_snapshot(&path, &Config::default()).aggregate,
            "orange"
        );
    }

    #[test]
    fn unreadable_file_reports_an_error_and_no_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_state(dir.path(), "{ broken");
        let snap = build_snapshot(&path, &Config::default());
        assert!(!snap.ok);
        assert!(snap.exists);
        assert!(snap.error.unwrap().contains("Could not read"));
        assert!(snap.sessions.is_empty());
    }

    #[test]
    fn stale_sessions_render_as_done() {
        let dir = tempfile::tempdir().unwrap();
        let old = (Utc::now() - Duration::hours(STALE_AFTER_HOURS + 2)).to_rfc3339();
        let path = write_state(
            dir.path(),
            &format!(r#"{{"sessions":{{"a":{{"status":"active","last_updated":"{old}"}}}}}}"#),
        );
        let snap = build_snapshot(&path, &Config::default());
        assert_eq!(snap.aggregate, "gray");
        assert!(snap.sessions.iter().all(|s| s.is_done));
    }
}
