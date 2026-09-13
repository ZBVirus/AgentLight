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
use agentlight_server::{hash_token, router, AppState};

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
    let app = router(AppState::new(engine, Some(hash_token("secret"))));

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
    assert_eq!(
        body["capabilities"]["ingest"],
        json!(["upsert", "snapshot"])
    );
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
        "source_kind",
        "source_label",
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
    let app = router(AppState::new(engine, Some(hash_token("secret"))));

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

    // The stored digest is not itself a credential.
    let digest = hash_token("secret");
    let hash_tried = app
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/snapshot",
            Some(&digest),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(hash_tried.status(), StatusCode::UNAUTHORIZED);

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
async fn root_serves_the_built_in_client() {
    let (engine, _) = engine_with_sessions();
    let app = router(AppState::new(engine, Some(hash_token("secret"))));

    let response = app
        .oneshot(request(Method::GET, "/", None, None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.starts_with("text/html"));
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(body.contains("<!doctype html>"));
    assert!(body.contains("EventSource"));
    // The self-service pairing form ships with the client.
    assert!(body.contains("id=\"pair-code\""));
    assert!(body.contains("/api/v1/pair"));
    assert!(body.contains("Forget this device"));
}

#[tokio::test]
async fn query_token_is_accepted() {
    let (engine, _) = engine_with_sessions();
    let app = router(AppState::new(engine, Some(hash_token("secret"))));

    let response = app
        .oneshot(request(
            Method::GET,
            "/api/v1/snapshot?token=secret",
            None,
            None,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn query_token_is_rejected_when_wrong() {
    let (engine, _) = engine_with_sessions();
    let app = router(AppState::new(engine, Some(hash_token("secret"))));

    let response = app
        .oneshot(request(
            Method::GET,
            "/api/v1/snapshot?token=nope",
            None,
            None,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pair_exchanges_a_code_for_a_working_device_token() {
    let (engine, _) = engine_with_sessions();
    let state = AppState::new(engine, Some(hash_token("secret")));
    let code = state.pair_info().code;
    let app = router(state);

    let paired = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/pair",
            None,
            Some(json!({ "code": code, "device_name": "phone" })),
        ))
        .await
        .unwrap();
    assert_eq!(paired.status(), StatusCode::OK);
    let body = read_json(paired).await;
    let device_id = body["device_id"].as_str().unwrap().to_string();
    let token = body["token"].as_str().unwrap().to_string();
    assert!(!token.is_empty());

    let accepted = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/snapshot", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);

    let bogus = app
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/snapshot",
            Some("bogus"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(bogus.status(), StatusCode::UNAUTHORIZED);

    let revoked = app
        .clone()
        .oneshot(request(
            Method::DELETE,
            &format!("/api/v1/devices/{device_id}"),
            Some("secret"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);

    let after_revoke = app
        .oneshot(request(Method::GET, "/api/v1/snapshot", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(after_revoke.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pair_rejects_a_wrong_code() {
    let (engine, _) = engine_with_sessions();
    let app = router(AppState::new(engine, Some(hash_token("secret"))));

    let response = app
        .oneshot(request(
            Method::POST,
            "/api/v1/pair",
            None,
            Some(json!({ "code": "ZZZZZZZZ", "device_name": "phone" })),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = read_json(response).await;
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn regenerate_invalidates_the_previous_code() {
    let (engine, _) = engine_with_sessions();
    let state = AppState::new(engine, Some(hash_token("secret")));
    let old = state.pair_info().code;
    let fresh = state.regenerate_pairing().code;
    assert_ne!(old, fresh);
    let app = router(state);

    let stale = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/pair",
            None,
            Some(json!({ "code": old, "device_name": "phone" })),
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);

    let ok = app
        .oneshot(request(
            Method::POST,
            "/api/v1/pair",
            None,
            Some(json!({ "code": fresh, "device_name": "phone" })),
        ))
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
}

#[tokio::test]
async fn device_listing_requires_admin() {
    let (engine, _) = engine_with_sessions();
    let state = AppState::new(engine, Some(hash_token("secret")));
    let code = state.pair_info().code;
    let app = router(state);

    let paired = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/pair",
            None,
            Some(json!({ "code": code, "device_name": "phone" })),
        ))
        .await
        .unwrap();
    let token = read_json(paired).await["token"]
        .as_str()
        .unwrap()
        .to_string();

    let with_device = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/devices", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(with_device.status(), StatusCode::UNAUTHORIZED);

    let with_admin = app
        .clone()
        .oneshot(request(
            Method::GET,
            "/api/v1/devices",
            Some("secret"),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(with_admin.status(), StatusCode::OK);
    let devices = read_json(with_admin).await;
    assert_eq!(devices.as_array().unwrap().len(), 1);
    assert_eq!(devices[0]["name"], "phone");
    assert!(devices[0].get("token_hash").is_none());

    let no_admin = router(AppState::new(engine_with_sessions().0, None));
    let forbidden = no_admin
        .oneshot(request(Method::GET, "/api/v1/devices", None, None))
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn paired_device_without_admin_needs_a_device_token_and_cannot_manage() {
    let (engine, _) = engine_with_sessions();
    let state = AppState::new(engine, None);
    let code = state.pair_info().code;
    let app = router(state);

    let paired = app
        .clone()
        .oneshot(request(
            Method::POST,
            "/api/v1/pair",
            None,
            Some(json!({ "code": code, "device_name": "phone" })),
        ))
        .await
        .unwrap();
    let token = read_json(paired).await["token"]
        .as_str()
        .unwrap()
        .to_string();

    // With a paired device present, `/api/v1/*` is no longer open.
    let missing = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/snapshot", None, None))
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

    let with_device = app
        .clone()
        .oneshot(request(Method::GET, "/api/v1/snapshot", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(with_device.status(), StatusCode::OK);

    // No admin token means device management is closed with 403, even with a
    // valid device token.
    let manage = app
        .oneshot(request(Method::GET, "/api/v1/devices", Some(&token), None))
        .await
        .unwrap();
    assert_eq!(manage.status(), StatusCode::FORBIDDEN);
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
