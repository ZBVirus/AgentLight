//! Integration test for the SSE `HubSource` path against a tiny streaming mock.
//!
//! The mock owns only the slice of the protocol the source needs: it answers
//! `/api/v1/events` with `text/event-stream`, no `Content-Length`, writes one
//! `event: update` frame, then closes. Because each connection serves exactly
//! one frame, a second [`SourceEvent::Changed`] proves the source reconnected
//! rather than silently dying. `/api/v1/snapshot` is served too, so the poll
//! fallback remains exercised if the stream ever fails.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use agentlight_core::{SourceEvent, StateSource};
use agentlight_hub_client::{HubConfig, HubSource};

const SNAPSHOT: &str = r#"{
  "ok": true,
  "error": null,
  "state_path": "/state.json",
  "exists": true,
  "aggregate": "green",
  "counts": { "needs_help": 0, "active": 1, "inactive": 0, "done": 0, "total": 1 },
  "sessions": [],
  "yellow_mode": "any_inactive",
  "generated_at": "2026-09-12T10:00:01Z"
}"#;

struct MockSseHub {
    addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MockSseHub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock sse hub");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let stop = Arc::new(AtomicBool::new(false));
        let connections = Arc::new(AtomicUsize::new(0));

        let thread = {
            let stop = stop.clone();
            let connections = connections.clone();
            thread::Builder::new()
                .name("mock-sse-hub".to_string())
                .spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        match listener.accept() {
                            Ok((mut stream, _)) => {
                                connections.fetch_add(1, Ordering::SeqCst);
                                let _ = respond(&mut stream);
                            }
                            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(5));
                            }
                            Err(_) => break,
                        }
                    }
                })
                .expect("spawn mock sse hub")
        };

        Self {
            addr,
            connections,
            stop,
            thread: Some(thread),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for MockSseHub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn respond(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let path = read_request_path(stream)?;
    match path.as_str() {
        "/api/v1/events" => {
            let head = "HTTP/1.1 200 OK\r\n\
                        Content-Type: text/event-stream\r\n\
                        Connection: close\r\n\
                        \r\n";
            stream.write_all(head.as_bytes())?;
            // A keep-alive comment first exercises comment skipping, then one
            // real frame. Closing after the frame forces a reconnect.
            stream.write_all(b": keep-alive\n\n")?;
            stream.write_all(b"event: update\ndata: {\"revision\":1}\n\n")?;
            stream.flush()
        }
        "/api/v1/snapshot" => {
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\
                 \r\n\
                 {SNAPSHOT}",
                SNAPSHOT.len()
            );
            stream.write_all(response.as_bytes())?;
            stream.flush()
        }
        _ => {
            let body = "{\"error\":{\"code\":\"not_found\"}}";
            let response = format!(
                "HTTP/1.1 404 Not Found\r\n\
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
    }
}

fn read_request_path(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
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

    let head = String::from_utf8_lossy(&buffer[..header_end]);
    let request_line = head.lines().next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let _method = parts.next();
    Ok(parts.next().unwrap_or_default().to_string())
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
fn sse_stream_emits_changed_and_reconnects_after_close() {
    let hub = MockSseHub::start();
    let source = HubSource::new(HubConfig::new(hub.base_url()).with_poll_ms(50));

    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    source.subscribe(Arc::new(move |event: SourceEvent| {
        assert_eq!(event, SourceEvent::Changed);
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    assert!(
        wait_for(|| hits.load(Ordering::SeqCst) >= 1, Duration::from_secs(3)),
        "no SourceEvent::Changed from the SSE stream"
    );

    // The mock serves one frame per connection, so a second hit (and a second
    // accepted connection) shows the source reconnected instead of dying.
    assert!(
        wait_for(
            || hits.load(Ordering::SeqCst) >= 2 && hub.connections.load(Ordering::SeqCst) >= 2,
            Duration::from_secs(3)
        ),
        "source did not reconnect (hits={}, connections={})",
        hits.load(Ordering::SeqCst),
        hub.connections.load(Ordering::SeqCst)
    );
}
