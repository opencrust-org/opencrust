use async_trait::async_trait;
use opencrust_common::Result;

/// Trait that all plugins (extensions) must implement.
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Unique plugin identifier.
    fn id(&self) -> &str;

    /// Human-readable name.
    fn name(&self) -> &str;

    /// Plugin version string.
    fn version(&self) -> &str;

    /// Called when the plugin is loaded.
    async fn on_load(&mut self, ctx: &PluginContext) -> Result<()>;

    /// Called when the plugin is unloaded.
    async fn on_unload(&mut self) -> Result<()>;
}

/// Context provided to plugins at load time.
pub struct PluginContext {
    pub data_dir: std::path::PathBuf,
    pub config: serde_json::Value,
}
