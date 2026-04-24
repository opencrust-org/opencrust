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
    TrajectoryStore,
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
/// Minimum number of tool calls in a turn before the agent is nudged to consider create_skill.
const SKILL_REFLECTION_THRESHOLD: usize = 3;
/// Default max skills injected per turn when semantic retrieval is active.
const DEFAULT_SKILL_RECALL_LIMIT: usize = 5;
/// Minimum cosine similarity for a skill to be considered relevant (0–1).
const SKILL_SIMILARITY_THRESHOLD: f64 = 0.25;
/// Minimum confidence (0–1) required before the refine nudge applies a patch.
const SKILL_REFINE_CONFIDENCE_THRESHOLD: f64 = 0.7;

/// Default base system prompt when none is configured.
const DEFAULT_BASE_SYSTEM_PROMPT: &str = "\
You are a personal AI assistant powered by OpenCrust. You help the user by answering \
questions, searching their documents, and executing tasks using your available tools. \
Be concise and accurate. If you don't know something, say so. Do not make up information. \
Always respond in the same language the user writes in.";

/// A skill with its pre-computed embedding for semantic retrieval.
struct IndexedSkill {
    skill: opencrust_skills::SkillDefinition,
    embedding: Vec<f32>,
}

/// Manages agent sessions, tool execution, and LLM provider routing.
pub struct AgentRuntime {
    providers: RwLock<Vec<Arc<dyn LlmProvider>>>,
    default_provider: RwLock<Option<String>>,
    memory: Option<Arc<dyn MemoryProvider>>,
    embeddings: Option<Arc<dyn EmbeddingProvider>>,
    tools: Vec<Box<dyn Tool>>,
    system_prompt: Option<String>,
    dna_content: RwLock<Option<String>>,
    /// Flat skills block injected when embedding provider is absent or skill count ≤ recall limit.
    skills_content: RwLock<Option<String>>,
    /// Semantic skill index — populated when embedding provider is present.
    /// When non-empty, `skills_content` is unused and retrieval is semantic.
    skills_index: RwLock<Vec<IndexedSkill>>,
    skill_recall_limit: usize,
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
    /// Per-session DNA content override. When set, replaces the global dna_content for that session.
    session_dna_override: DashMap<String, String>,
    /// Per-session skills content override. When set, replaces global skill retrieval for that session.
    session_skills_override: DashMap<String, String>,
    /// When true, accumulate debug info (tool calls) per session.
    debug: bool,
    /// Debug info accumulated during message processing, keyed by session_id.
    debug_accumulator: Mutex<HashMap<String, Vec<String>>>,
    /// Optional trajectory store. When set, every tool call and turn end is persisted.
    trajectory_store: Option<Arc<TrajectoryStore>>,
    /// Per-session turn counter used to order trajectory events.
    session_turn_index: DashMap<String, u32>,
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

/// Bundles the LLM call parameters needed by `skill_nudge_followup`.
struct NudgeContext<'a> {
    provider: &'a dyn LlmProvider,
    messages: &'a [ChatMessage],
    system: &'a Option<String>,
    model: &'a str,
    max_tokens: u32,
    /// The raw skill block injected into the system prompt for this turn.
    /// Used by `skill_refine_nudge_followup` to locate skill CHANGELOG files.
    skills_content: Option<&'a str>,
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
            skills_index: RwLock::new(Vec::new()),
            skill_recall_limit: DEFAULT_SKILL_RECALL_LIMIT,
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
            session_dna_override: DashMap::new(),
            session_skills_override: DashMap::new(),
            debug: false,
            debug_accumulator: Mutex::new(HashMap::new()),
            trajectory_store: None,
            session_turn_index: DashMap::new(),
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

    /// Get a clone of the current flat skills content (fallback path).
    pub fn skills_content(&self) -> Option<String> {
        self.skills_content.read().unwrap().clone()
    }

    /// Override the semantic skill recall limit (default: 5).
    pub fn set_skill_recall_limit(&mut self, limit: usize) {
        self.skill_recall_limit = limit;
    }

    /// Index skills for semantic retrieval. When an embedding provider is configured and
    /// the skill count exceeds `skill_recall_limit`, each skill is embedded and stored in
    /// `skills_index` so only the most relevant ones are injected per turn.
    ///
    /// Falls back to injecting all skills as a flat block when:
    /// - no embedding provider is configured, or
    /// - skill count ≤ `skill_recall_limit` (embedding call would be wasted).
    pub async fn index_skills(&self, skills: Vec<opencrust_skills::SkillDefinition>) {
        // Clear both stores first.
        *self.skills_index.write().unwrap() = Vec::new();

        if skills.is_empty() {
            *self.skills_content.write().unwrap() = None;
            return;
        }

        // If few skills or no embedding provider: inject all (no semantic search needed).
        if skills.len() <= self.skill_recall_limit || self.embeddings.is_none() {
            *self.skills_content.write().unwrap() = Some(skill_prompt_block(&skills));
            info!(
                "skills: injecting all {} skill(s) (below recall limit or no embedder)",
                skills.len()
            );
            return;
        }

        // Embed each skill's compact text representation.
        let mut indexed: Vec<IndexedSkill> = Vec::with_capacity(skills.len());
        for skill in &skills {
            let text = skill_embed_text(skill);
            match self.embed_document(&text).await {
                Some(embedding) => indexed.push(IndexedSkill {
                    skill: skill.clone(),
                    embedding,
                }),
                None => {
                    warn!(
                        "skills: failed to embed '{}', falling back to inject-all",
                        skill.frontmatter.name
                    );
                    // Partial failure → fall back to inject-all.
                    *self.skills_content.write().unwrap() = Some(skill_prompt_block(&skills));
                    return;
                }
            }
        }

        info!(
            "skills: indexed {} skill(s) for semantic retrieval (recall limit: {})",
            indexed.len(),
            self.skill_recall_limit
        );
        *self.skills_index.write().unwrap() = indexed;
        *self.skills_content.write().unwrap() = None;
    }

