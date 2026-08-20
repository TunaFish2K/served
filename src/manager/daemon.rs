use std::{
    fs,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use tokio::{
    io::copy_bidirectional,
    net::{UnixListener, UnixStream},
    signal::unix::{SignalKind, signal},
    sync::{mpsc, oneshot},
};
use tracing::{info, warn};

use super::{AttachSetup, DaemonExit, ManagerState, PrepareAttachError};
use crate::{
    config::manager_environment,
    paths::ServedPaths,
    protocol::{HandoffStream, Request, Response, framed, into_handoff, receive_json, send_json},
};

enum ManagerCommand {
    Request {
        request: Request,
        reply: oneshot::Sender<std::result::Result<Response, String>>,
    },
    Attach {
        name: String,
        reply: oneshot::Sender<std::result::Result<AttachSetup, PrepareAttachError>>,
    },
    Shutdown {
        reply: oneshot::Sender<std::result::Result<(), String>>,
        finished: oneshot::Receiver<()>,
    },
    Handoff {
        executable: PathBuf,
    },
    Relinquish,
}

pub async fn run_daemon(paths: ServedPaths) -> Result<DaemonExit> {
    bootstrap_paths(&paths)?;
    let (listener, socket_path, generation_path) = bind_manager_socket(&paths).await?;
    let (commands, mut command_receiver) = mpsc::channel(64);
    let (runner_updates, mut runner_update_receiver) = mpsc::channel(256);
    let mut state = ManagerState::new(paths, manager_environment(), runner_updates);
    state.restore_services().await;
    info!("served manager is ready");

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
    let mut handoff_executable = None;
    let mut relinquished = false;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accept manager connection")?;
                let commands = commands.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, commands).await {
                        warn!(%error, "manager connection ended with error");
                    }
                });
            }
            Some(command) = command_receiver.recv() => match command {
                ManagerCommand::Request { request, reply } => {
                    let result = state.handle_request(request).await;
                    let _ = reply.send(result);
                }
                ManagerCommand::Attach { name, reply } => {
                    let _ = reply.send(state.prepare_attach(name).await);
                }
                ManagerCommand::Shutdown { reply, finished } => {
                    let result = state.stop_all().await;
                    let _ = reply.send(result);
                    let _ = finished.await;
                    break;
                }
                ManagerCommand::Handoff { executable } => {
                    handoff_executable = Some(executable);
                    break;
                }
                ManagerCommand::Relinquish => {
                    relinquished = true;
                    break;
                }
            },
            Some(update) = runner_update_receiver.recv() => {
                state.handle_runner_update(update).await;
            }
            _ = sigterm.recv() => {
                let _ = state.stop_all().await;
                break;
            }
            _ = sigint.recv() => {
                let _ = state.stop_all().await;
                break;
            }
            else => break,
        }
    }

    drop(listener);
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(&generation_path);
    if let Some(executable) = handoff_executable {
        reexec_manager(&executable).context("handoff manager process")?;
    }
    Ok(if relinquished {
        DaemonExit::Relinquished
    } else {
        DaemonExit::Stopped
    })
}

fn bootstrap_paths(paths: &ServedPaths) -> Result<()> {
    fs::create_dir_all(paths.registry_dir()).context("create served enable registry")?;
    let state_served_dir = paths.state_home.join("served");
    fs::create_dir_all(&state_served_dir).context("create served state directory")?;
    fs::set_permissions(&state_served_dir, fs::Permissions::from_mode(0o700))
        .context("restrict served state directory")?;
    fs::create_dir_all(&paths.runtime_dir).context("create served runtime directory")?;
    fs::set_permissions(&paths.runtime_dir, fs::Permissions::from_mode(0o700))
        .context("restrict served runtime directory")?;
    fs::create_dir_all(paths.runners_dir()).context("create served runner directory")?;
    fs::set_permissions(paths.runners_dir(), fs::Permissions::from_mode(0o700))
        .context("restrict served runner directory")?;
    let log_directory = paths.logs_dir();
    if let Err(error) = fs::create_dir_all(&log_directory) {
        warn!(path = %log_directory.display(), %error, "cannot create log directory; persistent logs will fall back to memory");
    } else if let Err(error) =
        fs::set_permissions(&log_directory, fs::Permissions::from_mode(0o700))
    {
        warn!(path = %log_directory.display(), %error, "cannot restrict served log directory");
    }
    Ok(())
}

async fn bind_manager_socket(paths: &ServedPaths) -> Result<(UnixListener, PathBuf, PathBuf)> {
    let socket_path = paths.socket_path();
    if let Ok(metadata) = fs::symlink_metadata(&socket_path) {
        if metadata.file_type().is_socket() {
            match UnixStream::connect(&socket_path).await {
                Ok(_) => bail!(
                    "manager already running; socket is in use at {}",
                    socket_path.display()
                ),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
                {
                    fs::remove_file(&socket_path).context("remove stale manager socket")?;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("check existing manager socket {}", socket_path.display())
                    });
                }
            }
        } else {
            bail!(
                "manager socket path is not a socket: {}",
                socket_path.display()
            );
        }
    }
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind manager socket {}", socket_path.display()))?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
        .context("set manager socket permissions")?;
    let generation_path = paths.manager_generation();
    let generation = format!("{:032x}", rand::random::<u128>());
    fs::write(&generation_path, generation)
        .with_context(|| format!("write manager generation {}", generation_path.display()))?;
    fs::set_permissions(&generation_path, fs::Permissions::from_mode(0o600))
        .context("set manager generation permissions")?;
    Ok((listener, socket_path, generation_path))
}

