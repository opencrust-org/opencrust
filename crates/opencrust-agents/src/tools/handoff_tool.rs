use async_trait::async_trait;
use opencrust_common::Result;
use opencrust_config::AppConfig;
use std::sync::{Arc, OnceLock, RwLock, Weak};

use super::{Tool, ToolContext, ToolOutput};
use crate::AgentRuntime;

/// Maximum handoff nesting depth. Prevents A → B → A infinite loops.
const MAX_HANDOFF_DEPTH: u8 = 3;

/// Tool that delegates the current task to a named specialist agent.
///
/// The runtime reference is wired after construction via `HandoffHandle::wire()`
/// to break the bootstrap chicken-and-egg cycle (`register_tool` requires `&mut`
/// before `Arc::new`, but the Arc is needed by the tool itself).
pub struct HandoffTool {
    /// Weak reference to the owning runtime, set after `Arc::new(runtime)`.
    runtime: Arc<OnceLock<Weak<AgentRuntime>>>,
    /// Shared config for resolving per-agent overrides (provider, system_prompt, tools…).
    config: Arc<RwLock<AppConfig>>,
}

impl HandoffTool {
    /// Create a new (unwired) `HandoffTool` and return it alongside a handle
    /// that must be wired to the runtime `Arc` after construction.
    pub fn new(config: Arc<RwLock<AppConfig>>) -> (Self, HandoffHandle) {
        let holder = Arc::new(OnceLock::new());
        let tool = Self {
            runtime: Arc::clone(&holder),
            config,
        };
        let handle = HandoffHandle { holder };
        (tool, handle)
    }
}

/// Returned by `HandoffTool::new()`. Call `wire()` once `Arc<AgentRuntime>` exists.
pub struct HandoffHandle {
    holder: Arc<OnceLock<Weak<AgentRuntime>>>,
}

impl HandoffHandle {
    /// Wire the tool to the live runtime. Safe to call only once.
    pub fn wire(&self, runtime: &Arc<AgentRuntime>) {
        // Ignore the error — it means wire() was called twice, which is harmless.
        let _ = self.holder.set(Arc::downgrade(runtime));
    }
}

#[async_trait]
impl Tool for HandoffTool {
    fn name(&self) -> &str {
        "handoff"
    }

    fn description(&self) -> &str {
        "Delegate the current task to a specialist agent and return its response. \
         Use this when the user's request is better handled by a different agent \
         (e.g. a coder agent for programming tasks, a researcher for web research)."
    }

    fn system_hint(&self) -> Option<&str> {
        Some(
            "Use `handoff` to route tasks to specialist agents defined in `agents:` config. \
             Provide the exact `agent_id` and a clear `message` with full context. \
             Always incorporate the specialist's response into your final reply.",
        )
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "The named agent to delegate to (must exist in the `agents:` config section)."
                },
                "message": {
                    "type": "string",
                    "description": "The task or context to pass to the target agent."
                }
            },
            "required": ["agent_id", "message"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
        let agent_id = match input.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return Ok(ToolOutput::error("missing required parameter: 'agent_id'")),
        };
        let message = match input.get("message").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return Ok(ToolOutput::error("missing required parameter: 'message'")),
        };

        // Depth guard — reuse heartbeat_depth field to track handoff nesting.
        if context.heartbeat_depth >= MAX_HANDOFF_DEPTH {
            return Ok(ToolOutput::error(format!(
                "handoff depth limit ({MAX_HANDOFF_DEPTH}) reached: \
                 refusing to delegate further to prevent infinite agent loops"
            )));
        }

        let runtime = match self.runtime.get().and_then(|w| w.upgrade()) {
            Some(r) => r,
            None => {
                return Ok(ToolOutput::error(
                    "handoff tool is not wired to a runtime — \
                     call HandoffHandle::wire() after Arc::new(runtime)",
                ));
            }
        };

        // Resolve target agent config from the shared AppConfig.
        let ac = {
            let cfg = self.config.read().unwrap();
            cfg.agents.get(&agent_id).cloned()
        };
        let ac = match ac {
            Some(a) => a,
            None => {
                return Ok(ToolOutput::error(format!(
                    "unknown agent '{agent_id}' — check the `agents:` section in config"
                )));
            }
        };

        // Each handoff gets its own ephemeral session so history doesn't bleed.
        let handoff_session = format!("{}-handoff-{agent_id}", context.session_id);
        let child_depth = context.heartbeat_depth + 1;

        // Apply per-agent tool whitelist.
        if !ac.tools.is_empty() {
            runtime.set_session_tool_config(&handoff_session, Some(ac.tools.clone()), None);
        }
        // Apply per-agent DNA and skills overrides.
        if let Some(dna_path) = &ac.dna_file {
            let content = std::fs::read_to_string(dna_path)
                .ok()
                .filter(|s| !s.trim().is_empty());
            runtime.set_session_dna_override(&handoff_session, content);
        }
        if let Some(skills_path) = &ac.skills_dir {
            use opencrust_skills::SkillScanner;
            let block = SkillScanner::new(skills_path)
                .discover()
                .ok()
                .filter(|v| !v.is_empty())
                .map(|skills| {
                    let body = skills
                        .iter()
                        .map(|s| format!("### {}\n{}\n", s.frontmatter.name, s.body))
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("## Agent Skills\n\n{body}")
                });
            runtime.set_session_skills_override(&handoff_session, block);
        }

        let result = runtime
            .process_message_with_agent_config_at_depth(
                &handoff_session,
                &message,
                &[],
                None,
                context.user_id.as_deref(),
                ac.provider.as_deref(),
                ac.model.as_deref(),
                ac.system_prompt.as_deref(),
                ac.max_tokens,
                ac.max_context_tokens,
                child_depth,
            )
            .await;

        match result {
            Ok(response) => Ok(ToolOutput::success(format!("[{agent_id}]: {response}"))),
            Err(e) => Ok(ToolOutput::error(format!("agent '{agent_id}' failed: {e}"))),
        }
    }
}
