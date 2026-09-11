//! Turns the raw [`HookState`] into display rows: name resolution, project
//! naming, ordering, and `done` retention. Mirrors clawlight's `session.rs`
//! merge step, minus the Claude `sessions-index.json` half (a host-side reader
//! of the shared state file does not have those indexes).

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::state::{HookState, Status};

/// Number of newest `done` sessions kept when `show_done` is off. Matches
/// clawlight.
pub const DONE_RETENTION: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DisplaySession {
    pub session_id: String,
    pub name: String,
    pub status: Status,
    pub status_label: String,
    pub project_name: String,
    pub project_path: String,
    pub harness: Option<String>,
    pub harness_badge: Option<String>,
    pub last_updated: String,
    pub is_done: bool,
}

/// Short badge for a harness: `oc`, `cx`, `co`, …; `None` for Claude Code
/// (absent harness), which shows no badge.
pub fn harness_badge(harness: &str) -> String {
    let trimmed = harness.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    trimmed.chars().take(2).collect::<String>().to_lowercase()
}

pub fn project_short_name(project_path: &str) -> String {
    if project_path.is_empty() {
        return "unknown".to_string();
    }
    std::path::Path::new(project_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| project_path.to_string())
}

/// First `n` characters, char-safe (`session_id` is ASCII today, but this can
/// never panic on a multibyte boundary).
pub fn char_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Truncate by characters, appending `...` when shortened. Byte slicing would
/// panic on arbitrary UTF-8 names.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(3)).collect();
    format!("{kept}...")
}

pub fn load_sessions(
    state: &HookState,
    show_done: bool,
    done_retention: usize,
) -> Vec<DisplaySession> {
    let mut rows: Vec<DisplaySession> = state
        .sessions
        .iter()
        .map(|(id, session)| {
            let name = session
                .name
                .clone()
                .unwrap_or_else(|| format!("Session {}", char_prefix(id, 8)));
            let project_path = session.project_path.clone().unwrap_or_default();
            let harness = session
                .harness
                .as_ref()
                .map(|h| h.trim().to_string())
                .filter(|h| !h.is_empty());
            DisplaySession {
                session_id: id.clone(),
                name,
                status: session.status,
                status_label: session.status.label().to_string(),
                project_name: project_short_name(&project_path),
                project_path,
                harness_badge: harness.as_deref().map(harness_badge),
                harness,
                last_updated: session.last_updated.clone().unwrap_or_default(),
                is_done: session.status == Status::Done,
            }
        })
        .collect();

    rows.sort_by(|a, b| {
        a.status
            .urgency()
            .cmp(&b.status.urgency())
            .then_with(|| parse_ts(&b.last_updated).cmp(&parse_ts(&a.last_updated)))
    });

    if !show_done {
        let mut done_seen = 0usize;
        rows.retain(|row| {
            if row.status == Status::Done {
                done_seen += 1;
                done_seen <= done_retention
            } else {
                true
            }
        });
    }

    rows
}

fn parse_ts(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Map a normalized [`crate::source::Session`] back to the display row the
/// frontend consumes. The engine calls this after merging and retention.
pub fn display_session(model: &crate::source::Session) -> DisplaySession {
    let project_path = model.project.clone().unwrap_or_default();
    DisplaySession {
        session_id: model.key.session_id.clone(),
        name: model.name.clone(),
        status: model.status,
        status_label: model.status.label().to_string(),
        project_name: project_short_name(&project_path),
        project_path,
        harness: model.harness.clone(),
        harness_badge: model.badge.clone(),
        last_updated: model.last_updated.clone(),
        is_done: model.is_done,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SessionStatus;

    fn session(
        name: Option<&str>,
        status: Status,
        updated: &str,
        harness: Option<&str>,
    ) -> SessionStatus {
        SessionStatus {
            status,
            last_updated: Some(updated.to_string()),
            project_path: Some("/home/me/projects/agentlight".to_string()),
            notification_type: None,
            name: name.map(str::to_string),
            harness: harness.map(str::to_string),
        }
    }

    #[test]
    fn sort_puts_needs_help_first_then_recency() {
        let mut state = HookState::default();
        state.sessions.insert(
            "a".into(),
            session(
                Some("active recent"),
                Status::Active,
                "2026-01-02T00:00:00Z",
                None,
            ),
        );
        state.sessions.insert(
            "b".into(),
            session(
                Some("help"),
                Status::NeedsHelp,
                "2026-01-01T00:00:00Z",
                None,
            ),
        );
        state.sessions.insert(
            "c".into(),
            session(Some("idle"), Status::Inactive, "2026-01-03T00:00:00Z", None),
        );
        let rows = load_sessions(&state, false, DONE_RETENTION);
        let order: Vec<_> = rows.iter().map(|r| r.status).collect();
        assert_eq!(
            order,
            vec![Status::NeedsHelp, Status::Active, Status::Inactive]
        );
    }

    #[test]
    fn keeps_only_newest_done_unless_requested() {
        let mut state = HookState::default();
        for i in 0..8 {
            state.sessions.insert(
                format!("done{i}"),
                session(
                    None,
                    Status::Done,
                    &format!("2026-01-0{}T00:00:00Z", i + 1),
                    None,
                ),
            );
        }
        state.sessions.insert(
            "live".into(),
            session(None, Status::Active, "2026-01-01T00:00:00Z", None),
        );

        let kept = load_sessions(&state, false, DONE_RETENTION);
        assert_eq!(kept.iter().filter(|r| r.is_done).count(), DONE_RETENTION);
        assert_eq!(kept.iter().filter(|r| !r.is_done).count(), 1);

        let all = load_sessions(&state, true, DONE_RETENTION);
        assert_eq!(all.iter().filter(|r| r.is_done).count(), 8);
    }

    #[test]
    fn falls_back_to_session_id_prefix() {
        let mut state = HookState::default();
        state.sessions.insert(
            "abcdefghijkl".into(),
            session(None, Status::Active, "2026-01-01T00:00:00Z", None),
        );
        let rows = load_sessions(&state, false, DONE_RETENTION);
        assert_eq!(rows[0].name, "Session abcdefgh");
    }

    #[test]
    fn badges_come_from_harness() {
        assert_eq!(harness_badge("opencode"), "op");
        assert_eq!(harness_badge("codex"), "co");
        assert_eq!(harness_badge("copilot"), "co");
        assert_eq!(harness_badge("Claude"), "cl");
        assert_eq!(harness_badge(""), "");
    }

    #[test]
    fn truncate_is_char_boundary_safe() {
        let s = "日本語のプロンプトでセッションを開始してくださいね、これはとても長いテストです本当に長いですね";
        let out = truncate(s, 20);
        assert!(out.ends_with("..."));
        assert!(out.chars().count() <= 20);
        assert_eq!(truncate("short", 20), "short");
    }

    #[test]
    fn project_name_is_the_last_path_segment() {
        let mut state = HookState::default();
        state.sessions.insert(
            "a".into(),
            session(
                None,
                Status::Active,
                "2026-01-01T00:00:00Z",
                Some("opencode"),
            ),
        );
        let rows = load_sessions(&state, false, DONE_RETENTION);
        assert_eq!(rows[0].project_name, "agentlight");
        assert_eq!(rows[0].harness_badge.as_deref(), Some("op"));
        assert_eq!(rows[0].status_label, "working");
    }
}
