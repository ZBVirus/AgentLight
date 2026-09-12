//! `agentlight-hook` — pipe one session event (or a batch) to an AgentLight
//! hub running in events mode (`AGENTLIGHT_SOURCE=events`).
//!
//! This is the producer side of the push path: an agent integration formats a
//! [`SessionEvent`](agentlight_source_events::SessionEvent) and calls this
//! binary rather than writing a shared `state.json`.
//!
//! Input is either a single event object or `{ "events": [ ... ] }`, read from
//! stdin or passed with `--json '<json>'`. The hub address and credential come
//! from the environment:
//!
//! - `AGENTLIGHT_HUB_URL` — base URL, default `http://127.0.0.1:8787`.
//! - `AGENTLIGHT_TOKEN` — optional bearer token (admin or device).
//!
//! The token is a credential: it is never printed, in success or failure.

use std::io::Read;

use agentlight_hub_client::{HubClient, HubConfig, DEFAULT_BASE_URL};
use agentlight_source_events::SessionEvent;

const USAGE: &str = "\
agentlight-hook — push session events to an AgentLight hub

USAGE:
    agentlight-hook [--json '<json>']

Reads a single SessionEvent object or {\"events\":[ ... ]} from stdin unless
--json supplies it inline, then POSTs to /api/v1/ingest.

ENVIRONMENT:
    AGENTLIGHT_HUB_URL    Hub base URL (default http://127.0.0.1:8787)
    AGENTLIGHT_TOKEN      Bearer token; optional, never printed

EXIT:
    0  the hub accepted the batch
    1  invalid input, transport failure, or the hub rejected the request
";

fn main() {
    if let Err(message) = run() {
        eprintln!("agentlight-hook: {message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let input = match parse_args()? {
        Some(json) => json,
        None => read_stdin()?,
    };

    let events = parse_events(&input)?;
    if events.is_empty() {
        return Err("no events to send".to_string());
    }

    let client = HubClient::new(hub_config());
    match client.ingest(&events) {
        Ok(accepted) => {
            println!("accepted {accepted} event(s)");
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Parse `--json <value>`, `-h`/`--help`. Returns `Some(json)` when inline JSON
/// was supplied, `None` when the caller should read stdin.
fn parse_args() -> Result<Option<String>, String> {
    let mut args = std::env::args().skip(1);
    let mut json = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--json requires a JSON argument".to_string())?;
                json = Some(value);
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    Ok(json)
}

fn read_stdin() -> Result<String, String> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|error| format!("could not read stdin: {error}"))?;
    if input.trim().is_empty() {
        return Err("no input: pipe a SessionEvent or use --json".to_string());
    }
    Ok(input)
}

/// Accept either a single event object or an `{ "events": [ ... ] }` envelope.
fn parse_events(input: &str) -> Result<Vec<SessionEvent>, String> {
    let value: serde_json::Value =
        serde_json::from_str(input).map_err(|error| format!("invalid JSON: {error}"))?;
    if let Some(batch) = value.get("events") {
        return serde_json::from_value(batch.clone())
            .map_err(|error| format!("invalid event batch: {error}"));
    }
    let event: SessionEvent =
        serde_json::from_value(value).map_err(|error| format!("invalid session event: {error}"))?;
    Ok(vec![event])
}

fn hub_config() -> HubConfig {
    let base_url =
        env_non_empty("AGENTLIGHT_HUB_URL").unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let mut config = HubConfig::new(base_url);
    if let Some(token) = env_non_empty("AGENTLIGHT_TOKEN") {
        config = config.with_token(token);
    }
    config
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentlight_core::Status;

    #[test]
    fn parses_a_single_event() {
        let events = parse_events(r#"{"session_id":"s1","status":"needs_help"}"#).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id, "s1");
        assert_eq!(events[0].status, Status::NeedsHelp);
        assert!(events[0].name.is_none());
    }

    #[test]
    fn parses_an_event_batch_and_ignores_unknown_fields() {
        let events =
            parse_events(r#"{"events":[{"session_id":"s1","status":"active","future":1}]}"#)
                .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, Status::Active);
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(parse_events("not json").is_err());
        assert!(parse_events(r#"{"session_id":"s1"}"#).is_err());
    }
}
