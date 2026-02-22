use std::collections::HashMap;
use std::path::{Path, PathBuf};

use opencrust_common::{Error, Result};
use tracing::info;

use crate::model::{AppConfig, McpServerConfig};

pub struct ConfigLoader {
    config_dir: PathBuf,
}

impl ConfigLoader {
    pub fn new() -> Result<Self> {
        let config_dir = Self::default_config_dir();
        Ok(Self { config_dir })
    }

    pub fn default_config_dir() -> PathBuf {
        let home_config = dirs::home_dir().map(|h| h.join(".opencrust"));
        let xdg_config = dirs::config_dir().map(|c| c.join("opencrust"));

        match (xdg_config, home_config) {
            (Some(xdg), Some(home)) => {
                // If XDG exists, prefer it.
                if xdg.exists() {
                    xdg
                }
                // If Home exists (and XDG doesn't), use Home (migration/legacy case).
                else if home.exists() {
                    home
                }
                // If neither exists, prefer XDG for new installs.
                else {
                    xdg
                }
            }
            (Some(xdg), None) => xdg,
            (None, Some(home)) => home,
            (None, None) => PathBuf::from(".opencrust"),
        }
    }

    pub fn with_dir(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Returns true if a config file (YAML or TOML) exists on disk.
    pub fn config_file_exists(&self) -> bool {
        self.config_dir.join("config.yml").exists() || self.config_dir.join("config.toml").exists()
    }

    pub fn load(&self) -> Result<AppConfig> {
        let yaml_path = self.config_dir.join("config.yml");
        let toml_path = self.config_dir.join("config.toml");

        if yaml_path.exists() {
            info!("loading config from {}", yaml_path.display());
            let contents = std::fs::read_to_string(&yaml_path)?;
            serde_yaml::from_str(&contents)
                .map_err(|e| Error::Config(format!("failed to parse YAML config: {e}")))
        } else if toml_path.exists() {
            info!("loading config from {}", toml_path.display());
            let contents = std::fs::read_to_string(&toml_path)?;
            toml::from_str(&contents)
                .map_err(|e| Error::Config(format!("failed to parse TOML config: {e}")))
        } else {
            info!("no config file found, using defaults");
            Ok(AppConfig::default())
        }
    }

    /// Load MCP server configs from `~/.opencrust/mcp.json` (Claude Desktop compatible format).
    /// Returns an empty map if the file does not exist.
    pub fn load_mcp_json(&self) -> HashMap<String, McpServerConfig> {
        let mcp_path = self.config_dir.join("mcp.json");
        if !mcp_path.exists() {
            return HashMap::new();
        }

        let contents = match std::fs::read_to_string(&mcp_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("failed to read mcp.json: {e}");
                return HashMap::new();
            }
        };

        #[derive(serde::Deserialize)]
        struct McpJsonFile {
            #[serde(default, rename = "mcpServers")]
            mcp_servers: HashMap<String, McpServerConfig>,
        }

        match serde_json::from_str::<McpJsonFile>(&contents) {
            Ok(file) => {
                info!(
                    "loaded {} MCP server(s) from mcp.json",
                    file.mcp_servers.len()
                );
                file.mcp_servers
            }
            Err(e) => {
                tracing::warn!("failed to parse mcp.json: {e}");
                HashMap::new()
            }
        }
    }

