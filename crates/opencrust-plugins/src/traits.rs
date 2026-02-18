use async_trait::async_trait;
use opencrust_common::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Represents the capabilities required by a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Capability {
    /// Filesystem access scoped to explicit host paths.
    Filesystem {
        read_paths: Vec<String>,
        write_paths: Vec<String>,
    },
    /// Network access. List of allowed domains.
    Network(Vec<String>),
    /// Environment variables. List of allowed variable names.
    EnvVars(Vec<String>),
}

/// Input passed to a plugin execution.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginInput {
    /// Command line arguments.
    pub args: Vec<String>,
    /// Environment variables to set.
    pub env: HashMap<String, String>,
    /// Standard input data.
    pub stdin: Vec<u8>,
}

/// Output returned from a plugin execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginOutput {
    /// Standard output data.
    pub stdout: Vec<u8>,
    /// Standard error data.
    pub stderr: Vec<u8>,
    /// Exit status code.
    pub status: i32,
}

/// Trait that all plugins must implement.
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Human-readable name.
    fn name(&self) -> &str;

    /// Plugin description.
    fn description(&self) -> &str;

    /// List of capabilities required by the plugin.
    fn capabilities(&self) -> Vec<Capability>;

    /// Execute the plugin with the given input.
    async fn execute(&self, input: PluginInput) -> Result<PluginOutput>;
}
