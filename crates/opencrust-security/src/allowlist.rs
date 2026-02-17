use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Persisted allowlist data.
#[derive(Debug, Serialize, Deserialize)]
struct AllowlistData {
    mode: String,
    owner: Option<String>,
    users: Vec<String>,
}

/// Manages which users are allowed to interact with the assistant per channel.
pub struct Allowlist {
    allowed_users: HashSet<String>,
    owner: Option<String>,
    mode: AllowlistMode,
    path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
            owner: None,
            mode: AllowlistMode::Open,
            path: None,
        }
    }

    pub fn restricted(users: impl IntoIterator<Item = String>) -> Self {
        Self {
            allowed_users: users.into_iter().collect(),
            owner: None,
            mode: AllowlistMode::Restricted,
            path: None,
        }
    }

    /// Load an allowlist from disk, or create a new restricted one if the file doesn't exist.
    pub fn load_or_create(path: &Path) -> Self {
        if path.is_file() {
            match std::fs::read_to_string(path) {
                Ok(contents) => match serde_json::from_str::<AllowlistData>(&contents) {
                    Ok(data) => {
                        let mode = if data.mode == "open" {
                            AllowlistMode::Open
                        } else {
                            AllowlistMode::Restricted
                        };
                        info!(
                            "loaded allowlist from {} ({} users, owner={:?})",
                            path.display(),
                            data.users.len(),
                            data.owner,
                        );
                        return Self {
                            allowed_users: data.users.into_iter().collect(),
                            owner: data.owner,
                            mode,
                            path: Some(path.to_path_buf()),
                        };
                    }
                    Err(e) => warn!("invalid allowlist file, creating new: {e}"),
                },
                Err(e) => warn!("failed to read allowlist file, creating new: {e}"),
            }
        }

        info!("creating new allowlist at {}", path.display());
        let mut list = Self::restricted(Vec::<String>::new());
        list.path = Some(path.to_path_buf());
        list
    }

    pub fn is_allowed(&self, user_id: &str) -> bool {
        match self.mode {
            AllowlistMode::Open => true,
            AllowlistMode::Restricted => self.allowed_users.contains(user_id),
        }
    }

    /// Returns true if no owner has been set yet (first user should auto-claim).
    pub fn needs_owner(&self) -> bool {
        self.mode == AllowlistMode::Restricted && self.owner.is_none()
    }

    /// Set the owner and add them to the allowlist. Returns false if owner already set.
    pub fn claim_owner(&mut self, user_id: impl Into<String>) -> bool {
        if self.owner.is_some() {
            return false;
        }
        let uid = user_id.into();
        self.owner = Some(uid.clone());
        self.allowed_users.insert(uid);
        self.save();
        true
    }

    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    pub fn is_owner(&self, user_id: &str) -> bool {
        self.owner.as_deref() == Some(user_id)
    }

    pub fn add(&mut self, user_id: impl Into<String>) {
        self.allowed_users.insert(user_id.into());
        self.save();
    }

    pub fn remove(&mut self, user_id: &str) -> bool {
        let removed = self.allowed_users.remove(user_id);
        if removed {
            self.save();
        }
        removed
    }

    pub fn list_users(&self) -> Vec<&str> {
        self.allowed_users.iter().map(|s| s.as_str()).collect()
    }

    pub fn mode(&self) -> &AllowlistMode {
        &self.mode
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };

        let data = AllowlistData {
            mode: match self.mode {
                AllowlistMode::Open => "open".to_string(),
                AllowlistMode::Restricted => "restricted".to_string(),
            },
            owner: self.owner.clone(),
            users: self.allowed_users.iter().cloned().collect(),
        };

        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!("failed to create allowlist directory: {e}");
                return;
            }
        }

        match serde_json::to_string_pretty(&data) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    warn!("failed to write allowlist: {e}");
                }
            }
            Err(e) => warn!("failed to serialize allowlist: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn first_user_claims_ownership() {
        let mut allowlist = Allowlist::restricted(Vec::<String>::new());
        assert!(allowlist.needs_owner());

        assert!(allowlist.claim_owner("user-1"));
        assert!(!allowlist.needs_owner());
        assert!(allowlist.is_owner("user-1"));
        assert!(allowlist.is_allowed("user-1"));

        // Second claim fails
        assert!(!allowlist.claim_owner("user-2"));
        assert!(!allowlist.is_owner("user-2"));
    }

    #[test]
    fn persistence_round_trip() {
        let dir =
            std::env::temp_dir().join(format!("opencrust-allowlist-test-{}", std::process::id()));
        let path = dir.join("allowlist.json");

        {
            let mut list = Allowlist::load_or_create(&path);
            list.claim_owner("owner-1");
            list.add("friend-1");
        }

        let list = Allowlist::load_or_create(&path);
        assert!(list.is_owner("owner-1"));
        assert!(list.is_allowed("owner-1"));
        assert!(list.is_allowed("friend-1"));
        assert!(!list.is_allowed("stranger"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
