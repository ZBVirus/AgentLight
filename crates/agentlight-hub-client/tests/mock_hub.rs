//! Tests against a tiny in-process HTTP/1.1 server.
//!
//! This deliberately does not depend on `agentlight-server` (which is under
//! active development): the mock owns the smallest slice of the protocol the
//! client actually needs and records every request for inspection.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use agentlight_core::{
    SessionKey, SourceCommand, SourceEvent, SourceHealth, SourceId, StateSource, Status,
};
use agentlight_hub_client::{HubClient, HubConfig, HubSource};

const SNAPSHOT_NEEDS_HELP: &str = r#"{
  "ok": true,
  "error": null,
  "state_path": "/state.json",
  "exists": true,
  "aggregate": "red",
  "counts": { "needs_help": 1, "active": 0, "inactive": 0, "done": 0, "total": 1 },
  "sessions": [
    {
      "session_id": "s1",
      "name": "Fix auth",
      "status": "needs_help",
      "status_label": "needs help",
      "project_name": "agentlight",
      "project_path": "/work/agentlight",
      "harness": "opencode",
      "harness_badge": "op",
      "last_updated": "2026-09-12T10:00:00Z",
      "is_done": false
    }
  ],
  "yellow_mode": "any_inactive",
  "generated_at": "2026-09-12T10:00:01Z"
}"#;

const SNAPSHOT_ACTIVE: &str = r#"{
  "ok": true,
  "error": null,
  "state_path": "/state.json",
  "exists": true,
  "aggregate": "green",
  "counts": { "needs_help": 0, "active": 1, "inactive": 0, "done": 0, "total": 1 },
  "sessions": [
    {
      "session_id": "s1",
      "name": "Fix auth",
      "status": "active",
      "status_label": "working",
      "project_name": "agentlight",
      "project_path": "/work/agentlight",
      "harness": "opencode",
      "harness_badge": "op",
      "last_updated": "2026-09-12T10:00:00Z",
      "is_done": false
    }
  ],
  "yellow_mode": "any_inactive",
  "generated_at": "2026-09-12T10:00:01Z"
}"#;

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Recorded {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

struct MockState {
    snapshot_status: u16,
    snapshot_body: String,
    command_status: u16,
    command_body: String,
}

struct MockHub {
    addr: SocketAddr,
    state: Arc<Mutex<MockState>>,
    requests: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MockHub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock hub");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let state = Arc::new(Mutex::new(MockState {
            snapshot_status: 200,
            snapshot_body: SNAPSHOT_ACTIVE.to_string(),
            command_status: 200,
            command_body: "{\"revision\":1}".to_string(),
        }));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread = {
            let state = state.clone();
            let requests = requests.clone();
            let stop = stop.clone();
            thread::Builder::new()
                .name("mock-hub".to_string())
                .spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        match listener.accept() {
                            Ok((mut stream, _)) => {
                                let _ = handle_connection(&mut stream, &state, &requests);
                            }
                            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(_) => break,
                        }
                    }
                })
                .expect("spawn mock hub")
        };

        Self {
            addr,
            state,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn set_snapshot(&self, status: u16, body: impl Into<String>) {
        let mut state = self.state.lock().unwrap();
        state.snapshot_status = status;
        state.snapshot_body = body.into();
    }

    fn set_command(&self, status: u16, body: impl Into<String>) {
        let mut state = self.state.lock().unwrap();
        state.command_status = status;
        state.command_body = body.into();
    }

    fn recorded(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockHub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle_connection(
    stream: &mut TcpStream,
    state: &Mutex<MockState>,
    requests: &Mutex<Vec<Recorded>>,
) -> std::io::Result<()> {
    let request = read_request(stream)?;
    let (status, body) = {
        let state = state.lock().unwrap();
        match request.path.as_str() {
            "/api/v1/snapshot" => (state.snapshot_status, state.snapshot_body.clone()),
            "/api/v1/commands" => (state.command_status, state.command_body.clone()),
            _ => (404, "{\"error\":{\"code\":\"not_found\"}}".to_string()),
        }
    };
    requests.lock().unwrap().push(request);
    write_response(stream, status, &body)
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Recorded> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;

    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        if let Some(position) = find(&buffer, b"\r\n\r\n") {
            break position + 4;
        }
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before headers",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > 1 << 20 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request headers too large",
            ));
        }
    };

    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_string(), value.trim().to_string()));
        }
    }

    let content_length: usize = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);

    let mut body_bytes = buffer[header_end..].to_vec();
    while body_bytes.len() < content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        body_bytes.extend_from_slice(&chunk[..read]);
    }
    body_bytes.truncate(content_length);

    Ok(Recorded {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body_bytes).to_string(),
    })
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn wait_for(mut predicate: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    predicate()
}

