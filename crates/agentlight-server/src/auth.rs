//! Static bearer-token auth for `/api/v1/*`.
//!
//! `/healthz` stays open so a supervisor or load balancer can probe liveness
//! without a secret. When no token is configured, every route is open.

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;

use crate::app::AppState;
use crate::error::ApiError;

const BEARER_PREFIX: &str = "Bearer ";

/// Reject requests that do not carry the configured bearer token.
pub async fn require_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if let Some(expected) = state.token() {
        let provided = request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix(BEARER_PREFIX));
        match provided {
            Some(token) if constant_time_eq(token.as_bytes(), expected.as_bytes()) => {}
            _ => return Err(ApiError::unauthorized("missing or invalid bearer token")),
        }
    }
    Ok(next.run(request).await)
}

/// Compare without early-exit so token length and content do not leak timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
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
}