async fn handle_connection(
    stream: UnixStream,
    commands: mpsc::Sender<ManagerCommand>,
) -> Result<()> {
    let mut frame = framed(stream);
    let hello = receive_json::<Request>(&mut frame).await?;
    match hello {
        Request::Hello { version } if version == crate::protocol::PROTOCOL_VERSION => {
            send_json(
                &mut frame,
                &Response::Hello {
                    version: crate::protocol::PROTOCOL_VERSION,
                },
            )
            .await?;
        }
        Request::Hello { version } => {
            send_json(
                &mut frame,
                &Response::Error {
                    message: format!("unsupported protocol version {version}"),
                },
            )
            .await?;
            return Ok(());
        }
        _ => {
            send_json(
                &mut frame,
                &Response::Error {
                    message: "protocol handshake required".to_owned(),
                },
            )
            .await?;
            return Ok(());
        }
    }

    loop {
        let request = match receive_json::<Request>(&mut frame).await {
            Ok(request) => request,
            Err(_) => return Ok(()),
        };
        match request {
            Request::Attach { name } => {
                let (reply, receiver) = oneshot::channel();
                commands
                    .send(ManagerCommand::Attach { name, reply })
                    .await
                    .context("prepare manager attach")?;
                match receiver.await.context("receive manager attach response")? {
                    Ok(setup) => {
                        send_json(&mut frame, &Response::Attach { token: setup.token }).await?;
                        let client_stream = into_handoff(frame).context("handoff client socket")?;
                        proxy_attach(client_stream, setup.runner_stream).await;
                    }
                    Err(PrepareAttachError::Message(message)) => {
                        send_json(&mut frame, &Response::Error { message }).await?;
                    }
                    Err(PrepareAttachError::CrashLoop {
                        name,
                        recent_failures,
                        latest_log,
                    }) => {
                        send_json(
                            &mut frame,
                            &Response::AttachUnavailable {
                                name,
                                recent_failures,
                                window_seconds: 60,
                                latest_log,
                            },
                        )
                        .await?;
                    }
                }
                return Ok(());
            }
            Request::ManagerHandoff { executable } => {
                let executable = match validate_handoff_executable(&executable) {
                    Ok(executable) => executable,
                    Err(message) => {
                        send_json(&mut frame, &Response::Error { message }).await?;
                        return Ok(());
                    }
                };
                send_json(&mut frame, &Response::Ok).await?;
                commands
                    .send(ManagerCommand::Handoff { executable })
                    .await
                    .context("request manager handoff")?;
                return Ok(());
            }
            Request::ManagerRelinquish => {
                send_json(&mut frame, &Response::Ok).await?;
                commands
                    .send(ManagerCommand::Relinquish)
                    .await
                    .context("request manager relinquish")?;
                return Ok(());
            }
            Request::ManagerShutdown => {
                let (reply, receiver) = oneshot::channel();
                let (finished_sender, finished_receiver) = oneshot::channel();
                commands
                    .send(ManagerCommand::Shutdown {
                        reply,
                        finished: finished_receiver,
                    })
                    .await
                    .context("request manager shutdown")?;
                let response = match receiver.await.context("receive manager shutdown")? {
                    Ok(()) => Response::Ok,
                    Err(message) => Response::Error { message },
                };
                let _ = send_json(&mut frame, &response).await;
                let _ = finished_sender.send(());
                return Ok(());
            }
            request => {
                let (reply, receiver) = oneshot::channel();
                commands
                    .send(ManagerCommand::Request { request, reply })
                    .await
                    .context("send command to manager")?;
                let response = match receiver.await.context("receive manager response")? {
                    Ok(response) => response,
                    Err(message) => Response::Error { message },
                };
                send_json(&mut frame, &response).await?;
            }
        }
    }
}

async fn proxy_attach(mut client: HandoffStream, mut runner: HandoffStream) {
    let _ = copy_bidirectional(&mut client, &mut runner).await;
}

fn validate_handoff_executable(value: &str) -> std::result::Result<PathBuf, String> {
    let executable = PathBuf::from(value);
    if !executable.is_absolute() {
        return Err("manager handoff executable must be an absolute path".to_owned());
    }
    let metadata = fs::metadata(&executable)
        .map_err(|error| format!("inspect manager handoff executable: {error}"))?;
    if !metadata.is_file() {
        return Err("manager handoff executable is not a regular file".to_owned());
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err("manager handoff executable is not executable".to_owned());
    }
    Ok(executable)
}

fn reexec_manager(binary: &Path) -> Result<()> {
    let error = Command::new(binary).arg("daemon").exec();
    Err(error).context("exec new served daemon")
}
