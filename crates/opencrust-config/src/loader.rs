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
