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

    let (ws, _) = connect_async(&ws_url).await.expect("Should connect with correct token");
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

    let (ws, _) = connect_async(&ws_url).await.expect("Should connect without token if none configured");
    let (_ws, _) = ws.split();
}
