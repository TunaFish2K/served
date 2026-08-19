use std::{
    io,
    pin::Pin,
    task::{Context as TaskContext, Poll},
};

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::UnixStream,
};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub const MAX_FRAME_LENGTH: usize = 1024 * 1024;

pub type Frame = Framed<UnixStream, LengthDelimitedCodec>;

#[derive(Debug)]
pub struct HandoffStream {
    inner: UnixStream,
    read_buf: Vec<u8>,
    read_offset: usize,
}

pub fn into_handoff(frame: Frame) -> Result<HandoffStream> {
    let parts = frame.into_parts();
    if !parts.write_buf.is_empty() {
        bail!("protocol write buffer is not empty during raw socket handoff");
    }
    Ok(HandoffStream {
        inner: parts.io,
        read_buf: parts.read_buf.to_vec(),
        read_offset: 0,
    })
}

impl AsyncRead for HandoffStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.read_offset < self.read_buf.len() {
            let count = (self.read_buf.len() - self.read_offset).min(buffer.remaining());
            if count > 0 {
                let end = self.read_offset + count;
                buffer.put_slice(&self.read_buf[self.read_offset..end]);
                self.read_offset = end;
                return Poll::Ready(Ok(()));
            }
        }
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}

impl AsyncWrite for HandoffStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

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
