//! Gateway authentication.
//!
//! When `gateway.api_key` is set, every protected route accepts exactly two
//! kinds of credential:
//!
//! - **The API key**, for scripts, the CLI and A2A callers:
//!   `Authorization: Bearer <key>` (or a `token` / `api_key` query parameter
//!   for clients that cannot set headers, such as some WebSocket libraries).
//! - **A browser session cookie**, for the webchat. The user proves they hold
//!   the key once via `POST /api/auth/login`; the server answers with an
//!   `HttpOnly; SameSite=Strict` cookie holding a random session ID, so the key
//!   itself never lives in the browser.
//!
//! Because browsers attach cookies automatically, cookie-authenticated requests
//! that change state - and every WebSocket upgrade - must also come from the
//! gateway's own origin (the `Origin` header must match `Host` or
//! `X-Forwarded-Host`). This blocks cross-site request forgery and cross-site
//! WebSocket hijacking.
//!
//! When no key is configured, [`require_auth`] lets everything through (a
//! default install binds to 127.0.0.1) and [`require_auth_strict`] rejects
//! everything (used for routes that must never be open, like the Google
//! integration).

use std::net::SocketAddr;

use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use tracing::warn;
use url::form_urlencoded;

use crate::state::{BROWSER_SESSION_TTL, SharedState};

/// Name of the browser session cookie.
pub const SESSION_COOKIE: &str = "opencrust_session";

/// Environment variable (and vault entry) holding the gateway API key.
pub const GATEWAY_KEY_ENV: &str = "OPENCRUST_GATEWAY_API_KEY";

/// Resolve the gateway API key the same way as other credentials: the vault,
/// then `gateway.api_key` from config, then `OPENCRUST_GATEWAY_API_KEY`.
/// Blank values count as unset.
pub fn resolve_gateway_api_key(config_key: Option<&str>) -> Option<String> {
    crate::bootstrap::resolve_api_key(config_key, GATEWAY_KEY_ENV, GATEWAY_KEY_ENV)
        .filter(|key| !key.trim().is_empty())
}

/// What a request authenticated with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Credential {
    ApiKey,
    BrowserSession,
}

/// Protect a route whenever a gateway key is configured.
pub async fn require_auth(
    State(state): State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    match state.config.gateway.api_key.as_deref() {
        Some(key) => guard(&state, key, req, next).await,
        None => next.run(req).await,
    }
}

/// Protect a route, and reject every request when no gateway key is
/// configured at all.
pub async fn require_auth_strict(
    State(state): State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    match state.config.gateway.api_key.as_deref() {
        Some(key) => guard(&state, key, req, next).await,
        None => error(
            StatusCode::FORBIDDEN,
            "Gateway API key is required for integration endpoints. Set OPENCRUST_GATEWAY_API_KEY.",
        ),
    }
}

async fn guard(state: &SharedState, key: &str, req: Request<Body>, next: Next) -> Response {
    match authenticate(state, key, req.headers(), req.uri()) {
        Some(Credential::ApiKey) => next.run(req).await,
        Some(Credential::BrowserSession) => {
            if needs_origin_check(req.method(), req.headers()) && !origin_allowed(req.headers()) {
                warn!(
                    "rejected cross-origin {} {} authenticated by session cookie",
                    req.method(),
                    req.uri().path()
                );
                return error(StatusCode::FORBIDDEN, "Cross-origin request rejected.");
            }
            next.run(req).await
        }
        None => error(
            StatusCode::UNAUTHORIZED,
            "Invalid or missing gateway API key.",
        ),
    }
}

fn authenticate(
    state: &SharedState,
    key: &str,
    headers: &HeaderMap,
    uri: &Uri,
) -> Option<Credential> {
    if presented_key(headers, uri).is_some_and(|token| constant_time_token_eq(&token, key)) {
        return Some(Credential::ApiKey);
    }
    if session_cookie(headers).is_some_and(|id| state.validate_browser_session(id)) {
        return Some(Credential::BrowserSession);
    }
    None
}

