use std::sync::Arc;

use futures::future::join_all;
use opencrust_common::Result;
use opencrust_db::{MemoryEntry, MemoryProvider, MemoryRole, NewMemoryEntry, RecallQuery};
use tracing::info;

use crate::embeddings::EmbeddingProvider;
use crate::providers::LlmProvider;

/// Manages agent sessions, tool execution, and LLM provider routing.
pub struct AgentRuntime {
    providers: Vec<Box<dyn LlmProvider>>,
    default_provider: Option<String>,
    memory: Option<Arc<dyn MemoryProvider>>,
    embeddings: Option<Arc<dyn EmbeddingProvider>>,
}

impl AgentRuntime {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            default_provider: None,
            memory: None,
            embeddings: None,
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

    pub fn set_memory_provider(&mut self, memory: Arc<dyn MemoryProvider>) {
        self.memory = Some(memory);
        info!("memory provider attached to agent runtime");
    }

    pub fn has_memory_provider(&self) -> bool {
        self.memory.is_some()
    }

    pub fn set_embedding_provider(&mut self, embeddings: Arc<dyn EmbeddingProvider>) {
        self.embeddings = Some(embeddings);
        info!("embedding provider attached to agent runtime");
    }

    pub fn has_embedding_provider(&self) -> bool {
        self.embeddings.is_some()
    }

    pub async fn on_session_start(
        &self,
        session_id: &str,
        continuity_key: Option<&str>,
    ) -> Result<()> {
        self.remember_system_event(
            session_id,
            continuity_key,
            "session_started",
            "Session started",
        )
        .await
    }

    pub async fn on_session_end(
        &self,
        session_id: &str,
        continuity_key: Option<&str>,
    ) -> Result<()> {
        self.remember_system_event(session_id, continuity_key, "session_ended", "Session ended")
            .await
    }

    pub async fn remember_turn(
        &self,
        session_id: &str,
        continuity_key: Option<&str>,
        user_id: Option<&str>,
        user_input: &str,
        assistant_output: &str,
    ) -> Result<()> {
        let Some(memory) = &self.memory else {
            return Ok(());
        };

        let user_embedding = self.embed_document(user_input).await;
        let assistant_embedding = self.embed_document(assistant_output).await;

        memory
            .remember(NewMemoryEntry {
                session_id: session_id.to_string(),
                channel_id: None,
                user_id: user_id.map(|s| s.to_string()),
                continuity_key: continuity_key.map(|s| s.to_string()),
                role: MemoryRole::User,
                content: user_input.to_string(),
                embedding: user_embedding,
                embedding_model: self.embedding_model(),
                metadata: serde_json::json!({ "kind": "turn_user" }),
            })
            .await?;

        memory
            .remember(NewMemoryEntry {
                session_id: session_id.to_string(),
                channel_id: None,
                user_id: user_id.map(|s| s.to_string()),
                continuity_key: continuity_key.map(|s| s.to_string()),
                role: MemoryRole::Assistant,
                content: assistant_output.to_string(),
                embedding: assistant_embedding,
                embedding_model: self.embedding_model(),
                metadata: serde_json::json!({ "kind": "turn_assistant" }),
            })
            .await?;

        Ok(())
    }

    pub async fn recall_context(
        &self,
        query_text: &str,
        session_id: Option<&str>,
        continuity_key: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        let Some(memory) = &self.memory else {
            return Ok(Vec::new());
        };

        let query_embedding = self.embed_query(query_text).await;

        memory
            .recall(RecallQuery {
                query_text: Some(query_text.to_string()),
                query_embedding,
                session_id: session_id.map(|s| s.to_string()),
                continuity_key: continuity_key.map(|s| s.to_string()),
                limit,
            })
            .await
    }

    pub async fn health_check_all(&self) -> Result<Vec<(String, bool)>> {
        let checks = self.providers.iter().map(|provider| async {
            let provider_id = provider.provider_id().to_string();
            let ok = provider.health_check().await.unwrap_or(false);
            (provider_id, ok)
        });

        Ok(join_all(checks).await)
    }

    async fn remember_system_event(
        &self,
        session_id: &str,
        continuity_key: Option<&str>,
        event: &str,
        content: &str,
    ) -> Result<()> {
        let Some(memory) = &self.memory else {
            return Ok(());
        };

        memory
            .remember(NewMemoryEntry {
                session_id: session_id.to_string(),
                channel_id: None,
                user_id: None,
                continuity_key: continuity_key.map(|s| s.to_string()),
                role: MemoryRole::System,
                content: content.to_string(),
                embedding: None,
                embedding_model: None,
                metadata: serde_json::json!({ "kind": event }),
            })
            .await?;

        Ok(())
    }

    async fn embed_document(&self, text: &str) -> Option<Vec<f32>> {
        let provider = self.embeddings.as_ref()?;
        provider
            .embed_documents(&[text.to_string()])
            .await
            .ok()
            .and_then(|mut v| v.pop())
    }

    async fn embed_query(&self, text: &str) -> Option<Vec<f32>> {
        let provider = self.embeddings.as_ref()?;
        provider.embed_query(text).await.ok()
    }

    fn embedding_model(&self) -> Option<String> {
        self.embeddings
            .as_ref()
            .map(|provider| provider.model().to_string())
    }
}

impl Default for AgentRuntime {
    fn default() -> Self {
        Self::new()
    }
}
