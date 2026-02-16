use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionId(String);

#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct UserId(String);

#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChannelId(String);

macro_rules! impl_id_type {
    ($t:ty) => {
        impl $t {
            pub fn new() -> Self {
                Self(Uuid::new_v4().to_string())
            }

            pub fn from_str(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl Default for $t {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

impl_id_type!(SessionId);
impl_id_type!(UserId);
impl_id_type!(ChannelId);
