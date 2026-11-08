use std::net::TcpListener;

use futures::StreamExt;
use opencrust_config::AppConfig;
use opencrust_gateway::GatewayServer;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;

/// Pick a random available port.
fn random_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind to random port");
    listener.local_addr().unwrap().port()
}

/// Start the gateway in the background and return the WebSocket URL base.
async fn start_test_gateway(config: AppConfig) -> String {
    let port = config.gateway.port;
    tokio::spawn(async move {
        let server = GatewayServer::new(config);
        let _ = server.run().await;
    });

    // Wait for the server to be ready
    for _ in 0..50 {
        if TcpListener::bind(format!("127.0.0.1:{port}")).is_err() {
            break; // port is in use = server is up
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    format!("ws://127.0.0.1:{port}/ws")
}

#[tokio::test]
async fn ws_rejects_missing_api_key_if_configured() {
    let port = random_port();
    let mut config = AppConfig::default();
    config.gateway.port = port;
    config.gateway.api_key = Some("secret-token".to_string());
    config.memory.enabled = false;

    let ws_url = start_test_gateway(config).await;

    // Connect without token
    let result = connect_async(&ws_url).await;
    assert!(result.is_err(), "Should fail without token");

    if let Err(tokio_tungstenite::tungstenite::Error::Http(resp)) = result {
        assert_eq!(resp.status(), 401);
    }
}

#[tokio::test]
async fn ws_rejects_wrong_api_key() {
    let port = random_port();
    let mut config = AppConfig::default();
    config.gateway.port = port;
    config.gateway.api_key = Some("secret-token".to_string());
    config.memory.enabled = false;

    let base_url = start_test_gateway(config).await;
    let ws_url = format!("{}?token=wrong-token", base_url);

    let result = connect_async(&ws_url).await;
    assert!(result.is_err(), "Should fail with wrong token");

    if let Err(tokio_tungstenite::tungstenite::Error::Http(resp)) = result {
        assert_eq!(resp.status(), 401);
    }
}

#[tokio::test]
async fn ws_accepts_correct_api_key_query_param() {
    let port = random_port();
    let mut config = AppConfig::default();
    config.gateway.port = port;
    config.gateway.api_key = Some("secret-token".to_string());
    config.memory.enabled = false;

    let base_url = start_test_gateway(config).await;
    let ws_url = format!("{}?token=secret-token", base_url);

    let (ws, _) = connect_async(&ws_url)
        .await
        .expect("Should connect with correct token");
    let (_ws, _) = ws.split();
}

#[tokio::test]
async fn ws_accepts_correct_api_key_header() {
    let port = random_port();
    let mut config = AppConfig::default();
    config.gateway.port = port;
    config.gateway.api_key = Some("secret-token".to_string());
    config.memory.enabled = false;

    let ws_url = start_test_gateway(config).await;

    let request = tokio_tungstenite::tungstenite::handshake::client::Request::builder()
        .uri(&ws_url)
        .header("Authorization", "Bearer secret-token")
        .header("Sec-WebSocket-Key", generate_key())
        .header("Sec-WebSocket-Version", "13")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Host", format!("127.0.0.1:{}", port))
        .body(())
        .unwrap();

    let (ws, _) = connect_async(request)
        .await
        .expect("Should connect with correct header token");
    let (_ws, _) = ws.split();
}

#[tokio::test]
async fn ws_allows_access_if_no_api_key_configured() {
    let port = random_port();
    let mut config = AppConfig::default();
    config.gateway.port = port;
    config.gateway.api_key = None;
    config.memory.enabled = false;

    let ws_url = start_test_gateway(config).await;

    let (ws, _) = connect_async(&ws_url)
        .await
        .expect("Should connect without token if none configured");
    let (_ws, _) = ws.split();
}

/// Start a gateway with an isolated data dir and wait until `/health` answers.
/// Returns the HTTP base URL.
async fn start_http_gateway(api_key: Option<&str>) -> String {
    let port = random_port();
    let mut config = AppConfig::default();
    config.gateway.port = port;
    config.gateway.api_key = api_key.map(str::to_string);
    config.memory.enabled = false;
    config.data_dir =
        Some(std::env::temp_dir().join(format!("opencrust-test-{}", uuid::Uuid::new_v4())));

    tokio::spawn(async move {
        let _ = GatewayServer::new(config).run().await;
    });

    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    for _ in 0..100 {
        if client
            .get(format!("{base}/health"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    base
}

#[tokio::test]
async fn a2a_task_endpoints_require_api_key_if_configured() {
    let base = start_http_gateway(Some("secret-token")).await;
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "message": { "role": "user", "parts": [{ "type": "text", "text": "hi" }] }
    });

    let create = client
        .post(format!("{base}/a2a/tasks"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), 401);

    let get = client
        .get(format!("{base}/a2a/tasks/some-id"))
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), 401);

    let cancel = client
        .post(format!("{base}/a2a/tasks/some-id/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(cancel.status(), 401);

    let wrong = client
        .get(format!("{base}/a2a/tasks/some-id"))
        .bearer_auth("wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);

    // With the key, the request reaches the handler (unknown task -> 404).
    let authed = client
        .get(format!("{base}/a2a/tasks/some-id"))
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(authed.status(), 404);
}

#[tokio::test]
async fn a2a_agent_card_stays_public_with_api_key() {
    let base = start_http_gateway(Some("secret-token")).await;
    let resp = reqwest::get(format!("{base}/.well-known/agent.json"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn a2a_task_endpoints_open_without_api_key() {
    let base = start_http_gateway(None).await;
    let resp = reqwest::get(format!("{base}/a2a/tasks/some-id"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ---------------------------------------------------------------------------
// Gateway-wide auth: API key for clients, session cookie for browsers.
// ---------------------------------------------------------------------------

/// Log in with `key` and return the `opencrust_session=<id>` cookie pair.
async fn log_in(base: &str, key: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/auth/login"))
        .header("Origin", base)
        .json(&serde_json::json!({ "key": key }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204, "login should succeed");
    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .expect("login sets a cookie")
        .to_str()
        .unwrap()
        .to_string();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    set_cookie.split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn webchat_page_hands_out_no_credentials() {
    let base = start_http_gateway(Some("secret-token")).await;
    let resp = reqwest::get(format!("{base}/")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("set-cookie").is_none());
    let body = resp.text().await.unwrap();
    assert!(!body.contains("__OPENCRUST_GATEWAY_KEY__"));
}

#[tokio::test]
async fn protected_routes_reject_anonymous_requests_when_key_configured() {
    let base = start_http_gateway(Some("secret-token")).await;
    let client = reqwest::Client::new();
    let json = serde_json::json!({});

    let cases: &[(&str, &str)] = &[
        ("GET", "/api/status"),
        ("GET", "/api/sessions"),
        ("POST", "/api/sessions"),
        ("POST", "/api/sessions/s1/messages"),
        ("GET", "/api/sessions/s1/history"),
        ("POST", "/api/sessions/s1/upload"),
        ("GET", "/api/providers"),
        ("POST", "/api/providers"),
        ("GET", "/api/usage"),
        ("GET", "/api/mcp"),
        ("POST", "/a2a/tasks"),
        ("GET", "/ws"),
    ];
    for (method, path) in cases {
        let url = format!("{base}{path}");
        let req = match *method {
            "GET" => client.get(&url),
            _ => client.post(&url).json(&json),
        };
        let status = req.send().await.unwrap().status();
        assert_eq!(status, 401, "{method} {path} should require auth");
    }

    for path in ["/health", "/api/auth-check", "/.well-known/agent.json"] {
        let status = client
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 200, "{path} should stay public");
    }
}

#[tokio::test]
async fn bearer_key_grants_access() {
    let base = start_http_gateway(Some("secret-token")).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/status"))
        .bearer_auth("secret-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn browser_login_session_lifecycle() {
    let base = start_http_gateway(Some("secret-token")).await;
    let client = reqwest::Client::new();

    // Wrong key is refused and sets no cookie.
    let bad = client
        .post(format!("{base}/api/auth/login"))
        .header("Origin", &base)
        .json(&serde_json::json!({ "key": "nope" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);
    assert!(bad.headers().get("set-cookie").is_none());

    let cookie = log_in(&base, "secret-token").await;

    let check: serde_json::Value = client
        .get(format!("{base}/api/auth-check"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(check["auth_required"], true);
    assert_eq!(check["authenticated"], true);

    // The cookie works for reads...
    let status = client
        .get(format!("{base}/api/status"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 200);

    // ...and for same-origin writes...
    let same_origin = client
        .post(format!("{base}/api/sessions"))
        .header("Cookie", &cookie)
        .header("Origin", &base)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(same_origin, 201);

    // ...but not for writes from another origin (CSRF).
    let cross_origin = client
        .post(format!("{base}/api/sessions"))
        .header("Cookie", &cookie)
        .header("Origin", "https://evil.example")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(cross_origin, 403);

    // Logout revokes the session server-side.
    let logout = client
        .post(format!("{base}/api/auth/logout"))
        .header("Cookie", &cookie)
        .header("Origin", &base)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), 204);
    let after = client
        .get(format!("{base}/api/status"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(after, 401);
}

fn ws_request_with_cookie(
    base: &str,
    cookie: &str,
    origin: &str,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let host = base.trim_start_matches("http://");
    tokio_tungstenite::tungstenite::handshake::client::Request::builder()
        .uri(format!("ws://{host}/ws"))
        .header("Cookie", cookie)
        .header("Origin", origin)
        .header("Sec-WebSocket-Key", generate_key())
        .header("Sec-WebSocket-Version", "13")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Host", host)
        .body(())
        .unwrap()
}

#[tokio::test]
async fn ws_accepts_session_cookie_from_same_origin_only() {
    let base = start_http_gateway(Some("secret-token")).await;
    let cookie = log_in(&base, "secret-token").await;

    let (ws, _) = connect_async(ws_request_with_cookie(&base, &cookie, &base))
        .await
        .expect("same-origin upgrade with a session cookie should connect");
    drop(ws);

    // Cross-site WebSocket hijacking: a foreign page riding the cookie.
    let result = connect_async(ws_request_with_cookie(
        &base,
        &cookie,
        "https://evil.example",
    ))
    .await;
    match result {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status(), 403)
        }
        other => panic!("expected 403, got {other:?}"),
    }
}

#[tokio::test]
async fn everything_open_when_no_key_configured() {
    let base = start_http_gateway(None).await;
    let client = reqwest::Client::new();

    let status = client
        .get(format!("{base}/api/status"))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, 200);

    let check: serde_json::Value = client
        .get(format!("{base}/api/auth-check"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(check["auth_required"], false);
    assert_eq!(check["authenticated"], true);

    let login = client
        .post(format!("{base}/api/auth/login"))
        .json(&serde_json::json!({ "key": "anything" }))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 400);
}
