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
