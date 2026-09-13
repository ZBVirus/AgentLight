//! Integration test for the SSE seed. Binds a real server on `127.0.0.1:0` and
//! reads the raw HTTP response so the very first frame is observable.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use agentlight_core::{Config, Engine};
use agentlight_server::{start, ServerConfig};

/// Read from `stream` until `needle` appears or the read times out. Returns the
/// bytes accumulated so far.
fn read_until(stream: &mut TcpStream, needle: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 1024];
    while !raw.windows(needle.len()).any(|window| window == needle) {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }
    raw
}

#[test]
fn events_streams_the_current_state_as_the_first_frame() {
    let engine = Engine::new(Config::default());
    let config = ServerConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        admin_token_hash: None,
        ..ServerConfig::default()
    };
    let mut handle = start(engine, config).unwrap();

    let mut stream = TcpStream::connect(handle.addr()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET /api/v1/events HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    stream.flush().unwrap();

    // The seed frame is the only update this source-less engine produces, so
    // waiting for `"revision"` is guaranteed to land inside the first frame.
    let raw = read_until(&mut stream, b"\"revision\"");
    // Close the socket before joining so the long-lived SSE task can end.
    drop(stream);
    handle.shutdown();

    let text = String::from_utf8_lossy(&raw).into_owned();
    let (headers, body) = text.split_once("\r\n\r\n").expect("complete HTTP response");
    assert!(headers.starts_with("HTTP/1.1 200"), "{text}");
    let body = body.trim_start_matches(['\r', '\n']);
    let first_frame = body.split("\n\n").next().unwrap_or(body);
    assert!(
        first_frame.contains("event: update"),
        "first frame was not an update: {text:?}"
    );
    assert!(first_frame.contains("\"revision\""), "{text:?}");
}
