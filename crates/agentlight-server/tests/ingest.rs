//! Events-mode (push) server tests. Everything runs in-process via
//! `tower::ServiceExt::oneshot`; no socket is bound.

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use agentlight_core::SourceKind;
use agentlight_server::{hash_token, router, AppState, ServerConfig};

fn events_app() -> axum::Router {
    let config = ServerConfig {
        source_kind: SourceKind::Push,
        admin_token_hash: Some(hash_token("secret")),
        ..ServerConfig::default()
    };
    router(AppState::from_config(&config))
}

fn request(method: Method, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    builder.body(body).unwrap()
}

async fn read_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn snapshot(app: &axum::Router, token: &str) -> Value {
    let response = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/snapshot", Some(token), None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    read_json(response).await
}

#[tokio::test]
async fn ingest_accepts_a_batch_and_snapshot_shows_it() {
    let app = events_app();

    let response = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/ingest",
            Some("secret"),
            Some(json!({
                "events": [
                    {
                        "session_id": "a",
                        "status": "needs_help",
                        "name": "Fix auth",
                        "project_path": "/work/agentlight",
                        "harness": "opencode",
                        "last_updated": "2026-01-01T01:00:00Z",
                        "unknown_field": 1
                    },
                    { "session_id": "b", "status": "active", "name": "Run tests" }
                ],
                "extra": true
            })),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(read_json(response).await["accepted"], 2);

    let body = snapshot(&app, "secret").await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["source_kind"], "push");
    assert_eq!(body["source_label"], "events");
    assert_eq!(body["aggregate"], "red");
    assert_eq!(body["counts"]["total"], 2);
    assert_eq!(body["counts"]["needs_help"], 1);
    assert_eq!(body["counts"]["active"], 1);
    assert_eq!(body["sessions"][0]["session_id"], "a");
    assert_eq!(body["sessions"][0]["status"], "needs_help");
    assert_eq!(body["sessions"][0]["harness_badge"], "op");
}

#[tokio::test]
async fn ingest_requires_a_token() {
    let app = events_app();

    let response = app
        .oneshot(request(
            Method::POST,
            "/api/v1/ingest",
            None,
            Some(json!({ "events": [] })),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn follow_up_ingest_updates_the_status() {
    let app = events_app();

    let first = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/ingest",
            Some("secret"),
            Some(json!({ "events": [{ "session_id": "a", "status": "active" }] })),
        ))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(snapshot(&app, "secret").await["aggregate"], "green");

    let second = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/ingest",
            Some("secret"),
            Some(json!({ "events": [
                { "session_id": "a", "status": "needs_help", "last_updated": "2026-01-01T02:00:00Z" }
            ] })),
        ))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::OK);

    let body = snapshot(&app, "secret").await;
    assert_eq!(body["aggregate"], "red");
    assert_eq!(body["counts"]["needs_help"], 1);
    assert_eq!(body["counts"]["total"], 1);
}

#[tokio::test]
async fn ingest_is_rejected_in_file_mode() {
    let config = ServerConfig {
        admin_token_hash: Some(hash_token("secret")),
        ..ServerConfig::default()
    };
    let app = router(AppState::from_config(&config));

    let response = app
        .oneshot(request(
            Method::POST,
            "/api/v1/ingest",
            Some("secret"),
            Some(json!({ "events": [] })),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_json(response).await["error"]["code"], "bad_request");
}