/// The key from `Authorization` (optionally `Bearer `-prefixed), or else from a
/// `token` / `api_key` query parameter.
fn presented_key(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    let from_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.strip_prefix("Bearer ").unwrap_or(value).to_string());

    from_header.or_else(|| {
        uri.query().and_then(|query| {
            form_urlencoded::parse(query.as_bytes())
                .find(|(name, _)| name == "token" || name == "api_key")
                .map(|(_, value)| value.into_owned())
        })
    })
}

/// The browser session ID from the `Cookie` header(s), if present.
fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value)
}

/// Safe methods are exempt, except WebSocket upgrades: a socket is a two-way
/// channel to the agent, so it gets the same protection as a POST.
fn needs_origin_check(method: &Method, headers: &HeaderMap) -> bool {
    let safe = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    !safe || is_websocket_upgrade(headers)
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
}

/// Whether the request's `Origin` is this gateway.
///
/// A request with no `Origin` header did not come from a page script (browsers
/// always send it on cross-origin requests, WebSocket upgrades and non-GET
/// fetches), so it is allowed. `X-Forwarded-Host` is accepted as well as `Host`
/// so the webchat works behind a reverse proxy that rewrites `Host`; a hostile
/// page cannot set that header on a credentialed cross-site request.
fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(origin_authority) = origin
        .to_str()
        .ok()
        .and_then(|value| url::Url::parse(value).ok())
        .and_then(|url| {
            let host = url.host_str()?.to_string();
            Some(match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host,
            })
        })
    else {
        // Unparseable or opaque ("null") origins never match.
        return false;
    };

    let host = headers.get(header::HOST);
    let forwarded_host = headers.get("x-forwarded-host");
    [host, forwarded_host]
        .into_iter()
        .flatten()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split(',').next())
        .any(|authority| authority.trim().eq_ignore_ascii_case(&origin_authority))
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    key: String,
}

/// POST /api/auth/login - exchange the gateway key for a browser session cookie.
pub async fn login(
    State(state): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    let Some(key) = state.config.gateway.api_key.as_deref() else {
        return error(
            StatusCode::BAD_REQUEST,
            "Authentication is not enabled on this gateway.",
        );
    };

    if !origin_allowed(&headers) {
        return error(StatusCode::FORBIDDEN, "Cross-origin request rejected.");
    }

    // Throttle guesses per client IP.
    let rate_limit = state.current_config().gateway.rate_limit.clone();
    if let Err(e) = state.check_user_rate_limit(&format!("login:{}", addr.ip()), &rate_limit) {
        return error(StatusCode::TOO_MANY_REQUESTS, &e);
    }

    if !constant_time_token_eq(&body.key, key) {
        warn!("failed gateway login from {}", addr.ip());
        return error(StatusCode::UNAUTHORIZED, "Invalid gateway API key.");
    }

    let Some(session_id) = new_session_id() else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not create a login session.",
        );
    };
    state.issue_browser_session(session_id.clone());

    let cookie = session_cookie_header(
        &session_id,
        BROWSER_SESSION_TTL.as_secs(),
        request_is_https(&headers),
    );
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response()
}

/// POST /api/auth/logout - end the browser session and clear its cookie.
pub async fn logout(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    if !origin_allowed(&headers) {
        return error(StatusCode::FORBIDDEN, "Cross-origin request rejected.");
    }
    if let Some(session_id) = session_cookie(&headers) {
        state.revoke_browser_session(session_id);
    }
    let cookie = session_cookie_header("", 0, request_is_https(&headers));
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response()
}

/// GET /api/auth-check - whether auth is required and whether this request
/// already carries a valid credential. Public, so the webchat can decide
/// whether to show its login form.
pub async fn auth_check(
    State(state): State<SharedState>,
    headers: HeaderMap,
    uri: Uri,
) -> Json<serde_json::Value> {
    let (required, authenticated) = match state.config.gateway.api_key.as_deref() {
        Some(key) => (true, authenticate(&state, key, &headers, &uri).is_some()),
        None => (false, true),
    };
    Json(serde_json::json!({
        "auth_required": required,
        "authenticated": authenticated,
    }))
}

