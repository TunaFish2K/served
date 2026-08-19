use std::{path::PathBuf, time::Duration};

use tokio::{sync::mpsc, task::JoinHandle, time::sleep};

use crate::{
    ipc::{receive_json, send_json},
    runner_protocol::{RunnerRequest, RunnerResponse, RunnerStatus},
};

const LEGACY_STATUS_INTERVAL: Duration = Duration::from_secs(1);

pub(super) enum RunnerUpdate {
    Status {
        name: String,
        generation: u64,
        status: Box<RunnerStatus>,
    },
    Unavailable {
        name: String,
        generation: u64,
        error: String,
    },
}

pub(super) fn spawn(
    socket: PathBuf,
    name: String,
    generation: u64,
    updates: mpsc::Sender<RunnerUpdate>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut frame = match crate::runner_protocol::connect(&socket, &name).await {
            Ok(frame) => frame,
            Err(_) => {
                poll_legacy(socket, name, generation, updates).await;
                return;
            }
        };
        if send_json(&mut frame, &RunnerRequest::WatchStatus)
            .await
            .is_err()
        {
            poll_legacy(socket, name, generation, updates).await;
            return;
        }

        let first = match receive_json::<RunnerResponse>(&mut frame).await {
            Ok(RunnerResponse::Status { status }) => status,
            _ => {
                poll_legacy(socket, name, generation, updates).await;
                return;
            }
        };
        if updates
            .send(RunnerUpdate::Status {
                name: name.clone(),
                generation,
                status: Box::new(first),
            })
            .await
            .is_err()
        {
            return;
        }

        loop {
            match receive_json::<RunnerResponse>(&mut frame).await {
                Ok(RunnerResponse::Status { status }) => {
                    if updates
                        .send(RunnerUpdate::Status {
                            name: name.clone(),
                            generation,
                            status: Box::new(status),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(_) | Err(_) => {
                    poll_legacy(socket, name, generation, updates).await;
                    return;
                }
            }
        }
    })
}

async fn poll_legacy(
    socket: PathBuf,
    name: String,
    generation: u64,
    updates: mpsc::Sender<RunnerUpdate>,
) {
    loop {
        let update =
            match crate::runner_protocol::request(&socket, &name, RunnerRequest::Status).await {
                Ok(RunnerResponse::Status { status }) => RunnerUpdate::Status {
                    name: name.clone(),
                    generation,
                    status: Box::new(status),
                },
                Ok(response) => RunnerUpdate::Unavailable {
                    name: name.clone(),
                    generation,
                    error: format!("unexpected runner status response: {response:?}"),
                },
                Err(error) => RunnerUpdate::Unavailable {
                    name: name.clone(),
                    generation,
                    error: error.to_string(),
                },
            };
        if updates.send(update).await.is_err() {
            return;
        }
        sleep(LEGACY_STATUS_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ipc::{framed, receive_json, send_json},
        runner_protocol::{RUNNER_PROTOCOL_VERSION, RunnerServiceState},
    };
    use tempfile::tempdir;
    use tokio::{net::UnixListener, time::timeout};

    fn stopped_status() -> RunnerStatus {
        RunnerStatus {
            name: "api".to_owned(),
            runner_pid: 42,
            state: RunnerServiceState::Stopped,
            pid: None,
            pid_start_time: None,
            tty: false,
            restart: "no".to_owned(),
            persist_logs: false,
            attach_active: false,
            output_tail: String::new(),
            recent_failures: 0,
            window_seconds: 60,
            latest_log: None,
            spec: None,
        }
    }

    async fn handshake(stream: tokio::net::UnixStream) -> crate::ipc::Frame {
        let mut frame = framed(stream);
        assert!(matches!(
            receive_json::<RunnerRequest>(&mut frame)
                .await
                .expect("runner hello"),
            RunnerRequest::Hello { version, ref name }
                if version == RUNNER_PROTOCOL_VERSION && name == "api"
        ));
        send_json(
            &mut frame,
            &RunnerResponse::Hello {
                version: RUNNER_PROTOCOL_VERSION,
                name: "api".to_owned(),
            },
        )
        .await
        .expect("hello response");
        frame
    }

    #[tokio::test]
    async fn falls_back_to_status_polling_for_an_older_v1_runner() {
        let directory = tempdir().expect("tempdir");
        let socket = directory.path().join("runner.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake runner");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("watch connection");
            let mut frame = handshake(stream).await;
            assert!(matches!(
                receive_json::<RunnerRequest>(&mut frame)
                    .await
                    .expect("watch request"),
                RunnerRequest::WatchStatus
            ));
            drop(frame);

            let (stream, _) = listener.accept().await.expect("legacy status connection");
            let mut frame = handshake(stream).await;
            assert!(matches!(
                receive_json::<RunnerRequest>(&mut frame)
                    .await
                    .expect("status request"),
                RunnerRequest::Status
            ));
            send_json(
                &mut frame,
                &RunnerResponse::Status {
                    status: stopped_status(),
                },
            )
            .await
            .expect("status response");
        });

        let (updates, mut receiver) = mpsc::channel(4);
        let watcher = spawn(socket, "api".to_owned(), 7, updates);
        let update = timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("watcher timeout")
            .expect("watcher update");
        assert!(matches!(
            update,
            RunnerUpdate::Status {
                ref name,
                generation: 7,
                ref status,
            } if name == "api" && matches!(status.state, RunnerServiceState::Stopped)
        ));

        watcher.abort();
        server.await.expect("fake runner task");
    }

    #[tokio::test]
    async fn keeps_polling_after_an_established_watch_disconnects() {
        let directory = tempdir().expect("tempdir");
        let socket = directory.path().join("runner.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake runner");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("watch connection");
            let mut frame = handshake(stream).await;
            assert!(matches!(
                receive_json::<RunnerRequest>(&mut frame)
                    .await
                    .expect("watch request"),
                RunnerRequest::WatchStatus
            ));
            send_json(
                &mut frame,
                &RunnerResponse::Status {
                    status: stopped_status(),
                },
            )
            .await
            .expect("initial status");
            drop(frame);

            let (stream, _) = listener.accept().await.expect("recovery status connection");
            let mut frame = handshake(stream).await;
            assert!(matches!(
                receive_json::<RunnerRequest>(&mut frame)
                    .await
                    .expect("status request"),
                RunnerRequest::Status
            ));
            let mut status = stopped_status();
            status.state = RunnerServiceState::Running;
            status.pid = Some(99);
            send_json(&mut frame, &RunnerResponse::Status { status })
                .await
                .expect("status response");
        });

        let (updates, mut receiver) = mpsc::channel(4);
        let watcher = spawn(socket, "api".to_owned(), 9, updates);
        assert!(matches!(
            timeout(Duration::from_secs(2), receiver.recv())
                .await
                .expect("initial update timeout")
                .expect("initial update"),
            RunnerUpdate::Status { ref status, .. }
                if matches!(status.state, RunnerServiceState::Stopped)
        ));
        assert!(matches!(
            timeout(Duration::from_secs(2), receiver.recv())
                .await
                .expect("recovery update timeout")
                .expect("recovery update"),
            RunnerUpdate::Status {
                generation: 9,
                ref status,
                ..
            } if matches!(status.state, RunnerServiceState::Running) && status.pid == Some(99)
        ));

        watcher.abort();
        server.await.expect("fake runner task");
    }
}
