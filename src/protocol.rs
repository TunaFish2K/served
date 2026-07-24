use std::io;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_LENGTH: usize = 1024 * 1024;

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
    Disable {
        target: Target,
    },
    Restart {
        target: Target,
    },
    Attach {
        name: String,
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
        eof: bool,
        content: String,
    },
    Error {
        message: String,
    },
}

pub type Frame = Framed<UnixStream, LengthDelimitedCodec>;

pub fn framed(stream: UnixStream) -> Frame {
    let mut codec = LengthDelimitedCodec::new();
    codec.set_max_frame_length(MAX_FRAME_LENGTH);
    Framed::new(stream, codec)
}

pub async fn send_json<T: Serialize>(frame: &mut Frame, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value).context("serialize protocol message")?;
    frame
        .send(bytes.into())
        .await
        .context("send protocol message")?;
    Ok(())
}

pub async fn receive_json<T: for<'de> Deserialize<'de>>(frame: &mut Frame) -> Result<T> {
    let bytes = frame
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("protocol connection closed"))??;
    serde_json::from_slice(&bytes).context("decode protocol message")
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
}
