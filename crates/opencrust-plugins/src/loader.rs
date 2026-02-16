use opencrust_common::{Error, Result};
use std::path::Path;
use tracing::info;

use crate::manifest::PluginManifest;

/// Discovers and loads plugins from the plugins directory.
pub struct PluginLoader {
    plugins_dir: std::path::PathBuf,
}

impl PluginLoader {
    pub fn new(plugins_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            plugins_dir: plugins_dir.into(),
        }
    }

    /// Scan the plugins directory and return all valid manifests.
    pub fn discover(&self) -> Result<Vec<PluginManifest>> {
        if !self.plugins_dir.exists() {
            return Ok(Vec::new());
        }

        let mut manifests = Vec::new();

        let entries = std::fs::read_dir(&self.plugins_dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                match self.load_manifest(&path) {
                    Ok(manifest) => {
                        info!("discovered plugin: {} v{}", manifest.name, manifest.version);
                        manifests.push(manifest);
                    }
                    Err(e) => {
                        tracing::warn!("skipping invalid plugin at {}: {}", path.display(), e);
                    }
                }
            }
        }

        Ok(manifests)
    }

    fn load_manifest(&self, plugin_dir: &Path) -> Result<PluginManifest> {
        let manifest_path = plugin_dir.join("manifest.json");
        if !manifest_path.exists() {
            return Err(Error::Plugin("missing manifest.json".into()));
        }

        let contents = std::fs::read_to_string(&manifest_path)?;
        serde_json::from_str(&contents).map_err(|e| Error::Plugin(format!("invalid manifest: {e}")))
    }
}
