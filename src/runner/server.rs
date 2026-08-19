use std::{
    fs,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use nix::unistd::setsid;
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot, watch},
};
use tracing::{info, warn};

use super::{AttachError, CRASH_WINDOW, RunnerCommand, RunnerState, initial_status};
use crate::{
    ipc::{framed, into_handoff, receive_json, send_json},
    runner_protocol::{RUNNER_PROTOCOL_VERSION, RunnerRequest, RunnerResponse, RunnerStatus},
    worker::WORKER_EVENT_CAPACITY,
};

pub async fn run(name: String, socket_path: PathBuf) -> Result<()> {
    let _ = setsid();
    let Some(parent) = socket_path.parent() else {
        bail!("runner socket has no parent directory");
    };
    fs::create_dir_all(parent).context("create runner directory")?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .context("restrict runner directory")?;
    prepare_socket(&socket_path).await?;
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind runner socket {}", socket_path.display()))?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
        .context("set runner socket permissions")?;

    let (events, mut event_receiver) = mpsc::channel(WORKER_EVENT_CAPACITY);
    let (commands, mut command_receiver) = mpsc::channel(64);
    let (status_updates, _) = watch::channel(initial_status(&name));
    let mut state = RunnerState::new(
        name.clone(),
        socket_path.clone(),
        events,
        status_updates.clone(),
    );
    info!(service = %name, "served runner is ready");

    let result = loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accept runner connection")?;
                let commands = commands.clone();
                let name = name.clone();
                let status_updates = status_updates.clone();
                tokio::spawn(async move {
                    if let Err(error) =
                        handle_connection(stream, commands, name, status_updates).await
                    {
                        warn!(%error, "runner connection ended with error");
                    }
                });
            }
            Some(command) = command_receiver.recv() => match command {
                RunnerCommand::Request { request, reply } => {
                    let result = state.handle_request(request).await;
                    state.publish_status();
                    let _ = reply.send(result);
                }
                RunnerCommand::PrepareAttach { reply } => {
                    let result = state.prepare_attach();
                    state.publish_status();
                    let _ = reply.send(result);
                }
                RunnerCommand::AttachStream { token, stream } => {
                    state.attach_stream(token, stream).await;
                }
                RunnerCommand::Exit => break Ok(()),
            },
            Some(event) = event_receiver.recv() => state.handle_event(event),
            else => break Ok(()),
        }
    };

    let _ = state.stop().await;
    let _ = fs::remove_file(&socket_path);
    result
}

async fn prepare_socket(socket_path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(socket_path) {
        if !metadata.file_type().is_socket() {
            bail!(
                "runner socket path is not a socket: {}",
                socket_path.display()
            );
        }
        match UnixStream::connect(socket_path).await {
            Ok(_) => bail!("runner socket is already in use: {}", socket_path.display()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                fs::remove_file(socket_path).context("remove stale runner socket")?;
            }
            Err(error) => return Err(error).context("check existing runner socket"),
        }
    }
    Ok(())
}

