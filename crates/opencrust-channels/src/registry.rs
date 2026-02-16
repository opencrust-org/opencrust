use std::collections::HashMap;

use opencrust_common::Result;
use tracing::info;

use crate::traits::Channel;

/// Central registry of all available messaging channels.
pub struct ChannelRegistry {
    channels: HashMap<String, Box<dyn Channel>>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
        }
    }

    pub fn register(&mut self, channel: Box<dyn Channel>) {
        let channel_type = channel.channel_type().to_string();
        info!("registered channel: {}", channel_type);
        self.channels.insert(channel_type, channel);
    }

    pub fn get(&self, channel_type: &str) -> Option<&dyn Channel> {
        self.channels.get(channel_type).map(|c| c.as_ref())
    }

    pub fn get_mut(&mut self, channel_type: &str) -> Option<&mut Box<dyn Channel>> {
        self.channels.get_mut(channel_type)
    }

    pub fn list(&self) -> Vec<&str> {
        self.channels.keys().map(|k| k.as_str()).collect()
    }

    pub async fn connect_all(&mut self) -> Result<()> {
        for (name, channel) in &mut self.channels {
            info!("connecting channel: {}", name);
            channel.connect().await?;
        }
        Ok(())
    }

    pub async fn disconnect_all(&mut self) -> Result<()> {
        for (name, channel) in &mut self.channels {
            info!("disconnecting channel: {}", name);
            channel.disconnect().await?;
        }
        Ok(())
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}
