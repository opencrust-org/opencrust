use std::collections::HashSet;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::allowlist::Allowlist;
use crate::pairing::PairingManager;

/// DM authorization policy for a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DmPolicy {
    /// Allow all DMs without any auth check.
    Open,
    /// Require pairing code (default global allowlist behavior).
    Pairing,
    /// Only allow users in the per-channel allowlist.
    Allowlist,
}

/// Group message policy for a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupPolicy {
    /// Process all group messages.
    Open,
    /// Only process messages that mention the bot.
    Mention,
    /// Ignore all group messages.
    Disabled,
}

/// Result of a DM authorization check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmAuthResult {
    /// User is allowed - proceed with message processing.
    Allowed,
    /// User is blocked - do not process.
    Blocked,
    /// No per-channel policy set - fall back to global allowlist behavior.
    UseGlobalAllowlist,
}

/// Per-channel authorization policy.
#[derive(Debug, Clone, Default)]
pub struct ChannelPolicy {
    pub dm_policy: Option<DmPolicy>,
    pub group_policy: Option<GroupPolicy>,
    pub channel_allowlist: HashSet<String>,
}

impl ChannelPolicy {
    /// Parse policy from channel settings map.
    pub fn from_settings(settings: &std::collections::HashMap<String, serde_json::Value>) -> Self {
        let mut policy = Self::default();

        if let Some(val) = settings.get("dm_policy")
            && let Some(s) = val.as_str()
        {
            match serde_json::from_value::<DmPolicy>(serde_json::Value::String(s.to_string())) {
                Ok(p) => policy.dm_policy = Some(p),
                Err(_) => warn!("unknown dm_policy value: {s:?}"),
            }
        }

        if let Some(val) = settings.get("group_policy")
            && let Some(s) = val.as_str()
        {
            match serde_json::from_value::<GroupPolicy>(serde_json::Value::String(s.to_string())) {
                Ok(p) => policy.group_policy = Some(p),
                Err(_) => warn!("unknown group_policy value: {s:?}"),
            }
        }

        if let Some(val) = settings.get("allowlist")
            && let Some(arr) = val.as_array()
        {
            for item in arr {
                if let Some(s) = item.as_str() {
                    policy.channel_allowlist.insert(s.to_string());
                }
            }
        }

        policy
    }

    /// Check whether a group message should be processed.
    /// Returns `true` if the message should be processed.
    pub fn should_process_group(&self, is_mentioned: bool) -> bool {
        match &self.group_policy {
            None => true,
            Some(GroupPolicy::Open) => true,
            Some(GroupPolicy::Mention) => is_mentioned,
            Some(GroupPolicy::Disabled) => false,
        }
    }

    /// Check DM authorization against the per-channel policy.
    pub fn authorize_dm(&self, user_id: &str) -> DmAuthResult {
        match &self.dm_policy {
            None => DmAuthResult::UseGlobalAllowlist,
            Some(DmPolicy::Open) => DmAuthResult::Allowed,
            Some(DmPolicy::Pairing) => DmAuthResult::UseGlobalAllowlist,
            Some(DmPolicy::Allowlist) => {
                if self.channel_allowlist.contains(user_id) {
                    DmAuthResult::Allowed
                } else {
                    DmAuthResult::Blocked
                }
            }
        }
    }
}

