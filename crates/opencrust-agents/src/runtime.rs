use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use dashmap::DashMap;

use futures::StreamExt;
use futures::future::join_all;
use opencrust_common::{Error, Result};
use opencrust_db::{
    DocumentStore, MemoryEntry, MemoryProvider, MemoryRole, NewMemoryEntry, RecallQuery,
};
use tokio::sync::mpsc;
use tracing::{info, instrument, warn};

use crate::embeddings::EmbeddingProvider;
use crate::providers::{
    ChatMessage, ChatRole, ContentBlock, LlmProvider, LlmRequest, MessagePart, StreamEvent,
    ToolDefinition,
};
use crate::tools::{Tool, ToolContext, ToolOutput};

/// Maximum number of tool-use round-trips before the loop is forcibly stopped.
const MAX_TOOL_ITERATIONS: usize = 10;

/// Default base system prompt when none is configured.
const DEFAULT_BASE_SYSTEM_PROMPT: &str = "\
You are a personal AI assistant powered by OpenCrust. You help the user by answering \
questions, searching their documents, and executing tasks using your available tools. \
Be concise and accurate. If you don't know something, say so. Do not make up information. \
Always respond in the same language the user writes in.";

/// Manages agent sessions, tool execution, and LLM provider routing.
pub struct AgentRuntime {
    providers: RwLock<Vec<Arc<dyn LlmProvider>>>,
    default_provider: RwLock<Option<String>>,
    memory: Option<Arc<dyn MemoryProvider>>,
    embeddings: Option<Arc<dyn EmbeddingProvider>>,
    tools: Vec<Box<dyn Tool>>,
    system_prompt: Option<String>,
    dna_content: RwLock<Option<String>>,
    skills_content: RwLock<Option<String>>,
    max_tokens: Option<u32>,
    max_context_tokens: Option<usize>,
    recall_limit: usize,
    summarization_enabled: bool,
    /// Accumulated token usage per session, keyed by session_id.
    /// Tuple: (input_tokens, output_tokens, provider_id, model).
    usage_accumulator: Mutex<HashMap<String, (u32, u32, String, String)>>,
    /// Per-session tool configuration: (allowed_tools, call_count, budget).
    /// `allowed_tools = None` means all tools allowed.
    session_tool_config: DashMap<String, SessionToolConfig>,
    /// Per-session user display name, set by the channel layer before processing.
    session_user_name: DashMap<String, String>,
    /// When true, accumulate debug info (tool calls) per session.
    debug: bool,
    /// Debug info accumulated during message processing, keyed by session_id.
    debug_accumulator: Mutex<HashMap<String, Vec<String>>>,
    /// Path to the document store DB for auto-RAG injection.
    doc_db_path: Option<PathBuf>,
    /// Cached document store opened once at startup for auto-RAG.
    doc_store: Option<Arc<DocumentStore>>,
    /// True when at least one document has been ingested into the store.
    /// Guards the embedding call in auto_rag_context — skip when false.
    has_documents: AtomicBool,
}

/// Per-session tool configuration set before processing a message.
#[derive(Debug, Clone, Default)]
struct SessionToolConfig {
    allowed_tools: Option<Vec<String>>,
    call_count: u32,
    budget: Option<u32>,
}