#[test]
fn snapshot_parses_into_normalized_sessions_and_health() {
    let hub = MockHub::start();
    hub.set_snapshot(200, SNAPSHOT_NEEDS_HELP);
    let source = HubSource::new(HubConfig::new(hub.base_url()).with_id("hub"));

    let snapshot = source.snapshot(Utc::now());
    assert_eq!(snapshot.health, SourceHealth::Ready);
    assert_eq!(snapshot.source, SourceId::new("hub"));
    assert_eq!(snapshot.sessions.len(), 1);

    let session = &snapshot.sessions[0];
    assert_eq!(session.key, SessionKey::new(SourceId::new("hub"), "s1"));
    assert_eq!(session.name, "Fix auth");
    assert_eq!(session.status, Status::NeedsHelp);
    assert_eq!(session.project.as_deref(), Some("/work/agentlight"));
    assert_eq!(session.harness.as_deref(), Some("opencode"));
    assert_eq!(session.badge.as_deref(), Some("op"));
    assert_eq!(session.last_updated, "2026-09-12T10:00:00Z");
    assert_eq!(
        session.updated_at,
        Some(
            DateTime::parse_from_rfc3339("2026-09-12T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        )
    );
    assert!(!session.is_done);

    let client = HubClient::new(HubConfig::new(hub.base_url()));
    let parsed = client.snapshot().expect("client snapshot");
    assert!(parsed.ok);
    assert_eq!(parsed.aggregate, "red");
    assert_eq!(parsed.sessions.len(), 1);
}

#[test]
fn health_maps_missing_and_unreadable() {
    let hub = MockHub::start();
    let source = HubSource::new(HubConfig::new(hub.base_url()));

    hub.set_snapshot(
        200,
        r#"{"ok":false,"error":null,"state_path":"/s","exists":false,"aggregate":"gray","counts":{"needs_help":0,"active":0,"inactive":0,"done":0,"total":0},"sessions":[],"yellow_mode":"any_inactive","generated_at":"2026-09-12T10:00:01Z"}"#,
    );
    assert_eq!(source.snapshot(Utc::now()).health, SourceHealth::Missing);

    hub.set_snapshot(
        200,
        r#"{"ok":false,"error":"Could not read state file","state_path":"/s","exists":true,"aggregate":"gray","counts":{"needs_help":0,"active":0,"inactive":0,"done":0,"total":0},"sessions":[],"yellow_mode":"any_inactive","generated_at":"2026-09-12T10:00:01Z"}"#,
    );
    match source.snapshot(Utc::now()).health {
        SourceHealth::Unreadable(message) => assert!(message.contains("Could not read")),
        other => panic!("expected unreadable, got {other:?}"),
    }
}

#[test]
fn server_error_and_garbage_yield_unreadable_without_panicking() {
    let hub = MockHub::start();
    hub.set_snapshot(500, "{\"error\":{\"code\":\"internal_error\"}}");
    let source = HubSource::new(HubConfig::new(hub.base_url()));
    match source.snapshot(Utc::now()).health {
        SourceHealth::Unreadable(_) => {}
        other => panic!("expected unreadable for HTTP 500, got {other:?}"),
    }

    hub.set_snapshot(200, "not json at all");
    match source.snapshot(Utc::now()).health {
        SourceHealth::Unreadable(_) => {}
        other => panic!("expected unreadable for garbage body, got {other:?}"),
    }
}

#[test]
fn token_header_is_sent_when_configured() {
    let hub = MockHub::start();
    hub.set_snapshot(200, SNAPSHOT_ACTIVE);
    let client = HubClient::new(HubConfig::new(hub.base_url()).with_token("secret"));

    client.snapshot().expect("snapshot");

    let requests = hub.recorded();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/api/v1/snapshot");
    assert_eq!(requests[0].header("authorization"), Some("Bearer secret"));
}

#[test]
fn no_token_header_when_unset() {
    let hub = MockHub::start();
    hub.set_snapshot(200, SNAPSHOT_ACTIVE);
    let client = HubClient::new(HubConfig::new(hub.base_url()));

    client.snapshot().expect("snapshot");

    let requests = hub.recorded();
    assert_eq!(requests[0].header("authorization"), None);
}

#[test]
fn commands_post_expected_tagged_bodies() {
    let hub = MockHub::start();
    hub.set_command(200, "{\"revision\":8}");
    let source = HubSource::new(HubConfig::new(hub.base_url()).with_id("hub"));

    source
        .command(SourceCommand::RemoveSession(SessionKey::new(
            SourceId::new("hub"),
            "abc",
        )))
        .expect("remove_session");

    let requests = hub.recorded();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/api/v1/commands");
    assert_eq!(requests[0].header("content-type"), Some("application/json"));
    assert_eq!(
        requests[0].body,
        "{\"command\":\"remove_session\",\"source\":\"hub\",\"session_id\":\"abc\"}"
    );

    source
        .command(SourceCommand::ClearDone)
        .expect("clear_done");
    let requests = hub.recorded();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].path, "/api/v1/commands");
    assert_eq!(requests[1].body, "{\"command\":\"clear_done\"}");

    source
        .command(SourceCommand::RemoveSession(SessionKey::new(
            SourceId::new("other"),
            "abc",
        )))
        .expect("ignored other-source key");
    assert_eq!(hub.recorded().len(), 2);
}

#[test]
fn subscribe_fires_when_snapshot_changes() {
    let hub = MockHub::start();
    hub.set_snapshot(200, SNAPSHOT_ACTIVE);
    let source = HubSource::new(HubConfig::new(hub.base_url()).with_poll_ms(50));

    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    source.subscribe(Arc::new(move |event: SourceEvent| {
        assert_eq!(event, SourceEvent::Changed);
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    assert!(
        wait_for(|| hits.load(Ordering::SeqCst) >= 1, Duration::from_secs(3)),
        "no initial change event"
    );

    hub.set_snapshot(200, SNAPSHOT_NEEDS_HELP);
    assert!(
        wait_for(|| hits.load(Ordering::SeqCst) >= 2, Duration::from_secs(3)),
        "no change event after the snapshot changed"
    );
}
