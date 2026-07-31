use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    paths::ServedPaths,
    protocol::{
        Frame, HandoffStream, Request, Response, ServiceInfo, Target, connect, into_handoff,
        receive_json, send_json,
    },
};
use anyhow::{Context, Result, bail};
use thiserror::Error;

pub struct AttachSession {
    pub stream: HandoffStream,
    pub token: String,
}

#[derive(Debug, Clone, Error)]
#[error(
    "attach: service {name:?} is not running after {recent_failures} failures in {window_seconds} seconds"
)]
pub struct AttachUnavailable {
    pub name: String,
    pub recent_failures: u32,
    pub window_seconds: u64,
    pub latest_log: Option<PathBuf>,
}

pub async fn request(paths: &ServedPaths, request: Request) -> Result<Response> {
    crate::protocol::request(&paths.socket_path(), request).await
}

pub async fn attach(paths: &ServedPaths, name: String) -> Result<AttachSession> {
    let mut frame = connect(&paths.socket_path()).await?;
    send_json(&mut frame, &Request::Attach { name }).await?;
    let token = match receive_json::<Response>(&mut frame).await? {
        Response::Attach { token } => token,
        Response::AttachUnavailable {
            name,
            recent_failures,
            window_seconds,
            latest_log,
        } => {
            return Err(AttachUnavailable {
                name,
                recent_failures,
                window_seconds,
                latest_log: latest_log.map(PathBuf::from),
            }
            .into());
        }
        Response::Error { message } => anyhow::bail!("attach: {message}"),
        response => anyhow::bail!("unexpected attach response: {response:?}"),
    };
    Ok(AttachSession {
        stream: into_handoff(frame)?,
        token,
    })
}

pub async fn attach_current(
    paths: &ServedPaths,
    directory: impl AsRef<Path>,
) -> Result<(String, AttachSession)> {
    let directory = fs::canonicalize(directory).context("canonicalize current directory")?;
    let response = request(paths, Request::List).await?;
    let Response::Services { services } = response else {
        bail!("unexpected manager response while resolving current service")
    };
    let name = service_name_for_directory(&services, &directory)?;
    let session = attach(paths, name.clone()).await?;
    Ok((name, session))
}

pub async fn open_resize_control(paths: &ServedPaths) -> Result<Frame> {
    connect(&paths.socket_path()).await
}

pub async fn send_resize(
    frame: &mut Frame,
    name: &str,
    token: &str,
    cols: u16,
    rows: u16,
) -> Result<()> {
    send_json(
        frame,
        &Request::Resize {
            name: name.to_owned(),
            token: token.to_owned(),
            cols,
            rows,
        },
    )
    .await?;
    match receive_json::<Response>(frame).await? {
        Response::Ok => Ok(()),
        Response::Error { message } => bail!("resize: {message}"),
        response => bail!("unexpected resize response: {response:?}"),
    }
}

fn service_name_for_directory(services: &[ServiceInfo], directory: &Path) -> Result<String> {
    services
        .iter()
        .find(|service| Path::new(&service.directory) == directory)
        .map(|service| service.name.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no enabled service for current directory {}",
                directory.display()
            )
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ServiceState;

    fn service(directory: &str, name: &str) -> ServiceInfo {
        ServiceInfo {
            name: name.to_owned(),
            directory: directory.to_owned(),
            state: ServiceState::Running,
            pid: Some(42),
            tty: true,
            restart: "never".to_owned(),
            persist_logs: false,
            attach_active: false,
            output_tail: String::new(),
        }
    }

    #[test]
    fn resolves_current_directory_to_enabled_service_name() {
        let services = vec![service("/srv/api", "api")];
        assert_eq!(
            service_name_for_directory(&services, Path::new("/srv/api")).expect("service"),
            "api"
        );
    }

    #[test]
    fn rejects_directory_without_enabled_service() {
        let services = vec![service("/srv/api", "api")];
        let error = service_name_for_directory(&services, Path::new("/srv/worker"))
            .expect_err("directory must not resolve");
        assert!(error.to_string().contains("no enabled service"));
    }
}
