use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

pub const CONFIG_FILE: &str = ".served.json5";
pub const LEGACY_CONFIG_FILE: &str = ".served.json";
pub const ENV_FILE: &str = ".env.served";
pub const DEFAULT_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const DEFAULT_LOG_MAX_FILES: u32 = 3;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("service directory does not exist: {0}")]
    MissingDirectory(PathBuf),
    #[error("service directory is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("missing {CONFIG_FILE} or {LEGACY_CONFIG_FILE} in {0}")]
    MissingConfig(PathBuf),
    #[error("invalid service name {0:?}; use letters, digits, '.', '_' or '-'")]
    InvalidName(String),
    #[error("service command must not be empty")]
    EmptyCommand,
    #[error("invalid environment key {0:?}")]
    InvalidEnvKey(String),
    #[error("log_max_bytes must be greater than zero")]
    InvalidLogMaxBytes,
    #[error("log_max_files must be greater than zero")]
    InvalidLogMaxFiles,
    #[error("I/O error while reading service configuration: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot read configuration {path}: {source}")]
    ReadSource {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON5 in {path}: {source}")]
    Json5 {
        path: PathBuf,
        #[source]
        source: json5::Error,
    },
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

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "never" => Some(Self::Never),
            "on-failure" => Some(Self::OnFailure),
            "always" => Some(Self::Always),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OnFailure => "on-failure",
            Self::Always => "always",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub name: String,
    pub command: String,
    /// Source-only setting; resolved into LoadedService.directory before launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default = "default_tty")]
    pub tty: bool,
    #[serde(rename = "syncRowsCols", default = "default_sync_rows_cols")]
    pub sync_rows_cols: bool,
    #[serde(default)]
    pub restart: RestartPolicy,
    #[serde(default)]
    pub persist_logs: bool,
    #[serde(default = "default_log_max_bytes")]
    pub log_max_bytes: u64,
    #[serde(default = "default_log_max_files")]
    pub log_max_files: u32,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn default_tty() -> bool {
    true
}

fn default_sync_rows_cols() -> bool {
    true
}

fn default_log_max_bytes() -> u64 {
    DEFAULT_LOG_MAX_BYTES
}

fn default_log_max_files() -> u32 {
    DEFAULT_LOG_MAX_FILES
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
        if self.log_max_bytes == 0 {
            return Err(ConfigError::InvalidLogMaxBytes);
        }
        if self.log_max_files == 0 {
            return Err(ConfigError::InvalidLogMaxFiles);
        }
        for key in self.env.keys() {
            validate_env_key(key)?;
        }
        Ok(())
    }

    pub fn template(directory: &Path) -> Self {
        Self {
            name: default_service_name(directory),
            command: "./run.sh".to_owned(),
            cwd: None,
            tty: true,
            sync_rows_cols: true,
            restart: RestartPolicy::Never,
            persist_logs: false,
            log_max_bytes: DEFAULT_LOG_MAX_BYTES,
            log_max_files: DEFAULT_LOG_MAX_FILES,
            env: BTreeMap::new(),
        }
    }
}

pub fn default_service_name(directory: &Path) -> String {
    directory
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
        .collect()
}

