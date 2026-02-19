use opencrust_config::{AppConfig, NamedAgentConfig};

/// Resolve which named agent config to use for a given request.
///
/// Priority: explicit agent_id > channel setting > "default" agent > legacy config.
pub fn resolve<'a>(
    config: &'a AppConfig,
    agent_id: Option<&str>,
    channel_id: Option<&str>,
) -> Option<&'a NamedAgentConfig> {
    // 1. Explicit agent_id
    if let Some(id) = agent_id
        && let Some(agent) = config.agents.get(id)
    {
        return Some(agent);
    }

    // 2. Channel setting: look for `agent_id` in channel config's settings
    if let Some(ch_id) = channel_id
        && let Some(ch) = config.channels.get(ch_id)
        && let Some(serde_json::Value::String(agent_name)) = ch.settings.get("agent_id")
        && let Some(agent) = config.agents.get(agent_name.as_str())
    {
        return Some(agent);
    }

    // 3. "default" named agent
    if let Some(agent) = config.agents.get("default") {
        return Some(agent);
    }

    // 4. No named agent found — caller should fall back to legacy `agent:` config
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrust_config::AppConfig;

    #[test]
    fn resolve_explicit_agent_id() {
        let mut config = AppConfig::default();
        config.agents.insert(
            "helper".to_string(),
            NamedAgentConfig {
                provider: Some("claude".to_string()),
                model: None,
                system_prompt: Some("I help.".to_string()),
                max_tokens: None,
                max_context_tokens: None,
                tools: vec![],
            },
        );
        let result = resolve(&config, Some("helper"), None);
        assert!(result.is_some());
        assert_eq!(result.unwrap().system_prompt.as_deref(), Some("I help."));
    }

    #[test]
    fn resolve_falls_back_to_default() {
        let mut config = AppConfig::default();
        config.agents.insert(
            "default".to_string(),
            NamedAgentConfig {
                provider: None,
                model: None,
                system_prompt: Some("Default agent.".to_string()),
                max_tokens: None,
                max_context_tokens: None,
                tools: vec![],
            },
        );
        let result = resolve(&config, None, None);
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().system_prompt.as_deref(),
            Some("Default agent.")
        );
    }

    #[test]
    fn resolve_returns_none_when_no_agents() {
        let config = AppConfig::default();
        assert!(resolve(&config, None, None).is_none());
    }
}