    /// Return the skills block to inject into the system prompt for a given user message.
    ///
    /// When a semantic index is available, embeds the query and returns the top-K most
    /// relevant skills. Falls back to the flat block when no index exists.
    pub async fn relevant_skills_content(&self, user_text: &str) -> Option<String> {
        // Clone the indexed data while holding the lock, then release it before
        // any await point so the non-Send RwLockReadGuard doesn't cross a yield.
        let entries: Vec<(Vec<f32>, opencrust_skills::SkillDefinition)> = {
            let index = self.skills_index.read().unwrap();
            if index.is_empty() {
                return self.skills_content();
            }
            index
                .iter()
                .map(|e| (e.embedding.clone(), e.skill.clone()))
                .collect()
        };

        let query_emb = match self.embed_query(user_text).await {
            Some(e) => e,
            None => return self.skills_content(),
        };

        let mut scored: Vec<(f64, usize)> = entries
            .iter()
            .enumerate()
            .map(|(i, (emb, _))| (cosine_similarity(&query_emb, emb), i))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let top: Vec<&opencrust_skills::SkillDefinition> = scored
            .iter()
            .take(self.skill_recall_limit)
            .filter(|(score, _)| *score >= SKILL_SIMILARITY_THRESHOLD)
            .map(|(_, i)| &entries[*i].1)
            .collect();

        tracing::debug!(
            "skills: semantic retrieval — top {} of {} skill(s) above threshold {:.2}",
            top.len(),
            entries.len(),
            SKILL_SIMILARITY_THRESHOLD
        );

        if top.is_empty() {
            return None;
        }

        Some(skill_prompt_block_refs(&top))
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

    pub fn set_trajectory_store(&mut self, store: Arc<TrajectoryStore>) {
        self.trajectory_store = Some(store);
    }

    /// Analyse stored trajectories and return skill suggestions for tool sequences
    /// that have been repeated at least `min_occurrences` times.
    ///
    /// Returns an empty vec when trajectory collection is disabled or no patterns
    /// meet the threshold.
    pub fn suggest_skills(
        &self,
        min_occurrences: usize,
    ) -> Vec<crate::skill_suggester::SkillSuggestion> {
        let Some(store) = &self.trajectory_store else {
            return Vec::new();
        };
        let skills_dir = self.skills_dir();
        crate::skill_suggester::suggest_from_trajectories(store, &skills_dir, min_occurrences)
    }

    /// Increment the per-session turn counter and return the index for this turn.
    fn traj_advance_turn(&self, session_id: &str) -> u32 {
        let mut entry = self
            .session_turn_index
            .entry(session_id.to_string())
            .or_insert(0);
        let idx = *entry;
        *entry += 1;
        idx
    }

    fn traj_log_tool_call(
        &self,
        session_id: &str,
        turn_index: u32,
        name: &str,
        input: &serde_json::Value,
    ) {
        if let Some(store) = &self.trajectory_store {
            store
                .log_tool_call(session_id, turn_index, name, &input.to_string())
                .unwrap_or_else(|e| warn!("trajectory log_tool_call failed: {e}"));
        }
    }

    fn traj_log_tool_result(
        &self,
        session_id: &str,
        turn_index: u32,
        name: &str,
        output: &str,
        latency_ms: u64,
    ) {
        if let Some(store) = &self.trajectory_store {
            store
                .log_tool_result(session_id, turn_index, name, output, latency_ms)
                .unwrap_or_else(|e| warn!("trajectory log_tool_result failed: {e}"));
        }
    }

    fn traj_log_turn_end(&self, session_id: &str, turn_index: u32, output: &str, tokens: u32) {
        if let Some(store) = &self.trajectory_store {
            store
                .log_turn_end(session_id, turn_index, output, tokens)
                .unwrap_or_else(|e| warn!("trajectory log_turn_end failed: {e}"));
        }
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
    ///
    /// Preserves the existing `call_count` so the budget is enforced across
    /// the whole session, not reset on every incoming message.
    pub fn set_session_tool_config(
        &self,
        session_id: &str,
        allowed_tools: Option<Vec<String>>,
        budget: Option<u32>,
    ) {
        self.session_tool_config
            .entry(session_id.to_string())
            .and_modify(|cfg| {
                cfg.allowed_tools = allowed_tools.clone();
                cfg.budget = budget;
                // call_count is intentionally preserved
            })
            .or_insert(SessionToolConfig {
                allowed_tools,
                call_count: 0,
                budget,
            });
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

    /// Retain only DNA overrides whose session IDs satisfy the predicate.
    pub fn retain_session_dna_overrides<F>(&self, f: F)
    where
        F: Fn(&str) -> bool,
    {
        self.session_dna_override.retain(|id, _| f(id));
    }

    /// Retain only skills overrides whose session IDs satisfy the predicate.
    pub fn retain_session_skills_overrides<F>(&self, f: F)
    where
        F: Fn(&str) -> bool,
    {
        self.session_skills_override.retain(|id, _| f(id));
    }

    /// Returns `true` if a DNA override is stored for the session.
    /// Intended for use in tests and diagnostics.
    pub fn has_session_dna_override(&self, session_id: &str) -> bool {
        self.session_dna_override.contains_key(session_id)
    }

    /// Returns `true` if a skills override is stored for the session.
    /// Intended for use in tests and diagnostics.
    pub fn has_session_skills_override(&self, session_id: &str) -> bool {
        self.session_skills_override.contains_key(session_id)
    }

    /// Get the user display name for a session.
    fn session_user_name(&self, session_id: &str) -> Option<String> {
        self.session_user_name.get(session_id).map(|v| v.clone())
    }

    /// Override the DNA content for a specific session (per-agent personality).
    /// Pass `None` to clear the override and fall back to global dna.md.
    pub fn set_session_dna_override(&self, session_id: &str, content: Option<String>) {
        match content {
            Some(c) => {
                self.session_dna_override.insert(session_id.to_string(), c);
            }
            None => {
                self.session_dna_override.remove(session_id);
            }
        }
    }

    /// Override the skills content for a specific session (per-agent skill set).
    /// Pass `None` to clear the override and fall back to global skill retrieval.
    pub fn set_session_skills_override(&self, session_id: &str, content: Option<String>) {
        match content {
            Some(c) => {
                self.session_skills_override
                    .insert(session_id.to_string(), c);
            }
            None => {
                self.session_skills_override.remove(session_id);
            }
        }
    }

    /// Return the effective DNA content for a session — session override takes priority.
    fn session_dna_content(&self, session_id: &str) -> Option<String> {
        self.session_dna_override
            .get(session_id)
            .map(|v| v.clone())
            .or_else(|| self.dna_content())
    }

    /// Return the effective skills content for a session — session override takes priority.
    /// When an override is set the flat block is returned directly (no semantic search).
    async fn session_skills_content(&self, session_id: &str, user_text: &str) -> Option<String> {
        if let Some(content) = self.session_skills_override.get(session_id) {
            return Some(content.clone());
        }
        self.relevant_skills_content(user_text).await
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

        // Self-learning guidance — injected as a dedicated section when create_skill is
        // registered. Kept separate from the tool list so the model reads it as a
        // first-class instruction rather than an inline tool hint.
        if self.tools.iter().any(|t| t.name() == "create_skill") {
            parts.push(self_learning_guidance());
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

    /// Execute a tool, logging call/result to the trajectory store and recording debug info.
    async fn run_tool(
        &self,
        session_id: &str,
        traj_turn_index: u32,
        context: &ToolContext,
        name: &str,
        input: &serde_json::Value,
    ) -> ToolOutput {
        self.traj_log_tool_call(session_id, traj_turn_index, name, input);
        let t0 = std::time::Instant::now();
        let output = match self.check_tool_allowed(session_id, name) {
            Err(e) => ToolOutput::error(e.to_string()),
            Ok(()) => match self.find_tool(name) {
                Some(tool) => tool
                    .execute(context, input.clone())
                    .await
                    .unwrap_or_else(|e| ToolOutput::error(e.to_string())),
                None => ToolOutput::error(format!("unknown tool: {}", name)),
            },
        };
        let latency_ms = t0.elapsed().as_millis() as u64;
        self.traj_log_tool_result(
            session_id,
            traj_turn_index,
            name,
            &output.content,
            latency_ms,
        );
        self.record_debug_tool_call(session_id, name, &input.to_string());
        output
    }

    /// Return the skills directory from the registered `create_skill` tool, if any.
    fn skills_dir(&self) -> std::path::PathBuf {
        self.tools
            .iter()
            .find(|t| t.name() == "create_skill")
            .and_then(|t| t.skills_dir_hint())
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    }

    /// Return a reflection nudge when the agent has completed a complex multi-tool workflow
    /// and the `create_skill` tool is available. Returns `None` when below threshold or
    /// when self-learning is disabled (tool not registered).
    /// Make a follow-up LLM call so the model generates a natural confirmation question
    /// asking the user whether to save the workflow as a skill.
    ///
    /// Passes `tools: vec![]` to prevent the model from entering another tool loop.
    /// Returns `None` if `create_skill` is not registered, the threshold is not met,
    /// or the follow-up call fails.
    async fn skill_nudge_followup(
        &self,
        tool_call_count: usize,
        ctx: NudgeContext<'_>,
        session_id: &str,
    ) -> Option<String> {
        let NudgeContext {
            provider,
            messages,
            system,
            model,
            max_tokens,
            skills_content: _,
        } = ctx;
        if tool_call_count < SKILL_REFLECTION_THRESHOLD {
            return None;
        }
        if !self.tools.iter().any(|t| t.name() == "create_skill") {
            return None;
        }
        let mut msgs = messages.to_vec();
        msgs.push(ChatMessage {
            role: ChatRole::User,
            content: MessagePart::Text(
                "[internal] You just completed a multi-step workflow using several tools. \
                 Ask the user in a short, friendly way (in the same language they used) \
                 whether they would like to save this workflow as a reusable skill. \
                 Reply with only that question — no preamble, no explanation."
                    .to_string(),
            ),
        });
        let request = LlmRequest {
            model: model.to_string(),
            messages: msgs,
            system: system.clone(),
            max_tokens: Some(max_tokens.min(256)),
            temperature: None,
            tools: vec![], // no tools — prevents re-entering the tool loop
        };
        match provider.complete(&request).await {
            Ok(response) => {
                if let Some(usage) = &response.usage {
                    self.accumulate_usage(
                        session_id,
                        provider.provider_id(),
                        &response.model,
                        usage.input_tokens,
                        usage.output_tokens,
                    );
                }
                let text = extract_text(&response.content);
                strip_think_blocks(&text)
            }
            Err(e) => {
                warn!("skill nudge follow-up LLM call failed: {}", e);
                None
            }
        }
    }

    /// Fire a self-improvement nudge when the agent used an existing skill.
    ///
    /// Makes a follow-up LLM call with `create_skill` available so the model can
    /// call `patch` autonomously if the skill had gaps. Returns a brief user-visible
    /// note when a patch was applied, or `None` when no improvement was needed.
    async fn skill_refine_nudge_followup(
        &self,
        tool_call_count: usize,
        ctx: NudgeContext<'_>,
        session_id: &str,
    ) -> Option<String> {
        let NudgeContext {
            provider,
            messages,
            system,
            model,
            max_tokens,
            skills_content,
        } = ctx;
        if tool_call_count < SKILL_REFLECTION_THRESHOLD {
            return None;
        }
        let create_skill_def = self
            .tools
            .iter()
            .find(|t| t.name() == "create_skill")
            .map(|t| ToolDefinition {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })?;
        // Build patch history context from CHANGELOG files of injected skills.
        let changelog_context = skills_content
            .map(|sc| build_changelog_context(sc, &self.skills_dir()))
            .unwrap_or_default();
        let history_note = if changelog_context.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nRecent patch history for the skill(s) used:\n{changelog_context}\
                 Only patch if you found a NEW gap not already addressed by recent changes."
            )
        };
        // ── Round 1: confidence assessment (no tools, JSON reply) ────────────
        let mut msgs = messages.to_vec();
        msgs.push(ChatMessage {
            role: ChatRole::User,
            content: MessagePart::Text(format!(
                "[internal] You just completed a task using an existing skill. \
                 Assess whether the skill needs improvement. \
                 Reply with a JSON object only — no other text:\n\
                 {{\"should_patch\": <true|false>, \"confidence\": <0.0–1.0>, \
                 \"reason\": \"<one line>\", \"skill_name\": \"<name>\"}}\n\
                 confidence = how certain you are that a gap exists (not how bad the gap is). \
                 If the skill worked well, set should_patch=false.{history_note}"
            )),
        });
        let assess_request = LlmRequest {
            model: model.to_string(),
            messages: msgs.clone(),
            system: system.clone(),
            max_tokens: Some(128),
            temperature: None,
            tools: vec![],
        };
        let assess_response = match provider.complete(&assess_request).await {
            Ok(r) => r,
            Err(e) => {
                warn!("skill refine assess LLM call failed: {}", e);
                return None;
            }
        };
        if let Some(usage) = &assess_response.usage {
            self.accumulate_usage(
                session_id,
                provider.provider_id(),
                &assess_response.model,
                usage.input_tokens,
                usage.output_tokens,
            );
        }
        // Parse assessment JSON; skip patch if confidence < threshold.
        let assessment_text = extract_text(&assess_response.content);
        let assessment = parse_refine_assessment(&assessment_text);
        if !assessment.should_patch || assessment.confidence < SKILL_REFINE_CONFIDENCE_THRESHOLD {
            return None;
        }

        // ── Round 2: execute patch (with create_skill tool) ──────────────────
        msgs.push(ChatMessage {
            role: ChatRole::Assistant,
            content: MessagePart::Text(assessment_text),
        });
        msgs.push(ChatMessage {
            role: ChatRole::User,
            content: MessagePart::Text(
                "[internal] Confidence threshold met. \
                 Call create_skill with action='patch' to apply the improvement now. \
                 Include the 'reason' field from your assessment."
                    .to_string(),
            ),
        });
        let request = LlmRequest {
            model: model.to_string(),
            messages: msgs,
            system: system.clone(),
            max_tokens: Some(max_tokens.min(512)),
            temperature: None,
            tools: vec![create_skill_def],
        };
        let response = match provider.complete(&request).await {
            Ok(r) => r,
            Err(e) => {
                warn!("skill refine nudge LLM call failed: {}", e);
                return None;
            }
        };
        if let Some(usage) = &response.usage {
            self.accumulate_usage(
                session_id,
                provider.provider_id(),
                &response.model,
                usage.input_tokens,
                usage.output_tokens,
            );
        }
        // Look for a patch tool call in the response.
        for block in &response.content {
            if let ContentBlock::ToolUse { name, input, .. } = block
                && name == "create_skill"
                && input.get("action").and_then(|v| v.as_str()) == Some("patch")
            {
                let skill_name = input
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let ctx = ToolContext {
                    session_id: session_id.to_string(),
                    user_id: None,
                    heartbeat_depth: 0,
                    allowed_tools: None,
                };
                if let Some(tool) = self.find_tool("create_skill") {
                    match tool.execute(&ctx, input.clone()).await {
                        Ok(out) if !out.is_error => {
                            return Some(format!(
                                "_(Skill '{skill_name}' updated based on this session.)_"
                            ));
                        }
                        Ok(out) => {
                            warn!("skill refine patch failed: {}", out.content);
                        }
                        Err(e) => {
                            warn!("skill refine patch error: {}", e);
                        }
                    }
                }
                return None;
            }
        }
        None
    }

    /// Dispatch to `skill_refine_nudge_followup` when skills were injected,
    /// or `skill_nudge_followup` when discovering a new workflow.
    async fn skill_completion_followup(
        &self,
        tool_call_count: usize,
        skills_were_injected: bool,
        ctx: NudgeContext<'_>,
        session_id: &str,
    ) -> Option<String> {
        if skills_were_injected {
            self.skill_refine_nudge_followup(tool_call_count, ctx, session_id)
                .await
        } else {
            self.skill_nudge_followup(tool_call_count, ctx, session_id)
                .await
        }
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
    /// `depth` is the handoff nesting level — 0 for direct user requests.
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
        max_context_tokens_override: Option<usize>,
    ) -> Result<String> {
        self.process_message_with_agent_config_at_depth(
            session_id,
            user_text,
            conversation_history,
            continuity_key,
            user_id,
            provider_id,
            model_override,
            system_prompt_override,
            max_tokens_override,
            max_context_tokens_override,
            0,
        )
        .await
    }

    /// Same as `process_message_with_agent_config` but carries a handoff `depth`
    /// so nested agent-to-agent calls propagate the correct nesting level to tools.
    #[allow(clippy::too_many_arguments)]
    pub async fn process_message_with_agent_config_at_depth(
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
        max_context_tokens_override: Option<usize>,
        depth: u8,
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

        let dna = self.session_dna_content(session_id);
        let skills = self.session_skills_content(session_id, user_text).await;
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

        let max_ctx = max_context_tokens_override
            .or(self.max_context_tokens)
            .unwrap_or(100_000);
        trim_messages_to_budget(&mut messages, &system, &tool_defs, max_ctx);

        let mut tool_call_count: usize = 0;
        let traj_turn_index = self.traj_advance_turn(session_id);
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
                let mut final_text = extract_text(&response.content);
                if let Err(e) = self
                    .remember_turn(session_id, continuity_key, user_id, user_text, &final_text)
                    .await
                {
                    warn!("failed to store turn in memory: {}", e);
                }
                if let Some(followup) = self
                    .skill_completion_followup(
                        tool_call_count,
                        skills.is_some(),
                        NudgeContext {
                            provider: provider.as_ref(),
                            messages: &messages,
                            system: &system,
                            model: &effective_model,
                            max_tokens: effective_max_tokens,
                            skills_content: skills.as_deref(),
                        },
                        session_id,
                    )
                    .await
                {
                    final_text.push_str("\n\n");
                    final_text.push_str(&followup);
                }
                self.traj_log_turn_end(session_id, traj_turn_index, &final_text, 0);
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
                        heartbeat_depth: depth,
                        allowed_tools: self.session_allowed_tools(session_id),
                    };
                    let output = self
                        .run_tool(session_id, traj_turn_index, &context, name, input)
                        .await;
                    tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: output.content,
                    });
                }
            }
            tool_call_count += response
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                .count();

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
        max_context_tokens_override: Option<usize>,
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

        let dna = self.session_dna_content(session_id);
        let skills = self.session_skills_content(session_id, user_text).await;
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

        let max_ctx = max_context_tokens_override
            .or(self.max_context_tokens)
            .unwrap_or(100_000);
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

        let mut tool_call_count: usize = 0;
        let traj_turn_index = self.traj_advance_turn(session_id);
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
                let mut final_text = extract_text(&response.content);
                if let Err(e) = self
                    .remember_turn(session_id, continuity_key, user_id, user_text, &final_text)
                    .await
                {
                    warn!("failed to store turn in memory: {}", e);
                }
                if let Some(followup) = self
                    .skill_completion_followup(
                        tool_call_count,
                        skills.is_some(),
                        NudgeContext {
                            provider: provider.as_ref(),
                            messages: &messages,
                            system: &system,
                            model: &effective_model,
                            max_tokens: effective_max_tokens,
                            skills_content: skills.as_deref(),
                        },
                        session_id,
                    )
                    .await
                {
                    final_text.push_str("\n\n");
                    final_text.push_str(&followup);
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
                    let output = self
                        .run_tool(session_id, traj_turn_index, &context, name, input)
                        .await;
                    tool_results.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: output.content,
                    });
                }
            }
            tool_call_count += response
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                .count();

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
        let skills = self.relevant_skills_content(memory_text).await;
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

        let mut tool_call_count: usize = 0;
        let traj_turn_index = self.traj_advance_turn(session_id);
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
                let mut final_text = extract_text(&response.content);

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

                // Post-completion: let the LLM generate a natural follow-up question
                // asking the user whether to save the workflow as a skill.
                if let Some(followup) = self
                    .skill_completion_followup(
                        tool_call_count,
                        skills.is_some(),
                        NudgeContext {
                            provider: provider.as_ref(),
                            messages: &messages,
                            system: &system,
                            model: "",
                            max_tokens: self.max_tokens.unwrap_or(4096),
                            skills_content: skills.as_deref(),
                        },
                        session_id,
                    )
                    .await
                {
                    final_text.push_str("\n\n");
                    final_text.push_str(&followup);
                }

                self.traj_log_turn_end(session_id, traj_turn_index, &final_text, 0);
                return Ok(final_text);
            }

            // Count tool calls made in this iteration before executing them.
            tool_call_count += response
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                .count();

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
                    let output = self
                        .run_tool(session_id, traj_turn_index, &context, name, input)
                        .await;
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
        let skills = self.relevant_skills_content(memory_text).await;
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
        let mut tool_call_count: usize = 0;
        let traj_turn_index = self.traj_advance_turn(session_id);
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

                        if let Some(followup) = self
                            .skill_completion_followup(
                                tool_call_count,
                                skills.is_some(),
                                NudgeContext {
                                    provider: provider.as_ref(),
                                    messages: &messages,
                                    system: &system,
                                    model: "",
                                    max_tokens: self.max_tokens.unwrap_or(4096),
                                    skills_content: skills.as_deref(),
                                },
                                session_id,
                            )
                            .await
                        {
                            let chunk = format!("\n\n{followup}");
                            let _ = delta_tx.send(chunk.clone()).await;
                            full_response.push_str(&chunk);
                        }

                        return Ok(full_response);
                    }

                    // Count tool calls from this streaming iteration.
                    tool_call_count += tool_uses.len();

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
                        let output = self
                            .run_tool(session_id, traj_turn_index, &context, name, &input)
                            .await;
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

                        if let Some(followup) = self
                            .skill_completion_followup(
                                tool_call_count,
                                skills.is_some(),
                                NudgeContext {
                                    provider: provider.as_ref(),
                                    messages: &messages,
                                    system: &system,
                                    model: "",
                                    max_tokens: self.max_tokens.unwrap_or(4096),
                                    skills_content: skills.as_deref(),
                                },
                                session_id,
                            )
                            .await
                        {
                            let chunk = format!("\n\n{followup}");
                            let _ = delta_tx.send(chunk.clone()).await;
                            full_response.push_str(&chunk);
                        }

                        return Ok(full_response);
                    }

                    // Count tool calls from this fallback iteration.
                    tool_call_count += response
                        .content
                        .iter()
                        .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                        .count();

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
                            let output = self
                                .run_tool(session_id, traj_turn_index, &context, name, input)
                                .await;
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
        let skills = self.relevant_skills_content(memory_text).await;
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

        let mut tool_call_count: usize = 0;
        let traj_turn_index = self.traj_advance_turn(session_id);
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
                let mut final_text = extract_text(&response.content);

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

                if let Some(followup) = self
                    .skill_completion_followup(
                        tool_call_count,
                        skills.is_some(),
                        NudgeContext {
                            provider: provider.as_ref(),
                            messages: &messages,
                            system: &system,
                            model: "",
                            max_tokens: self.max_tokens.unwrap_or(4096),
                            skills_content: skills.as_deref(),
                        },
                        session_id,
                    )
                    .await
                {
                    final_text.push_str("\n\n");
                    final_text.push_str(&followup);
                }

                return Ok((final_text, new_summary));
            }

            // Count tool calls from this iteration.
            tool_call_count += response
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                .count();

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
                    let output = self
                        .run_tool(session_id, traj_turn_index, &context, name, input)
                        .await;
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
        let skills = self.relevant_skills_content(memory_text).await;
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
        let mut tool_call_count: usize = 0;
        let traj_turn_index = self.traj_advance_turn(session_id);
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

                        // Post-completion reflection nudge.
                        if let Some(followup) = self
                            .skill_completion_followup(
                                tool_call_count,
                                skills.is_some(),
                                NudgeContext {
                                    provider: provider.as_ref(),
                                    messages: &messages,
                                    system: &system,
                                    model: "",
                                    max_tokens: self.max_tokens.unwrap_or(4096),
                                    skills_content: skills.as_deref(),
                                },
                                session_id,
                            )
                            .await
                        {
                            let chunk = format!("\n\n{followup}");
                            let _ = delta_tx.send(chunk.clone()).await;
                            full_response.push_str(&chunk);
                        }

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

                    // Count tool calls from this streaming-summarized iteration.
                    tool_call_count += tool_uses.len();

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
                        let output = self
                            .run_tool(session_id, traj_turn_index, &context, name, &input)
                            .await;
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

                        // Post-completion reflection nudge (fallback path).
                        if let Some(followup) = self
                            .skill_completion_followup(
                                tool_call_count,
                                skills.is_some(),
                                NudgeContext {
                                    provider: provider.as_ref(),
                                    messages: &messages,
                                    system: &system,
                                    model: "",
                                    max_tokens: self.max_tokens.unwrap_or(4096),
                                    skills_content: skills.as_deref(),
                                },
                                session_id,
                            )
                            .await
                        {
                            let chunk = format!("\n\n{followup}");
                            let _ = delta_tx.send(chunk.clone()).await;
                            full_response.push_str(&chunk);
                        }

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

                    // Count tool calls from this fallback iteration.
                    tool_call_count += response
                        .content
                        .iter()
                        .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                        .count();

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
                            let output = self
                                .run_tool(session_id, traj_turn_index, &context, name, input)
                                .await;
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

        // When memory_text contains a group-context header (LINE group RAG injects
        // "[Recent group context]\n...\n---\n<current message>"), use only the
        // current message as the search query so the long history doesn't dilute
        // keyword/vector matching against document chunks.
        let query = match user_text.rfind("\n---\n") {
            Some(pos) => user_text[pos + 5..].trim(),
            None => user_text.trim(),
        };

        let query_embedding = self.embed_query(query).await;
        if query_embedding.is_none() {
            info!("auto_rag: no embedding provider, falling back to keyword search");
        }

        let chunks = store
            .hybrid_search_chunks(query, query_embedding.as_deref(), TOP_K, THRESHOLD)
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

    /// Answer `question` from `context_block` with a single, tool-free LLM call.
    ///
    /// Used by the group-chat RAG layer so that retrieved chat history is
    /// reported as-is, without the agent invoking file/bash tools to "verify"
    /// paths or other information that already exists in the context.
    pub async fn synthesize_from_context(
        &self,
        session_id: &str,
        context_block: &str,
        question: &str,
        history: &[ChatMessage],
    ) -> Result<String> {
        let provider = self
            .default_provider()
            .ok_or_else(|| Error::Agent("no LLM provider configured".into()))?;

        let system = Some(
            "You extract and report information from a provided group-chat context block. \
             Answer the user's question using ONLY what is written in [Group chat context]. \
             Do NOT check files, verify paths, run commands, or call any tools. \
             The context is the ground truth — report it directly."
                .to_string(),
        );

        let user_content = format!("[Group chat context]\n{context_block}\n\n---\n{question}");

        let mut messages: Vec<ChatMessage> = history.to_vec();
        messages.push(ChatMessage {
            role: ChatRole::User,
            content: MessagePart::Text(user_content),
        });

        let request = LlmRequest {
            model: String::new(),
            messages,
            system,
            max_tokens: Some(self.max_tokens.unwrap_or(4096)),
            temperature: None,
            tools: vec![], // structurally no tools — prevents FileRead/Bash from firing
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

        let text = extract_text(&response.content);
        if text.is_empty() {
            return Err(Error::Agent(
                "empty response from LLM during context synthesis".into(),
            ));
        }

        Ok(text)
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
fn self_learning_guidance() -> String {
    "## Self-Learning\n\
     You can persist reusable knowledge using `create_skill`. \
     Consider it **proactively** after completing a multi-step workflow — \
     especially if you had to reason through a sequence of tools or commands \
     that you would need to figure out again from scratch next time.\n\n\
     **Good triggers:**\n\
     - You used 3 or more tools to complete a task\n\
     - You reasoned through a non-obvious command sequence\n\
     - The workflow is domain-specific and not easily looked up\n\n\
     **Before saving, honestly answer all 3:**\n\
     1. Would I need to figure this out again from scratch? (if no → skip)\n\
     2. Is it specific enough to be actionable? (vague tips → skip)\n\
     3. Does a similar skill already exist? (if yes → skip)\n\n\
     If yes to (1) and (2) and no to (3): **ask the user for confirmation before saving** \
     (e.g. 'I found a reusable workflow — would you like me to save it as a skill?'). \
     Only call `create_skill` after the user confirms.\n\n\
     **Improving existing skills (action='patch'):**\n\
     If you retrieved an existing skill and noticed gaps — steps that were unclear, \
     outdated, or missing — you may call `create_skill` with `action='patch'` to \
     improve it autonomously. No user confirmation is required for patches."
        .to_string()
}

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
             If the context above seems incomplete or does not fully cover the question, \
             call the doc_search tool to retrieve additional chunks before answering. \
             If the document context does not cover the question at all, fall back to memory \
             or general knowledge and say so briefly.]\n\n\
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

/// Strip `<think>…</think>` reasoning blocks emitted by Qwen3 and similar
/// models running in thinking mode, then trim the remainder.
/// Returns `None` if nothing is left after stripping.
fn strip_think_blocks(text: &str) -> Option<String> {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    loop {
        match remaining.find("<think>") {
            None => {
                result.push_str(remaining);
                break;
            }
            Some(start) => {
                result.push_str(&remaining[..start]);
                match remaining[start..].find("</think>") {
                    None => break, // unclosed tag — discard the rest
                    Some(end) => {
                        remaining = &remaining[start + end + "</think>".len()..];
                    }
                }
            }
        }
    }
    let trimmed = result.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Cosine similarity between two embedding vectors. Returns 0.0 when inputs are
/// empty, different lengths, or either magnitude is zero.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let dot: f64 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| *x as f64 * *y as f64)
        .sum();
    let mag_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let mag_b: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        return 0.0;
    }
    dot / (mag_a * mag_b)
}

