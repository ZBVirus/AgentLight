//! Turns the raw [`HookState`] into display rows: name resolution, project
//! naming, ordering, and `done` retention. Mirrors clawlight's `session.rs`
//! merge step, minus the Claude `sessions-index.json` half (a host-side reader
//! of the shared state file does not have those indexes).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::SessionLink;
use crate::state::{HookState, Status};

/// Number of newest `done` sessions kept when `show_done` is off. Matches
/// clawlight.
pub const DONE_RETENTION: usize = 5;

/// The hub wire contract is this exact type; `Deserialize` lets remote clients
/// parse `GET /api/v1/snapshot` back into the canonical shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Optional deep link back to the session in its harness UI.
    #[serde(default)]
    pub url: Option<String>,
    /// True for tool-spawned child sessions (subagents). Additive; older
    /// payloads omit it and default to a main session.
    #[serde(default)]
    pub is_subagent: bool,
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
                url: None,
                is_subagent: false,
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
        url: model.url.clone(),
        is_subagent: model.subagent,
    }
}

/// Build a session's "Open" URL from the configured link mode.
///
/// `producer_url` is the URL the source attached (the plugin or hub template);
/// it is used only in [`SessionLink::Producer`] mode, so the app can follow the
/// producer when asked while defaulting to its own OpenCode v2 link.
pub fn build_session_link(
    mode: SessionLink,
    base: Option<&str>,
    session_id: &str,
    harness: Option<&str>,
    producer_url: Option<&str>,
) -> Option<String> {
    if session_id.trim().is_empty() {
        return None;
    }
    // A v1/v2 web link only makes sense for an opencode session; leave other
    // harnesses (codex, copilot, claude) without one rather than open a dead URL.
    let is_opencode = harness.is_none_or(|h| h.trim().eq_ignore_ascii_case("opencode"));
    if !is_opencode && matches!(mode, SessionLink::OpencodeV1 | SessionLink::OpencodeV2) {
        return None;
    }
    let base = base.map(str::trim).filter(|value| !value.is_empty());
    match mode {
        SessionLink::Off => None,
        SessionLink::Producer => producer_url
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_string),
        SessionLink::OpencodeV1 => {
            let base = base
                .unwrap_or("http://localhost:4096")
                .trim_end_matches('/');
            Some(format!("{base}/session/{session_id}"))
        }
        SessionLink::OpencodeV2 => {
            // The v2 web UI keys a server by its origin, URL-safe base64 with
            // the padding stripped, e.g. `/server/<key>/session/<id>`.
            let base = base
                .unwrap_or("http://localhost:4096")
                .trim_end_matches('/');
            let key = base64_url(base.as_bytes());
            Some(format!("{base}/server/{key}/session/{session_id}"))
        }
    }
}

/// URL-safe base64 without padding, matching OpenCode v2's `serverKey`.
fn base64_url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 63) as usize] as char);
        }
    }
    out
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
    fn display_session_keeps_the_session_url() {
        let model = crate::source::Session::new(
            crate::source::SessionKey::new(crate::source::SourceId::new("hub"), "s1"),
            "Fix auth",
            Status::Active,
        )
        .with_url("http://localhost:4096/session/s1");
        assert_eq!(
            display_session(&model).url.as_deref(),
            Some("http://localhost:4096/session/s1")
        );
    }

    #[test]
    fn session_link_defaults_to_the_v2_route() {
        let link = build_session_link(SessionLink::OpencodeV2, None, "ses_1", None, None).unwrap();
        assert_eq!(
            link,
            "http://localhost:4096/server/aHR0cDovL2xvY2FsaG9zdDo0MDk2/session/ses_1"
        );
    }

    #[test]
    fn session_link_v1_uses_the_session_route() {
        let link = build_session_link(SessionLink::OpencodeV1, None, "abc", None, None).unwrap();
        assert_eq!(link, "http://localhost:4096/session/abc");
    }

    #[test]
    fn session_link_honors_a_custom_base() {
        let link = build_session_link(
            SessionLink::OpencodeV2,
            Some("http://localhost:49374/"),
            "s1",
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            link,
            "http://localhost:49374/server/aHR0cDovL2xvY2FsaG9zdDo0OTM3NA/session/s1"
        );
    }

    #[test]
    fn session_link_producer_uses_the_attached_url() {
        let link = build_session_link(
            SessionLink::Producer,
            None,
            "s1",
            None,
            Some("http://opencode/session/s1"),
        );
        assert_eq!(link.as_deref(), Some("http://opencode/session/s1"));
        assert_eq!(
            build_session_link(SessionLink::Producer, None, "s1", None, None),
            None
        );
    }

    #[test]
    fn session_link_skips_non_opencode_harnesses() {
        assert_eq!(
            build_session_link(SessionLink::OpencodeV2, None, "s1", Some("claude"), None),
            None
        );
        assert!(
            build_session_link(SessionLink::OpencodeV2, None, "s1", Some("opencode"), None)
                .is_some()
        );
    }

    #[test]
    fn session_link_off_and_empty_id_produce_nothing() {
        assert_eq!(
            build_session_link(SessionLink::Off, None, "s1", None, Some("http://x/1")),
            None
        );
        assert_eq!(
            build_session_link(SessionLink::OpencodeV2, None, "", None, None),
            None
        );
    }

    #[test]
    fn base64_url_is_unpadded_and_url_safe() {
        assert_eq!(
            base64_url(b"http://127.0.0.1:49374"),
            "aHR0cDovLzEyNy4wLjAuMTo0OTM3NA"
        );
        // 0xfb 0xff would map to '+' and '/' in standard base64.
        assert_eq!(base64_url(&[0xfb, 0xff]), "-_8");
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