impl AgentRuntime {
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(Vec::new()),
            default_provider: RwLock::new(None),
            memory: None,
            embeddings: None,
            tools: Vec::new(),
            system_prompt: None,
            dna_content: RwLock::new(None),
            skills_content: RwLock::new(None),
            max_tokens: None,
            max_context_tokens: None,
            recall_limit: 10,
            doc_db_path: None,
            doc_store: None,
            has_documents: AtomicBool::new(false),
            summarization_enabled: true,
            usage_accumulator: Mutex::new(HashMap::new()),
            session_tool_config: DashMap::new(),
            session_user_name: DashMap::new(),
            debug: false,
            debug_accumulator: Mutex::new(HashMap::new()),
        }
    }

    pub fn system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    pub fn set_system_prompt(&mut self, prompt: String) {
        self.system_prompt = Some(prompt);
    }

    /// Append text to the existing system prompt (or create one if none exists).
    pub fn append_system_prompt(&mut self, text: &str) {
        match &mut self.system_prompt {
            Some(existing) => {
                existing.push_str("\n\n");
                existing.push_str(text);
            }
            None => {
                self.system_prompt = Some(text.to_string());
            }
        }
    }

    /// Set the DNA content (personality/tone from dna.md). Uses `&self` via RwLock
    /// so it works after Arc wrapping for hot-reload.
    pub fn set_dna_content(&self, content: Option<String>) {
        *self.dna_content.write().unwrap() = content;
    }

    /// Get a clone of the current DNA content.
    pub fn dna_content(&self) -> Option<String> {
        self.dna_content.read().unwrap().clone()
    }

    /// Set the skills block injected into the system prompt. Uses `&self` via RwLock
    /// so it works after Arc wrapping for hot-reload.
    pub fn set_skills_content(&self, content: Option<String>) {
        *self.skills_content.write().unwrap() = content;
    }

    /// Get a clone of the current skills content.
    pub fn skills_content(&self) -> Option<String> {
        self.skills_content.read().unwrap().clone()
    }

    pub fn set_max_tokens(&mut self, max_tokens: u32) {
        self.max_tokens = Some(max_tokens);
    }

    pub fn set_max_context_tokens(&mut self, max_context_tokens: usize) {
        self.max_context_tokens = Some(max_context_tokens);
    }

    pub fn set_recall_limit(&mut self, limit: usize) {
        self.recall_limit = limit;
    }

    pub fn set_summarization_enabled(&mut self, enabled: bool) {
        self.summarization_enabled = enabled;
    }

    /// Accumulate usage for a session turn. Tokens are summed across multiple
    /// tool-loop iterations within a single message.
    fn accumulate_usage(
        &self,
        session_id: &str,
        provider_id: &str,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
    ) {
        let mut acc = self.usage_accumulator.lock().unwrap();
        let entry = acc
            .entry(session_id.to_string())
            .or_insert_with(|| (0, 0, provider_id.to_string(), model.to_string()));
        entry.0 += input_tokens;
        entry.1 += output_tokens;
    }

    /// Drain and return the accumulated usage for a session, if any.
    pub fn take_session_usage(&self, session_id: &str) -> Option<(u32, u32, String, String)> {
        self.usage_accumulator.lock().unwrap().remove(session_id)
    }

    /// Set the tool configuration for a session before processing a message.
    /// `allowed_tools = None` means all tools are permitted.
    /// `budget = None` means no per-session call-count cap.
    pub fn set_session_tool_config(
        &self,
        session_id: &str,
        allowed_tools: Option<Vec<String>>,
        budget: Option<u32>,
    ) {
        self.session_tool_config.insert(
            session_id.to_string(),
            SessionToolConfig {
                allowed_tools,
                call_count: 0,
                budget,
            },
        );
    }

    /// Remove the tool configuration for a session (called during cleanup).
    pub fn clear_session_tool_config(&self, session_id: &str) {
        self.session_tool_config.remove(session_id);
    }

    /// Retain only configs whose session IDs satisfy the predicate.
    /// Used by the gateway cleanup task to evict expired sessions.
    pub fn retain_session_tool_configs<F>(&self, f: F)
    where
        F: Fn(&str) -> bool,
    {
        self.session_tool_config.retain(|id, _| f(id));
    }

    /// Set the display name of the user for a session. The channel layer calls
    /// this before `process_message_*` so the LLM knows who is talking.
    pub fn set_session_user_name(&self, session_id: &str, name: &str) {
        self.session_user_name
            .insert(session_id.to_string(), name.to_string());
    }

    /// Remove the stored user name for a session (used by cleanup).
    pub fn clear_session_user_name(&self, session_id: &str) {
        self.session_user_name.remove(session_id);
    }

    /// Retain only session user names whose session IDs satisfy the predicate.
    pub fn retain_session_user_names<F>(&self, f: F)
    where
        F: Fn(&str) -> bool,
    {
        self.session_user_name.retain(|id, _| f(id));
    }

    /// Get the user display name for a session.
    fn session_user_name(&self, session_id: &str) -> Option<String> {
        self.session_user_name.get(session_id).map(|v| v.clone())
    }

    /// Return the `allowed_tools` list for a session (used to populate `ToolContext`).
    fn session_allowed_tools(&self, session_id: &str) -> Option<Vec<String>> {
        self.session_tool_config
            .get(session_id)
            .and_then(|cfg| cfg.allowed_tools.clone())
    }

    /// Check whether `tool_name` may be executed for this session and increment
    /// the call counter. Returns an error if the tool is blocked or the budget
    /// has been exhausted.
    fn check_tool_allowed(&self, session_id: &str, tool_name: &str) -> Result<()> {
        if let Some(mut cfg) = self.session_tool_config.get_mut(session_id) {
            // Enforce allowlist
            if let Some(ref allowed) = cfg.allowed_tools
                && !allowed.iter().any(|t| t.as_str() == tool_name)
            {
                return Err(Error::Agent(format!(
                    "tool '{}' is not permitted for this session",
                    tool_name
                )));
            }
            // Enforce budget
            if let Some(budget) = cfg.budget
                && cfg.call_count >= budget
            {
                return Err(Error::Agent(format!(
                    "tool call budget of {} exhausted for session",
                    budget
                )));
            }
            cfg.call_count += 1;
        }
        Ok(())
    }

    pub fn register_provider(&self, provider: Arc<dyn LlmProvider>) {
        let id = provider.provider_id().to_string();
        info!("registered LLM provider: {}", id);
        {
            let mut default = self.default_provider.write().unwrap();
            if default.is_none() {
                *default = Some(id);
            }
        }
        self.providers.write().unwrap().push(provider);
    }

    pub fn get_provider(&self, id: &str) -> Option<Arc<dyn LlmProvider>> {
        self.providers
            .read()
            .unwrap()
            .iter()
            .find(|p| p.provider_id() == id)
            .cloned()
    }

    pub fn default_provider(&self) -> Option<Arc<dyn LlmProvider>> {
        let default_id = self.default_provider.read().unwrap().clone();
        default_id.and_then(|id| self.get_provider(&id))
    }

    /// Return the IDs of all registered providers.
    pub fn provider_ids(&self) -> Vec<String> {
        self.providers
            .read()
            .unwrap()
            .iter()
            .map(|p| p.provider_id().to_string())
            .collect()
    }

    /// Set the default provider by ID. Returns `true` if the provider exists.
    pub fn set_default_provider_id(&self, id: &str) -> bool {
        let exists = self
            .providers
            .read()
            .unwrap()
            .iter()
            .any(|p| p.provider_id() == id);
        if exists {
            *self.default_provider.write().unwrap() = Some(id.to_string());
        }
        exists
    }

    /// Return the current default provider ID.
    pub fn default_provider_id(&self) -> Option<String> {
        self.default_provider.read().unwrap().clone()
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

    pub fn embedding_provider(&self) -> Option<Arc<dyn EmbeddingProvider>> {
        self.embeddings.clone()
    }

    pub fn set_debug(&mut self, debug: bool) {
        self.debug = debug;
    }

    pub fn debug(&self) -> bool {
        self.debug
    }

    /// Build the base prompt: operating instructions + tool guidance.
    /// This is the layer that sits above DNA and below dynamic context.
    pub fn base_prompt_with_tools(&self) -> Option<String> {
        let base = self
            .system_prompt
            .as_deref()
            .unwrap_or(DEFAULT_BASE_SYSTEM_PROMPT);

        let mut parts = vec![base.to_string()];

        // Auto-generate tool guidance from registered tools
        let hints: Vec<String> = self
            .tools
            .iter()
            .map(|t| {
                t.system_hint()
                    .map(|h| format!("  - {}: {}", t.name(), h))
                    .unwrap_or_else(|| format!("  - {}: {}", t.name(), t.description()))
            })
            .collect();

        if !hints.is_empty() {
            parts.push(format!("Available tools:\n{}", hints.join("\n")));
        }

        Some(parts.join("\n\n"))
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

    pub fn register_tool(&mut self, tool: Box<dyn Tool>) {
        info!("registered tool: {}", tool.name());
        self.tools.push(tool);
    }

    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    fn find_tool(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    /// Record a tool call for debug output.
    fn record_debug_tool_call(&self, session_id: &str, name: &str, input_snippet: &str) {
        if !self.debug {
            return;
        }
        let entry = if input_snippet.len() > 80 {
            format!("{name}({}...)", &input_snippet[..80])
        } else {
            format!("{name}({input_snippet})")
        };
        info!("[debug] {session_id}: {entry}");
        let mut acc = self
            .debug_accumulator
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        acc.entry(session_id.to_string()).or_default().push(entry);
    }

    /// Take accumulated debug info for a session. Returns None if debug is off or no data.
    pub fn take_debug_info(&self, session_id: &str) -> Option<Vec<String>> {
        if !self.debug {
            return None;
        }
        let mut acc = self
            .debug_accumulator
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        acc.remove(session_id)
    }

    /// Run the full conversation loop: recall context, call LLM, execute tools, return response.
    pub async fn process_message(
        &self,
        session_id: &str,
        user_text: &str,
        conversation_history: &[ChatMessage],
    ) -> Result<String> {
        self.process_message_with_context(session_id, user_text, conversation_history, None, None)
            .await
    }

    /// Same as `process_message` but includes continuity/user context for shared memory.
    pub async fn process_message_with_context(
        &self,
        session_id: &str,
        user_text: &str,
        conversation_history: &[ChatMessage],
        continuity_key: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<String> {
        self.process_message_impl(
            session_id,
            MessagePart::Text(user_text.to_string()),
            user_text,
            conversation_history,
            continuity_key,
            user_id,
            0,
        )
        .await
    }

    /// Process a message with content blocks (e.g. text + images).
    ///
    /// `user_text_for_memory` is used for memory recall/storage since you can't
    /// search against binary image data.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_message_with_blocks(
        &self,
        session_id: &str,
        blocks: Vec<ContentBlock>,
        user_text_for_memory: &str,
        conversation_history: &[ChatMessage],
        continuity_key: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<String> {
        self.process_message_impl(
            session_id,
            MessagePart::Parts(blocks),
            user_text_for_memory,
            conversation_history,
            continuity_key,
            user_id,
            0,
        )
        .await
    }

    /// Process a scheduled heartbeat message. Tools receive the heartbeat depth
    /// so that recursive scheduling is allowed up to a chain limit.
    pub async fn process_heartbeat(
        &self,
        session_id: &str,
        user_text: &str,
        conversation_history: &[ChatMessage],
        continuity_key: Option<&str>,
        user_id: Option<&str>,
        heartbeat_depth: u8,
    ) -> Result<String> {
        self.process_message_impl(
            session_id,
            MessagePart::Text(user_text.to_string()),
            user_text,
            conversation_history,
            continuity_key,
            user_id,
            heartbeat_depth,
        )
        .await
    }

    /// Process a message with context and session summary support.
    ///
    /// Returns `(response_text, updated_summary)` where `updated_summary` is `Some`
    /// if the context window triggered summarization.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_message_with_context_and_summary(
        &self,
        session_id: &str,
        user_text: &str,
        conversation_history: &[ChatMessage],
        session_summary: Option<&str>,
        continuity_key: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<(String, Option<String>)> {
        self.process_message_summarized_impl(
            session_id,
            MessagePart::Text(user_text.to_string()),
            user_text,
            conversation_history,
            session_summary,
            continuity_key,
            user_id,
            0,
        )
        .await
    }

    /// Streaming variant with session summary support.
    ///
    /// Returns `(response_text, updated_summary)`.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_message_streaming_with_context_and_summary(
        &self,
        session_id: &str,
        user_text: &str,
        conversation_history: &[ChatMessage],
        delta_tx: mpsc::Sender<String>,
        session_summary: Option<&str>,
        continuity_key: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<(String, Option<String>)> {
        self.process_message_streaming_summarized_impl(
            session_id,
            MessagePart::Text(user_text.to_string()),
            user_text,
            conversation_history,
            delta_tx,
            session_summary,
            continuity_key,
            user_id,
        )
        .await
    }

    /// Process content blocks (e.g. images) with session summary support.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_message_with_blocks_and_summary(
        &self,
        session_id: &str,
        blocks: Vec<ContentBlock>,
        user_text_for_memory: &str,
        conversation_history: &[ChatMessage],
        session_summary: Option<&str>,
        continuity_key: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<(String, Option<String>)> {
        self.process_message_summarized_impl(
            session_id,
            MessagePart::Parts(blocks),
            user_text_for_memory,
            conversation_history,
            session_summary,
            continuity_key,
            user_id,
            0,
        )
        .await
    }

    /// Streaming variant for content blocks with session summary support.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_message_streaming_with_blocks_and_summary(
        &self,
        session_id: &str,
        blocks: Vec<ContentBlock>,
        user_text_for_memory: &str,
        conversation_history: &[ChatMessage],
        delta_tx: mpsc::Sender<String>,
        session_summary: Option<&str>,
        continuity_key: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<(String, Option<String>)> {
        self.process_message_streaming_summarized_impl(
            session_id,
            MessagePart::Parts(blocks),
            user_text_for_memory,
            conversation_history,
            delta_tx,
            session_summary,
            continuity_key,
            user_id,
        )
        .await
    }

    /// Process a message with explicit agent config overrides (for multi-agent routing).
    #[allow(clippy::too_many_arguments)]
    pub async fn process_message_with_agent_config(
        &self,
        session_id: &str,
        user_text: &str,
        conversation_history: &[ChatMessage],
        continuity_key: Option<&str>,
        user_id: Option<&str>,
        provider_id: Option<&str>,
        model_override: Option<&str>,
        system_prompt_override: Option<&str>,
        max_tokens_override: Option<u32>,
    ) -> Result<String> {
        let provider: Arc<dyn LlmProvider> = if let Some(pid) = provider_id {
            self.get_provider(pid)
                .ok_or_else(|| Error::Agent(format!("provider '{pid}' not found")))?
        } else {
            self.default_provider()
                .ok_or_else(|| Error::Agent("no LLM provider configured".into()))?
        };

        let _system_prompt_override = system_prompt_override;
        let effective_model = model_override
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(|m| m.to_string())
            .unwrap_or_default();
        let effective_max_tokens = max_tokens_override.or(self.max_tokens).unwrap_or(4096);

        let memory_context = match self
            .recall_context(
                user_text,
                Some(session_id),
                continuity_key,
                self.recall_limit,
            )
            .await
        {
            Ok(entries) if !entries.is_empty() => {
                let context: Vec<String> = entries.iter().map(|e| e.content.clone()).collect();
                Some(format!(
                    "Relevant context from memory:\n- {}",
                    context.join("\n- ")
                ))
            }
            Err(e) => {
                warn!("memory recall failed, continuing without context: {}", e);
                None
            }
            _ => None,
        };

        let dna = self.dna_content();
        let skills = self.skills_content();
        let base_prompt = self.base_prompt_with_tools();
        let rag_context = self.auto_rag_context(user_text).await;
        let user_display = self.session_user_name(session_id);
        let system = build_system_prompt(
            base_prompt.as_deref(),
            skills.as_deref(),
            dna.as_deref(),
            rag_context.as_deref(),
            memory_context.as_deref(),
            None,
            user_display.as_deref(),
        );

        let tool_defs = self.tool_definitions();

        let mut messages: Vec<ChatMessage> = conversation_history.to_vec();
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: MessagePart::Text(user_text.to_string()),
        });

        let max_ctx = self.max_context_tokens.unwrap_or(100_000);
        trim_messages_to_budget(&mut messages, &system, &tool_defs, max_ctx);

        for _iteration in 0..MAX_TOOL_ITERATIONS {
            let request = LlmRequest {
                model: effective_model.clone(),
                messages: messages.clone(),
                system: system.clone(),
                max_tokens: Some(effective_max_tokens),
                temperature: None,
                tools: tool_defs.clone(),
            };

            let response = provider.complete(&request).await?;

            if let Some(usage) = &response.usage {
                self.accumulate_usage(
                    session_id,
                    provider.provider_id(),
                    &response.model,
                    usage.input_tokens,
                    usage.output_tokens,
                );
            }

            let has_tool_use = response
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }));

            if !has_tool_use {
                let final_text = extract_text(&response.content);
                if let Err(e) = self
                    .remember_turn(session_id, continuity_key, user_id, user_text, &final_text)
                    .await
                {
                    warn!("failed to store turn in memory: {}", e);
                }
                return Ok(final_text);
            }

            messages.push(ChatMessage {
                role: ChatRole::Assistant,
                content: MessagePart::Parts(response.content.clone()),
            });

            let mut tool_results = Vec::new();
            for block in &response.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    let context = crate::tools::ToolContext {
                        session_id: session_id.to_string(),
                        user_id: user_id.map(|s| s.to_string()),
                        heartbeat_depth: 0,
                        allowed_tools: self.session_allowed_tools(session_id),
                    };
                    let output = match self.check_tool_allowed(session_id, name) {
                        Err(e) => ToolOutput::error(e.to_string()),
                        Ok(()) => match self.find_tool(name) {
                            Some(tool) => tool
                                .execute(&context, input.clone())
                                .await
                                .unwrap_or_else(|e| ToolOutput::error(e.to_string())),
                            None => ToolOutput::error(format!("unknown tool: {}", name)),
                        },
                    };
                    tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: output.content,
                    });
                }
            }

            messages.push(ChatMessage {
                role: ChatRole::User,
                content: MessagePart::Parts(tool_results),
            });
        }

        Err(Error::Agent(format!(
            "tool loop exceeded maximum of {} iterations",
            MAX_TOOL_ITERATIONS
        )))
    }

    /// Same as `process_message_with_agent_config` but with session summary support.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_message_with_agent_config_and_summary(
        &self,
        session_id: &str,
        user_text: &str,
        conversation_history: &[ChatMessage],
        continuity_key: Option<&str>,
        user_id: Option<&str>,
        provider_id: Option<&str>,
        model_override: Option<&str>,
        system_prompt_override: Option<&str>,
        max_tokens_override: Option<u32>,
        session_summary: Option<&str>,
    ) -> Result<(String, Option<String>)> {
        let provider: Arc<dyn LlmProvider> = if let Some(pid) = provider_id {
            self.get_provider(pid)
                .ok_or_else(|| Error::Agent(format!("provider '{pid}' not found")))?
        } else {
            self.default_provider()
                .ok_or_else(|| Error::Agent("no LLM provider configured".into()))?
        };

        let _system_prompt_override = system_prompt_override;
        let effective_model = model_override
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(|m| m.to_string())
            .unwrap_or_default();
        let effective_max_tokens = max_tokens_override.or(self.max_tokens).unwrap_or(4096);

        let memory_context = match self
            .recall_context(
                user_text,
                Some(session_id),
                continuity_key,
                self.recall_limit,
            )
            .await
        {
            Ok(entries) if !entries.is_empty() => {
                let context: Vec<String> = entries.iter().map(|e| e.content.clone()).collect();
                Some(format!(
                    "Relevant context from memory:\n- {}",
                    context.join("\n- ")
                ))
            }
            Err(e) => {
                warn!("memory recall failed, continuing without context: {}", e);
                None
            }
            _ => None,
        };

        let dna = self.dna_content();
        let skills = self.skills_content();
        let base_prompt = self.base_prompt_with_tools();
        let rag_context = self.auto_rag_context(user_text).await;
        let user_display = self.session_user_name(session_id);
        let system = build_system_prompt(
            base_prompt.as_deref(),
            skills.as_deref(),
            dna.as_deref(),
            rag_context.as_deref(),
            memory_context.as_deref(),
            session_summary,
            user_display.as_deref(),
        );

        let tool_defs = self.tool_definitions();

        let mut messages: Vec<ChatMessage> = conversation_history.to_vec();
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: inject_rag_into_content(
                MessagePart::Text(user_text.to_string()),
                rag_context.as_deref(),
            ),
        });

        let max_ctx = self.max_context_tokens.unwrap_or(100_000);
        let new_summary = compact_messages(
            &mut messages,
            &system,
            &tool_defs,
            max_ctx,
            provider.as_ref(),
            session_summary,
            self.summarization_enabled,
        )
        .await;

        let system = if new_summary.is_some() {
            build_system_prompt(
                base_prompt.as_deref(),
                skills.as_deref(),
                dna.as_deref(),
                rag_context.as_deref(),
                memory_context.as_deref(),
                new_summary.as_deref(),
                user_display.as_deref(),
            )
        } else {
            system
        };

        for _iteration in 0..MAX_TOOL_ITERATIONS {
            let request = LlmRequest {
                model: effective_model.clone(),
                messages: messages.clone(),
                system: system.clone(),
                max_tokens: Some(effective_max_tokens),
                temperature: None,
                tools: tool_defs.clone(),
            };

            let response = provider.complete(&request).await?;

            if let Some(usage) = &response.usage {
                self.accumulate_usage(
                    session_id,
                    provider.provider_id(),
                    &response.model,
                    usage.input_tokens,
                    usage.output_tokens,
                );
            }

            let has_tool_use = response
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }));

            if !has_tool_use {
                let final_text = extract_text(&response.content);
                if let Err(e) = self
                    .remember_turn(session_id, continuity_key, user_id, user_text, &final_text)
                    .await
                {
                    warn!("failed to store turn in memory: {}", e);
                }
                return Ok((final_text, new_summary));
            }

            messages.push(ChatMessage {
                role: ChatRole::Assistant,
                content: MessagePart::Parts(response.content.clone()),
            });

            let mut tool_results = Vec::new();
            for block in &response.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    let context = crate::tools::ToolContext {
                        session_id: session_id.to_string(),
                        user_id: user_id.map(|s| s.to_string()),
                        heartbeat_depth: 0,
                        allowed_tools: self.session_allowed_tools(session_id),
                    };
                    let output = match self.check_tool_allowed(session_id, name) {
                        Err(e) => ToolOutput::error(e.to_string()),
                        Ok(()) => match self.find_tool(name) {
                            Some(tool) => tool
                                .execute(&context, input.clone())
                                .await
                                .unwrap_or_else(|e| ToolOutput::error(e.to_string())),
                            None => ToolOutput::error(format!("unknown tool: {}", name)),
                        },
                    };
                    tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: output.content,
                    });
                }
            }

            messages.push(ChatMessage {
                role: ChatRole::User,
                content: MessagePart::Parts(tool_results),
            });
        }

        Err(Error::Agent(format!(
            "tool loop exceeded maximum of {} iterations",
            MAX_TOOL_ITERATIONS
        )))
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(self, user_content, conversation_history), fields(provider_id, continuity_key = ?continuity_key))]
    async fn process_message_impl(
        &self,
        session_id: &str,
        user_content: MessagePart,
        memory_text: &str,
        conversation_history: &[ChatMessage],
        continuity_key: Option<&str>,
        user_id: Option<&str>,
        heartbeat_depth: u8,
    ) -> Result<String> {
        let provider: Arc<dyn LlmProvider> = self
            .default_provider()
            .ok_or_else(|| Error::Agent("no LLM provider configured".into()))?;

        // Build system message: system_prompt + memory context
        let memory_context = match self
            .recall_context(
                memory_text,
                Some(session_id),
                continuity_key,
                self.recall_limit,
            )
            .await
        {
            Ok(entries) if !entries.is_empty() => {
                let context: Vec<String> = entries.iter().map(|e| e.content.clone()).collect();
                Some(format!(
                    "Relevant context from memory:\n- {}",
                    context.join("\n- ")
                ))
            }
            Err(e) => {
                warn!("memory recall failed, continuing without context: {}", e);
                None
            }
            _ => None,
        };

        let dna = self.dna_content();
        let skills = self.skills_content();
        let base_prompt = self.base_prompt_with_tools();
        let rag_context = self.auto_rag_context(memory_text).await;
        let user_display = self.session_user_name(session_id);
        let system = build_system_prompt(
            base_prompt.as_deref(),
            skills.as_deref(),
            dna.as_deref(),
            rag_context.as_deref(),
            memory_context.as_deref(),
            None,
            user_display.as_deref(),
        );

        let tool_defs = self.tool_definitions();

        let mut messages: Vec<ChatMessage> = conversation_history.to_vec();
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: inject_rag_into_content(user_content, rag_context.as_deref()),
        });

        // Trim conversation history to fit context window
        let max_ctx = self.max_context_tokens.unwrap_or(100_000);
        trim_messages_to_budget(&mut messages, &system, &tool_defs, max_ctx);

        for _iteration in 0..MAX_TOOL_ITERATIONS {
            let request = LlmRequest {
                model: String::new(),
                messages: messages.clone(),
                system: system.clone(),
                max_tokens: Some(self.max_tokens.unwrap_or(4096)),
                temperature: None,
                tools: tool_defs.clone(),
            };

            let response = provider.complete(&request).await?;

            let has_tool_use = response
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }));

            if !has_tool_use {
                let final_text = extract_text(&response.content);

                // Store turn in memory (best-effort)
                if let Err(e) = self
                    .remember_turn(
                        session_id,
                        continuity_key,
                        user_id,
                        memory_text,
                        &final_text,
                    )
                    .await
                {
                    warn!("failed to store turn in memory: {}", e);
                }

                return Ok(final_text);
            }

            // Append the assistant's response (including tool_use blocks) to history
            messages.push(ChatMessage {
                role: ChatRole::Assistant,
                content: MessagePart::Parts(response.content.clone()),
            });

            // Execute each tool and collect results
            let mut tool_results = Vec::new();
            for block in &response.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    let context = ToolContext {
                        session_id: session_id.to_string(),
                        user_id: user_id.map(|s| s.to_string()),
                        heartbeat_depth,
                        allowed_tools: self.session_allowed_tools(session_id),
                    };
                    let output = match self.check_tool_allowed(session_id, name) {
                        Err(e) => ToolOutput::error(e.to_string()),
                        Ok(()) => match self.find_tool(name) {
                            Some(tool) => tool
                                .execute(&context, input.clone())
                                .await
                                .unwrap_or_else(|e| ToolOutput::error(e.to_string())),
                            None => ToolOutput::error(format!("unknown tool: {}", name)),
                        },
                    };
                    self.record_debug_tool_call(session_id, name, &input.to_string());
                    tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: output.content,
                    });
                }
            }

            // Append tool results as a user message
            messages.push(ChatMessage {
                role: ChatRole::User,
                content: MessagePart::Parts(tool_results),
            });
        }

        Err(Error::Agent(format!(
            "tool loop exceeded maximum of {} iterations",
            MAX_TOOL_ITERATIONS
        )))
    }

    /// Run the conversation loop with streaming. Text deltas are sent through
    /// `delta_tx` as they arrive. Returns the final accumulated response text.
    pub async fn process_message_streaming(
        &self,
        session_id: &str,
        user_text: &str,
        conversation_history: &[ChatMessage],
        delta_tx: mpsc::Sender<String>,
    ) -> Result<String> {
        self.process_message_streaming_with_context(
            session_id,
            user_text,
            conversation_history,
            delta_tx,
            None,
            None,
        )
        .await
    }

    /// Streaming variant with continuity/user context for shared memory.
    pub async fn process_message_streaming_with_context(
        &self,
        session_id: &str,
        user_text: &str,
        conversation_history: &[ChatMessage],
        delta_tx: mpsc::Sender<String>,
        continuity_key: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<String> {
        self.process_message_streaming_impl(
            session_id,
            MessagePart::Text(user_text.to_string()),
            user_text,
            conversation_history,
            delta_tx,
            continuity_key,
            user_id,
        )
        .await
    }

    /// Streaming variant that accepts content blocks (e.g. text + images).
    #[allow(clippy::too_many_arguments)]
    pub async fn process_message_streaming_with_blocks(
        &self,
        session_id: &str,
        blocks: Vec<ContentBlock>,
        user_text_for_memory: &str,
        conversation_history: &[ChatMessage],
        delta_tx: mpsc::Sender<String>,
        continuity_key: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<String> {
        self.process_message_streaming_impl(
            session_id,
            MessagePart::Parts(blocks),
            user_text_for_memory,
            conversation_history,
            delta_tx,
            continuity_key,
            user_id,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(self, user_content, conversation_history, delta_tx), fields(provider_id, continuity_key = ?continuity_key))]
    async fn process_message_streaming_impl(
        &self,
        session_id: &str,
        user_content: MessagePart,
        memory_text: &str,
        conversation_history: &[ChatMessage],
        delta_tx: mpsc::Sender<String>,
        continuity_key: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<String> {
        let provider: Arc<dyn LlmProvider> = self
            .default_provider()
            .ok_or_else(|| Error::Agent("no LLM provider configured".into()))?;

        // Build system message (same as process_message)
        let memory_context = match self
            .recall_context(
                memory_text,
                Some(session_id),
                continuity_key,
                self.recall_limit,
            )
            .await
        {
            Ok(entries) if !entries.is_empty() => {
                let context: Vec<String> = entries.iter().map(|e| e.content.clone()).collect();
                Some(format!(
                    "Relevant context from memory:\n- {}",
                    context.join("\n- ")
                ))
            }
            Err(e) => {
                warn!("memory recall failed, continuing without context: {}", e);
                None
            }
            _ => None,
        };

        let dna = self.dna_content();
        let skills = self.skills_content();
        let base_prompt = self.base_prompt_with_tools();
        let rag_context = self.auto_rag_context(memory_text).await;
        let user_display = self.session_user_name(session_id);
        let system = build_system_prompt(
            base_prompt.as_deref(),
            skills.as_deref(),
            dna.as_deref(),
            rag_context.as_deref(),
            memory_context.as_deref(),
            None,
            user_display.as_deref(),
        );

        let tool_defs = self.tool_definitions();

        let mut messages: Vec<ChatMessage> = conversation_history.to_vec();
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: inject_rag_into_content(user_content, rag_context.as_deref()),
        });

        let max_ctx = self.max_context_tokens.unwrap_or(100_000);
        trim_messages_to_budget(&mut messages, &system, &tool_defs, max_ctx);

        let mut full_response = String::new();
        for _iteration in 0..MAX_TOOL_ITERATIONS {
            let request = LlmRequest {
                model: String::new(),
                messages: messages.clone(),
                system: system.clone(),
                max_tokens: Some(self.max_tokens.unwrap_or(4096)),
                temperature: None,
                tools: tool_defs.clone(),
            };

            // Try streaming; fall back to non-streaming if not supported
            let stream_result = provider.stream_complete(&request).await;

            match stream_result {
                Ok(mut stream) => {
                    // Consume stream, collecting the full response and forwarding text deltas
                    let mut response_text = String::new();
                    let mut tool_uses: Vec<(String, String, String)> = Vec::new(); // (id, name, input_json)
                    let mut current_tool: Option<(String, String, String)> = None;
                    let mut _stop_reason: Option<String> = None;

                    while let Some(event) = stream.next().await {
                        match event? {
                            StreamEvent::TextDelta(text) => {
                                response_text.push_str(&text);
                                let _ = delta_tx.send(text).await;
                            }
                            StreamEvent::ToolUseStart { id, name, .. } => {
                                current_tool = Some((id, name, String::new()));
                            }
                            StreamEvent::InputJsonDelta(json) => {
                                if let Some((_, _, ref mut input)) = current_tool {
                                    input.push_str(&json);
                                }
                            }
                            StreamEvent::ContentBlockStop { .. } => {
                                if let Some(tool) = current_tool.take() {
                                    tool_uses.push(tool);
                                }
                            }
                            StreamEvent::MessageDelta {
                                stop_reason: sr,
                                usage,
                            } => {
                                // OpenAI/vLLM never sends ContentBlockStop, so flush the
                                // in-flight tool when the stream signals tool_calls done.
                                if let Some(tool) = current_tool.take() {
                                    tool_uses.push(tool);
                                }
                                _stop_reason = sr;
                                if let Some(u) = usage {
                                    self.accumulate_usage(
                                        session_id,
                                        provider.provider_id(),
                                        provider.configured_model().unwrap_or(""),
                                        u.input_tokens,
                                        u.output_tokens,
                                    );
                                }
                            }
                            StreamEvent::MessageStop => break,
                        }
                    }

                    // Safety flush: if ContentBlockStop was never emitted (OpenAI/vLLM
                    // streaming), the last tool may still be pending.
                    if let Some(tool) = current_tool.take() {
                        tool_uses.push(tool);
                    }

                    if tool_uses.is_empty() {
                        full_response.push_str(&response_text);

                        if let Err(e) = self
                            .remember_turn(
                                session_id,
                                continuity_key,
                                user_id,
                                memory_text,
                                &full_response,
                            )
                            .await
                        {
                            warn!("failed to store turn in memory: {}", e);
                        }

                        return Ok(full_response);
                    }

                    // Build assistant response with text + tool_use blocks
                    let mut content_blocks = Vec::new();
                    if !response_text.is_empty() {
                        content_blocks.push(ContentBlock::Text {
                            text: response_text.clone(),
                        });
                        full_response.push_str(&response_text);
                    }

                    for (id, name, input_json) in &tool_uses {
                        let input: serde_json::Value =
                            serde_json::from_str(input_json).unwrap_or_default();
                        content_blocks.push(ContentBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input,
                        });
                    }

                    messages.push(ChatMessage {
                        role: ChatRole::Assistant,
                        content: MessagePart::Parts(content_blocks),
                    });

                    // Execute tools
                    let mut tool_results = Vec::new();
                    for (id, name, input_json) in &tool_uses {
                        let input: serde_json::Value =
                            serde_json::from_str(input_json).unwrap_or_default();
                        let context = ToolContext {
                            session_id: session_id.to_string(),
                            user_id: user_id.map(|s| s.to_string()),
                            heartbeat_depth: 0,
                            allowed_tools: self.session_allowed_tools(session_id),
                        };
                        let output = match self.check_tool_allowed(session_id, name) {
                            Err(e) => ToolOutput::error(e.to_string()),
                            Ok(()) => match self.find_tool(name) {
                                Some(tool) => tool
                                    .execute(&context, input)
                                    .await
                                    .unwrap_or_else(|e| ToolOutput::error(e.to_string())),
                                None => ToolOutput::error(format!("unknown tool: {}", name)),
                            },
                        };
                        self.record_debug_tool_call(session_id, name, input_json);
                        tool_results.push(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: output.content,
                        });
                    }

                    messages.push(ChatMessage {
                        role: ChatRole::User,
                        content: MessagePart::Parts(tool_results),
                    });

                    // Add separator between iterations
                    if !full_response.is_empty() {
                        full_response.push_str("\n\n");
                        let _ = delta_tx.send("\n\n".to_string()).await;
                    }
                }
                Err(_) => {
                    // Streaming not supported — fall back to non-streaming
                    let response = provider.complete(&request).await?;

                    if let Some(usage) = &response.usage {
                        self.accumulate_usage(
                            session_id,
                            provider.provider_id(),
                            &response.model,
                            usage.input_tokens,
                            usage.output_tokens,
                        );
                    }

                    let has_tool_use = response
                        .content
                        .iter()
                        .any(|block| matches!(block, ContentBlock::ToolUse { .. }));

                    if !has_tool_use {
                        let final_text = extract_text(&response.content);
                        let _ = delta_tx.send(final_text.clone()).await;
                        full_response.push_str(&final_text);

                        if let Err(e) = self
                            .remember_turn(
                                session_id,
                                continuity_key,
                                user_id,
                                memory_text,
                                &full_response,
                            )
                            .await
                        {
                            warn!("failed to store turn in memory: {}", e);
                        }

                        return Ok(full_response);
                    }

                    messages.push(ChatMessage {
                        role: ChatRole::Assistant,
                        content: MessagePart::Parts(response.content.clone()),
                    });

                    let mut tool_results = Vec::new();
                    for block in &response.content {
                        if let ContentBlock::ToolUse { id, name, input } = block {
                            let context = ToolContext {
                                session_id: session_id.to_string(),
                                user_id: user_id.map(|s| s.to_string()),
                                heartbeat_depth: 0,
                                allowed_tools: self.session_allowed_tools(session_id),
                            };
                            let output = match self.check_tool_allowed(session_id, name) {
                                Err(e) => ToolOutput::error(e.to_string()),
                                Ok(()) => match self.find_tool(name) {
                                    Some(tool) => tool
                                        .execute(&context, input.clone())
                                        .await
                                        .unwrap_or_else(|e| ToolOutput::error(e.to_string())),
                                    None => ToolOutput::error(format!("unknown tool: {}", name)),
                                },
                            };
                            tool_results.push(ContentBlock::ToolResult {
                                tool_use_id: id.clone(),
                                content: output.content,
                            });
                        }
                    }

                    messages.push(ChatMessage {
                        role: ChatRole::User,
                        content: MessagePart::Parts(tool_results),
                    });
                }
            }
        }

        Err(Error::Agent(format!(
            "tool loop exceeded maximum of {} iterations",
            MAX_TOOL_ITERATIONS
        )))
    }

    /// Non-streaming impl with summary support.
    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(self, user_content, conversation_history, session_summary), fields(provider_id, continuity_key = ?continuity_key))]
    async fn process_message_summarized_impl(
        &self,
        session_id: &str,
        user_content: MessagePart,
        memory_text: &str,
        conversation_history: &[ChatMessage],
        session_summary: Option<&str>,
        continuity_key: Option<&str>,
        user_id: Option<&str>,
        heartbeat_depth: u8,
    ) -> Result<(String, Option<String>)> {
        let provider: Arc<dyn LlmProvider> = self
            .default_provider()
            .ok_or_else(|| Error::Agent("no LLM provider configured".into()))?;

        let memory_context = match self
            .recall_context(
                memory_text,
                Some(session_id),
                continuity_key,
                self.recall_limit,
            )
            .await
        {
            Ok(entries) if !entries.is_empty() => {
                let context: Vec<String> = entries.iter().map(|e| e.content.clone()).collect();
                Some(format!(
                    "Relevant context from memory:\n- {}",
                    context.join("\n- ")
                ))
            }
            Err(e) => {
                warn!("memory recall failed, continuing without context: {}", e);
                None
            }
            _ => None,
        };

        let dna = self.dna_content();
        let skills = self.skills_content();
        let base_prompt = self.base_prompt_with_tools();
        let rag_context = self.auto_rag_context(memory_text).await;
        let user_display = self.session_user_name(session_id);
        let system = build_system_prompt(
            base_prompt.as_deref(),
            skills.as_deref(),
            dna.as_deref(),
            rag_context.as_deref(),
            memory_context.as_deref(),
            session_summary,
            user_display.as_deref(),
        );

        let tool_defs = self.tool_definitions();

        let mut messages: Vec<ChatMessage> = conversation_history.to_vec();
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: inject_rag_into_content(user_content, rag_context.as_deref()),
        });

        let max_ctx = self.max_context_tokens.unwrap_or(100_000);
        let new_summary = compact_messages(
            &mut messages,
            &system,
            &tool_defs,
            max_ctx,
            provider.as_ref(),
            session_summary,
            self.summarization_enabled,
        )
        .await;

        // If we got a new summary, rebuild system prompt with it
        let system = if new_summary.is_some() {
            build_system_prompt(
                base_prompt.as_deref(),
                skills.as_deref(),
                dna.as_deref(),
                rag_context.as_deref(),
                memory_context.as_deref(),
                new_summary.as_deref(),
                user_display.as_deref(),
            )
        } else {
            system
        };

        for _iteration in 0..MAX_TOOL_ITERATIONS {
            let request = LlmRequest {
                model: String::new(),
                messages: messages.clone(),
                system: system.clone(),
                max_tokens: Some(self.max_tokens.unwrap_or(4096)),
                temperature: None,
                tools: tool_defs.clone(),
            };

            let response = provider.complete(&request).await?;

            if let Some(usage) = &response.usage {
                self.accumulate_usage(
                    session_id,
                    provider.provider_id(),
                    &response.model,
                    usage.input_tokens,
                    usage.output_tokens,
                );
            }

            let has_tool_use = response
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }));

            if !has_tool_use {
                let final_text = extract_text(&response.content);

                if let Err(e) = self
                    .remember_turn(
                        session_id,
                        continuity_key,
                        user_id,
                        memory_text,
                        &final_text,
                    )
                    .await
                {
                    warn!("failed to store turn in memory: {}", e);
                }

                return Ok((final_text, new_summary));
            }

            messages.push(ChatMessage {
                role: ChatRole::Assistant,
                content: MessagePart::Parts(response.content.clone()),
            });

            let mut tool_results = Vec::new();
            for block in &response.content {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    let context = ToolContext {
                        session_id: session_id.to_string(),
                        user_id: user_id.map(|s| s.to_string()),
                        heartbeat_depth,
                        allowed_tools: self.session_allowed_tools(session_id),
                    };
                    let output = match self.check_tool_allowed(session_id, name) {
                        Err(e) => ToolOutput::error(e.to_string()),
                        Ok(()) => match self.find_tool(name) {
                            Some(tool) => tool
                                .execute(&context, input.clone())
                                .await
                                .unwrap_or_else(|e| ToolOutput::error(e.to_string())),
                            None => ToolOutput::error(format!("unknown tool: {}", name)),
                        },
                    };
                    self.record_debug_tool_call(session_id, name, &input.to_string());
                    tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: output.content,
                    });
                }
            }

            messages.push(ChatMessage {
                role: ChatRole::User,
                content: MessagePart::Parts(tool_results),
            });
        }

        Err(Error::Agent(format!(
            "tool loop exceeded maximum of {} iterations",
            MAX_TOOL_ITERATIONS
        )))
    }

    /// Streaming impl with summary support.
    #[allow(clippy::too_many_arguments)]
    #[instrument(skip(self, user_content, conversation_history, delta_tx, session_summary), fields(provider_id, continuity_key = ?continuity_key))]
    async fn process_message_streaming_summarized_impl(
        &self,
        session_id: &str,
        user_content: MessagePart,
        memory_text: &str,
        conversation_history: &[ChatMessage],
        delta_tx: mpsc::Sender<String>,
        session_summary: Option<&str>,
        continuity_key: Option<&str>,
        user_id: Option<&str>,
    ) -> Result<(String, Option<String>)> {
        let provider: Arc<dyn LlmProvider> = self
            .default_provider()
            .ok_or_else(|| Error::Agent("no LLM provider configured".into()))?;

        let memory_context = match self
            .recall_context(
                memory_text,
                Some(session_id),
                continuity_key,
                self.recall_limit,
            )
            .await
        {
            Ok(entries) if !entries.is_empty() => {
                let context: Vec<String> = entries.iter().map(|e| e.content.clone()).collect();
                Some(format!(
                    "Relevant context from memory:\n- {}",
                    context.join("\n- ")
                ))
            }
            Err(e) => {
                warn!("memory recall failed, continuing without context: {}", e);
                None
            }
            _ => None,
        };

        let dna = self.dna_content();
        let skills = self.skills_content();
        let base_prompt = self.base_prompt_with_tools();
        let rag_context = self.auto_rag_context(memory_text).await;
        let user_display = self.session_user_name(session_id);
        let system = build_system_prompt(
            base_prompt.as_deref(),
            skills.as_deref(),
            dna.as_deref(),
            rag_context.as_deref(),
            memory_context.as_deref(),
            session_summary,
            user_display.as_deref(),
        );

        let tool_defs = self.tool_definitions();

        let mut messages: Vec<ChatMessage> = conversation_history.to_vec();
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: inject_rag_into_content(user_content, rag_context.as_deref()),
        });

        let max_ctx = self.max_context_tokens.unwrap_or(100_000);
        let new_summary = compact_messages(
            &mut messages,
            &system,
            &tool_defs,
            max_ctx,
            provider.as_ref(),
            session_summary,
            self.summarization_enabled,
        )
        .await;

        let system = if new_summary.is_some() {
            build_system_prompt(
                base_prompt.as_deref(),
                skills.as_deref(),
                dna.as_deref(),
                rag_context.as_deref(),
                memory_context.as_deref(),
                new_summary.as_deref(),
                user_display.as_deref(),
            )
        } else {
            system
        };

        let mut full_response = String::new();
        for _iteration in 0..MAX_TOOL_ITERATIONS {
            let request = LlmRequest {
                model: String::new(),
                messages: messages.clone(),
                system: system.clone(),
                max_tokens: Some(self.max_tokens.unwrap_or(4096)),
                temperature: None,
                tools: tool_defs.clone(),
            };

            let stream_result = provider.stream_complete(&request).await;

            match stream_result {
                Ok(mut stream) => {
                    let mut response_text = String::new();
                    let mut tool_uses: Vec<(String, String, String)> = Vec::new();
                    let mut current_tool: Option<(String, String, String)> = None;
                    let mut _stop_reason: Option<String> = None;

                    while let Some(event) = stream.next().await {
                        match event? {
                            StreamEvent::TextDelta(text) => {
                                response_text.push_str(&text);
                                let _ = delta_tx.send(text).await;
                            }
                            StreamEvent::ToolUseStart { id, name, .. } => {
                                current_tool = Some((id, name, String::new()));
                            }
                            StreamEvent::InputJsonDelta(json) => {
                                if let Some((_, _, ref mut input)) = current_tool {
                                    input.push_str(&json);
                                }
                            }
                            StreamEvent::ContentBlockStop { .. } => {
                                if let Some(tool) = current_tool.take() {
                                    tool_uses.push(tool);
                                }
                            }
                            StreamEvent::MessageDelta {
                                stop_reason: sr,
                                usage,
                            } => {
                                // OpenAI/vLLM never sends ContentBlockStop, so flush the
                                // in-flight tool when the stream signals tool_calls done.
                                if let Some(tool) = current_tool.take() {
                                    tool_uses.push(tool);
                                }
                                _stop_reason = sr;
                                if let Some(u) = usage {
                                    self.accumulate_usage(
                                        session_id,
                                        provider.provider_id(),
                                        provider.configured_model().unwrap_or(""),
                                        u.input_tokens,
                                        u.output_tokens,
                                    );
                                }
                            }
                            StreamEvent::MessageStop => break,
                        }
                    }

                    // Safety flush: if ContentBlockStop was never emitted (OpenAI/vLLM
                    // streaming), the last tool may still be pending.
                    if let Some(tool) = current_tool.take() {
                        tool_uses.push(tool);
                    }

                    if tool_uses.is_empty() {
                        full_response.push_str(&response_text);

                        if let Err(e) = self
                            .remember_turn(
                                session_id,
                                continuity_key,
                                user_id,
                                memory_text,
                                &full_response,
                            )
                            .await
                        {
                            warn!("failed to store turn in memory: {}", e);
                        }

                        return Ok((full_response, new_summary));
                    }

                    let mut content_blocks = Vec::new();
                    if !response_text.is_empty() {
                        content_blocks.push(ContentBlock::Text {
                            text: response_text.clone(),
                        });
                        full_response.push_str(&response_text);
                    }

                    for (id, name, input_json) in &tool_uses {
                        let input: serde_json::Value =
                            serde_json::from_str(input_json).unwrap_or_default();
                        content_blocks.push(ContentBlock::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input,
                        });
                    }

                    messages.push(ChatMessage {
                        role: ChatRole::Assistant,
                        content: MessagePart::Parts(content_blocks),
                    });

                    let mut tool_results = Vec::new();
                    for (id, name, input_json) in &tool_uses {
                        let input: serde_json::Value =
                            serde_json::from_str(input_json).unwrap_or_default();
                        let context = ToolContext {
                            session_id: session_id.to_string(),
                            user_id: user_id.map(|s| s.to_string()),
                            heartbeat_depth: 0,
                            allowed_tools: self.session_allowed_tools(session_id),
                        };
                        let output = match self.check_tool_allowed(session_id, name) {
                            Err(e) => ToolOutput::error(e.to_string()),
                            Ok(()) => match self.find_tool(name) {
                                Some(tool) => tool
                                    .execute(&context, input)
                                    .await
                                    .unwrap_or_else(|e| ToolOutput::error(e.to_string())),
                                None => ToolOutput::error(format!("unknown tool: {}", name)),
                            },
                        };
                        self.record_debug_tool_call(session_id, name, input_json);
                        tool_results.push(ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: output.content,
                        });
                    }

                    messages.push(ChatMessage {
                        role: ChatRole::User,
                        content: MessagePart::Parts(tool_results),
                    });

                    if !full_response.is_empty() {
                        full_response.push_str("\n\n");
                        let _ = delta_tx.send("\n\n".to_string()).await;
                    }
                }
                Err(_) => {
                    let response = provider.complete(&request).await?;

                    let has_tool_use = response
                        .content
                        .iter()
                        .any(|block| matches!(block, ContentBlock::ToolUse { .. }));

                    if !has_tool_use {
                        let final_text = extract_text(&response.content);
                        let _ = delta_tx.send(final_text.clone()).await;
                        full_response.push_str(&final_text);

                        if let Err(e) = self
                            .remember_turn(
                                session_id,
                                continuity_key,
                                user_id,
                                memory_text,
                                &full_response,
                            )
                            .await
                        {
                            warn!("failed to store turn in memory: {}", e);
                        }

                        return Ok((full_response, new_summary));
                    }

                    messages.push(ChatMessage {
                        role: ChatRole::Assistant,
                        content: MessagePart::Parts(response.content.clone()),
                    });

                    let mut tool_results = Vec::new();
                    for block in &response.content {
                        if let ContentBlock::ToolUse { id, name, input } = block {
                            let context = ToolContext {
                                session_id: session_id.to_string(),
                                user_id: user_id.map(|s| s.to_string()),
                                heartbeat_depth: 0,
                                allowed_tools: self.session_allowed_tools(session_id),
                            };
                            let output = match self.check_tool_allowed(session_id, name) {
                                Err(e) => ToolOutput::error(e.to_string()),
                                Ok(()) => match self.find_tool(name) {
                                    Some(tool) => tool
                                        .execute(&context, input.clone())
                                        .await
                                        .unwrap_or_else(|e| ToolOutput::error(e.to_string())),
                                    None => ToolOutput::error(format!("unknown tool: {}", name)),
                                },
                            };
                            tool_results.push(ContentBlock::ToolResult {
                                tool_use_id: id.clone(),
                                content: output.content,
                            });
                        }
                    }

                    messages.push(ChatMessage {
                        role: ChatRole::User,
                        content: MessagePart::Parts(tool_results),
                    });
                }
            }
        }

        Err(Error::Agent(format!(
            "tool loop exceeded maximum of {} iterations",
            MAX_TOOL_ITERATIONS
        )))
    }

    pub async fn health_check_all(&self) -> Result<Vec<(String, bool)>> {
        let providers: Vec<Arc<dyn LlmProvider>> = self.providers.read().unwrap().clone();
        let checks = providers.iter().map(|provider| async {
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

    /// Set the path to the document store for auto-RAG context injection.
    /// Opens and caches the store so subsequent requests reuse the same connection.
    /// Also checks whether any documents are already ingested to seed `has_documents`.
    pub fn set_doc_db_path(&mut self, path: PathBuf) {
        match DocumentStore::open(&path) {
            Ok(store) => {
                info!("auto_rag: document store cached at {}", path.display());
                let has_docs = store
                    .list_documents()
                    .map(|d| !d.is_empty())
                    .unwrap_or(false);
                self.has_documents.store(has_docs, Ordering::Relaxed);
                if has_docs {
                    info!("auto_rag: documents present, RAG active");
                } else {
                    info!("auto_rag: no documents ingested yet, embedding calls will be skipped");
                }
                self.doc_store = Some(Arc::new(store));
            }
            Err(e) => {
                warn!("auto_rag: failed to open document store for caching: {e}");
            }
        }
        self.doc_db_path = Some(path);
    }

    /// Notify the runtime that a document was ingested so auto-RAG becomes active.
    /// Call this after a successful ingest to avoid per-message document count queries.
    pub fn notify_document_ingested(&self) {
        self.has_documents.store(true, Ordering::Relaxed);
    }

    /// Auto-inject RAG context: embed the user query, search the document store,
    /// and return a formatted context string if any chunks score above the threshold.
    ///
    /// Returns `None` when no embedding provider is set, no doc DB path is configured,
    /// or no chunks score above the similarity threshold.
    async fn auto_rag_context(&self, user_text: &str) -> Option<String> {
        let store = self.doc_store.as_ref()?;

        // Skip the embedding API call entirely when no documents have been ingested.
        // If the flag is false, do a single lazy DB check in case documents were
        // ingested after startup (e.g. via REST API or channel command). Once a
        // document is detected the flag stays true for the lifetime of the process.
        if !self.has_documents.load(Ordering::Relaxed) {
            let has = store
                .list_documents()
                .map(|d| !d.is_empty())
                .unwrap_or(false);
            if !has {
                return None;
            }
            self.has_documents.store(true, Ordering::Relaxed);
            info!("auto_rag: documents detected, enabling RAG");
        }

        const THRESHOLD: f64 = 0.42;
        const TOP_K: usize = 3;

        let query_embedding = self.embed_query(user_text).await;
        if query_embedding.is_none() {
            warn!("auto_rag: no embedding provider, skipping");
            return None;
        }

        let chunks = store
            .hybrid_search_chunks(user_text, query_embedding.as_deref(), TOP_K, THRESHOLD)
            .unwrap_or_default();

        info!("auto_rag: found {} chunks above threshold", chunks.len());
        for c in &chunks {
            tracing::debug!("auto_rag: chunk '{}' score={:.4}", c.document_name, c.score);
        }

        if chunks.is_empty() {
            return None;
        }

        let mut parts = vec![
            "=== DOCUMENT CONTEXT (retrieved) ===".to_string(),
            "The following excerpts were retrieved from ingested documents. \
             Use them to answer the question. Prefer this information over memory or general knowledge \
             when it is relevant. Do NOT ask for a file path."
                .to_string(),
        ];
        for chunk in &chunks {
            parts.push(format!(
                "--- Source: {} (relevance: {:.2}) ---\n{}",
                chunk.document_name, chunk.score, chunk.text
            ));
        }
        parts.push("=== END DOCUMENT CONTEXT ===".to_string());
        Some(parts.join("\n\n"))
    }
}

impl Default for AgentRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Rough token estimate: ~4 characters per token.
fn estimate_tokens(
    messages: &[ChatMessage],
    system: &Option<String>,
    tools: &[ToolDefinition],
) -> usize {
    let mut chars: usize = 0;
    if let Some(s) = system {
        chars += s.len();
    }
    for msg in messages {
        match &msg.content {
            MessagePart::Text(t) => chars += t.len(),
            MessagePart::Parts(parts) => {
                for part in parts {
                    match part {
                        ContentBlock::Text { text } => chars += text.len(),
                        ContentBlock::ToolUse { input, .. } => chars += input.to_string().len(),
                        ContentBlock::ToolResult { content, .. } => chars += content.len(),
                        ContentBlock::Image { .. } => chars += 1000,
                    }
                }
            }
        }
    }
    for tool in tools {
        chars += tool.description.len() + tool.input_schema.to_string().len();
    }
    chars / 4
}

/// Drop the oldest messages until the estimated token count fits the budget.
/// Always keeps at least the last message (the current user input).
fn trim_messages_to_budget(
    messages: &mut Vec<ChatMessage>,
    system: &Option<String>,
    tools: &[ToolDefinition],
    max_tokens: usize,
) {
    while messages.len() > 1 && estimate_tokens(messages, system, tools) > max_tokens {
        messages.remove(0);
    }
}

/// Summarization-aware message compaction.
///
/// When estimated tokens exceed 75% of `max_tokens` and summarization is enabled,
/// drops the oldest messages and calls the LLM to produce a rolling summary.
/// Falls back to naive trimming on failure or when summarization is disabled.
///
/// Returns `Some(new_summary)` if summarization was performed.
async fn compact_messages(
    messages: &mut Vec<ChatMessage>,
    system: &Option<String>,
    tools: &[ToolDefinition],
    max_tokens: usize,
    provider: &dyn LlmProvider,
    existing_summary: Option<&str>,
    summarization_enabled: bool,
) -> Option<String> {
    let current_tokens = estimate_tokens(messages, system, tools);
    let threshold = max_tokens * 3 / 4; // 75%

    if current_tokens <= threshold {
        return None;
    }

    if !summarization_enabled || messages.len() <= 1 {
        trim_messages_to_budget(messages, system, tools, max_tokens);
        return None;
    }

    // Identify messages to drop (oldest until under 70% to leave room for summary)
    let target = max_tokens * 7 / 10;
    let mut drop_count = 0;
    {
        let mut temp = messages.clone();
        while temp.len() > 1 && estimate_tokens(&temp, system, tools) > target {
            temp.remove(0);
            drop_count += 1;
        }
    }

    if drop_count == 0 {
        trim_messages_to_budget(messages, system, tools, max_tokens);
        return None;
    }

    // Extract messages to summarize
    let to_summarize: Vec<&ChatMessage> = messages[..drop_count].iter().collect();
    let mut summary_input = String::new();
    if let Some(existing) = existing_summary {
        summary_input.push_str("Previous summary:\n");
        summary_input.push_str(existing);
        summary_input.push_str("\n\n");
    }
    summary_input.push_str("Recent conversation to incorporate:\n");
    for msg in &to_summarize {
        let role = match msg.role {
            ChatRole::User => "User",
            ChatRole::Assistant => "Assistant",
            ChatRole::System => "System",
            ChatRole::Tool => "Tool",
        };
        let text = match &msg.content {
            MessagePart::Text(t) => t.clone(),
            MessagePart::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" "),
        };
        summary_input.push_str(&format!("{role}: {text}\n"));
    }

    let summarize_request = LlmRequest {
        model: String::new(),
        messages: vec![ChatMessage {
            role: ChatRole::User,
            content: MessagePart::Text(summary_input),
        }],
        system: Some(
            "Summarize this conversation concisely. Preserve key facts, user identity, \
             decisions, and ongoing topics. Be brief."
                .to_string(),
        ),
        max_tokens: Some(500),
        temperature: Some(0.0),
        tools: Vec::new(),
    };

    match provider.complete(&summarize_request).await {
        Ok(response) => {
            let summary_text = extract_text(&response.content);
            if summary_text.is_empty() {
                warn!("summarization returned empty response, falling back to trim");
                trim_messages_to_budget(messages, system, tools, max_tokens);
                return None;
            }

            // Remove the old messages
            messages.drain(..drop_count);
            info!(
                "compacted conversation: dropped {} messages, summary len={}",
                drop_count,
                summary_text.len()
            );
            Some(summary_text)
        }
        Err(e) => {
            warn!("summarization failed, falling back to trim: {e}");
            trim_messages_to_budget(messages, system, tools, max_tokens);
            None
        }
    }
}

/// The bootstrap instruction injected when no dna.md exists yet.
/// The agent will ask the user a few questions and write dna.md itself.
/// Build the bootstrap instruction with the resolved config directory path.
/// We can't use a const because `~` doesn't expand in file paths and the
/// home directory must be resolved at runtime.
fn bootstrap_instruction() -> String {
    // Use the same resolution logic as ConfigLoader::default_config_dir():
    // prefer XDG config dir if it exists, fall back to ~/.opencrust/
    let config_dir = {
        let xdg = dirs::config_dir().map(|c| c.join("opencrust"));
        let home = dirs::home_dir().map(|h| h.join(".opencrust"));
        match (xdg, home) {
            (Some(xdg), Some(home)) => {
                if xdg.exists() {
                    xdg
                } else if home.exists() {
                    home
                } else {
                    xdg
                }
            }
            (Some(xdg), None) => xdg,
            (None, Some(home)) => home,
            (None, None) => std::path::PathBuf::from(".opencrust"),
        }
    };
    let dna_path = config_dir.join("dna.md");
    format!(
        "IMPORTANT: You have not been personalized yet. Your FIRST priority before doing \
         ANYTHING else is to collect the user's preferences. Do NOT answer their question yet. \
         Instead, introduce yourself briefly and ask:\n\
         1. What should I call you?\n\
         2. What should I call myself?\n\
         3. How do you prefer I communicate - casual, professional, or something else?\n\
         4. Any specific guidelines or things to avoid?\n\n\
         Keep it to 2-3 sentences. Once they answer, use the file_write tool to create \
         {} with a markdown document capturing their preferences and your identity, \
         then continue helping with whatever they originally asked.\n\n\
         If the user explicitly says to skip or ignores the questions twice, write a minimal \
         {} with sensible defaults and move on.",
        dna_path.display(),
        dna_path.display()
    )
}

/// Build the system prompt by combining all layers:
/// 1. Base system prompt + tool guidance (from effective_system_prompt)
/// 2. DNA content (personality)
/// 3. Past memory context (labeled, from semantic recall across sessions)
/// 4. Session summary
///
/// RAG document context is intentionally NOT included here — it is injected
/// directly into the user message by `inject_rag_into_content` so that models
/// which de-prioritize system prompts (e.g. vLLM-hosted Qwen3) still see it.
///
/// Source priority rule injected into the prompt:
///   documents (RAG, in user message) > memory > general knowledge
///
/// When no DNA content exists, a bootstrap instruction is injected
/// so the agent can collect user preferences on first interaction.
fn build_system_prompt(
    effective_prompt: Option<&str>,
    skills_content: Option<&str>,
    dna_content: Option<&str>,
    _rag_context: Option<&str>,
    memory_context: Option<&str>,
    session_summary: Option<&str>,
    user_display_name: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(prompt) = effective_prompt {
        parts.push(prompt.to_string());
    }
    if let Some(skills) = skills_content {
        parts.push(skills.to_string());
    }
    if let Some(dna) = dna_content {
        parts.push(dna.to_string());
    } else {
        parts.push(bootstrap_instruction());
    }
    if let Some(name) = user_display_name {
        parts.push(format!(
            "The user you are currently speaking with is named: {name}"
        ));
    }
    if let Some(ctx) = memory_context {
        parts.push(format!(
            "## Relevant memories from past conversations\n\
             The following was recalled from previous sessions with this user. \
             Use it for context and personalisation, but prefer document sources \
             when they contradict each other.\n\n{ctx}"
        ));
    }
    if let Some(summary) = session_summary {
        parts.push(format!(
            "## Conversation summary\n\
             The earlier part of this session has been summarised below.\n\n{summary}"
        ));
    }
    Some(parts.join("\n\n"))
}

/// Prepend RAG context directly into the user message so the model cannot ignore it.
/// Only modifies Text messages; multipart (image) messages are returned unchanged.
///
/// The injected block is clearly labeled so the model knows the source is
/// a retrieved document (not memory or general knowledge) and can cite it
/// appropriately. Source priority: documents > memory > general knowledge.
fn inject_rag_into_content(user_content: MessagePart, rag_context: Option<&str>) -> MessagePart {
    let Some(rag) = rag_context else {
        return user_content;
    };
    match user_content {
        MessagePart::Text(text) => MessagePart::Text(format!(
            "{rag}\n\n\
             [Answer the question below using the document context above as your primary source. \
             If the document context does not cover the question, fall back to memory or general knowledge \
             and say so briefly.]\n\n\
             ---\n\n\
             {text}"
        )),
        other => other,
    }
}

fn extract_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: ChatRole, text: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: MessagePart::Text(text.to_string()),
        }
    }

    #[test]
    fn build_system_prompt_all_parts() {
        let base = Some("You are helpful.");
        let dna = Some("Be kind.");
        let mem = Some("User likes Rust.");
        let sum = Some("We discussed project setup.");
        let result = build_system_prompt(base, None, dna, None, mem, sum, None).unwrap();
        assert!(result.contains("You are helpful."));
        assert!(result.contains("Be kind."));
        assert!(result.contains("User likes Rust."));
        assert!(result.contains("Relevant memories from past conversations"));
        assert!(result.contains("Conversation summary"));
        assert!(result.contains("We discussed project setup."));
    }

    #[test]
    fn build_system_prompt_base_before_dna() {
        let base = Some("You are helpful.");
        let dna = Some("You are a pirate.");
        let result = build_system_prompt(base, None, dna, None, None, None, None).unwrap();
        let base_pos = result.find("helpful").unwrap();
        let dna_pos = result.find("pirate").unwrap();
        assert!(base_pos < dna_pos);
    }

    #[test]
    fn build_system_prompt_no_summary() {
        let result =
            build_system_prompt(Some("Base."), None, Some("DNA."), None, None, None, None).unwrap();
        assert!(result.contains("Base."));
        assert!(result.contains("DNA."));
        assert!(!result.contains("Conversation summary"));
    }

    #[test]
    fn build_system_prompt_summary_only() {
        let result =
            build_system_prompt(None, None, None, None, None, Some("A summary."), None).unwrap();
        assert!(result.contains("Conversation summary"));
        assert!(result.contains("A summary."));
    }

    #[test]
    fn build_system_prompt_bootstrap_when_no_dna() {
        let result = build_system_prompt(None, None, None, None, None, None, None).unwrap();
        assert!(result.contains("have not been personalized yet"));
        assert!(result.contains("dna.md"));
    }

    #[test]
    fn build_system_prompt_dna_only() {
        let result = build_system_prompt(
            None,
            None,
            Some("You are a pirate."),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(result.contains("You are a pirate."));
    }

    #[test]
    fn build_system_prompt_with_user_name() {
        let result = build_system_prompt(
            Some("You are helpful."),
            None,
            Some("DNA content."),
            None,
            None,
            None,
            Some("Alice"),
        )
        .unwrap();
        assert!(result.contains("Alice"));
        assert!(result.contains("currently speaking with"));
    }

    #[test]
    fn build_system_prompt_user_name_none_no_effect() {
        let result =
            build_system_prompt(Some("Base."), None, Some("DNA."), None, None, None, None).unwrap();
        assert!(!result.contains("currently speaking with"));
    }

    #[test]
    fn session_user_name_set_and_get() {
        let runtime = AgentRuntime::new();
        assert!(runtime.session_user_name("sess").is_none());
        runtime.set_session_user_name("sess", "Alice");
        assert_eq!(runtime.session_user_name("sess").unwrap(), "Alice");
        runtime.clear_session_user_name("sess");
        assert!(runtime.session_user_name("sess").is_none());
    }

    #[test]
    fn retain_session_user_names_removes_evicted() {
        let runtime = AgentRuntime::new();
        runtime.set_session_user_name("keep", "Alice");
        runtime.set_session_user_name("drop", "Bob");
        runtime.retain_session_user_names(|id| id == "keep");
        assert!(runtime.session_user_name("keep").is_some());
        assert!(runtime.session_user_name("drop").is_none());
    }

    #[test]
    fn dna_content_set_and_get() {
        let runtime = AgentRuntime::new();
        assert!(runtime.dna_content().is_none());
        runtime.set_dna_content(Some("Be friendly.".to_string()));
        assert_eq!(runtime.dna_content().unwrap(), "Be friendly.");
        runtime.set_dna_content(None);
        assert!(runtime.dna_content().is_none());
    }

    #[test]
    fn trim_messages_to_budget_keeps_last() {
        // Each message ~25 chars = ~6 tokens, system = 0, tools = 0
        let mut messages = vec![
            make_msg(ChatRole::User, "aaaaaaaaaaaaaaaaaaaaaaaa"),
            make_msg(ChatRole::Assistant, "bbbbbbbbbbbbbbbbbbbbbbbb"),
            make_msg(ChatRole::User, "cccccccccccccccccccccccc"),
        ];
        // Total ~18 tokens, budget = 10 -> should drop until 1 left
        trim_messages_to_budget(&mut messages, &None, &[], 10);
        assert_eq!(messages.len(), 1);
        assert!(matches!(&messages[0].content, MessagePart::Text(t) if t.starts_with('c')));
    }

    #[test]
    fn trim_messages_to_budget_no_op_under_budget() {
        let mut messages = vec![
            make_msg(ChatRole::User, "hi"),
            make_msg(ChatRole::Assistant, "hello"),
        ];
        let original_len = messages.len();
        trim_messages_to_budget(&mut messages, &None, &[], 100_000);
        assert_eq!(messages.len(), original_len);
    }

    #[tokio::test]
    async fn compact_messages_under_budget_returns_none() {
        struct NeverCallProvider;
        #[async_trait::async_trait]
        impl LlmProvider for NeverCallProvider {
            fn provider_id(&self) -> &str {
                "never"
            }
            async fn complete(
                &self,
                _request: &LlmRequest,
            ) -> Result<crate::providers::LlmResponse> {
                panic!("should not be called");
            }
            async fn health_check(&self) -> Result<bool> {
                Ok(true)
            }
        }

        let mut messages = vec![
            make_msg(ChatRole::User, "hi"),
            make_msg(ChatRole::Assistant, "hello"),
        ];
        let original_len = messages.len();
        let provider = NeverCallProvider;

        let result =
            compact_messages(&mut messages, &None, &[], 100_000, &provider, None, true).await;

        assert!(result.is_none());
        assert_eq!(messages.len(), original_len);
    }

    #[tokio::test]
    async fn compact_messages_disabled_falls_back_to_trim() {
        struct NeverCallProvider;
        #[async_trait::async_trait]
        impl LlmProvider for NeverCallProvider {
            fn provider_id(&self) -> &str {
                "never"
            }
            async fn complete(
                &self,
                _request: &LlmRequest,
            ) -> Result<crate::providers::LlmResponse> {
                panic!("should not be called when summarization disabled");
            }
            async fn health_check(&self) -> Result<bool> {
                Ok(true)
            }
        }

        let long_text = "x".repeat(4000); // ~1000 tokens
        let mut messages = vec![
            make_msg(ChatRole::User, &long_text),
            make_msg(ChatRole::Assistant, &long_text),
            make_msg(ChatRole::User, "latest question"),
        ];
        let provider = NeverCallProvider;

        let result = compact_messages(
            &mut messages,
            &None,
            &[],
            100, // tiny budget to force trimming
            &provider,
            None,
            false, // summarization disabled
        )
        .await;

        assert!(result.is_none());
        // Should have trimmed to fit, keeping at least the last message
        assert!(messages.len() <= 2);
    }

    #[tokio::test]
    async fn compact_messages_summarizes_when_over_budget() {
        struct MockSummarizer;
        #[async_trait::async_trait]
        impl LlmProvider for MockSummarizer {
            fn provider_id(&self) -> &str {
                "mock"
            }
            async fn complete(
                &self,
                _request: &LlmRequest,
            ) -> Result<crate::providers::LlmResponse> {
                Ok(crate::providers::LlmResponse {
                    content: vec![ContentBlock::Text {
                        text: "Summary: user asked about Rust.".to_string(),
                    }],
                    model: String::new(),
                    usage: None,
                    stop_reason: None,
                })
            }
            async fn health_check(&self) -> Result<bool> {
                Ok(true)
            }
        }

        let long_text = "x".repeat(4000);
        let mut messages = vec![
            make_msg(ChatRole::User, &long_text),
            make_msg(ChatRole::Assistant, &long_text),
            make_msg(ChatRole::User, "latest question"),
        ];
        let provider = MockSummarizer;

        let result = compact_messages(
            &mut messages,
            &None,
            &[],
            1500, // budget that's less than total but leaves room after dropping old msgs
            &provider,
            None,
            true,
        )
        .await;

        assert!(result.is_some());
        assert!(result.unwrap().contains("Summary: user asked about Rust."));
        // Old messages should have been drained
        assert!(messages.len() < 3);
    }

    #[test]
    fn estimate_tokens_basic() {
        let messages = vec![make_msg(ChatRole::User, "hello world")]; // 11 chars
        let tokens = estimate_tokens(&messages, &None, &[]);
        assert_eq!(tokens, 11 / 4); // 2
    }

    #[test]
    fn recall_limit_default_is_10() {
        let runtime = AgentRuntime::new();
        assert_eq!(runtime.recall_limit, 10);
    }

    #[test]
    fn summarization_enabled_default_is_true() {
        let runtime = AgentRuntime::new();
        assert!(runtime.summarization_enabled);
    }

    #[test]
    fn set_recall_limit_works() {
        let mut runtime = AgentRuntime::new();
        runtime.set_recall_limit(20);
        assert_eq!(runtime.recall_limit, 20);
    }

    #[test]
    fn set_summarization_enabled_works() {
        let mut runtime = AgentRuntime::new();
        runtime.set_summarization_enabled(false);
        assert!(!runtime.summarization_enabled);
    }

    // --- Tool safety ---

    #[test]
    fn set_session_tool_config_and_check_allowed_tool() {
        let runtime = AgentRuntime::new();
        runtime.set_session_tool_config("sess", Some(vec!["bash".to_string()]), None);
        assert!(runtime.check_tool_allowed("sess", "bash").is_ok());
        assert!(runtime.check_tool_allowed("sess", "file_read").is_err());
    }

    #[test]
    fn check_tool_allowed_permits_all_when_no_config() {
        let runtime = AgentRuntime::new();
        // No config set — all tools pass
        assert!(runtime.check_tool_allowed("sess", "any_tool").is_ok());
    }

    #[test]
    fn check_tool_allowed_permits_all_when_allowed_tools_is_none() {
        let runtime = AgentRuntime::new();
        runtime.set_session_tool_config("sess", None, None);
        assert!(runtime.check_tool_allowed("sess", "bash").is_ok());
        assert!(runtime.check_tool_allowed("sess", "file_read").is_ok());
    }

    #[test]
    fn budget_exhaustion_blocks_tool_call() {
        let runtime = AgentRuntime::new();
        runtime.set_session_tool_config("sess", None, Some(2));
        assert!(runtime.check_tool_allowed("sess", "bash").is_ok()); // call 1
        assert!(runtime.check_tool_allowed("sess", "bash").is_ok()); // call 2
        let err = runtime.check_tool_allowed("sess", "bash"); // call 3 → blocked
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("budget"));
    }

    #[test]
    fn retain_session_tool_configs_removes_evicted_sessions() {
        let runtime = AgentRuntime::new();
        runtime.set_session_tool_config("keep", Some(vec!["bash".to_string()]), None);
        runtime.set_session_tool_config("drop", None, None);
        runtime.retain_session_tool_configs(|id| id == "keep");
        assert!(runtime.check_tool_allowed("keep", "bash").is_ok());
        // "drop" has no config → passes through (None config = all allowed)
        assert!(runtime.check_tool_allowed("drop", "bash").is_ok());
    }

    #[test]
    fn session_allowed_tools_returns_none_when_no_config() {
        let runtime = AgentRuntime::new();
        assert!(runtime.session_allowed_tools("sess").is_none());
    }

    #[test]
    fn session_allowed_tools_returns_configured_list() {
        let runtime = AgentRuntime::new();
        let tools = vec!["bash".to_string(), "web_search".to_string()];
        runtime.set_session_tool_config("sess", Some(tools.clone()), None);
        assert_eq!(runtime.session_allowed_tools("sess"), Some(tools));
    }

    // ---- RAG cache tests -------------------------------------------------------

    /// Stub embedding provider that returns a fixed vector regardless of input.
    struct FixedEmbedding(Vec<f32>);

    #[async_trait::async_trait]
    impl EmbeddingProvider for FixedEmbedding {
        fn provider_id(&self) -> &str {
            "fixed"
        }

        fn model(&self) -> &str {
            "fixed"
        }

        async fn embed_documents(
            &self,
            texts: &[String],
        ) -> opencrust_common::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| self.0.clone()).collect())
        }

        async fn embed_query(&self, _text: &str) -> opencrust_common::Result<Vec<f32>> {
            Ok(self.0.clone())
        }

        async fn health_check(&self) -> opencrust_common::Result<bool> {
            Ok(true)
        }
    }

    /// Seed an in-memory DocumentStore with one resume chunk and return it.
    fn resume_store() -> DocumentStore {
        let store = DocumentStore::in_memory().expect("in-memory store");

        let doc_id = store
            .add_document("resume.pdf", None, "application/pdf")
            .expect("add document");

        // Embed the chunk as a 3-dim unit vector pointing toward [1, 0, 0].
        let resume_embedding: Vec<f32> = vec![1.0, 0.0, 0.0];
        store
            .add_chunk(
                &doc_id,
                0,
                "John Doe — Senior Rust Engineer with 10 years of experience.",
                Some(&resume_embedding),
                Some("fixed"),
                Some(3),
                None,
            )
            .expect("add chunk");

        store
    }

    /// Query pointing toward the resume chunk (high cosine similarity).
    fn resume_query_embedding() -> Vec<f32> {
        vec![0.99, 0.1, 0.0]
    }

    /// Query pointing in a completely different direction (near-zero similarity).
    fn unrelated_query_embedding() -> Vec<f32> {
        vec![0.0, 0.0, 1.0]
    }

    #[tokio::test]
    async fn auto_rag_returns_context_for_resume_question() {
        let store = Arc::new(resume_store());

        // Resume query embedding is close to [1,0,0] → high similarity.
        let embed = Arc::new(FixedEmbedding(resume_query_embedding()));

        let mut runtime = AgentRuntime::new();
        runtime.doc_store = Some(store);
        runtime.set_embedding_provider(embed);

        let ctx = runtime
            .auto_rag_context("What is your work experience?")
            .await;

        assert!(
            ctx.is_some(),
            "RAG context should be injected for resume query"
        );
        let ctx = ctx.unwrap();
        assert!(
            ctx.contains("resume.pdf"),
            "context should name the source document"
        );
        assert!(
            ctx.contains("Senior Rust Engineer"),
            "context should include chunk text"
        );
        assert!(
            ctx.contains("relevance:"),
            "context should include relevance score"
        );
    }

    #[tokio::test]
    async fn auto_rag_returns_none_for_unrelated_question() {
        let store = Arc::new(resume_store());

        // Unrelated query embedding is orthogonal to resume chunk → similarity ≈ 0.
        let embed = Arc::new(FixedEmbedding(unrelated_query_embedding()));

        let mut runtime = AgentRuntime::new();
        runtime.doc_store = Some(store);
        runtime.set_embedding_provider(embed);

        let ctx = runtime
            .auto_rag_context("What is the weather in Bangkok today?")
            .await;

        assert!(
            ctx.is_none(),
            "RAG context should NOT be injected for unrelated question (similarity below threshold)"
        );
    }
}
