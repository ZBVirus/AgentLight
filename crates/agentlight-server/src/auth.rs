//! Bearer-token auth for `/api/v1/*`.
//!
//! A request may authenticate with either the static admin token or a paired
//! device token. `/healthz` stays open so a supervisor or load balancer can
//! probe liveness without a secret. When neither an admin token nor a paired
//! device exists, every route is open (the loopback default).
//!
//! The token is accepted either as `Authorization: Bearer <token>` or as a
//! `token` query parameter. The query form exists for `EventSource`, which
//! cannot set request headers from the browser. Only the SHA-256 hash of the
//! admin token is stored; the provided token is hashed and the digests are
//! compared constant-time. Device tokens are handled the same way.

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;

use crate::app::AppState;
use crate::devices::hash_token;
use crate::error::ApiError;

const BEARER_PREFIX: &str = "Bearer ";

/// Reject requests that carry neither the admin token nor a known device token.
pub async fn require_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !state.auth_required() {
        return Ok(next.run(request).await);
    }

    let provided = provided_token(&request);
    let authorized = provided.as_deref().is_some_and(|token| {
        if let Some(expected) = state.admin_token_hash() {
            let hashed = hash_token(token);
            if constant_time_eq(hashed.as_bytes(), expected.as_bytes()) {
                return true;
            }
        }
        match state.devices().authenticate(token) {
            Some(id) => {
                state.devices().touch(&id);
                true
            }
            None => false,
        }
    });

    if !authorized {
        return Err(ApiError::unauthorized("missing or invalid bearer token"));
    }
    Ok(next.run(request).await)
}

/// Reject requests that do not carry the static admin token. Device tokens are
/// deliberately not enough to manage devices. When no admin token is
/// configured, device management is closed with `403`.
pub async fn require_admin(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let Some(expected) = state.admin_token_hash() else {
        return Err(ApiError::forbidden("admin token is not configured"));
    };
    let provided = provided_token(&request);
    match provided {
        Some(token) if constant_time_eq(hash_token(&token).as_bytes(), expected.as_bytes()) => {
            Ok(next.run(request).await)
        }
        _ => Err(ApiError::unauthorized("missing or invalid admin token")),
    }
}

/// Pull the bearer token from the `Authorization` header or the `token` query.
fn provided_token(request: &Request) -> Option<String> {
    let header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix(BEARER_PREFIX))
        .map(str::to_string);
    header.or_else(|| token_from_query(request.uri().query()))
}

/// Pull `token` out of a query string, percent-decoding the value.
fn token_from_query(query: Option<&str>) -> Option<String> {
    let query = query?;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        if parts.next() == Some("token") {
            return parts.next().map(percent_decode);
        }
    }
    None
}

/// Minimal `application/x-www-form-urlencoded` value decoder: `%XX` escapes
/// plus `+` for space. Tokens with unusual characters survive `encodeURIComponent`.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                    out.push(hi << 4 | lo);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Compare without early-exit so token length and content do not leak timing.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_only_equal_slices() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secrez"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn token_from_query_decodes_the_value() {
        assert_eq!(token_from_query(Some("token=abc")), Some("abc".into()));
        assert_eq!(
            token_from_query(Some("a=1&token=abc&b=2")),
            Some("abc".into())
        );
        assert_eq!(
            token_from_query(Some("token=a%2Bb%20c")),
            Some("a+b c".into())
        );
        assert_eq!(token_from_query(Some("token")), None);
        assert_eq!(token_from_query(Some("other=abc")), None);
        assert_eq!(token_from_query(None), None);
    }
}
