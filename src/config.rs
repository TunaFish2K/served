use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CONFIG_FILE: &str = ".served.json";
pub const ENV_FILE: &str = ".env.served";

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
    #[error("invalid environment key {0:?}")]
    InvalidEnvKey(String),
    #[error("I/O error while reading service configuration: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON5 in {CONFIG_FILE}: {0}")]
    Json5(#[from] json5::Error),
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
    #[serde(default)]
    pub env: BTreeMap<String, String>,
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
        for key in self.env.keys() {
            validate_env_key(key)?;
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
            env: BTreeMap::new(),
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
    let config: ServiceConfig = json5::from_str(&fs::read_to_string(config_path)?)?;
    config.validate()?;

    let mut environment = manager_environment.clone();
    let env_path = directory.join(ENV_FILE);
    if env_path.exists() {
        for item in dotenvy::from_path_iter(env_path)? {
            let (key, value) = item?;
            validate_env_key(&key)?;
            environment.insert(key, value);
        }
    }
    for (key, value) in &config.env {
        environment.insert(key.clone(), value.clone());
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
        fs::write(config_path, template_source(directory))?;
    }
    Ok(())
}

fn validate_env_key(key: &str) -> Result<(), ConfigError> {
    if key.is_empty() || key.contains('=') || key.contains('\0') {
        return Err(ConfigError::InvalidEnvKey(key.to_owned()));
    }
    Ok(())
}

fn template_source(directory: &Path) -> String {
    let config = ServiceConfig::template(directory);
    format!(
        r#"// served service configuration (JSON5)
//
// This file is read by the manager when the service is enabled or restarted.
// Existing files are never rewritten by `served edit`; keep your own comments.
{{
  // Globally unique service name. Use only letters, digits, '.', '_' and '-'.
  // Renaming an enabled service requires disabling and enabling it again.
  name: "{name}",

  // Shell script executed with `/bin/sh -c` from this service directory.
  // Multiple commands may be separated by real newlines or shell operators.
  command: "{command}",

  // Allocate a runner-owned PTY. When false, stdout/stderr use pipes and
  // attach is read-only; when true, one attach client may write to the PTY.
  tty: {tty},

  // When tty is true, attach keeps the PTY rows/columns in sync with the
  // attaching terminal. Changes apply to the running PTY; false keeps its size.
  // This setting has no effect for pipe services.
  syncRowsCols: {sync_rows_cols},

  // Restart policy after the process exits: 'never', 'on-failure', or 'always'.
  // 'on-failure' restarts only a non-zero exit; 'always' also restarts success.
  restart: "{restart}",

  // Keep complete raw output on disk under the served state directory when true.
  // When false, the manager keeps bounded in-memory history only.
  persist_logs: {persist_logs},

  // Literal environment values layered over the manager environment. These
  // values are not shell-expanded. A legacy .env.served file is read first
  // when present, so keys here take precedence over that compatibility source.
  env: {{
    // PORT: "8080",
  }},
}}
"#,
        name = config.name,
        command = config.command,
        tty = config.tty,
        sync_rows_cols = config.sync_rows_cols,
        restart = match config.restart {
            RestartPolicy::Never => "never",
            RestartPolicy::OnFailure => "on-failure",
            RestartPolicy::Always => "always",
        },
        persist_logs = config.persist_logs,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn loads_json5_and_legacy_dotenv_overlay() {
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
        .expect("env.served");
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
    fn json5_environment_overrides_legacy_dotenv_without_expansion() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join(CONFIG_FILE),
            r#"{
                // JSON5 comments and trailing commas are accepted.
                name: 'api',
                command: 'echo ok',
                env: {
                    PORT: 'json5',
                    BIN: '${HOME}/bin',
                },
            }"#,
        )
        .expect("config");
        fs::write(directory.path().join(ENV_FILE), "PORT=dotenv\nLEGACY=yes\n")
            .expect("env.served");
        let mut base = BTreeMap::new();
        base.insert("HOME".to_owned(), "/home/test".to_owned());

        let service = load_service(directory.path(), &base).expect("load");
        assert_eq!(service.environment.get("PORT"), Some(&"json5".to_owned()));
        assert_eq!(service.environment.get("LEGACY"), Some(&"yes".to_owned()));
        assert_eq!(
            service.environment.get("BIN"),
            Some(&"${HOME}/bin".to_owned())
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
        assert!(error.to_string().contains("invalid JSON5"));
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

    #[test]
    fn ignores_project_env_and_reads_only_served_env() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join(CONFIG_FILE),
            r#"{"name":"api","command":"echo ok"}"#,
        )
        .expect("config");
        fs::write(directory.path().join(".env"), "PORT=project\n").expect("project env");
        fs::write(directory.path().join(ENV_FILE), "PORT=served\n").expect("served env");

        let service = load_service(directory.path(), &BTreeMap::new()).expect("load");
        assert_eq!(service.environment.get("PORT"), Some(&"served".to_owned()));

        fs::remove_file(directory.path().join(ENV_FILE)).expect("remove served env");
        let service =
            load_service(directory.path(), &BTreeMap::new()).expect("load without served env");
        assert!(!service.environment.contains_key("PORT"));
    }

    #[test]
    fn template_creates_annotated_json5_without_legacy_env_file() {
        let directory = tempdir().expect("tempdir");

        write_template(directory.path()).expect("write template");

        let source = fs::read_to_string(directory.path().join(CONFIG_FILE)).expect("template");
        assert!(source.contains("// Globally unique service name"));
        assert!(source.contains("env: {"));
        assert!(directory.path().join(CONFIG_FILE).is_file());
        assert!(!directory.path().join(ENV_FILE).exists());
        assert!(!directory.path().join(".env").exists());

        let service = load_service(directory.path(), &BTreeMap::new()).expect("load template");
        assert_eq!(service.config.env, BTreeMap::new());
    }

    #[test]
    fn template_does_not_rewrite_existing_source() {
        let directory = tempdir().expect("tempdir");
        let original = "// keep this broken source available for repair\n{name: 'custom',\n";
        fs::write(directory.path().join(CONFIG_FILE), original).expect("config");

        write_template(directory.path()).expect("write template");

        assert_eq!(
            fs::read_to_string(directory.path().join(CONFIG_FILE)).expect("read config"),
            original
        );
    }
}
