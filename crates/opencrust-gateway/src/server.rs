use std::sync::Arc;

use opencrust_common::Result;
use opencrust_config::AppConfig;
use tokio::net::TcpListener;
use tracing::info;

use crate::router::build_router;
use crate::state::AppState;

/// The main gateway server that binds to a port and serves the API + WebSocket.
pub struct GatewayServer {
    config: AppConfig,
}

impl GatewayServer {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    pub async fn run(self) -> Result<()> {
        let addr = format!("{}:{}", self.config.gateway.host, self.config.gateway.port);

        let state = Arc::new(AppState::new(self.config));
        let app = build_router(state);

        let listener = TcpListener::bind(&addr).await?;
        info!("OpenCrust gateway listening on {}", addr);

        axum::serve(listener, app)
            .await
            .map_err(|e| opencrust_common::Error::Gateway(format!("server error: {e}")))?;

        Ok(())
    }
}