/// Shared DM auth check that encapsulates the full auth flow.
///
/// Returns:
/// - `Ok(None)` - user is authorized, proceed with message processing
/// - `Ok(Some(welcome))` - user was just paired, return welcome message
/// - `Err("__blocked__")` - user is blocked
pub fn check_dm_auth(
    policy: &ChannelPolicy,
    allowlist: &mut Allowlist,
    pairing: &Mutex<PairingManager>,
    user_id: &str,
    user_name: &str,
    text: &str,
    label: &str,
) -> Result<Option<String>, String> {
    // Check per-channel policy first
    match policy.authorize_dm(user_id) {
        DmAuthResult::Allowed => return Ok(None),
        DmAuthResult::Blocked => {
            warn!("{label}: blocked user {user_name} ({user_id}) by channel allowlist");
            return Err("__blocked__".to_string());
        }
        DmAuthResult::UseGlobalAllowlist => {
            // Fall through to global allowlist logic below
        }
    }

    // Owner auto-claim (first user to message becomes owner)
    if allowlist.needs_owner() {
        allowlist.claim_owner(user_id);
        info!("{label}: auto-paired owner {user_name} ({user_id})");
        let welcome = if user_name.is_empty() {
            "Welcome! You are now the owner of this OpenCrust bot.\n\n\
             Send /pair to generate a code for adding other users.\n\
             Send /help for available commands."
                .to_string()
        } else {
            format!(
                "Welcome, {user_name}! You are now the owner of this OpenCrust bot.\n\n\
                 Use /pair to generate a code for adding other users.\n\
                 Use /help for available commands."
            )
        };
        return Ok(Some(welcome));
    }

    // Check global allowlist
    if allowlist.is_allowed(user_id) {
        return Ok(None);
    }

    // Try pairing code (6-digit number)
    let trimmed = text.trim();
    if trimmed.len() == 6 && trimmed.chars().all(|c| c.is_ascii_digit()) {
        let claimed = pairing.lock().unwrap().claim(trimmed, user_id);
        if claimed.is_some() {
            allowlist.add(user_id);
            info!("{label}: paired user {user_name} ({user_id}) via code");
            let welcome = if user_name.is_empty() {
                "Welcome! You now have access to this bot.".to_string()
            } else {
                format!("Welcome, {user_name}! You now have access to this bot.")
            };
            return Ok(Some(welcome));
        }
    }

    // Not authorized
    warn!("{label}: unauthorized user {user_name} ({user_id})");
    Err("__blocked__".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn from_settings_parses_all_variants() {
        let mut settings = std::collections::HashMap::new();
        settings.insert(
            "dm_policy".to_string(),
            serde_json::Value::String("open".to_string()),
        );
        settings.insert(
            "group_policy".to_string(),
            serde_json::Value::String("mention".to_string()),
        );
        settings.insert(
            "allowlist".to_string(),
            serde_json::json!(["user1", "user2"]),
        );

        let policy = ChannelPolicy::from_settings(&settings);
        assert_eq!(policy.dm_policy, Some(DmPolicy::Open));
        assert_eq!(policy.group_policy, Some(GroupPolicy::Mention));
        assert!(policy.channel_allowlist.contains("user1"));
        assert!(policy.channel_allowlist.contains("user2"));
    }

    #[test]
    fn from_settings_pairing_and_disabled() {
        let mut settings = std::collections::HashMap::new();
        settings.insert(
            "dm_policy".to_string(),
            serde_json::Value::String("pairing".to_string()),
        );
        settings.insert(
            "group_policy".to_string(),
            serde_json::Value::String("disabled".to_string()),
        );

        let policy = ChannelPolicy::from_settings(&settings);
        assert_eq!(policy.dm_policy, Some(DmPolicy::Pairing));
        assert_eq!(policy.group_policy, Some(GroupPolicy::Disabled));
    }

    #[test]
    fn from_settings_allowlist_dm_policy() {
        let mut settings = std::collections::HashMap::new();
        settings.insert(
            "dm_policy".to_string(),
            serde_json::Value::String("allowlist".to_string()),
        );

        let policy = ChannelPolicy::from_settings(&settings);
        assert_eq!(policy.dm_policy, Some(DmPolicy::Allowlist));
    }

    #[test]
    fn from_settings_unknown_values_default_to_none() {
        let mut settings = std::collections::HashMap::new();
        settings.insert(
            "dm_policy".to_string(),
            serde_json::Value::String("foobar".to_string()),
        );
        settings.insert(
            "group_policy".to_string(),
            serde_json::Value::String("baz".to_string()),
        );

        let policy = ChannelPolicy::from_settings(&settings);
        assert_eq!(policy.dm_policy, None);
        assert_eq!(policy.group_policy, None);
    }

    #[test]
    fn from_settings_missing_fields() {
        let settings = std::collections::HashMap::new();
        let policy = ChannelPolicy::from_settings(&settings);
        assert_eq!(policy.dm_policy, None);
        assert_eq!(policy.group_policy, None);
        assert!(policy.channel_allowlist.is_empty());
    }

    #[test]
    fn should_process_group_variants() {
        // None -> always process
        let policy = ChannelPolicy::default();
        assert!(policy.should_process_group(false));
        assert!(policy.should_process_group(true));

        // Open -> always process
        let policy = ChannelPolicy {
            group_policy: Some(GroupPolicy::Open),
            ..Default::default()
        };
        assert!(policy.should_process_group(false));
        assert!(policy.should_process_group(true));

        // Mention -> only when mentioned
        let policy = ChannelPolicy {
            group_policy: Some(GroupPolicy::Mention),
            ..Default::default()
        };
        assert!(!policy.should_process_group(false));
        assert!(policy.should_process_group(true));

        // Disabled -> never
        let policy = ChannelPolicy {
            group_policy: Some(GroupPolicy::Disabled),
            ..Default::default()
        };
        assert!(!policy.should_process_group(false));
        assert!(!policy.should_process_group(true));
    }

    #[test]
    fn authorize_dm_variants() {
        // None -> UseGlobalAllowlist
        let policy = ChannelPolicy::default();
        assert_eq!(
            policy.authorize_dm("anyone"),
            DmAuthResult::UseGlobalAllowlist
        );

        // Open -> always Allowed
        let policy = ChannelPolicy {
            dm_policy: Some(DmPolicy::Open),
            ..Default::default()
        };
        assert_eq!(policy.authorize_dm("anyone"), DmAuthResult::Allowed);

        // Pairing -> UseGlobalAllowlist (falls through to global)
        let policy = ChannelPolicy {
            dm_policy: Some(DmPolicy::Pairing),
            ..Default::default()
        };
        assert_eq!(
            policy.authorize_dm("anyone"),
            DmAuthResult::UseGlobalAllowlist
        );

        // Allowlist -> check channel_allowlist
        let mut channel_list = HashSet::new();
        channel_list.insert("user1".to_string());
        let policy = ChannelPolicy {
            dm_policy: Some(DmPolicy::Allowlist),
            channel_allowlist: channel_list,
            ..Default::default()
        };
        assert_eq!(policy.authorize_dm("user1"), DmAuthResult::Allowed);
        assert_eq!(policy.authorize_dm("user2"), DmAuthResult::Blocked);
    }

    #[test]
    fn check_dm_auth_open_skips_everything() {
        let policy = ChannelPolicy {
            dm_policy: Some(DmPolicy::Open),
            ..Default::default()
        };
        let mut allowlist = Allowlist::restricted(Vec::<String>::new());
        let pairing = Mutex::new(PairingManager::new(Duration::from_secs(300)));

        let result = check_dm_auth(
            &policy,
            &mut allowlist,
            &pairing,
            "user1",
            "Alice",
            "hi",
            "test",
        );
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn check_dm_auth_first_user_auto_claim() {
        let policy = ChannelPolicy::default();
        let mut allowlist = Allowlist::restricted(Vec::<String>::new());
        let pairing = Mutex::new(PairingManager::new(Duration::from_secs(300)));

        let result = check_dm_auth(
            &policy,
            &mut allowlist,
            &pairing,
            "user1",
            "Alice",
            "hi",
            "test",
        );
        assert!(result.is_ok());
        assert!(result.unwrap().is_some()); // Welcome message
        assert!(allowlist.is_owner("user1"));
    }

    #[test]
    fn check_dm_auth_pairing_code_claim() {
        let policy = ChannelPolicy::default();
        // Create allowlist with an existing owner so needs_owner() is false
        let mut allowlist = Allowlist::restricted(Vec::<String>::new());
        allowlist.claim_owner("owner1");
        let pairing = Mutex::new(PairingManager::new(Duration::from_secs(300)));
        let code = pairing.lock().unwrap().generate("test-channel");

        let result = check_dm_auth(
            &policy,
            &mut allowlist,
            &pairing,
            "user2",
            "Bob",
            &code,
            "test",
        );
        assert!(result.is_ok());
        let welcome = result.unwrap();
        assert!(welcome.is_some());
        assert!(welcome.unwrap().contains("Welcome, Bob"));
        assert!(allowlist.is_allowed("user2"));
    }

    #[test]
    fn check_dm_auth_blocked_user() {
        let policy = ChannelPolicy::default();
        let mut allowlist = Allowlist::restricted(Vec::<String>::new());
        allowlist.claim_owner("owner1");
        let pairing = Mutex::new(PairingManager::new(Duration::from_secs(300)));

        let result = check_dm_auth(
            &policy,
            &mut allowlist,
            &pairing,
            "stranger",
            "Eve",
            "hello",
            "test",
        );
        assert_eq!(result, Err("__blocked__".to_string()));
    }

    #[test]
    fn check_dm_auth_allowlist_only() {
        let mut channel_list = HashSet::new();
        channel_list.insert("vip".to_string());
        let policy = ChannelPolicy {
            dm_policy: Some(DmPolicy::Allowlist),
            channel_allowlist: channel_list,
            ..Default::default()
        };
        let mut allowlist = Allowlist::restricted(Vec::<String>::new());
        let pairing = Mutex::new(PairingManager::new(Duration::from_secs(300)));

        // VIP user passes
        let result = check_dm_auth(
            &policy,
            &mut allowlist,
            &pairing,
            "vip",
            "VIP",
            "hi",
            "test",
        );
        assert_eq!(result, Ok(None));

        // Non-VIP blocked
        let result = check_dm_auth(
            &policy,
            &mut allowlist,
            &pairing,
            "nobody",
            "Nobody",
            "hi",
            "test",
        );
        assert_eq!(result, Err("__blocked__".to_string()));
    }

    #[test]
    fn check_dm_auth_allowed_user_passes() {
        let policy = ChannelPolicy::default();
        let mut allowlist = Allowlist::restricted(vec!["user1".to_string()]);
        allowlist.claim_owner("user1");
        let pairing = Mutex::new(PairingManager::new(Duration::from_secs(300)));

        let result = check_dm_auth(
            &policy,
            &mut allowlist,
            &pairing,
            "user1",
            "Alice",
            "hello",
            "test",
        );
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn check_dm_auth_empty_username_welcome() {
        let policy = ChannelPolicy::default();
        let mut allowlist = Allowlist::restricted(Vec::<String>::new());
        let pairing = Mutex::new(PairingManager::new(Duration::from_secs(300)));

        let result = check_dm_auth(&policy, &mut allowlist, &pairing, "user1", "", "hi", "test");
        let welcome = result.unwrap().unwrap();
        assert!(welcome.starts_with("Welcome!"));
        assert!(!welcome.contains("Welcome, "));
    }

    #[test]
    fn default_policy_is_backwards_compatible() {
        let policy = ChannelPolicy::default();
        assert_eq!(policy.dm_policy, None);
        assert_eq!(policy.group_policy, None);
        assert!(policy.channel_allowlist.is_empty());
        // None dm_policy -> UseGlobalAllowlist
        assert_eq!(
            policy.authorize_dm("anyone"),
            DmAuthResult::UseGlobalAllowlist
        );
        // None group_policy -> process all
        assert!(policy.should_process_group(false));
    }
}
