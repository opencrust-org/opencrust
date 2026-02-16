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

#[cfg(test)]
mod tests {
    use super::PluginLoader;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "opencrust-plugin-loader-test-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ))
    }

    #[test]
    fn discover_returns_empty_when_plugins_dir_missing() {
        let dir = temp_dir("missing");
        let loader = PluginLoader::new(&dir);
        let manifests = loader.discover().expect("discover should not fail");
        assert!(manifests.is_empty());
    }

    #[test]
    fn discover_loads_valid_manifest() {
        let dir = temp_dir("valid");
        let plugin_dir = dir.join("my-plugin");
        fs::create_dir_all(&plugin_dir).expect("failed to create plugin dir");
        fs::write(
            plugin_dir.join("manifest.json"),
            r#"{
  "id": "my-plugin",
  "name": "My Plugin",
  "version": "0.1.0",
  "description": "test plugin"
}"#,
        )
        .expect("failed to write manifest");

        let loader = PluginLoader::new(&dir);
        let manifests = loader.discover().expect("discover should succeed");

        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].id, "my-plugin");
        assert_eq!(manifests[0].name, "My Plugin");
        assert_eq!(manifests[0].version, "0.1.0");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn discover_skips_invalid_plugin_directories() {
        let dir = temp_dir("skip-invalid");
        let valid_dir = dir.join("valid");
        let invalid_dir = dir.join("invalid");
        fs::create_dir_all(&valid_dir).expect("failed to create valid plugin dir");
        fs::create_dir_all(&invalid_dir).expect("failed to create invalid plugin dir");

        fs::write(
            valid_dir.join("manifest.json"),
            r#"{
  "id": "good",
  "name": "Good Plugin",
  "version": "1.2.3"
}"#,
        )
        .expect("failed to write valid manifest");

        let loader = PluginLoader::new(&dir);
        let manifests = loader.discover().expect("discover should succeed");

        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].id, "good");

        let _ = fs::remove_dir_all(dir);
    }
}