/// Compact text representation of a skill used for embedding at index time.
/// Combines name, description, and triggers so the vector captures intent.
/// Parsed result of the refine-nudge confidence assessment round.
struct RefineAssessment {
    should_patch: bool,
    confidence: f64,
}

/// Parse the JSON assessment produced by the confidence-check LLM call.
/// Returns a conservative default (should_patch=false) on any parse error.
fn parse_refine_assessment(text: &str) -> RefineAssessment {
    // Strip markdown code fences if present.
    let cleaned = text
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(cleaned) {
        let should_patch = v
            .get("should_patch")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let confidence = v.get("confidence").and_then(|c| c.as_f64()).unwrap_or(0.0);
        return RefineAssessment {
            should_patch,
            confidence,
        };
    }
    RefineAssessment {
        should_patch: false,
        confidence: 0.0,
    }
}

/// Extract skill names from the injected `# Active Skills` block and read
/// their CHANGELOG.md files. Returns a formatted string for the refine prompt,
/// or an empty string if no changelogs are found.
fn build_changelog_context(skills_block: &str, skills_dir: &std::path::Path) -> String {
    // Skill names appear as "## skill-name" headings in the block.
    let names: Vec<&str> = skills_block
        .lines()
        .filter_map(|l| l.strip_prefix("## "))
        .collect();
    let mut out = String::new();
    for name in names {
        let changelog = skills_dir.join(name).join("CHANGELOG.md");
        if let Ok(content) = std::fs::read_to_string(&changelog) {
            // Include only the first 500 chars to keep prompt compact.
            let snippet = if content.len() > 500 {
                &content[..500]
            } else {
                &content
            };
            out.push_str(&format!("### {name}\n{snippet}\n\n"));
        }
    }
    out
}

