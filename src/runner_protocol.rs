use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;

use crate::{
    config::{
        DEFAULT_LOG_MAX_BYTES, DEFAULT_LOG_MAX_FILES, LoadedService, RestartPolicy, ServiceConfig,
    },
    ipc::{Frame, framed, receive_json, send_json},
};

/// The runner protocol is separate from the public manager protocol. It stays
/// additive so an upgraded manager can continue to adopt an older runner.
pub const RUNNER_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub directory: String,
    pub config: ServiceConfig,
    pub environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireLaunchSpec {
    directory: String,
    config: WireServiceConfig,
    environment: BTreeMap<String, String>,
    #[serde(default)]
    log_max_bytes: Option<u64>,
    #[serde(default)]
    log_max_files: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireServiceConfig {
    name: String,
    command: String,
    #[serde(default = "default_tty")]
    tty: bool,
    #[serde(rename = "syncRowsCols", default = "default_sync_rows_cols")]
    sync_rows_cols: bool,
    #[serde(default)]
    restart: RestartPolicy,
    #[serde(default)]
    persist_logs: bool,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

fn default_tty() -> bool {
    true
}

fn default_sync_rows_cols() -> bool {
    true
}

impl From<&ServiceConfig> for WireServiceConfig {
    fn from(config: &ServiceConfig) -> Self {
        Self {
            name: config.name.clone(),
            command: config.command.clone(),
            tty: config.tty,
            sync_rows_cols: config.sync_rows_cols,
            restart: config.restart,
            persist_logs: config.persist_logs,
            env: config.env.clone(),
        }
    }
}

impl From<WireServiceConfig> for ServiceConfig {
    fn from(config: WireServiceConfig) -> Self {
        Self {
            name: config.name,
            command: config.command,
            tty: config.tty,
            sync_rows_cols: config.sync_rows_cols,
            restart: config.restart,
            persist_logs: config.persist_logs,
            log_max_bytes: DEFAULT_LOG_MAX_BYTES,
            log_max_files: DEFAULT_LOG_MAX_FILES,
            env: config.env,
        }
    }
}

impl Serialize for LaunchSpec {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        WireLaunchSpec {
            directory: self.directory.clone(),
            config: WireServiceConfig::from(&self.config),
            environment: self.environment.clone(),
            log_max_bytes: Some(self.config.log_max_bytes),
            log_max_files: Some(self.config.log_max_files),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LaunchSpec {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = WireLaunchSpec::deserialize(deserializer)?;
        let mut config = ServiceConfig::from(wire.config);
        config.log_max_bytes = wire.log_max_bytes.unwrap_or(DEFAULT_LOG_MAX_BYTES);
        config.log_max_files = wire.log_max_files.unwrap_or(DEFAULT_LOG_MAX_FILES);
        Ok(Self {
            directory: wire.directory,
            config,
            environment: wire.environment,
        })
    }
}

impl LaunchSpec {
    pub fn from_loaded(service: &LoadedService) -> Self {
        Self {
            directory: service.directory.display().to_string(),
            config: service.config.clone(),
            environment: service.environment.clone(),
        }
    }

    pub fn into_loaded(self) -> Result<LoadedService> {
        Ok(LoadedService {
            directory: PathBuf::from(&self.directory),
            config: self.config,
            environment: self.environment,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunnerServiceState {
    Starting,
    Running,
    Restarting,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerHistoryRecord {
    pub id: String,
    pub bytes: u64,
    pub current: bool,
    pub persisted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerStatus {
    pub name: String,
    pub runner_pid: u32,
    pub state: RunnerServiceState,
    pub pid: Option<u32>,
    pub pid_start_time: Option<u64>,
    pub tty: bool,
    pub restart: String,
    pub persist_logs: bool,
    pub attach_active: bool,
    pub output_tail: String,
    pub recent_failures: u32,
    pub window_seconds: u64,
    pub latest_log: Option<String>,
    pub spec: Option<LaunchSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerMetadata {
    pub name: String,
    pub runner_pid: u32,
    #[serde(default)]
    pub runner_start_time: Option<u64>,
    pub service_pid: Option<u32>,
    pub service_start_time: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RunnerRequest {
    Hello {
        version: u32,
        name: String,
    },
    Configure {
        spec: LaunchSpec,
        log_directory: String,
    },
    Restart {
        spec: LaunchSpec,
        log_directory: String,
    },
    Stop,
    Status,
    /// Stream an initial status followed by status changes on this connection.
    /// Older v1 runners close the connection when they see this additive request;
    /// callers must fall back to `Status` polling in that case.
    WatchStatus,
    Attach,
    Resize {
        token: String,
        cols: u16,
        rows: u16,
    },
    HistoryList,
    HistoryChunk {
        id: String,
        offset: u64,
        limit: u32,
    },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RunnerResponse {
    Hello {
        version: u32,
        name: String,
    },
    Ok,
    Status {
        status: RunnerStatus,
    },
    Attach {
        token: String,
    },
    AttachUnavailable {
        name: String,
        recent_failures: u32,
        window_seconds: u64,
        latest_log: Option<String>,
    },
    HistoryList {
        records: Vec<RunnerHistoryRecord>,
    },
    HistoryChunk {
        id: String,
        offset: u64,
        next_offset: u64,
        total: u64,
        total_lines: u64,
        eof: bool,
        content: String,
    },
    Error {
        message: String,
    },
}

pub async fn connect(socket_path: &std::path::Path, name: &str) -> Result<Frame> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connect to runner socket {}", socket_path.display()))?;
    let mut frame = framed(stream);
    send_json(
        &mut frame,
        &RunnerRequest::Hello {
            version: RUNNER_PROTOCOL_VERSION,
            name: name.to_owned(),
        },
    )
    .await?;
    match receive_json::<RunnerResponse>(&mut frame).await? {
        RunnerResponse::Hello {
            version,
            name: actual,
        } if version == RUNNER_PROTOCOL_VERSION => {
            if actual != name {
                bail!("runner socket belongs to service {actual:?}, not {name:?}");
            }
            Ok(frame)
        }
        RunnerResponse::Hello { version, .. } => {
            bail!("runner protocol version {version} is unsupported")
        }
        RunnerResponse::Error { message } => bail!("runner handshake failed: {message}"),
        _ => bail!("invalid runner handshake response"),
    }
}

pub async fn request(
    socket_path: &std::path::Path,
    name: &str,
    request: RunnerRequest,
) -> Result<RunnerResponse> {
    let mut frame = connect(socket_path, name).await?;
    send_json(&mut frame, &request).await?;
    let response = receive_json::<RunnerResponse>(&mut frame).await?;
    if let RunnerResponse::Error { message } = &response {
        bail!("{message}");
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> LaunchSpec {
        LaunchSpec {
            directory: "/tmp/service".to_owned(),
            config: ServiceConfig {
                name: "service".to_owned(),
                command: "echo ok".to_owned(),
                tty: false,
                sync_rows_cols: true,
                restart: RestartPolicy::Always,
                persist_logs: true,
                log_max_bytes: 128,
                log_max_files: 4,
                env: BTreeMap::new(),
            },
            environment: BTreeMap::new(),
        }
    }

    #[test]
    fn launch_spec_keeps_log_limits_outside_legacy_config() {
        let value = serde_json::to_value(spec()).expect("serialize launch spec");
        assert_eq!(value["log_max_bytes"], 128);
        assert_eq!(value["log_max_files"], 4);
        assert!(value["config"].get("log_max_bytes").is_none());
        assert!(value["config"].get("log_max_files").is_none());
    }

    #[test]
    fn launch_spec_defaults_limits_when_reading_an_old_runner_message() {
        let value = json!({
            "directory": "/tmp/service",
            "config": {
                "name": "service",
                "command": "echo ok",
                "tty": false,
                "syncRowsCols": true,
                "restart": "never",
                "persist_logs": false,
                "env": {},
            },
            "environment": {},
        });
        let parsed: LaunchSpec = serde_json::from_value(value).expect("deserialize old spec");
        assert_eq!(parsed.config.log_max_bytes, DEFAULT_LOG_MAX_BYTES);
        assert_eq!(parsed.config.log_max_files, DEFAULT_LOG_MAX_FILES);
    }

    #[test]
    fn watch_status_is_an_additive_v1_request() {
        let value = serde_json::to_value(RunnerRequest::WatchStatus).expect("serialize watch");
        assert_eq!(value, json!("WatchStatus"));
        let decoded: RunnerRequest = serde_json::from_value(value).expect("decode watch");
        assert!(matches!(decoded, RunnerRequest::WatchStatus));
    }

    #[test]
    fn runner_status_keeps_the_v1_wire_shape() {
        let status = RunnerStatus {
            name: "api".to_owned(),
            runner_pid: 10,
            state: RunnerServiceState::Running,
            pid: Some(11),
            pid_start_time: Some(12),
            tty: true,
            restart: "always".to_owned(),
            persist_logs: true,
            attach_active: false,
            output_tail: "ready".to_owned(),
            recent_failures: 2,
            window_seconds: 60,
            latest_log: Some("/tmp/latest.log".to_owned()),
            spec: None,
        };

        assert_eq!(
            serde_json::to_value(RunnerResponse::Status { status }).expect("serialize status"),
            json!({
                "Status": {
                    "status": {
                        "name": "api",
                        "runner_pid": 10,
                        "state": "Running",
                        "pid": 11,
                        "pid_start_time": 12,
                        "tty": true,
                        "restart": "always",
                        "persist_logs": true,
                        "attach_active": false,
                        "output_tail": "ready",
                        "recent_failures": 2,
                        "window_seconds": 60,
                        "latest_log": "/tmp/latest.log",
                        "spec": null
                    }
                }
            })
        );
    }
}
