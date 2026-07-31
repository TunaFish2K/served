use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;

use crate::{
    config::{LoadedService, ServiceConfig},
    protocol::{Frame, HistoryRecord, ServiceState, framed, receive_json, send_json},
};

/// The runner protocol is separate from the public manager protocol. It stays
/// additive so an upgraded manager can continue to adopt an older runner.
pub const RUNNER_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub directory: String,
    pub config: ServiceConfig,
    pub environment: BTreeMap<String, String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerStatus {
    pub name: String,
    pub runner_pid: u32,
    pub state: ServiceState,
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
        records: Vec<HistoryRecord>,
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
