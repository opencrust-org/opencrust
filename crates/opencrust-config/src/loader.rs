use std::path::{Path, PathBuf};

use opencrust_common::{Error, Result};
use tracing::info;

use crate::model::AppConfig;

pub struct ConfigLoader {
    config_dir: PathBuf,
}

impl ConfigLoader {
    pub fn new() -> Result<Self> {
        let config_dir = dirs::home_dir()
            .ok_or_else(|| Error::Config("could not determine home directory".into()))?
            .join(".opencrust");
        Ok(Self { config_dir })
    }

    pub fn with_dir(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            config_dir: config_dir.into(),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
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

    pub fn ensure_dirs(&self) -> Result<()> {
        let dirs = [
            self.config_dir.clone(),
            self.config_dir.join("sessions"),
            self.config_dir.join("credentials"),
            self.config_dir.join("plugins"),
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
        assert_eq!(config.gateway.port, 3000);
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
    fn ensure_dirs_creates_expected_subdirectories() {
        let dir = temp_dir("ensure-dirs");
        let loader = ConfigLoader::with_dir(&dir);

        loader.ensure_dirs().expect("ensure_dirs should succeed");

        assert!(dir.exists());
        assert!(dir.join("sessions").exists());
        assert!(dir.join("credentials").exists());
        assert!(dir.join("plugins").exists());
        assert!(dir.join("data").exists());

        let _ = fs::remove_dir_all(dir);
    }
}
