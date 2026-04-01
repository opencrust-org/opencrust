/// Minimal LINE webhook test server for local simulation.
///
/// Run with:
///   LINE_CHANNEL_SECRET=xxx LINE_CHANNEL_ACCESS_TOKEN=yyy \
///     cargo run --example line_test_server --features line -p opencrust-channels
use std::sync::Arc;

use axum::{Router, routing::post};
use opencrust_channels::line::{
    LineChannel, LineOnMessageFn,
    webhook::{LineWebhookState, line_webhook},
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let secret =
        std::env::var("LINE_CHANNEL_SECRET").expect("LINE_CHANNEL_SECRET env var is required");
    let access_token = std::env::var("LINE_CHANNEL_ACCESS_TOKEN")
        .expect("LINE_CHANNEL_ACCESS_TOKEN env var is required");

    let on_message: LineOnMessageFn = Arc::new(|user_id, _ctx, text, is_group, _| {
        Box::pin(async move {
            println!("[on_message] from={user_id} group={is_group} text={text:?}");
            Ok(format!("Echo: {text}"))
        })
    });

    let channel = Arc::new(LineChannel::new(access_token, secret.clone(), on_message));
    let state: LineWebhookState = Arc::new(vec![channel]);

    let app = Router::new()
        .route("/line/webhook", post(line_webhook))
        .with_state(state);

    let addr = "127.0.0.1:3099";
    println!("LINE test server running on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
