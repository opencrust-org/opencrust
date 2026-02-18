use crate::manifest::PluginManifest;
use crate::runtime::WasmRuntime;
use crate::traits::Plugin;
use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use tracing::{error, info, warn};

/// Discovers and loads plugins from the plugins directory.
#[derive(Clone)]
pub struct PluginLoader {
    plugins_dir: PathBuf,
}

impl PluginLoader {
    pub fn new(plugins_dir: impl Into<PathBuf>) -> Self {
        Self {
            plugins_dir: plugins_dir.into(),
        }
    }

    /// Scan the plugins directory and return all valid plugins.
    pub fn discover(&self) -> Result<Vec<Arc<dyn Plugin>>> {
        if !self.plugins_dir.exists() {
            return Ok(Vec::new());
        }

        let mut plugins = Vec::new();

        let entries = std::fs::read_dir(&self.plugins_dir).context("reading plugins dir")?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                match self.load_plugin(&path) {
                    Ok(plugin) => {
                        info!(
                            "loaded plugin: {} ({})",
                            plugin.name(),
                            plugin.description()
                        );
                        plugins.push(plugin);
                    }
                    Err(e) => {
                        // Only warn if it looks like a plugin (has plugin.toml)
                        if path.join("plugin.toml").exists() {
                            warn!("failed to load plugin at {}: {}", path.display(), e);
                        }
                    }
                }
            }
        }

        Ok(plugins)
    }

    fn load_plugin(&self, plugin_dir: &Path) -> Result<Arc<dyn Plugin>> {
        let manifest_path = plugin_dir.join("plugin.toml");
        if !manifest_path.exists() {
            anyhow::bail!("missing plugin.toml");
        }

        let manifest = PluginManifest::from_file(&manifest_path)?;

        // Find WASM file.
        // Try <name>.wasm first, then plugin.wasm
        let wasm_path_named = plugin_dir.join(format!("{}.wasm", manifest.plugin.name));
        let wasm_path_generic = plugin_dir.join("plugin.wasm");

        let wasm_path = if wasm_path_named.exists() {
            wasm_path_named
        } else if wasm_path_generic.exists() {
            wasm_path_generic
        } else {
            anyhow::bail!(
                "WASM file not found (expected {}.wasm or plugin.wasm)",
                manifest.plugin.name
            );
        };

        let runtime = WasmRuntime::new(manifest, wasm_path)?;
        Ok(Arc::new(runtime))
    }

    /// Watch for changes in the plugins directory.
    /// Returns a Watcher and a Receiver. The receiver gets a message when a change is detected.
    pub fn watch(&self) -> Result<(RecommendedWatcher, tokio::sync::mpsc::Receiver<()>)> {
        let (tx, rx) = tokio::sync::mpsc::channel(1);

        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    if event.kind.is_modify() || event.kind.is_create() || event.kind.is_remove() {
                        let _ = tx.blocking_send(());
                    }
                }
                Err(e) => error!("watch error: {}", e),
            })?;

        if self.plugins_dir.exists() {
            watcher.watch(&self.plugins_dir, RecursiveMode::Recursive)?;
        } else {
            warn!(
                "plugins directory {} does not exist, watch may not work until restart",
                self.plugins_dir.display()
            );
        }

        Ok((watcher, rx))
    }
}

/// In-memory plugin registry with optional hot-reload watching.
pub struct PluginRegistry {
    loader: PluginLoader,
    plugins: Arc<RwLock<HashMap<String, Arc<dyn Plugin>>>>,
    watcher: Option<RecommendedWatcher>,
    reload_task: Option<tokio::task::JoinHandle<()>>,
}

impl PluginRegistry {
    pub fn new(loader: PluginLoader) -> Self {
        Self {
            loader,
            plugins: Arc::new(RwLock::new(HashMap::new())),
            watcher: None,
            reload_task: None,
        }
    }

    pub fn from_dir(plugins_dir: impl Into<PathBuf>) -> Self {
        Self::new(PluginLoader::new(plugins_dir))
    }

    /// Reload all plugins from disk, replacing the current registry contents.
    pub fn reload(&self) -> Result<usize> {
        let discovered = self.loader.discover()?;
        let mut map = HashMap::new();
        for plugin in discovered {
            map.insert(plugin.name().to_string(), plugin);
        }

        if let Ok(mut guard) = self.plugins.write() {
            *guard = map;
            Ok(guard.len())
        } else {
            Err(anyhow::anyhow!("plugin registry lock poisoned"))
        }
    }

    /// Start watching the plugins directory and hot-reload on changes.
    pub fn start_hot_reload(&mut self) -> Result<()> {
        if self.reload_task.is_some() {
            return Ok(());
        }

        let (watcher, mut rx) = self.loader.watch()?;
        self.watcher = Some(watcher);

        let loader = self.loader.clone();
        let plugins = Arc::clone(&self.plugins);
        let task = tokio::spawn(async move {
            while rx.recv().await.is_some() {
                match loader.discover() {
                    Ok(discovered) => {
                        let mut map = HashMap::new();
                        for plugin in discovered {
                            map.insert(plugin.name().to_string(), plugin);
                        }
                        if let Ok(mut guard) = plugins.write() {
                            let count = map.len();
                            *guard = map;
                            info!("hot-reloaded plugins ({count} installed)");
                        } else {
                            error!("failed to hot-reload plugins: registry lock poisoned");
                        }
                    }
                    Err(e) => warn!("failed to reload plugins after filesystem change: {e}"),
                }
            }
        });
        self.reload_task = Some(task);
        Ok(())
    }

    pub fn list(&self) -> Vec<Arc<dyn Plugin>> {
        self.plugins
            .read()
            .map(|guard| guard.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Plugin>> {
        self.plugins
            .read()
            .ok()
            .and_then(|guard| guard.get(name).cloned())
    }
}

impl Drop for PluginRegistry {
    fn drop(&mut self) {
        if let Some(task) = self.reload_task.take() {
            task.abort();
        }
    }
}
