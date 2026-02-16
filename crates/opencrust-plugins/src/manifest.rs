use serde::{Deserialize, Serialize};

/// Describes a plugin's metadata and requirements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub license: Option<String>,

    /// Minimum OpenCrust version required.
    pub min_version: Option<String>,

    /// Plugin type determines how it is loaded.
    #[serde(default)]
    pub plugin_type: PluginType,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginType {
    /// WebAssembly plugin (sandboxed).
    #[default]
    Wasm,
    /// Native shared library (unsafe, requires trust).
    Native,
}
