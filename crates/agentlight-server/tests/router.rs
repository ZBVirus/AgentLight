//! Router tests. Everything runs in-process via `tower::ServiceExt::oneshot`;
//! no socket is bound.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

use agentlight_core::{
    Capabilities, Config, Engine, FixtureSource, Session, SessionKey, SourceId, Status,
};
use agentlight_server::{router, AppState};

fn session(id: &str, status: Status) -> Session {
    Session::new(
        SessionKey::new(SourceId::new("local"), id),
        format!("Session {id}"),
        status,
    )
}

fn engine_with_sessions() -> (Engine, Arc<FixtureSource>) {
    let engine = Engine::new(Config::default());
    let source = Arc::new(
        FixtureSource::new("local")
            .with_capabilities(Capabilities {
                remove_session: true,
                clear_done: true,
                push_events: false,
            })
            .with_sessions(vec![
                session("a", Status::NeedsHelp),
                session("b", Status::Active),
                session("c", Status::Done),
            ]),
    );
    engine.add_source(source.clone());
    (engine, source)
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

#[tokio::test]
async fn healthz_is_open_and_negotiates() {
    let (engine, _) = engine_with_sessions();
    let app = router(AppState::new(engine, Some("secret".into())));

    let response = app
        .oneshot(request(Method::GET, "/healthz", None, None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["schema_version"], 1);
    assert_eq!(body["capabilities"]["auth"], "bearer");
    assert_eq!(body["capabilities"]["events"][0], "sse");
}

#[tokio::test]
async fn snapshot_exposes_the_core_shape() {
    let (engine, _) = engine_with_sessions();
    let app = router(AppState::new(engine, None));

    let response = app
        .oneshot(request(Method::GET, "/api/v1/snapshot", None, None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    for field in [
        "ok",
        "error",
        "state_path",
        "exists",
        "aggregate",
        "counts",
        "sessions",
        "yellow_mode",
        "generated_at",
    ] {
        assert!(body.get(field).is_some(), "snapshot is missing `{field}`");
    }
    assert_eq!(body["ok"], true);
    assert_eq!(body["aggregate"], "red");
    assert_eq!(body["counts"]["total"], 3);
    assert_eq!(body["sessions"][0]["status"], "needs_help");
    assert_eq!(body["sessions"][0]["session_id"], "a");
}

#[tokio::test]
async fn auth_rejects_missing_and_wrong_tokens() {
    let (engine, _) = engine_with_sessions();
    let app = router(AppState::new(engine, Some("secret".into())));

    let missing = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/snapshot", None, None))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    let body = read_json(missing).await;
    assert_eq!(body["error"]["code"], "unauthorized");

    let wrong = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/snapshot", Some("nope"), None))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let right = app
        .oneshot(request(
            Method::GET,
            "/api/v1/snapshot",
            Some("secret"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(right.status(), StatusCode::OK);
}

#[tokio::test]
async fn remove_session_routes_to_the_source() {
    let (engine, _) = engine_with_sessions();
    let app = router(AppState::new(engine.clone(), None));

    let response = app
        .oneshot(request(
            Method::POST,
            "/api/v1/commands",
            None,
            Some(json!({ "command": "remove_session", "session_id": "a" })),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response).await;
    assert!(body["revision"].as_u64().unwrap() >= 1);

    let snapshot = engine.snapshot_now();
    assert_eq!(snapshot.counts.total, 2);
    assert!(snapshot.sessions.iter().all(|s| s.session_id != "a"));
}

#[tokio::test]
async fn remove_session_uses_an_explicit_source() {
    let (engine, _) = engine_with_sessions();
    let app = router(AppState::new(engine.clone(), None));

    let response = app
        .oneshot(request(
            Method::POST,
            "/api/v1/commands",
            None,
            Some(json!({
                "command": "remove_session",
                "source": "local",
                "session_id": "b"
            })),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let snapshot = engine.snapshot_now();
    assert_eq!(snapshot.counts.total, 2);
    assert!(snapshot.sessions.iter().all(|s| s.session_id != "b"));
}

#[tokio::test]
async fn clear_done_routes_to_the_source() {
    let (engine, _) = engine_with_sessions();
    let app = router(AppState::new(engine.clone(), None));

    let response = app
        .oneshot(request(
            Method::POST,
            "/api/v1/commands",
            None,
            Some(json!({ "command": "clear_done" })),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(engine.snapshot_now().counts.done, 0);
}

#[tokio::test]
async fn unknown_command_is_a_bad_request() {
    let (engine, _) = engine_with_sessions();
    let app = router(AppState::new(engine, None));

    let response = app
        .oneshot(request(
            Method::POST,
            "/api/v1/commands",
            None,
            Some(json!({ "command": "explode" })),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "bad_request");
}

#[tokio::test]
async fn events_opens_an_sse_stream() {
    let (engine, _) = engine_with_sessions();
    let app = router(AppState::new(engine, None));

    let response = app
        .oneshot(request(Method::GET, "/api/v1/events", None, None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.starts_with("text/event-stream"));
}
