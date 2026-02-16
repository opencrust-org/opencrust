use std::collections::HashSet;

/// Manages which users are allowed to interact with the assistant per channel.
pub struct Allowlist {
    allowed_users: HashSet<String>,
    mode: AllowlistMode,
}

pub enum AllowlistMode {
    /// Allow all users (no restrictions).
    Open,
    /// Only allow users in the allowlist.
    Restricted,
}

impl Allowlist {
    pub fn open() -> Self {
        Self {
            allowed_users: HashSet::new(),
            mode: AllowlistMode::Open,
        }
    }

    pub fn restricted(users: impl IntoIterator<Item = String>) -> Self {
        Self {
            allowed_users: users.into_iter().collect(),
            mode: AllowlistMode::Restricted,
        }
    }

    pub fn is_allowed(&self, user_id: &str) -> bool {
        match self.mode {
            AllowlistMode::Open => true,
            AllowlistMode::Restricted => self.allowed_users.contains(user_id),
        }
    }

    pub fn add(&mut self, user_id: impl Into<String>) {
        self.allowed_users.insert(user_id.into());
    }

    pub fn remove(&mut self, user_id: &str) -> bool {
        self.allowed_users.remove(user_id)
    }
}

#[cfg(test)]
mod tests {
    use super::Allowlist;

    #[test]
    fn open_mode_allows_any_user() {
        let allowlist = Allowlist::open();
        assert!(allowlist.is_allowed("user-1"));
        assert!(allowlist.is_allowed("anyone"));
    }

    #[test]
    fn restricted_mode_allows_only_known_users() {
        let allowlist = Allowlist::restricted(vec!["alice".to_string(), "bob".to_string()]);
        assert!(allowlist.is_allowed("alice"));
        assert!(allowlist.is_allowed("bob"));
        assert!(!allowlist.is_allowed("charlie"));
    }

    #[test]
    fn add_and_remove_updates_membership() {
        let mut allowlist = Allowlist::restricted(Vec::<String>::new());
        assert!(!allowlist.is_allowed("new-user"));

        allowlist.add("new-user");
        assert!(allowlist.is_allowed("new-user"));

        assert!(allowlist.remove("new-user"));
        assert!(!allowlist.is_allowed("new-user"));
        assert!(!allowlist.remove("new-user"));
    }
}