    /// Merge MCP configs from mcp.json and config.yml. Config.yml entries win on conflict.
    pub fn merged_mcp_config(&self, config: &AppConfig) -> HashMap<String, McpServerConfig> {
        let mut merged = self.load_mcp_json();
        // config.yml wins on conflicts
        for (name, server_config) in &config.mcp {
            merged.insert(name.clone(), server_config.clone());
        }
        merged
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        let dirs = [
            self.config_dir.clone(),
            self.config_dir.join("sessions"),
            self.config_dir.join("credentials"),
            self.config_dir.join("plugins"),
            self.config_dir.join("skills"),
            self.config_dir.join("data"),
        ];

        for dir in &dirs {
            if !dir.exists() {
                std::fs::create_dir_all(dir)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ConfigLoader;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "opencrust-config-test-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ))
    }

    #[test]
    fn load_returns_default_when_no_config_exists() {
        let dir = temp_dir("default");
        fs::create_dir_all(&dir).expect("failed to create temp dir");

        let loader = ConfigLoader::with_dir(&dir);
        let config = loader.load().expect("load should succeed");

        assert_eq!(config.gateway.host, "127.0.0.1");
        assert_eq!(config.gateway.port, 3888);
        assert!(config.channels.is_empty());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn load_prefers_yaml_over_toml_when_both_exist() {
        let dir = temp_dir("yaml-precedence");
        fs::create_dir_all(&dir).expect("failed to create temp dir");

        fs::write(
            dir.join("config.yml"),
            "gateway:\n  host: \"0.0.0.0\"\n  port: 4001\n",
        )
        .expect("failed to write yaml config");
        fs::write(
            dir.join("config.toml"),
            "[gateway]\nhost = \"127.0.0.2\"\nport = 4999\n",
        )
        .expect("failed to write toml config");

        let loader = ConfigLoader::with_dir(&dir);
        let config = loader.load().expect("load should succeed");

        assert_eq!(config.gateway.host, "0.0.0.0");
        assert_eq!(config.gateway.port, 4001);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn load_reads_toml_when_yaml_missing() {
        let dir = temp_dir("toml");
        fs::create_dir_all(&dir).expect("failed to create temp dir");

        fs::write(
            dir.join("config.toml"),
            "[gateway]\nhost = \"127.0.0.2\"\nport = 4002\n",
        )
        .expect("failed to write toml config");

        let loader = ConfigLoader::with_dir(&dir);
        let config = loader.load().expect("load should succeed");

        assert_eq!(config.gateway.host, "127.0.0.2");
        assert_eq!(config.gateway.port, 4002);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn load_mcp_json_returns_empty_when_file_missing() {
        let dir = temp_dir("mcp-missing");
        fs::create_dir_all(&dir).expect("failed to create temp dir");

        let loader = ConfigLoader::with_dir(&dir);
        let mcp = loader.load_mcp_json();
        assert!(mcp.is_empty());

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn merged_mcp_config_prefers_config_yml_on_conflict() {
        let dir = temp_dir("mcp-merge");
        fs::create_dir_all(&dir).expect("failed to create temp dir");

        // Write an mcp.json with one server
        let mcp_json = r#"{
            "mcpServers": {
                "server1": {
                    "command": "from-json",
                    "transport": "stdio"
                },
                "server2": {
                    "command": "only-in-json",
                    "transport": "stdio"
                }
            }
        }"#;
        fs::write(dir.join("mcp.json"), mcp_json).expect("failed to write mcp.json");

        let loader = ConfigLoader::with_dir(&dir);

        // Build an AppConfig with server1 overridden via config.yml mcp section
        let mut config = crate::model::AppConfig::default();
        config.mcp.insert(
            "server1".to_string(),
            crate::model::McpServerConfig {
                command: "from-config-yml".to_string(),
                args: vec![],
                env: std::collections::HashMap::new(),
                transport: "stdio".to_string(),
                url: None,
                enabled: None,
                timeout: None,
            },
        );

        let merged = loader.merged_mcp_config(&config);

        // server1 should come from config.yml (wins)
        assert_eq!(merged.get("server1").unwrap().command, "from-config-yml");
        // server2 should still come from mcp.json
        assert_eq!(merged.get("server2").unwrap().command, "only-in-json");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ensure_dirs_creates_expected_subdirectories() {
        let dir = temp_dir("ensure-dirs");
        let loader = ConfigLoader::with_dir(&dir);

        loader.ensure_dirs().expect("ensure_dirs should succeed");

        assert!(dir.exists());
        assert!(dir.join("sessions").exists());
        assert!(dir.join("credentials").exists());
        assert!(dir.join("plugins").exists());
        assert!(dir.join("skills").exists());
        assert!(dir.join("data").exists());

        let _ = fs::remove_dir_all(dir);
    }
}
