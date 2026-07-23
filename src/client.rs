use anyhow::{Result, bail};
use tokio::net::UnixStream;

use crate::{
    paths::ServedPaths,
    protocol::{Request, Response, Target, connect, receive_json, send_json},
};

pub async fn request(paths: &ServedPaths, request: Request) -> Result<Response> {
    crate::protocol::request(&paths.socket_path(), request).await
}

pub async fn attach(paths: &ServedPaths, name: String) -> Result<UnixStream> {
    let mut frame = connect(&paths.socket_path()).await?;
    send_json(&mut frame, &Request::Attach { name }).await?;
    match receive_json::<Response>(&mut frame).await? {
        Response::Ok => {}
        Response::Error { message } => anyhow::bail!("attach: {message}"),
        response => anyhow::bail!("unexpected attach response: {response:?}"),
    }
    Ok(frame.into_inner())
}

pub fn target(name: Option<String>, directory: std::path::PathBuf) -> Target {
    name.map(Target::Name)
        .unwrap_or_else(|| Target::Directory(directory.display().to_string()))
}

pub async fn expect_ok(paths: &ServedPaths, command: Request) -> Result<()> {
    match request(paths, command).await? {
        Response::Ok => Ok(()),
        Response::Error { message } => bail!("{message}"),
        response => bail!("unexpected manager response: {response:?}"),
    }
}