#[derive(Debug, Clone)]
pub struct LoadedService {
    pub directory: PathBuf,
    pub config_file: Option<PathBuf>,
    pub config: ServiceConfig,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigFileStatus {
    Current,
    Legacy,
    CurrentWithLegacy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedConfigFile {
    path: PathBuf,
    status: ConfigFileStatus,
}

impl ResolvedConfigFile {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn deprecation_warning(&self) -> Option<String> {
        let directory = self.path.parent()?;
        let current = directory.join(CONFIG_FILE);
        let legacy = directory.join(LEGACY_CONFIG_FILE);
        match self.status {
            ConfigFileStatus::Current => None,
            ConfigFileStatus::Legacy => Some(format!(
                "{} is deprecated; rename it to {}",
                legacy.display(),
                current.display()
            )),
            ConfigFileStatus::CurrentWithLegacy => Some(format!(
                "ignoring deprecated {} because {} exists",
                legacy.display(),
                current.display()
            )),
        }
    }

    fn log_deprecation_warning(&self) {
        if let Some(message) = self.deprecation_warning() {
            warn!(config = %self.path.display(), "{message}");
        }
    }
}

pub(crate) fn resolve_config_file(directory: &Path) -> Option<ResolvedConfigFile> {
    let current = directory.join(CONFIG_FILE);
    let legacy = directory.join(LEGACY_CONFIG_FILE);
    if current.is_file() {
        let status = if legacy.is_file() {
            ConfigFileStatus::CurrentWithLegacy
        } else {
            ConfigFileStatus::Current
        };
        Some(ResolvedConfigFile {
            path: current,
            status,
        })
    } else if legacy.is_file() {
        Some(ResolvedConfigFile {
            path: legacy,
            status: ConfigFileStatus::Legacy,
        })
    } else {
        None
    }
}

/// The durable configuration location, independent of the process working directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
pub enum ServiceSource {
    Directory(PathBuf),
    File(PathBuf),
}

pub fn load_service(
    directory: impl AsRef<Path>,
    manager_environment: &BTreeMap<String, String>,
) -> Result<LoadedService, ConfigError> {
    load_source(
        &ServiceSource::Directory(directory.as_ref().to_owned()),
        None,
        manager_environment,
    )
}

pub fn canonical_directory(directory: &Path) -> Result<PathBuf, ConfigError> {
    if !directory.exists() {
        return Err(ConfigError::MissingDirectory(directory.to_owned()));
    }
    if !directory.is_dir() {
        return Err(ConfigError::NotDirectory(directory.to_owned()));
    }
    Ok(fs::canonicalize(directory)?)
}

pub fn load_source(
    source: &ServiceSource,
    workdir: Option<&Path>,
    manager_environment: &BTreeMap<String, String>,
) -> Result<LoadedService, ConfigError> {
    let config_file = match source {
        ServiceSource::Directory(directory) => {
            let directory = canonical_directory(directory)?;
            let file = resolve_config_file(&directory)
                .ok_or_else(|| ConfigError::MissingConfig(directory.clone()))?;
            file.log_deprecation_warning();
            file.path
        }
        ServiceSource::File(path) => {
            let parent = canonical_directory(path.parent().unwrap_or(Path::new(".")))?;
            parent.join(path.file_name().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "configuration path must name a file",
                )
            })?)
        }
    };
    let source = fs::read_to_string(&config_file).map_err(|source| ConfigError::ReadSource {
        path: config_file.clone(),
        source,
    })?;
    let mut config: ServiceConfig =
        json5::from_str(&source).map_err(|source| ConfigError::Json5 {
            path: config_file.clone(),
            source,
        })?;
    config.validate()?;
    let config_directory = config_file.parent().expect("absolute configuration path");
    let configured_cwd = config.cwd.take();
    let directory = match workdir {
        Some(directory) => canonical_directory(directory)?,
        None => canonical_directory(
            &configured_cwd
                .map(|cwd| config_directory.join(cwd))
                .unwrap_or_else(|| config_directory.to_owned()),
        )?,
    };
    let mut environment = manager_environment.clone();
    let env_path = config_directory.join(ENV_FILE);
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
        config_file: Some(config_file),
        config,
        environment,
    })
}

pub fn manager_environment() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

pub fn write_template(directory: &Path) -> Result<(), ConfigError> {
    prepare_config_file(directory).map(|_| ())
}

pub(crate) fn prepare_config_file(directory: &Path) -> Result<ResolvedConfigFile, ConfigError> {
    fs::create_dir_all(directory)?;
    if let Some(config_file) = resolve_config_file(directory) {
        return Ok(config_file);
    }
    let path = directory.join(CONFIG_FILE);
    fs::write(&path, template_source(directory))?;
    Ok(ResolvedConfigFile {
        path,
        status: ConfigFileStatus::Current,
    })
}

