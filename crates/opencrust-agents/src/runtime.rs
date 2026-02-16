use opencrust_common::Result;
use tracing::info;

use crate::providers::LlmProvider;

/// Manages agent sessions, tool execution, and LLM provider routing.
pub struct AgentRuntime {
    providers: Vec<Box<dyn LlmProvider>>,
    default_provider: Option<String>,
}

impl AgentRuntime {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            default_provider: None,
        }
    }

    pub fn register_provider(&mut self, provider: Box<dyn LlmProvider>) {
        let id = provider.provider_id().to_string();
        info!("registered LLM provider: {}", id);
        if self.default_provider.is_none() {
            self.default_provider = Some(id);
        }
        self.providers.push(provider);
    }

    pub fn get_provider(&self, id: &str) -> Option<&dyn LlmProvider> {
        self.providers
            .iter()
            .find(|p| p.provider_id() == id)
            .map(|p| p.as_ref())
    }

    pub fn default_provider(&self) -> Option<&dyn LlmProvider> {
        self.default_provider
            .as_ref()
            .and_then(|id| self.get_provider(id))
    }

    pub async fn health_check_all(&self) -> Result<Vec<(String, bool)>> {
        let futures = self.providers.iter().map(|provider| async move {
            let ok = provider.health_check().await.unwrap_or(false);
            (provider.provider_id().to_string(), ok)
        });
        Ok(futures::future::join_all(futures).await)
    }
}

impl Default for AgentRuntime {
    fn default() -> Self {
        Self::new()
    }
}