async fn handle_connection(
    stream: UnixStream,
    commands: mpsc::Sender<RunnerCommand>,
    expected_name: String,
    status_updates: watch::Sender<RunnerStatus>,
) -> Result<()> {
    let mut frame = framed(stream);
    let hello = receive_json::<RunnerRequest>(&mut frame).await?;
    match hello {
        RunnerRequest::Hello { version, name }
            if version == RUNNER_PROTOCOL_VERSION && name == expected_name =>
        {
            send_json(
                &mut frame,
                &RunnerResponse::Hello {
                    version: RUNNER_PROTOCOL_VERSION,
                    name: expected_name.clone(),
                },
            )
            .await?;
        }
        RunnerRequest::Hello { version, name: _ } if version != RUNNER_PROTOCOL_VERSION => {
            send_json(
                &mut frame,
                &RunnerResponse::Error {
                    message: format!("unsupported runner protocol version {version}"),
                },
            )
            .await?;
            return Ok(());
        }
        RunnerRequest::Hello { name, .. } => {
            send_json(
                &mut frame,
                &RunnerResponse::Error {
                    message: format!("runner belongs to service {name:?}, not {expected_name:?}"),
                },
            )
            .await?;
            return Ok(());
        }
        _ => {
            send_json(
                &mut frame,
                &RunnerResponse::Error {
                    message: "runner handshake required".to_owned(),
                },
            )
            .await?;
            return Ok(());
        }
    }

    loop {
        let request = match receive_json::<RunnerRequest>(&mut frame).await {
            Ok(request) => request,
            Err(_) => return Ok(()),
        };
        if matches!(request, RunnerRequest::WatchStatus) {
            let mut statuses = status_updates.subscribe();
            let status = statuses.borrow().clone();
            send_json(&mut frame, &RunnerResponse::Status { status }).await?;
            loop {
                statuses
                    .changed()
                    .await
                    .context("runner status watch closed")?;
                let status = statuses.borrow_and_update().clone();
                send_json(&mut frame, &RunnerResponse::Status { status }).await?;
            }
        }
        if matches!(request, RunnerRequest::Attach) {
            let (reply, receiver) = oneshot::channel();
            commands
                .send(RunnerCommand::PrepareAttach { reply })
                .await
                .context("prepare runner attach")?;
            match receiver.await.context("receive runner attach response")? {
                Ok(token) => {
                    send_json(
                        &mut frame,
                        &RunnerResponse::Attach {
                            token: token.clone(),
                        },
                    )
                    .await?;
                    let stream = into_handoff(frame)?;
                    commands
                        .send(RunnerCommand::AttachStream { token, stream })
                        .await
                        .context("send runner attach stream")?;
                }
                Err(AttachError::Message(message)) => {
                    send_json(&mut frame, &RunnerResponse::Error { message }).await?;
                }
                Err(AttachError::CrashLoop {
                    name,
                    recent_failures,
                    latest_log,
                }) => {
                    send_json(
                        &mut frame,
                        &RunnerResponse::AttachUnavailable {
                            name,
                            recent_failures,
                            window_seconds: CRASH_WINDOW.as_secs(),
                            latest_log,
                        },
                    )
                    .await?;
                }
            }
            return Ok(());
        }

        let (reply, receiver) = oneshot::channel();
        let stop = matches!(request, RunnerRequest::Stop);
        commands
            .send(RunnerCommand::Request { request, reply })
            .await
            .context("send runner request")?;
        let response = receiver.await.context("receive runner response")?;
        let response = match response {
            Ok(response) => response,
            Err(message) => RunnerResponse::Error { message },
        };
        send_json(&mut frame, &response).await?;
        if stop {
            commands.send(RunnerCommand::Exit).await.ok();
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ipc::{framed, receive_json, send_json},
        runner_protocol::RunnerServiceState,
    };

    #[tokio::test]
    async fn watch_status_streams_the_initial_value_and_changes() {
        let (server, client) = UnixStream::pair().expect("unix stream pair");
        let (commands, _receiver) = mpsc::channel(1);
        let (statuses, _) = watch::channel(initial_status("api"));
        let server_task = tokio::spawn(handle_connection(
            server,
            commands,
            "api".to_owned(),
            statuses.clone(),
        ));
        let mut client = framed(client);

        send_json(
            &mut client,
            &RunnerRequest::Hello {
                version: RUNNER_PROTOCOL_VERSION,
                name: "api".to_owned(),
            },
        )
        .await
        .expect("send hello");
        assert!(matches!(
            receive_json::<RunnerResponse>(&mut client)
                .await
                .expect("hello response"),
            RunnerResponse::Hello { .. }
        ));
        send_json(&mut client, &RunnerRequest::WatchStatus)
            .await
            .expect("send watch");
        assert!(matches!(
            receive_json::<RunnerResponse>(&mut client)
                .await
                .expect("initial status"),
            RunnerResponse::Status { status }
                if matches!(status.state, RunnerServiceState::Stopped)
        ));

        let mut running = initial_status("api");
        running.state = RunnerServiceState::Running;
        running.pid = Some(42);
        statuses.send_replace(running);
        assert!(matches!(
            receive_json::<RunnerResponse>(&mut client)
                .await
                .expect("changed status"),
            RunnerResponse::Status { status }
                if matches!(status.state, RunnerServiceState::Running) && status.pid == Some(42)
        ));

        server_task.abort();
    }
}