/// 256 bits from the OS CSPRNG, hex-encoded.
fn new_session_id() -> Option<String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn session_cookie_header(value: &str, max_age_secs: u64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_secs}{secure}"
    )
}

/// Whether the browser reached us over HTTPS, directly or through a proxy.
/// Only decides the cookie's `Secure` flag, so a spoofed value can only make
/// the cookie stricter.
fn request_is_https(headers: &HeaderMap) -> bool {
    let forwarded_https = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"));
    let origin_https = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin.starts_with("https://"));
    forwarded_https || origin_https
}

pub(crate) fn constant_time_token_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "status": "error", "message": message })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn origin_matching_host_is_allowed() {
        let h = headers(&[
            ("origin", "http://127.0.0.1:3888"),
            ("host", "127.0.0.1:3888"),
        ]);
        assert!(origin_allowed(&h));
    }

    #[test]
    fn origin_default_port_matches_host_without_port() {
        let h = headers(&[
            ("origin", "https://chat.example.com"),
            ("host", "chat.example.com"),
        ]);
        assert!(origin_allowed(&h));
    }

    #[test]
    fn foreign_origin_is_rejected() {
        let h = headers(&[
            ("origin", "https://evil.example"),
            ("host", "127.0.0.1:3888"),
        ]);
        assert!(!origin_allowed(&h));
    }

    #[test]
    fn null_origin_is_rejected() {
        let h = headers(&[("origin", "null"), ("host", "127.0.0.1:3888")]);
        assert!(!origin_allowed(&h));
    }

    #[test]
    fn missing_origin_is_allowed() {
        let h = headers(&[("host", "127.0.0.1:3888")]);
        assert!(origin_allowed(&h));
    }

    #[test]
    fn origin_matching_forwarded_host_is_allowed() {
        let h = headers(&[
            ("origin", "https://chat.example.com"),
            ("host", "127.0.0.1:3888"),
            ("x-forwarded-host", "chat.example.com"),
        ]);
        assert!(origin_allowed(&h));
    }

    #[test]
    fn websocket_get_needs_origin_check() {
        let ws = headers(&[("upgrade", "websocket")]);
        assert!(needs_origin_check(&Method::GET, &ws));
        assert!(!needs_origin_check(&Method::GET, &HeaderMap::new()));
        assert!(needs_origin_check(&Method::POST, &HeaderMap::new()));
    }

    #[test]
    fn session_cookie_is_found_among_others() {
        let h = headers(&[("cookie", "theme=dark; opencrust_session=abc123; other=1")]);
        assert_eq!(session_cookie(&h), Some("abc123"));
        let none = headers(&[("cookie", "theme=dark")]);
        assert_eq!(session_cookie(&none), None);
    }

    #[test]
    fn presented_key_prefers_header_then_query() {
        let uri: Uri = "/x?token=from-query".parse().unwrap();
        let h = headers(&[("authorization", "Bearer from-header")]);
        assert_eq!(presented_key(&h, &uri).as_deref(), Some("from-header"));
        assert_eq!(
            presented_key(&HeaderMap::new(), &uri).as_deref(),
            Some("from-query")
        );
    }

    #[test]
    fn cookie_header_is_http_only_and_strict() {
        let cookie = session_cookie_header("abc", 60, false);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Max-Age=60"));
        assert!(!cookie.contains("Secure"));
        assert!(session_cookie_header("abc", 60, true).ends_with("; Secure"));
    }

    #[test]
    fn session_ids_are_256_bit_hex_and_unique() {
        let a = new_session_id().unwrap();
        let b = new_session_id().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_token_eq("abc123", "abc123"));
        assert!(!constant_time_token_eq("abc123", "abc124"));
        assert!(!constant_time_token_eq("abc123", "abc1234"));
    }
}
