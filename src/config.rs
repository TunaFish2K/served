use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CONFIG_FILE: &str = ".served.json";
pub const ENV_FILE: &str = ".env";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("service directory does not exist: {0}")]
    MissingDirectory(PathBuf),
    #[error("service directory is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("missing {CONFIG_FILE} in {0}")]
    MissingConfig(PathBuf),
    #[error("invalid service name {0:?}; use letters, digits, '.', '_' or '-'")]
    InvalidName(String),
    #[error("service command must not be empty")]
    EmptyCommand,
    #[error("invalid .env key {0:?}")]
    InvalidEnvKey(String),
    #[error("I/O error while reading service configuration: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON in {CONFIG_FILE}: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid dotenv data: {0}")]
    Dotenv(#[from] dotenvy::Error),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RestartPolicy {
    #[serde(rename = "never")]
    #[default]
    Never,
    #[serde(rename = "on-failure")]
    OnFailure,
    #[serde(rename = "always")]
    Always,
}

impl RestartPolicy {
    pub fn should_restart(self, success: bool) -> bool {
        match self {
            Self::Never => false,
            Self::OnFailure => !success,
            Self::Always => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub name: String,
    pub command: String,
    #[serde(default = "default_tty")]
    pub tty: bool,
    #[serde(rename = "syncRowsCols", default = "default_sync_rows_cols")]
    pub sync_rows_cols: bool,
    #[serde(default)]
    pub restart: RestartPolicy,
    #[serde(default)]
    pub persist_logs: bool,
}

fn default_tty() -> bool {
    true
}

fn default_sync_rows_cols() -> bool {
    true
}

impl ServiceConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.name.is_empty()
            || self.name == "."
            || self.name == ".."
            || self
                .name
                .chars()
                .any(|character| !character.is_ascii_alphanumeric() && !"._-".contains(character))
        {
            return Err(ConfigError::InvalidName(self.name.clone()));
        }
        if self.command.trim().is_empty() {
            return Err(ConfigError::EmptyCommand);
        }
        Ok(())
    }

    pub fn template(directory: &Path) -> Self {
        let name = directory
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("service")
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || "._-".contains(character) {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        Self {
            name,
            command: "./run.sh".to_owned(),
            tty: true,
            sync_rows_cols: true,
            restart: RestartPolicy::Never,
            persist_logs: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedService {
    pub directory: PathBuf,
    pub config: ServiceConfig,
    pub environment: BTreeMap<String, String>,
}

pub fn load_service(
    directory: impl AsRef<Path>,
    manager_environment: &BTreeMap<String, String>,
) -> Result<LoadedService, ConfigError> {
    let directory = directory.as_ref();
    if !directory.exists() {
        return Err(ConfigError::MissingDirectory(directory.to_path_buf()));
    }
    if !directory.is_dir() {
        return Err(ConfigError::NotDirectory(directory.to_path_buf()));
    }
    let directory = fs::canonicalize(directory)?;
    let config_path = directory.join(CONFIG_FILE);
    if !config_path.is_file() {
        return Err(ConfigError::MissingConfig(directory));
    }
    let config: ServiceConfig = serde_json::from_slice(&fs::read(config_path)?)?;
    config.validate()?;

    let mut environment = manager_environment.clone();
    let env_path = directory.join(ENV_FILE);
    if env_path.exists() {
        for item in dotenvy::from_path_iter(env_path)? {
            let (key, value) = item?;
            if key.is_empty() || key.contains('=') || key.contains('\0') {
                return Err(ConfigError::InvalidEnvKey(key));
            }
            environment.insert(key, value);
        }
    }

    Ok(LoadedService {
        directory,
        config,
        environment,
    })
}

pub fn manager_environment() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

pub fn write_template(directory: &Path) -> Result<(), ConfigError> {
    fs::create_dir_all(directory)?;
    let config_path = directory.join(CONFIG_FILE);
    if !config_path.exists() {
        let config = ServiceConfig::template(directory);
        let data = serde_json::to_vec_pretty(&config)?;
        fs::write(config_path, format!("{}\n", String::from_utf8_lossy(&data)))?;
    }
    let env_path = directory.join(ENV_FILE);
    if !env_path.exists() {
        fs::write(env_path, "")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn loads_direct_json_and_dotenv_overlay() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join(CONFIG_FILE),
            r#"{"name":"api","command":"echo ok"}"#,
        )
        .expect("config");
        fs::write(
            directory.path().join(ENV_FILE),
            "# a comment\nPORT=8080\nQUOTED=\"hello world\"\n",
        )
        .expect("env");
        let mut base = BTreeMap::new();
        base.insert("PORT".to_owned(), "old".to_owned());

        let service = load_service(directory.path(), &base).expect("load");
        assert!(service.config.tty);
        assert!(service.config.sync_rows_cols);
        assert!(!service.config.persist_logs);
        assert_eq!(service.environment.get("PORT"), Some(&"8080".to_owned()));
        assert_eq!(
            service.environment.get("QUOTED"),
            Some(&"hello world".to_owned())
        );
    }

    #[test]
    fn rejects_unknown_json_fields() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join(CONFIG_FILE),
            r#"{"name":"api","command":"echo ok","cwd":"/tmp"}"#,
        )
        .expect("config");
        let error = load_service(directory.path(), &BTreeMap::new()).expect_err("must reject");
        assert!(error.to_string().contains("invalid JSON"));
    }

    #[test]
    fn restart_policies_are_distinct() {
        assert!(!RestartPolicy::Never.should_restart(false));
        assert!(RestartPolicy::OnFailure.should_restart(false));
        assert!(!RestartPolicy::OnFailure.should_restart(true));
        assert!(RestartPolicy::Always.should_restart(true));
    }

    #[test]
    fn persistent_logs_round_trip_from_json() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join(CONFIG_FILE),
            r#"{"name":"api","command":"echo ok","persist_logs":true}"#,
        )
        .expect("config");
        let service = load_service(directory.path(), &BTreeMap::new()).expect("load");
        assert!(service.config.persist_logs);
    }

    #[test]
    fn sync_rows_cols_can_be_disabled_with_camel_case_json_key() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join(CONFIG_FILE),
            r#"{"name":"api","command":"echo ok","syncRowsCols":false}"#,
        )
        .expect("config");
        let service = load_service(directory.path(), &BTreeMap::new()).expect("load");
        assert!(!service.config.sync_rows_cols);
        let encoded = serde_json::to_value(&service.config).expect("serialize config");
        assert_eq!(
            encoded.get("syncRowsCols"),
            Some(&serde_json::Value::Bool(false))
        );
    }
}
