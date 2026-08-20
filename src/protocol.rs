use std::{collections::BTreeMap, io};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;

pub use crate::ipc::{
    Frame, HandoffStream, MAX_FRAME_LENGTH, framed, into_handoff, receive_json, send_json,
};

pub const PROTOCOL_VERSION: u32 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceKind {
    Enabled,
    Temporary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSpec {
    pub directory: String,
    pub name: String,
    pub argv: Vec<String>,
    pub tty: bool,
    #[serde(rename = "syncRowsCols")]
    pub sync_rows_cols: bool,
    pub restart: String,
    pub persist_logs: bool,
    pub log_max_bytes: u64,
    pub log_max_files: u32,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Target {
    Name(String),
    Directory(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    Hello {
        version: u32,
    },
    List,
    Enable {
        directory: String,
    },
    Run {
        spec: RunSpec,
    },
    Disable {
        target: Target,
    },
    Restart {
        target: Target,
    },
    Attach {
        name: String,
    },
    Resize {
        name: String,
        token: String,
        cols: u16,
        rows: u16,
    },
    HistoryList {
        target: Target,
    },
    HistoryChunk {
        target: Target,
        id: String,
        offset: u64,
        limit: u32,
    },
    /// Stop the manager and all managed runners. Used by systemd ExecStop.
    #[serde(rename = "ManagerShutdown")]
    ManagerShutdown,
    /// Replace the manager process with a specific executable without stopping runners.
    #[serde(rename = "ManagerHandoff")]
    ManagerHandoff {
        executable: String,
    },
    /// Exit the manager without stopping runners so another supervisor can adopt them.
    #[serde(rename = "ManagerRelinquish")]
    ManagerRelinquish,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServiceState {
    Starting,
    Running,
    Restarting,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub directory: String,
    pub kind: ServiceKind,
    pub state: ServiceState,
    pub pid: Option<u32>,
    pub tty: bool,
    pub restart: String,
    #[serde(default)]
    pub persist_logs: bool,
    pub attach_active: bool,
    pub output_tail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub id: String,
    pub bytes: u64,
    pub current: bool,
    pub persisted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Hello {
        version: u32,
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
    Ok,
    Services {
        services: Vec<ServiceInfo>,
    },
    HistoryList {
        service: String,
        records: Vec<HistoryRecord>,
    },
    HistoryChunk {
        service: String,
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

pub async fn connect(socket_path: &std::path::Path) -> Result<Frame> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connect to manager socket {}", socket_path.display()))?;
    let mut frame = framed(stream);
    send_json(
        &mut frame,
        &Request::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .await?;
    match receive_json::<Response>(&mut frame).await? {
        Response::Hello { version } if version == PROTOCOL_VERSION => Ok(frame),
        Response::Hello { version } => bail!("manager protocol version {version} is unsupported"),
        Response::Error { message } => bail!("manager handshake failed: {message}"),
        _ => bail!("invalid manager handshake response"),
    }
}

pub async fn request(socket_path: &std::path::Path, request: Request) -> Result<Response> {
    let mut frame = connect(socket_path).await?;
    send_json(&mut frame, &request).await?;
    let response = receive_json::<Response>(&mut frame).await?;
    if let Response::Error { message } = &response {
        bail!("{message}");
    }
    Ok(response)
}

pub fn io_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn request_round_trips_as_json() {
        let request = Request::Restart {
            target: Target::Name("api".to_owned()),
        };
        let value = serde_json::to_value(&request).expect("serialize");
        let decoded: Request = serde_json::from_value(value).expect("decode");
        assert!(matches!(decoded, Request::Restart { .. }));
    }

    #[test]
    fn history_chunk_round_trips_as_json() {
        let request = Request::HistoryChunk {
            target: Target::Name("api".to_owned()),
            id: "latest".to_owned(),
            offset: 48,
            limit: 1024,
        };
        let value = serde_json::to_value(&request).expect("serialize");
        let decoded: Request = serde_json::from_value(value).expect("decode");
        assert!(matches!(decoded, Request::HistoryChunk { .. }));
    }

    #[test]
    fn history_response_round_trips_logical_line_count() {
        let response = Response::HistoryChunk {
            service: "api".to_owned(),
            id: "latest".to_owned(),
            offset: 0,
            next_offset: 12,
            total: 12,
            total_lines: 3,
            eof: true,
            content: "one\ntwo\nthree".to_owned(),
        };
        let value = serde_json::to_value(&response).expect("serialize");
        let decoded: Response = serde_json::from_value(value).expect("decode");
        assert!(matches!(
            decoded,
            Response::HistoryChunk { total_lines: 3, .. }
        ));
    }

    #[test]
    fn resize_request_round_trips_as_json() {
        let request = Request::Resize {
            name: "vim".to_owned(),
            token: "session-token".to_owned(),
            cols: 120,
            rows: 40,
        };
        let value = serde_json::to_value(&request).expect("serialize");
        let decoded: Request = serde_json::from_value(value).expect("decode");
        assert!(matches!(
            decoded,
            Request::Resize {
                cols: 120,
                rows: 40,
                ..
            }
        ));
    }

    #[test]
    fn attach_unavailable_round_trips_crash_diagnostics() {
        let response = Response::AttachUnavailable {
            name: "api".to_owned(),
            recent_failures: 3,
            window_seconds: 60,
            latest_log: Some("/tmp/api/latest.log".to_owned()),
        };
        let value = serde_json::to_value(&response).expect("serialize");
        let decoded: Response = serde_json::from_value(value).expect("decode");
        assert!(matches!(
            decoded,
            Response::AttachUnavailable {
                recent_failures: 3,
                window_seconds: 60,
                latest_log: Some(path),
                ..
            } if path == "/tmp/api/latest.log"
        ));
    }

    #[test]
    fn manager_lifecycle_requests_round_trip_as_json() {
        for request in [
            Request::ManagerShutdown,
            Request::ManagerHandoff {
                executable: "/opt/served/bin/served".to_owned(),
            },
            Request::ManagerRelinquish,
        ] {
            let value = serde_json::to_value(&request).expect("serialize");
            let decoded: Request = serde_json::from_value(value).expect("decode");
            assert!(matches!(
                decoded,
                Request::ManagerShutdown
                    | Request::ManagerHandoff { .. }
                    | Request::ManagerRelinquish
            ));
        }
    }

    #[test]
    fn manager_request_keeps_the_v7_wire_shape() {
        assert_eq!(
            serde_json::to_value(Request::HistoryChunk {
                target: Target::Directory("/srv/api".to_owned()),
                id: "latest".to_owned(),
                offset: 12,
                limit: 4096,
            })
            .expect("serialize request"),
            serde_json::json!({
                "HistoryChunk": {
                    "target": { "Directory": "/srv/api" },
                    "id": "latest",
                    "offset": 12,
                    "limit": 4096
                }
            })
        );

        assert_eq!(
            serde_json::to_value(Request::Run {
                spec: RunSpec {
                    directory: "/srv/api".to_owned(),
                    name: "api".to_owned(),
                    argv: vec![
                        "printf".to_owned(),
                        "%s".to_owned(),
                        "hello world".to_owned()
                    ],
                    tty: false,
                    sync_rows_cols: true,
                    restart: "never".to_owned(),
                    persist_logs: false,
                    log_max_bytes: 1024,
                    log_max_files: 3,
                    env: BTreeMap::from([("PORT".to_owned(), "8080".to_owned())]),
                },
            })
            .expect("serialize run request"),
            serde_json::json!({
                "Run": {
                    "spec": {
                        "directory": "/srv/api",
                        "name": "api",
                        "argv": ["printf", "%s", "hello world"],
                        "tty": false,
                        "syncRowsCols": true,
                        "restart": "never",
                        "persist_logs": false,
                        "log_max_bytes": 1024,
                        "log_max_files": 3,
                        "env": { "PORT": "8080" }
                    }
                }
            })
        );
    }

    #[test]
    fn service_kind_keeps_the_v7_wire_shape() {
        let service = ServiceInfo {
            name: "scratch".to_owned(),
            directory: "/srv/scratch".to_owned(),
            state: ServiceState::Running,
            kind: ServiceKind::Temporary,
            tty: true,
            restart: "never".to_owned(),
            persist_logs: false,
            pid: Some(42),
            attach_active: false,
            output_tail: String::new(),
        };

        let value = serde_json::to_value(Response::Services {
            services: vec![service],
        })
        .expect("serialize services");
        assert_eq!(value["Services"]["services"][0]["kind"], "temporary");
    }

    #[tokio::test]
    async fn handoff_stream_reads_framed_buffer_before_socket() {
        let (stream, mut peer) = UnixStream::pair().expect("create unix stream pair");
        let frame = framed(stream);
        let mut parts = frame.into_parts();
        parts.read_buf.extend_from_slice(b"buffered");
        let mut handoff = into_handoff(Frame::from_parts(parts)).expect("create handoff stream");

        peer.write_all(b"socket")
            .await
            .expect("write socket suffix");
        let mut output = [0_u8; 14];
        handoff
            .read_exact(&mut output)
            .await
            .expect("read handoff bytes");

        assert_eq!(&output, b"bufferedsocket");
    }
}