pub(crate) fn prepare_explicit_config(path: &Path) -> Result<ResolvedConfigFile, ConfigError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let parent = fs::canonicalize(parent)?;
    let path = parent.join(path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "configuration path must name a file",
        )
    })?);
    use std::io::Write;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => file.write_all(template_source(&parent).as_bytes())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_file() => {}
        Err(error) => return Err(error.into()),
    }
    Ok(ResolvedConfigFile {
        path,
        status: ConfigFileStatus::Current,
    })
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
// The edit form preserves comments and changes only modified properties.
{{
  // Globally unique service name. Use only letters, digits, '.', '_' and '-'.
  // Renaming an enabled service requires disabling and enabling it again.
  name: "{name}",

  // Working directory, relative to this file or absolute. null uses this file's
  // directory. An enable --workdir override takes priority until re-enabled.
  cwd: null,

  // Shell script executed with `/bin/sh -c` from the resolved working directory.
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

  // Maximum bytes in one persistent log segment. When this limit is reached,
  // served rotates the segment and keeps writing to latest.log.
  log_max_bytes: {log_max_bytes},

  // Number of archived persistent segments to keep. latest.log is kept in
  // addition to these archives. Older segments are removed first.
  log_max_files: {log_max_files},

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
        log_max_bytes = config.log_max_bytes,
        log_max_files = config.log_max_files,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn explicit_source_resolves_cwd_and_environment_independently() {
        let root = tempfile::tempdir().unwrap();
        let configs = root.path().join("configs");
        let configured = root.path().join("configured");
        let override_dir = root.path().join("override");
        for directory in [&configs, &configured, &override_dir] {
            fs::create_dir(directory).unwrap();
        }
        let path = configs.join("api.custom");
        fs::write(
            &path,
            "{name:'api', command:'pwd', cwd:'../configured', env:{VALUE:'config'}}",
        )
        .unwrap();
        fs::write(configs.join(CONFIG_FILE), "invalid").unwrap();
        fs::write(configs.join(ENV_FILE), "VALUE=dotenv\nFROM_CONFIG=yes").unwrap();
        fs::write(override_dir.join(ENV_FILE), "FROM_CONFIG=no").unwrap();
        let source = ServiceSource::File(path.clone());
        let loaded = load_source(&source, None, &BTreeMap::new()).unwrap();
        assert_eq!(loaded.directory, fs::canonicalize(&configured).unwrap());
        assert_eq!(
            loaded.config_file.as_ref(),
            Some(&fs::canonicalize(&path).unwrap())
        );
        assert_eq!(loaded.config.cwd, None);
        assert_eq!(loaded.environment["VALUE"], "config");
        assert_eq!(loaded.environment["FROM_CONFIG"], "yes");
        let loaded = load_source(&source, Some(&override_dir), &BTreeMap::new()).unwrap();
        assert_eq!(loaded.directory, fs::canonicalize(&override_dir).unwrap());
        assert_eq!(loaded.environment["FROM_CONFIG"], "yes");
        fs::write(&path, "{name:'api', command:'pwd'}").unwrap();
        assert_eq!(
            load_source(&source, None, &BTreeMap::new())
                .unwrap()
                .directory,
            fs::canonicalize(&configs).unwrap()
        );
        fs::write(&path, "{name:'api', command:'pwd', cwd:'missing'}").unwrap();
        assert!(matches!(
            load_source(&source, None, &BTreeMap::new()),
            Err(ConfigError::MissingDirectory(_))
        ));
        assert!(load_source(&source, Some(&override_dir), &BTreeMap::new()).is_ok());
        fs::write(&path, "invalid").unwrap();
        assert!(matches!(
            load_source(&source, None, &BTreeMap::new()),
            Err(ConfigError::Json5 { .. })
        ));
        fs::remove_file(&path).unwrap();
        assert!(load_source(&source, None, &BTreeMap::new()).is_err());
    }

    #[test]
    fn directory_source_keeps_configuration_location_when_cwd_changes() {
        let root = tempfile::tempdir().unwrap();
        let workdir = root.path().join("work");
        fs::create_dir(&workdir).unwrap();
        fs::write(
            root.path().join(CONFIG_FILE),
            "{name:'api', command:'pwd', cwd:'work'}",
        )
        .unwrap();
        let service = load_service(root.path(), &BTreeMap::new()).unwrap();
        assert_eq!(service.directory, fs::canonicalize(workdir).unwrap());
        assert_eq!(
            service.config_file.unwrap(),
            fs::canonicalize(root.path().join(CONFIG_FILE)).unwrap()
        );
    }

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
        assert_eq!(service.config.log_max_bytes, DEFAULT_LOG_MAX_BYTES);
        assert_eq!(service.config.log_max_files, DEFAULT_LOG_MAX_FILES);
        assert_eq!(service.environment.get("PORT"), Some(&"8080".to_owned()));
        assert_eq!(
            service.environment.get("QUOTED"),
            Some(&"hello world".to_owned())
        );
    }

    #[test]
    fn loads_deprecated_json_filename() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join(LEGACY_CONFIG_FILE),
            r#"{name: "legacy", command: "echo ok"}"#,
        )
        .expect("legacy config");

        let resolved = resolve_config_file(directory.path()).expect("resolve legacy config");
        assert_eq!(resolved.path(), directory.path().join(LEGACY_CONFIG_FILE));
        assert!(
            resolved
                .deprecation_warning()
                .expect("deprecation warning")
                .contains("is deprecated")
        );
        let service = load_service(directory.path(), &BTreeMap::new()).expect("load legacy config");
        assert_eq!(service.config.name, "legacy");
    }

    #[test]
    fn json5_filename_takes_precedence_over_deprecated_json_filename() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join(LEGACY_CONFIG_FILE),
            r#"{name: "legacy", command: "echo legacy"}"#,
        )
        .expect("legacy config");
        fs::write(
            directory.path().join(CONFIG_FILE),
            r#"{name: "current", command: "echo current"}"#,
        )
        .expect("current config");

        let resolved = resolve_config_file(directory.path()).expect("resolve current config");
        assert_eq!(resolved.path(), directory.path().join(CONFIG_FILE));
        assert!(
            resolved
                .deprecation_warning()
                .expect("ignored legacy warning")
                .contains("ignoring deprecated")
        );
        let service =
            load_service(directory.path(), &BTreeMap::new()).expect("load current config");
        assert_eq!(service.config.name, "current");
    }

    #[test]
    fn invalid_json5_filename_does_not_fall_back_to_deprecated_json_filename() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join(LEGACY_CONFIG_FILE),
            r#"{name: "legacy", command: "echo legacy"}"#,
        )
        .expect("legacy config");
        fs::write(directory.path().join(CONFIG_FILE), "{ invalid").expect("invalid current config");

        let expected_path = fs::canonicalize(directory.path())
            .expect("canonical directory")
            .join(CONFIG_FILE);
        let error = load_service(directory.path(), &BTreeMap::new()).expect_err("reject current");
        assert!(matches!(
            error,
            ConfigError::Json5 { path, .. } if path == expected_path
        ));
    }

    #[test]
    fn missing_config_reports_both_supported_filenames() {
        let directory = tempdir().expect("tempdir");

        let error = load_service(directory.path(), &BTreeMap::new()).expect_err("missing config");
        let message = error.to_string();
        assert!(message.contains(CONFIG_FILE));
        assert!(message.contains(LEGACY_CONFIG_FILE));
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
            r#"{"name":"api","command":"echo ok","unknown_field":"/tmp"}"#,
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
    fn default_name_sanitizes_the_directory_component() {
        assert_eq!(
            default_service_name(Path::new("/srv/My temporary service!")),
            "My-temporary-service-"
        );
        assert_eq!(default_service_name(Path::new("/")), "service");
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
    fn log_limits_round_trip_and_reject_zero() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join(CONFIG_FILE),
            r#"{"name":"api","command":"echo ok","log_max_bytes":128,"log_max_files":4}"#,
        )
        .expect("config");
        let service = load_service(directory.path(), &BTreeMap::new()).expect("load");
        assert_eq!(service.config.log_max_bytes, 128);
        assert_eq!(service.config.log_max_files, 4);

        fs::write(
            directory.path().join(CONFIG_FILE),
            r#"{"name":"api","command":"echo ok","log_max_bytes":0}"#,
        )
        .expect("invalid config");
        assert!(matches!(
            load_service(directory.path(), &BTreeMap::new()),
            Err(ConfigError::InvalidLogMaxBytes)
        ));
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
        assert!(source.contains("log_max_bytes:"));
        assert!(source.contains("log_max_files:"));
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

    #[test]
    fn template_keeps_deprecated_config_without_creating_current_file() {
        let directory = tempdir().expect("tempdir");
        let original = "// keep legacy source\n{name: 'legacy', command: 'echo ok'}\n";
        fs::write(directory.path().join(LEGACY_CONFIG_FILE), original).expect("legacy config");

        write_template(directory.path()).expect("keep legacy template");

        assert!(!directory.path().join(CONFIG_FILE).exists());
        assert_eq!(
            fs::read_to_string(directory.path().join(LEGACY_CONFIG_FILE)).expect("read legacy"),
            original
        );
    }
}
