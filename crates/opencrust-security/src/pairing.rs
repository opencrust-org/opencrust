use rand::Rng;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Manages pairing codes for device and channel authentication.
pub struct PairingManager {
    codes: HashMap<String, PairingCode>,
    code_ttl: Duration,
}

struct PairingCode {
    code: String,
    created_at: Instant,
    claimed_by: Option<String>,
}

impl PairingManager {
    pub fn new(code_ttl: Duration) -> Self {
        Self {
            codes: HashMap::new(),
            code_ttl,
        }
    }

    /// Generate a new 6-digit pairing code for a channel.
    pub fn generate(&mut self, channel_id: &str) -> String {
        let code: String = {
            let mut rng = rand::rng();
            (0..6).map(|_| rng.random_range(0..=9).to_string()).collect()
        };

        self.codes.insert(
            channel_id.to_string(),
            PairingCode {
                code: code.clone(),
                created_at: Instant::now(),
                claimed_by: None,
            },
        );

        code
    }

    /// Attempt to claim a pairing code. Returns the channel ID if valid.
    pub fn claim(&mut self, code: &str, user_id: &str) -> Option<String> {
        self.cleanup_expired();

        let entry = self
            .codes
            .iter_mut()
            .find(|(_, pc)| pc.code == code && pc.claimed_by.is_none());

        if let Some((channel_id, pairing_code)) = entry {
            pairing_code.claimed_by = Some(user_id.to_string());
            Some(channel_id.clone())
        } else {
            None
        }
    }

    fn cleanup_expired(&mut self) {
        self.codes
            .retain(|_, pc| pc.created_at.elapsed() < self.code_ttl);
    }
}