fn skill_embed_text(skill: &opencrust_skills::SkillDefinition) -> String {
    let fm = &skill.frontmatter;
    let mut parts = vec![fm.name.clone(), fm.description.clone()];
    if !fm.triggers.is_empty() {
        parts.push(fm.triggers.join(" "));
    }
    parts.join(" | ")
}

/// Format an owned slice of `SkillDefinition`s into the prompt block injected into the
/// system prompt. Mirrors `build_skill_block` in bootstrap.rs.
fn skill_prompt_block(skills: &[opencrust_skills::SkillDefinition]) -> String {
    let refs: Vec<&opencrust_skills::SkillDefinition> = skills.iter().collect();
    skill_prompt_block_refs(&refs)
}

/// Format a slice of `SkillDefinition` references into the prompt block.
fn skill_prompt_block_refs(skills: &[&opencrust_skills::SkillDefinition]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut block = String::from("# Active Skills\n");
    for skill in skills {
        block.push_str(&format!(
            "\n## {}\n{}\n",
            skill.frontmatter.name, skill.frontmatter.description
        ));
        if !skill.frontmatter.triggers.is_empty() {
            block.push_str(&format!(
                "Triggers: {}\n",
                skill.frontmatter.triggers.join(", ")
            ));
        }
        block.push('\n');
        block.push_str(&skill.body);
        block.push('\n');
    }
    block
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

    // ── strip_think_blocks ────────────────────────────────────────────────────

    #[test]
    fn strip_think_blocks_no_tags() {
        assert_eq!(
            strip_think_blocks("Hello world"),
            Some("Hello world".to_string())
        );
    }

    #[test]
    fn strip_think_blocks_only_thinking_returns_none() {
        let input = "<think>I should ask the user</think>";
        assert_eq!(strip_think_blocks(input), None);
    }

    #[test]
    fn strip_think_blocks_thinking_then_answer() {
        let input = "<think>Reason here</think>\nWould you like to save this?";
        assert_eq!(
            strip_think_blocks(input),
            Some("Would you like to save this?".to_string())
        );
    }

    #[test]
    fn strip_think_blocks_multiple_blocks() {
        let input = "<think>first</think> Answer <think>second</think> more";
        assert_eq!(strip_think_blocks(input), Some("Answer  more".to_string()));
    }

    #[test]
    fn strip_think_blocks_unclosed_tag_discards_rest() {
        let input = "Before <think>unclosed";
        assert_eq!(strip_think_blocks(input), Some("Before".to_string()));
    }

    #[test]
    fn strip_think_blocks_empty_input() {
        assert_eq!(strip_think_blocks(""), None);
    }

    // ── build_system_prompt ───────────────────────────────────────────────────

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
    fn set_session_tool_config_preserves_call_count_across_messages() {
        // Regression test for issue #318: call_count must not reset when
        // set_session_tool_config is called again (as happens on every message).
        let runtime = AgentRuntime::new();

        // First message: configure budget of 3, use 2 calls
        runtime.set_session_tool_config("sess", None, Some(3));
        assert!(runtime.check_tool_allowed("sess", "bash").is_ok()); // call 1
        assert!(runtime.check_tool_allowed("sess", "bash").is_ok()); // call 2

        // Second message: set_session_tool_config is called again (simulating
        // a new incoming message). call_count must still be 2, not reset to 0.
        runtime.set_session_tool_config("sess", None, Some(3));
        assert!(runtime.check_tool_allowed("sess", "bash").is_ok()); // call 3
        let err = runtime.check_tool_allowed("sess", "bash"); // call 4 → blocked
        assert!(err.is_err(), "budget should be exhausted across messages");
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
    fn retain_session_dna_overrides_removes_evicted_sessions() {
        let runtime = AgentRuntime::new();
        runtime.set_session_dna_override("keep", Some("you are helpful".to_string()));
        runtime.set_session_dna_override("drop", Some("you are a pirate".to_string()));
        runtime.retain_session_dna_overrides(|id| id == "keep");
        assert!(runtime.session_dna_override.contains_key("keep"));
        assert!(!runtime.session_dna_override.contains_key("drop"));
    }

    #[test]
    fn retain_session_skills_overrides_removes_evicted_sessions() {
        let runtime = AgentRuntime::new();
        runtime.set_session_skills_override("keep", Some("skill: greet".to_string()));
        runtime.set_session_skills_override("drop", Some("skill: farewell".to_string()));
        runtime.retain_session_skills_overrides(|id| id == "keep");
        assert!(runtime.session_skills_override.contains_key("keep"));
        assert!(!runtime.session_skills_override.contains_key("drop"));
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
    async fn auto_rag_keyword_fallback_when_no_embedding_provider() {
        // No embedding provider set — should fall back to keyword search.
        let store = Arc::new(resume_store());
        let mut runtime = AgentRuntime::new();
        runtime.doc_store = Some(store);
        // Deliberately no embedding provider.

        // Query contains "Rust" and "Engineer" which appear in the chunk text.
        let ctx = runtime.auto_rag_context("Rust Engineer experience").await;

        assert!(
            ctx.is_some(),
            "keyword fallback should inject context when query terms match chunk text"
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
    }

    #[tokio::test]
    async fn auto_rag_keyword_fallback_returns_none_when_no_terms_match() {
        let store = Arc::new(resume_store());
        let mut runtime = AgentRuntime::new();
        runtime.doc_store = Some(store);

        // Query has no terms present in the chunk.
        let ctx = runtime.auto_rag_context("weather Bangkok forecast").await;

        assert!(
            ctx.is_none(),
            "keyword fallback should return None when no terms match"
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

    // ---- Semantic skill retrieval tests ----------------------------------------

    fn make_skill(
        name: &str,
        description: &str,
        triggers: Vec<&str>,
    ) -> opencrust_skills::SkillDefinition {
        opencrust_skills::SkillDefinition {
            frontmatter: opencrust_skills::SkillFrontmatter {
                name: name.to_string(),
                description: description.to_string(),
                rationale: None,
                triggers: triggers.into_iter().map(|t| t.to_string()).collect(),
                dependencies: Vec::new(),
                version: None,
                license: None,
                compatibility: None,
                metadata: None,
            },
            body: format!("Steps for {}", name),
            source_path: None,
        }
    }

    // --- cosine_similarity ---

    #[test]
    fn cosine_similarity_identical_vectors_returns_one() {
        let v = vec![1.0f32, 2.0, 3.0];
        let s = cosine_similarity(&v, &v);
        assert!(
            (s - 1.0).abs() < 1e-6,
            "identical vectors → similarity 1.0, got {s}"
        );
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors_returns_zero() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        let s = cosine_similarity(&a, &b);
        assert!(
            s.abs() < 1e-6,
            "orthogonal vectors → similarity 0.0, got {s}"
        );
    }

    #[test]
    fn cosine_similarity_opposite_vectors_returns_minus_one() {
        let a = vec![1.0f32, 0.0];
        let b = vec![-1.0f32, 0.0];
        let s = cosine_similarity(&a, &b);
        assert!(
            (s + 1.0).abs() < 1e-6,
            "opposite vectors → similarity -1.0, got {s}"
        );
    }

    #[test]
    fn cosine_similarity_empty_returns_zero() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn cosine_similarity_mismatched_lengths_returns_zero() {
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    }

    // --- skill_embed_text ---

    #[test]
    fn skill_embed_text_includes_name_and_description() {
        let skill = make_skill(
            "disk-cleanup",
            "Remove stale cache files",
            vec!["cleanup", "disk"],
        );
        let text = skill_embed_text(&skill);
        assert!(text.contains("disk-cleanup"));
        assert!(text.contains("Remove stale cache files"));
        assert!(text.contains("cleanup"));
    }

    #[test]
    fn skill_embed_text_no_triggers_excludes_separator() {
        let skill = make_skill("simple", "A simple skill", vec![]);
        let text = skill_embed_text(&skill);
        assert!(text.contains("simple"));
        assert!(text.contains("A simple skill"));
    }

    // --- skill_prompt_block ---

    #[test]
    fn skill_prompt_block_empty_returns_empty_string() {
        assert_eq!(skill_prompt_block(&[]), "");
    }

    #[test]
    fn skill_prompt_block_includes_name_description_and_body() {
        let skill = make_skill("git-cleanup", "Clean merged branches", vec!["git"]);
        let block = skill_prompt_block(&[skill]);
        assert!(block.contains("git-cleanup"));
        assert!(block.contains("Clean merged branches"));
        assert!(block.contains("Steps for git-cleanup"));
        assert!(block.contains("git")); // trigger
    }

    #[test]
    fn skill_prompt_block_multiple_skills_all_included() {
        let skills = vec![
            make_skill("skill-a", "Description A", vec![]),
            make_skill("skill-b", "Description B", vec![]),
        ];
        let block = skill_prompt_block(&skills);
        assert!(block.contains("skill-a"));
        assert!(block.contains("skill-b"));
    }

    // --- skill_recall_limit ---

    #[test]
    fn skill_recall_limit_default_is_five() {
        let runtime = AgentRuntime::new();
        assert_eq!(runtime.skill_recall_limit, DEFAULT_SKILL_RECALL_LIMIT);
        assert_eq!(runtime.skill_recall_limit, 5);
    }

    #[test]
    fn set_skill_recall_limit_updates_value() {
        let mut runtime = AgentRuntime::new();
        runtime.set_skill_recall_limit(10);
        assert_eq!(runtime.skill_recall_limit, 10);
    }

    // --- index_skills: fallback paths ---

    #[tokio::test]
    async fn index_skills_empty_clears_content() {
        let runtime = AgentRuntime::new();
        // Seed some content first
        runtime.set_skills_content(Some("old block".to_string()));
        runtime.index_skills(vec![]).await;
        assert!(runtime.skills_content().is_none());
        assert!(runtime.skills_index.read().unwrap().is_empty());
    }

    #[tokio::test]
    async fn index_skills_no_embedder_falls_back_to_inject_all() {
        // No embedding provider → inject-all regardless of skill count.
        let runtime = AgentRuntime::new();
        let skills = vec![
            make_skill("a", "desc a", vec![]),
            make_skill("b", "desc b", vec![]),
            make_skill("c", "desc c", vec![]),
            make_skill("d", "desc d", vec![]),
            make_skill("e", "desc e", vec![]),
            make_skill("f", "desc f", vec![]),
        ];
        runtime.index_skills(skills).await;

        // Index should be empty (inject-all path)
        assert!(runtime.skills_index.read().unwrap().is_empty());
        // Flat content should have all skills
        let content = runtime
            .skills_content()
            .expect("inject-all should populate skills_content");
        assert!(content.contains('a') || content.contains("desc a"));
        assert!(content.contains("desc f"));
    }

    #[tokio::test]
    async fn index_skills_below_recall_limit_falls_back_to_inject_all() {
        // Even with an embedding provider, if skill count ≤ limit → inject-all (no embed call).
        let runtime = AgentRuntime::new();
        // recall_limit default = 5, add exactly 3 skills
        let skills = vec![
            make_skill("x", "desc x", vec![]),
            make_skill("y", "desc y", vec![]),
            make_skill("z", "desc z", vec![]),
        ];
        runtime.index_skills(skills).await;

        assert!(runtime.skills_index.read().unwrap().is_empty());
        let content = runtime.skills_content().expect("should be inject-all");
        assert!(content.contains("desc x"));
    }

    #[tokio::test]
    async fn index_skills_with_embedder_above_limit_populates_index() {
        let mut runtime = AgentRuntime::new();
        // Use a fixed embedding so every skill gets embedded without a real API call.
        runtime.set_embedding_provider(Arc::new(FixedEmbedding(vec![1.0, 0.0, 0.0])));
        runtime.set_skill_recall_limit(2);

        let skills = vec![
            make_skill("p", "desc p", vec![]),
            make_skill("q", "desc q", vec![]),
            make_skill("r", "desc r", vec![]),
        ];
        runtime.index_skills(skills).await;

        // Should have used semantic indexing
        let idx = runtime.skills_index.read().unwrap();
        assert_eq!(idx.len(), 3, "all 3 skills should be indexed");
        // Flat content should be None (semantic path active)
        drop(idx);
        assert!(runtime.skills_content().is_none());
    }

    // --- relevant_skills_content ---

    #[tokio::test]
    async fn relevant_skills_content_no_index_returns_flat_block() {
        let runtime = AgentRuntime::new();
        runtime.set_skills_content(Some("flat block".to_string()));

        let result = runtime.relevant_skills_content("any query").await;
        assert_eq!(result, Some("flat block".to_string()));
    }

    #[tokio::test]
    async fn relevant_skills_content_with_index_returns_relevant_skill() {
        let mut runtime = AgentRuntime::new();
        // All skills share the same fixed embedding (cosine sim = 1.0 → all above threshold).
        runtime.set_embedding_provider(Arc::new(FixedEmbedding(vec![1.0, 0.0, 0.0])));
        runtime.set_skill_recall_limit(1); // retrieve only top-1

        let skills = vec![
            make_skill("top-skill", "best match", vec![]),
            make_skill("other-skill", "less relevant", vec![]),
        ];
        runtime.index_skills(skills).await;

        let result = runtime.relevant_skills_content("some query").await;
        // With recall_limit=1 and all skills equally similar, exactly 1 skill is returned.
        let block = result.expect("should return some skills");
        assert!(
            block.contains("top-skill") ^ block.contains("other-skill"),
            "exactly one skill should be returned, got: {block}"
        );
    }

    #[tokio::test]
    async fn relevant_skills_content_no_embed_provider_falls_back() {
        // Index is empty (no embed provider at index_skills time) → flat fallback.
        let runtime = AgentRuntime::new();
        runtime.set_skills_content(Some("fallback block".to_string()));
        // skills_index is empty by default

        let result = runtime.relevant_skills_content("query").await;
        assert_eq!(result, Some("fallback block".to_string()));
    }

    // ---- Checkbox integration tests --------------------------------------------
    //
    // These tests prove the three manual verification steps from the PR:
    //
    //   [✓] With embedding provider + >5 skills: only relevant skills appear
    //   [✓] Without embedding provider: all skills injected (backward-compatible)
    //   [✓] Hot-reload: index_skills with new skill list replaces the old index

    /// Keyword-aware embedding stub.
    /// - Document text containing "disk"    → [1, 0, 0]
    /// - Document text containing "network" → [0, 1, 0]
    /// - Everything else                    → [0, 0, 1]
    ///
    /// Query text uses the same logic so a "disk" query hits only "disk" skills.
    struct KeywordEmbedding;

    #[async_trait::async_trait]
    impl EmbeddingProvider for KeywordEmbedding {
        fn provider_id(&self) -> &str {
            "keyword"
        }
        fn model(&self) -> &str {
            "keyword"
        }
        async fn embed_documents(
            &self,
            texts: &[String],
        ) -> opencrust_common::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| keyword_vec(t)).collect())
        }
        async fn embed_query(&self, text: &str) -> opencrust_common::Result<Vec<f32>> {
            Ok(keyword_vec(text))
        }
        async fn health_check(&self) -> opencrust_common::Result<bool> {
            Ok(true)
        }
    }

    fn keyword_vec(text: &str) -> Vec<f32> {
        if text.contains("disk") {
            vec![1.0, 0.0, 0.0]
        } else if text.contains("network") {
            vec![0.0, 1.0, 0.0]
        } else {
            vec![0.0, 0.0, 1.0]
        }
    }

    /// Checkbox #1: With embedding provider + >5 skills, only skills whose
    /// embedding is similar to the user query appear in the injected block.
    ///
    /// Setup: 6 skills (1 disk-related, 5 network-related), recall_limit = 3.
    /// Query: about "disk cleanup" → vector [1,0,0].
    /// Expected: only the disk skill is injected; network skills are absent.
    #[tokio::test]
    async fn checkbox1_only_relevant_skills_injected_with_embedding_provider() {
        let mut runtime = AgentRuntime::new();
        runtime.set_embedding_provider(Arc::new(KeywordEmbedding));
        runtime.set_skill_recall_limit(3); // below total count of 6 → semantic path

        let skills = vec![
            make_skill(
                "disk-cleanup",
                "Remove stale disk cache files",
                vec!["disk"],
            ),
            make_skill(
                "network-ping",
                "Check network connectivity via ping",
                vec!["network"],
            ),
            make_skill(
                "network-dns",
                "Diagnose DNS resolution issues",
                vec!["network"],
            ),
            make_skill(
                "network-trace",
                "Trace network route to a host",
                vec!["network"],
            ),
            make_skill(
                "network-stats",
                "Show network interface statistics",
                vec!["network"],
            ),
            make_skill(
                "network-fw",
                "Review firewall network rules",
                vec!["network"],
            ),
        ];
        runtime.index_skills(skills).await;

        // Semantic index should be active (6 skills > recall_limit 3, embedder present)
        assert_eq!(runtime.skills_index.read().unwrap().len(), 6);
        assert!(
            runtime.skills_content().is_none(),
            "semantic path: flat content should be None"
        );

        // Query about disk → cosine([1,0,0], [1,0,0]) = 1.0, cosine([1,0,0], [0,1,0]) = 0.0
        let block = runtime
            .relevant_skills_content("how do I clean up disk space?")
            .await
            .expect("should return disk skill");

        assert!(
            block.contains("disk-cleanup"),
            "disk-cleanup should be in the injected block"
        );
        assert!(
            !block.contains("network-ping"),
            "network skills should NOT appear for a disk query"
        );
        assert!(
            !block.contains("network-dns"),
            "network skills should NOT appear for a disk query"
        );
    }

    /// Checkbox #2: Without an embedding provider, all skills are injected
    /// regardless of query — backward-compatible behavior.
    #[tokio::test]
    async fn checkbox2_no_embedding_provider_injects_all_skills() {
        let runtime = AgentRuntime::new(); // no embedding provider

        let skills = vec![
            make_skill("alpha", "Alpha skill description", vec!["alpha"]),
            make_skill("beta", "Beta skill description", vec!["beta"]),
            make_skill("gamma", "Gamma skill description", vec!["gamma"]),
            make_skill("delta", "Delta skill description", vec!["delta"]),
            make_skill("epsilon", "Epsilon skill description", vec!["epsilon"]),
            make_skill("zeta", "Zeta skill description", vec!["zeta"]),
        ];
        runtime.index_skills(skills).await;

        // No embedding provider → inject-all, semantic index must be empty.
        assert!(
            runtime.skills_index.read().unwrap().is_empty(),
            "without embedder, semantic index must stay empty"
        );

        let block = runtime
            .relevant_skills_content("anything at all")
            .await
            .expect("all skills should be injected");

        // Every skill must appear — no semantic filtering.
        for name in ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"] {
            assert!(
                block.contains(name),
                "skill '{name}' should be in inject-all block"
            );
        }
    }

    /// Checkbox #3: Hot-reload — calling `index_skills` a second time with a
    /// different skill list fully replaces the previous index.
    ///
    /// This mirrors what `spawn_skills_watcher` does in server.rs when it
    /// detects a change in the skills directory.
    #[tokio::test]
    async fn checkbox3_hot_reload_replaces_index() {
        let mut runtime = AgentRuntime::new();
        runtime.set_embedding_provider(Arc::new(KeywordEmbedding));
        runtime.set_skill_recall_limit(2); // 3 skills > 2 → semantic path

        // First load: 3 disk skills
        let first_skills = vec![
            make_skill("disk-a", "disk cleanup task A", vec!["disk"]),
            make_skill("disk-b", "disk cleanup task B", vec!["disk"]),
            make_skill("disk-c", "disk cleanup task C", vec!["disk"]),
        ];
        runtime.index_skills(first_skills).await;
        assert_eq!(
            runtime.skills_index.read().unwrap().len(),
            3,
            "first load: 3 skills"
        );

        // Hot-reload: replace with 4 network skills (simulating watcher file change)
        let reloaded_skills = vec![
            make_skill("net-a", "network diagnostics A", vec!["network"]),
            make_skill("net-b", "network diagnostics B", vec!["network"]),
            make_skill("net-c", "network diagnostics C", vec!["network"]),
            make_skill("net-d", "network diagnostics D", vec!["network"]),
        ];
        runtime.index_skills(reloaded_skills).await;

        // Old disk skills must be gone; only new network skills in index.
        assert_eq!(
            runtime.skills_index.read().unwrap().len(),
            4,
            "after reload: 4 skills"
        );

        let block_disk = runtime
            .relevant_skills_content("how do I free up disk space?")
            .await;

        let block_net = runtime
            .relevant_skills_content("how do I diagnose network connectivity?")
            .await;

        // Disk query → [1,0,0] vs network skills [0,1,0] → similarity 0 → nothing returned
        assert!(
            block_disk.is_none() || !block_disk.as_deref().unwrap_or("").contains("disk-a"),
            "old disk skills should not appear after reload"
        );

        // Network query → should return network skills
        let net_block = block_net.expect("network skills should match network query");
        assert!(
            net_block.contains("net-a") || net_block.contains("net-b"),
            "new network skills should appear after reload"
        );
    }

    // ──────────────────────────────────────────────────────────────────────────
    // skill_nudge_followup unit tests
    // ──────────────────────────────────────────────────────────────────────────

    struct FixedProvider {
        reply: &'static str,
    }
    #[async_trait::async_trait]
    impl LlmProvider for FixedProvider {
        fn provider_id(&self) -> &str {
            "fixed"
        }
        async fn complete(&self, request: &LlmRequest) -> Result<crate::providers::LlmResponse> {
            // Verify no tools are sent (would cause infinite loop)
            assert!(
                request.tools.is_empty(),
                "skill_nudge_followup must send tools=[] to prevent tool loop"
            );
            Ok(crate::providers::LlmResponse {
                content: vec![ContentBlock::Text {
                    text: self.reply.to_string(),
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

    /// Provider for refine-nudge tests: accepts tool definitions, returns a text reply.
    struct RefineTextProvider {
        reply: &'static str,
    }
    #[async_trait::async_trait]
    impl LlmProvider for RefineTextProvider {
        fn provider_id(&self) -> &str {
            "refine-text"
        }
        async fn complete(&self, _request: &LlmRequest) -> Result<crate::providers::LlmResponse> {
            Ok(crate::providers::LlmResponse {
                content: vec![ContentBlock::Text {
                    text: self.reply.to_string(),
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

    /// Provider that simulates the two-round refine nudge flow:
    /// - Round 1 (tools=[]): returns a high-confidence JSON assessment
    /// - Round 2 (tools=[create_skill]): returns a patch tool_use call
    struct RefinePatchProvider {
        skill_name: &'static str,
        new_body: &'static str,
        call_count: std::sync::atomic::AtomicUsize,
    }
    impl RefinePatchProvider {
        fn new(skill_name: &'static str, new_body: &'static str) -> Self {
            Self {
                skill_name,
                new_body,
                call_count: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }
    #[async_trait::async_trait]
    impl LlmProvider for RefinePatchProvider {
        fn provider_id(&self) -> &str {
            "refine-patch"
        }
        async fn complete(&self, request: &LlmRequest) -> Result<crate::providers::LlmResponse> {
            let round = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if round == 0 {
                // Round 1: assessment — no tools expected
                assert!(
                    request.tools.is_empty(),
                    "assessment round must send tools=[]"
                );
                let json = format!(
                    r#"{{"should_patch":true,"confidence":0.9,"reason":"step 2 was unclear","skill_name":"{}"}}"#,
                    self.skill_name
                );
                Ok(crate::providers::LlmResponse {
                    content: vec![ContentBlock::Text { text: json }],
                    model: String::new(),
                    usage: None,
                    stop_reason: None,
                })
            } else {
                // Round 2: patch execution — create_skill tool expected
                assert!(
                    request.tools.iter().any(|t| t.name == "create_skill"),
                    "patch round must send create_skill tool definition"
                );
                Ok(crate::providers::LlmResponse {
                    content: vec![ContentBlock::ToolUse {
                        id: "tu_1".to_string(),
                        name: "create_skill".to_string(),
                        input: serde_json::json!({
                            "action": "patch",
                            "name": self.skill_name,
                            "body": self.new_body,
                            "reason": "step 2 was unclear",
                        }),
                    }],
                    model: String::new(),
                    usage: None,
                    stop_reason: None,
                })
            }
        }
        async fn health_check(&self) -> Result<bool> {
            Ok(true)
        }
    }

    struct FailingProvider;
    #[async_trait::async_trait]
    impl LlmProvider for FailingProvider {
        fn provider_id(&self) -> &str {
            "failing"
        }
        async fn complete(&self, _request: &LlmRequest) -> Result<crate::providers::LlmResponse> {
            Err(opencrust_common::Error::Agent("simulated LLM error".into()))
        }
        async fn health_check(&self) -> Result<bool> {
            Ok(true)
        }
    }

    fn runtime_with_create_skill_tool(dir: &std::path::Path) -> AgentRuntime {
        let mut runtime = AgentRuntime::new();
        runtime.register_tool(Box::new(
            crate::tools::create_skill_tool::CreateSkillTool::new(dir),
        ));
        runtime
    }

    #[tokio::test]
    async fn nudge_returns_none_below_threshold() {
        let dir = tempfile::TempDir::new().unwrap();
        let runtime = runtime_with_create_skill_tool(dir.path());
        let provider = FixedProvider {
            reply: "Would you like to save this?",
        };
        // tool_call_count = 2, threshold = 3 → should not fire
        let result = runtime
            .skill_completion_followup(
                SKILL_REFLECTION_THRESHOLD - 1,
                false,
                NudgeContext {
                    provider: &provider,
                    messages: &[],
                    system: &None,
                    model: "",
                    max_tokens: 256,
                    skills_content: None,
                },
                "sess",
            )
            .await;
        assert!(result.is_none(), "should not fire below threshold");
    }

    #[tokio::test]
    async fn nudge_returns_none_when_skills_injected_and_no_patch_needed() {
        // When a skill was injected but the refine nudge determines no patch is needed
        // (model replies with text, no tool_use), the followup should return None.
        let dir = tempfile::TempDir::new().unwrap();
        let runtime = runtime_with_create_skill_tool(dir.path());
        let provider = RefineTextProvider { reply: "" };
        let result = runtime
            .skill_completion_followup(
                SKILL_REFLECTION_THRESHOLD,
                true,
                NudgeContext {
                    provider: &provider,
                    messages: &[],
                    system: &None,
                    model: "",
                    max_tokens: 256,
                    skills_content: None,
                },
                "sess",
            )
            .await;
        assert!(
            result.is_none(),
            "should return None when model signals no improvement needed"
        );
    }

    #[tokio::test]
    async fn nudge_returns_none_without_create_skill_tool() {
        let runtime = AgentRuntime::new(); // no create_skill registered
        let provider = FixedProvider {
            reply: "Would you like to save this?",
        };
        let result = runtime
            .skill_completion_followup(
                SKILL_REFLECTION_THRESHOLD + 5,
                false,
                NudgeContext {
                    provider: &provider,
                    messages: &[],
                    system: &None,
                    model: "",
                    max_tokens: 256,
                    skills_content: None,
                },
                "sess",
            )
            .await;
        assert!(
            result.is_none(),
            "should not fire without create_skill tool registered"
        );
    }

    #[tokio::test]
    async fn nudge_fires_at_threshold_and_returns_llm_text() {
        let dir = tempfile::TempDir::new().unwrap();
        let runtime = runtime_with_create_skill_tool(dir.path());
        let provider = FixedProvider {
            reply: "Would you like to save this workflow as a reusable skill?",
        };
        let result = runtime
            .skill_completion_followup(
                SKILL_REFLECTION_THRESHOLD,
                false,
                NudgeContext {
                    provider: &provider,
                    messages: &[make_msg(ChatRole::User, "help me with git rebase")],
                    system: &None,
                    model: "",
                    max_tokens: 256,
                    skills_content: None,
                },
                "sess",
            )
            .await;
        assert!(result.is_some(), "should fire at threshold");
        assert!(result.unwrap().contains("Would you like to save"));
    }

    #[tokio::test]
    async fn nudge_caps_max_tokens_at_256() {
        struct CheckMaxTokensProvider;
        #[async_trait::async_trait]
        impl LlmProvider for CheckMaxTokensProvider {
            fn provider_id(&self) -> &str {
                "check"
            }
            async fn complete(
                &self,
                request: &LlmRequest,
            ) -> Result<crate::providers::LlmResponse> {
                assert!(
                    request.max_tokens.unwrap_or(0) <= 256,
                    "max_tokens must be capped at 256, got {:?}",
                    request.max_tokens
                );
                Ok(crate::providers::LlmResponse {
                    content: vec![ContentBlock::Text {
                        text: "Save?".to_string(),
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

        let dir = tempfile::TempDir::new().unwrap();
        let runtime = runtime_with_create_skill_tool(dir.path());
        let provider = CheckMaxTokensProvider;
        // Pass a very large max_tokens; method must clamp to 256
        let result = runtime
            .skill_completion_followup(
                SKILL_REFLECTION_THRESHOLD,
                false,
                NudgeContext {
                    provider: &provider,
                    messages: &[],
                    system: &None,
                    model: "",
                    max_tokens: 8192,
                    skills_content: None,
                },
                "sess",
            )
            .await;
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn nudge_returns_none_on_provider_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let runtime = runtime_with_create_skill_tool(dir.path());
        let provider = FailingProvider;
        let result = runtime
            .skill_completion_followup(
                SKILL_REFLECTION_THRESHOLD,
                false,
                NudgeContext {
                    provider: &provider,
                    messages: &[],
                    system: &None,
                    model: "",
                    max_tokens: 256,
                    skills_content: None,
                },
                "sess",
            )
            .await;
        // Error should be swallowed — returns None, not Err
        assert!(
            result.is_none(),
            "provider error should yield None, not panic"
        );
    }

    #[tokio::test]
    async fn nudge_injects_internal_message_at_end_of_history() {
        struct CaptureProvider {
            captured: std::sync::Arc<std::sync::Mutex<Option<LlmRequest>>>,
        }
        #[async_trait::async_trait]
        impl LlmProvider for CaptureProvider {
            fn provider_id(&self) -> &str {
                "capture"
            }
            async fn complete(
                &self,
                request: &LlmRequest,
            ) -> Result<crate::providers::LlmResponse> {
                *self.captured.lock().unwrap() = Some(request.clone());
                Ok(crate::providers::LlmResponse {
                    content: vec![ContentBlock::Text {
                        text: "Save?".to_string(),
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

        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let dir = tempfile::TempDir::new().unwrap();
        let runtime = runtime_with_create_skill_tool(dir.path());
        let provider = CaptureProvider {
            captured: captured.clone(),
        };

        let history = vec![make_msg(ChatRole::User, "help me rebase")];
        runtime
            .skill_completion_followup(
                SKILL_REFLECTION_THRESHOLD,
                false,
                NudgeContext {
                    provider: &provider,
                    messages: &history,
                    system: &None,
                    model: "",
                    max_tokens: 256,
                    skills_content: None,
                },
                "sess",
            )
            .await;

        let req = captured
            .lock()
            .unwrap()
            .clone()
            .expect("provider must be called");
        // History has 1 message; injected internal message must be appended → total = 2
        assert_eq!(req.messages.len(), 2);
        let last = &req.messages[1];
        let text = match &last.content {
            MessagePart::Text(t) => t.clone(),
            _ => panic!("last message must be Text"),
        };
        assert!(
            text.contains("[internal]"),
            "injected message must contain [internal] marker"
        );
        assert!(matches!(last.role, ChatRole::User));
    }

    // ──────────────────────────────────────────────────────────────────────────
    // skill_refine_nudge_followup unit tests
    // ──────────────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn refine_nudge_returns_none_below_threshold() {
        let dir = tempfile::TempDir::new().unwrap();
        let runtime = runtime_with_create_skill_tool(dir.path());
        let provider = RefineTextProvider { reply: "" };
        let result = runtime
            .skill_refine_nudge_followup(
                SKILL_REFLECTION_THRESHOLD - 1,
                NudgeContext {
                    provider: &provider,
                    messages: &[],
                    system: &None,
                    model: "",
                    max_tokens: 256,
                    skills_content: None,
                },
                "sess",
            )
            .await;
        assert!(
            result.is_none(),
            "refine nudge must not fire below threshold"
        );
    }

    #[tokio::test]
    async fn refine_nudge_returns_none_without_create_skill_tool() {
        let runtime = AgentRuntime::new();
        let provider = RefineTextProvider { reply: "" };
        let result = runtime
            .skill_refine_nudge_followup(
                SKILL_REFLECTION_THRESHOLD,
                NudgeContext {
                    provider: &provider,
                    messages: &[],
                    system: &None,
                    model: "",
                    max_tokens: 256,
                    skills_content: None,
                },
                "sess",
            )
            .await;
        assert!(
            result.is_none(),
            "refine nudge must not fire when create_skill is not registered"
        );
    }

    #[tokio::test]
    async fn refine_nudge_returns_none_when_model_says_no_improvement() {
        let dir = tempfile::TempDir::new().unwrap();
        let runtime = runtime_with_create_skill_tool(dir.path());
        let provider = RefineTextProvider { reply: "" };
        let result = runtime
            .skill_refine_nudge_followup(
                SKILL_REFLECTION_THRESHOLD,
                NudgeContext {
                    provider: &provider,
                    messages: &[],
                    system: &None,
                    model: "",
                    max_tokens: 256,
                    skills_content: None,
                },
                "sess",
            )
            .await;
        assert!(
            result.is_none(),
            "empty model reply means no improvement needed — should return None"
        );
    }

    #[tokio::test]
    async fn refine_nudge_applies_patch_and_returns_note() {
        let dir = tempfile::TempDir::new().unwrap();
        // Pre-create the skill so patch can find it.
        let skill_dir = dir.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: test\n---\nOriginal body with enough chars to pass validation and be valid.",
        )
        .unwrap();

        let runtime = runtime_with_create_skill_tool(dir.path());
        let provider = RefinePatchProvider::new(
            "my-skill",
            "Updated body with improvements and enough characters to pass validation.",
        );
        let result = runtime
            .skill_refine_nudge_followup(
                SKILL_REFLECTION_THRESHOLD,
                NudgeContext {
                    provider: &provider,
                    messages: &[],
                    system: &None,
                    model: "",
                    max_tokens: 512,
                    skills_content: None,
                },
                "sess",
            )
            .await;
        assert!(
            result.is_some(),
            "should return a note when patch was applied"
        );
        assert!(
            result.unwrap().contains("my-skill"),
            "note should mention the skill name"
        );
        // Verify the skill file was updated.
        let updated = std::fs::read_to_string(skill_dir.join("SKILL.md")).unwrap();
        assert!(
            updated.contains("Updated body"),
            "SKILL.md should reflect the patch"
        );
        // CHANGELOG.md should be written inside the skill folder.
        let changelog = std::fs::read_to_string(skill_dir.join("CHANGELOG.md")).unwrap();
        assert!(
            changelog.contains("step 2 was unclear"),
            "CHANGELOG should record patch reason"
        );
    }

    #[tokio::test]
    async fn refine_nudge_skips_patch_when_confidence_too_low() {
        /// Provider that returns a low-confidence assessment on round 1.
        struct LowConfidenceProvider;
        #[async_trait::async_trait]
        impl LlmProvider for LowConfidenceProvider {
            fn provider_id(&self) -> &str {
                "low-conf"
            }
            async fn complete(
                &self,
                _request: &LlmRequest,
            ) -> Result<crate::providers::LlmResponse> {
                Ok(crate::providers::LlmResponse {
                    content: vec![ContentBlock::Text {
                        text: r#"{"should_patch":true,"confidence":0.5,"reason":"minor gap","skill_name":"x"}"#.to_string(),
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

        let dir = tempfile::TempDir::new().unwrap();
        let runtime = runtime_with_create_skill_tool(dir.path());
        let provider = LowConfidenceProvider;
        let result = runtime
            .skill_refine_nudge_followup(
                SKILL_REFLECTION_THRESHOLD,
                NudgeContext {
                    provider: &provider,
                    messages: &[],
                    system: &None,
                    model: "",
                    max_tokens: 512,
                    skills_content: None,
                },
                "sess",
            )
            .await;
        assert!(
            result.is_none(),
            "should not patch when confidence < SKILL_REFINE_CONFIDENCE_THRESHOLD"
        );
    }

    #[test]
    fn parse_refine_assessment_valid_json() {
        let text = r#"{"should_patch":true,"confidence":0.85,"reason":"gap","skill_name":"x"}"#;
        let a = parse_refine_assessment(text);
        assert!(a.should_patch);
        assert!((a.confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn parse_refine_assessment_with_code_fence() {
        let text = "```json\n{\"should_patch\":false,\"confidence\":0.3,\"reason\":\"ok\"}\n```";
        let a = parse_refine_assessment(text);
        assert!(!a.should_patch);
    }

    #[test]
    fn parse_refine_assessment_invalid_returns_no_patch() {
        let a = parse_refine_assessment("not json at all");
        assert!(!a.should_patch);
        assert_eq!(a.confidence, 0.0);
    }
}
