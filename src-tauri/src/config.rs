use std::{fs, path::PathBuf};

use tauri::{AppHandle, Manager};
use thiserror::Error;

use crate::models::AppConfig;

const CONFIG_FILE: &str = "config.json";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not resolve app config directory: {0}")]
    Path(String),
    #[error("could not read app config: {0}")]
    Read(String),
    #[error("could not write app config: {0}")]
    Write(String),
    #[error("invalid app config: {0}")]
    Invalid(String),
}

pub fn load(app: &AppHandle) -> Result<AppConfig, ConfigError> {
    let path = config_path(app)?;
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let bytes = fs::read(&path).map_err(|e| ConfigError::Read(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| ConfigError::Invalid(e.to_string()))
}

pub fn save(app: &AppHandle, config: &AppConfig) -> Result<(), ConfigError> {
    validate(config)?;
    let path = config_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| ConfigError::Write(e.to_string()))?;
    }
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| ConfigError::Invalid(e.to_string()))?;
    fs::write(path, bytes).map_err(|e| ConfigError::Write(e.to_string()))
}

fn config_path(app: &AppHandle) -> Result<PathBuf, ConfigError> {
    app.path()
        .app_config_dir()
        .map(|dir| dir.join(CONFIG_FILE))
        .map_err(|e| ConfigError::Path(e.to_string()))
}

fn validate(config: &AppConfig) -> Result<(), ConfigError> {
    if config.poll_interval_seconds < 30 {
        return Err(ConfigError::Invalid(
            "poll interval must be at least 30 seconds".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        assert!(validate(&AppConfig::default()).is_ok());
    }

    #[test]
    fn rejects_too_frequent_polling() {
        let mut config = AppConfig::default();
        config.poll_interval_seconds = 5;
        assert!(validate(&config).is_err());
    }
}
