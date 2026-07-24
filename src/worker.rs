use std::{
    io::{Read, Write},
    time::Duration,
};

use anyhow::Result;
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::net::UnixStream;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
    time::{sleep, timeout},
};

use crate::{config::LoadedService, protocol::Target};

const TERMINATION_TIMEOUT: Duration = Duration::from_secs(5);
const BACKOFF_BASE: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum WorkerCommand {
    Stop {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Restart {
        service: LoadedService,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Attach {
        stream: UnixStream,
        replay: Vec<u8>,
    },
}

#[derive(Debug)]
pub enum WorkerEvent {
    Starting {
        name: String,
        persist_logs: bool,
    },
    Started {
        name: String,
        pid: u32,
        tty: bool,
    },
    Output {
        name: String,
        bytes: Vec<u8>,
    },
    Exited {
        name: String,
        code: Option<i32>,
        success: bool,
    },
    Restarting {
        name: String,
        delay: Duration,
    },
    Stopped {
        name: String,
    },
    Failed {
        name: String,
        error: String,
    },
    AttachChanged {
        name: String,
        active: bool,
    },
}

#[derive(Debug)]
enum ProcessOutcome {
    Exited { code: Option<i32>, success: bool },
    Stopped,
    Restart { service: LoadedService },
}

#[derive(Debug)]
enum BackoffOutcome {
    Timer,
    Stop,
    Restart(LoadedService),
}

pub fn spawn_service(
    service: LoadedService,
    manager_environment: std::collections::BTreeMap<String, String>,
    events: mpsc::UnboundedSender<WorkerEvent>,
) -> mpsc::Sender<WorkerCommand> {
    let (commands, receiver) = mpsc::channel(16);
    tokio::spawn(run_service(service, manager_environment, events, receiver));
    commands
}

async fn run_service(
    mut service: LoadedService,
    manager_environment: std::collections::BTreeMap<String, String>,
    events: mpsc::UnboundedSender<WorkerEvent>,
    mut commands: mpsc::Receiver<WorkerCommand>,
) {
    let mut attempt = 0_u32;

    loop {
        let name = service.config.name.clone();
        let _ = events.send(WorkerEvent::Starting {
            name: name.clone(),
            persist_logs: service.config.persist_logs,
        });
        let process = if service.config.tty {
            run_pty(service.clone(), events.clone(), &mut commands).await
        } else {
            run_pipe(
                service.clone(),
                manager_environment.clone(),
                events.clone(),
                &mut commands,
            )
            .await
        };

        let outcome = match process {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = events.send(WorkerEvent::Failed {
                    name: name.clone(),
                    error,
                });
                if !service.config.restart.should_restart(false) {
                    let _ = events.send(WorkerEvent::Stopped { name });
                    return;
                }
                attempt = attempt.saturating_add(1);
                let delay = backoff(attempt);
                let _ = events.send(WorkerEvent::Restarting {
                    name: service.config.name.clone(),
                    delay,
                });
                match wait_backoff(delay, &mut commands).await {
                    BackoffOutcome::Timer => continue,
                    BackoffOutcome::Stop => return,
                    BackoffOutcome::Restart(next) => {
                        attempt = 0;
                        service = next;
                        continue;
                    }
                }
            }
        };

        match outcome {
            ProcessOutcome::Stopped => {
                let _ = events.send(WorkerEvent::Stopped { name });
                return;
            }
            ProcessOutcome::Restart { service: next } => {
                attempt = 0;
                service = next;
            }
            ProcessOutcome::Exited { code, success } => {
                let _ = events.send(WorkerEvent::Exited {
                    name: name.clone(),
                    code,
                    success,
                });
                if !service.config.restart.should_restart(success) {
                    let _ = events.send(WorkerEvent::Stopped { name });
                    return;
                }
                attempt = attempt.saturating_add(1);
                let delay = backoff(attempt);
                let _ = events.send(WorkerEvent::Restarting {
                    name: name.clone(),
                    delay,
                });
                match wait_backoff(delay, &mut commands).await {
                    BackoffOutcome::Timer => {}
                    BackoffOutcome::Stop => return,
                    BackoffOutcome::Restart(next) => {
                        attempt = 0;
                        service = next;
                    }
                }
            }
        }
    }
}

async fn wait_backoff(
    delay: Duration,
    commands: &mut mpsc::Receiver<WorkerCommand>,
) -> BackoffOutcome {
    tokio::select! {
        _ = sleep(delay) => BackoffOutcome::Timer,
        command = commands.recv() => match command {
            Some(WorkerCommand::Stop { reply }) => {
                let _ = reply.send(Ok(()));
                BackoffOutcome::Stop
            }
            Some(WorkerCommand::Restart { service, reply }) => {
                let _ = reply.send(Ok(()));
                BackoffOutcome::Restart(service)
            }
            Some(WorkerCommand::Attach { stream, .. }) => {
                drop(stream);
                BackoffOutcome::Timer
            }
            None => BackoffOutcome::Stop,
        }
    }
}

async fn run_pipe(
    service: LoadedService,
    manager_environment: std::collections::BTreeMap<String, String>,
    events: mpsc::UnboundedSender<WorkerEvent>,
    commands: &mut mpsc::Receiver<WorkerCommand>,
) -> Result<ProcessOutcome, String> {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(&service.config.command)
        .current_dir(&service.directory)
        .env_clear()
        .envs(manager_environment)
        .envs(service.environment.clone())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let pid = child
        .id()
        .ok_or_else(|| "managed shell did not expose a pid".to_owned())?;
    let (output_tx, _) = broadcast::channel::<Vec<u8>>(64);
    let stdout_task = child.stdout.take().map(|reader| {
        spawn_pipe_reader(
            reader,
            service.config.name.clone(),
            events.clone(),
            output_tx.clone(),
        )
    });
    let stderr_task = child.stderr.take().map(|reader| {
        spawn_pipe_reader(
            reader,
            service.config.name.clone(),
            events.clone(),
            output_tx.clone(),
        )
    });
    let name = service.config.name.clone();
    let _ = events.send(WorkerEvent::Started {
        name: name.clone(),
        pid,
        tty: false,
    });
    let mut wait_task = tokio::spawn(async move { child.wait().await });
    let mut attach_count = 0_usize;
    let (attach_done_tx, mut attach_done_rx) = mpsc::unbounded_channel::<()>();

    loop {
        tokio::select! {
            result = &mut wait_task => {
                abort_reader(stdout_task);
                abort_reader(stderr_task);
                if attach_count > 0 {
                    let _ = events.send(WorkerEvent::AttachChanged {
                        name: name.clone(),
                        active: false,
                    });
                }
                let status = result
                    .map_err(|error| error.to_string())?
                    .map_err(|error| error.to_string())?;
                return Ok(ProcessOutcome::Exited {
                    code: status.code(),
                    success: status.success(),
                });
            }
            command = commands.recv() => match command {
                Some(WorkerCommand::Stop { reply }) => {
                    terminate_and_reap(pid, &mut wait_task).await;
                    abort_reader(stdout_task);
                    abort_reader(stderr_task);
                    if attach_count > 0 {
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: false,
                        });
                    }
                    let _ = reply.send(Ok(()));
                    return Ok(ProcessOutcome::Stopped);
                }
                Some(WorkerCommand::Restart { service, reply }) => {
                    terminate_and_reap(pid, &mut wait_task).await;
                    abort_reader(stdout_task);
                    abort_reader(stderr_task);
                    if attach_count > 0 {
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: false,
                        });
                    }
                    let _ = reply.send(Ok(()));
                    return Ok(ProcessOutcome::Restart { service });
                }
                Some(WorkerCommand::Attach { stream, replay }) => {
                    if attach_count == 0 {
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: true,
                        });
                    }
                    attach_count += 1;
                    let output = output_tx.subscribe();
                    let done = attach_done_tx.clone();
                    tokio::spawn(async move {
                        relay_readonly_attach(stream, output, replay).await;
                        let _ = done.send(());
                    });
                }
                None => {
                    terminate_and_reap(pid, &mut wait_task).await;
                    abort_reader(stdout_task);
                    abort_reader(stderr_task);
                    if attach_count > 0 {
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: false,
                        });
                    }
                    return Ok(ProcessOutcome::Stopped);
                }
            },
            Some(_) = attach_done_rx.recv(), if attach_count > 0 => {
                attach_count -= 1;
                if attach_count == 0 {
                    let _ = events.send(WorkerEvent::AttachChanged {
                        name: name.clone(),
                        active: false,
                    });
                }
            }
        }
    }
}

fn spawn_pipe_reader<R>(
    mut reader: R,
    name: String,
    events: mpsc::UnboundedSender<WorkerEvent>,
    output: broadcast::Sender<Vec<u8>>,
) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(length) => {
                    let bytes = buffer[..length].to_vec();
                    let _ = output.send(bytes.clone());
                    let _ = events.send(WorkerEvent::Output {
                        name: name.clone(),
                        bytes,
                    });
                }
            }
        }
    })
}

async fn run_pty(
    service: LoadedService,
    events: mpsc::UnboundedSender<WorkerEvent>,
    commands: &mut mpsc::Receiver<WorkerCommand>,
) -> Result<ProcessOutcome, String> {
    let spawned = spawn_pty_process(&service).await?;
    let pid = spawned.pid;
    let name = service.config.name.clone();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (output_tx, _) = broadcast::channel::<Vec<u8>>(64);
    let reader_output = output_tx.clone();
    let reader_events = events.clone();
    let reader_name = name.clone();
    let reader_task = tokio::task::spawn_blocking(move || {
        let mut reader = spawned.reader;
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(length) => {
                    let bytes = buffer[..length].to_vec();
                    let _ = reader_output.send(bytes.clone());
                    let _ = reader_events.send(WorkerEvent::Output {
                        name: reader_name.clone(),
                        bytes,
                    });
                }
            }
        }
    });
    let writer_task = tokio::task::spawn_blocking(move || {
        let mut writer = spawned.writer;
        while let Some(bytes) = input_rx.blocking_recv() {
            if writer.write_all(&bytes).is_err() {
                return;
            }
            let _ = writer.flush();
        }
    });
    let _ = events.send(WorkerEvent::Started {
        name: name.clone(),
        pid,
        tty: true,
    });
    let mut status = spawned.status;
    let mut attach_active = false;
    let (attach_done_tx, mut attach_done_rx) = mpsc::unbounded_channel::<()>();

    loop {
        tokio::select! {
            result = &mut status => {
                reader_task.abort();
                writer_task.abort();
                if attach_active {
                    let _ = events.send(WorkerEvent::AttachChanged {
                        name: name.clone(),
                        active: false,
                    });
                }
                let (code, success) = result
                    .map_err(|error| error.to_string())?
                    .map(|code| (Some(code as i32), code == 0))
                    .unwrap_or((None, false));
                return Ok(ProcessOutcome::Exited { code, success });
            }
            Some(_) = attach_done_rx.recv(), if attach_active => {
                attach_active = false;
                let _ = events.send(WorkerEvent::AttachChanged {
                    name: name.clone(),
                    active: false,
                });
            }
            command = commands.recv() => match command {
                Some(WorkerCommand::Stop { reply }) => {
                    terminate_pty(pid, &mut status).await;
                    reader_task.abort();
                    writer_task.abort();
                    if attach_active {
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: false,
                        });
                    }
                    let _ = reply.send(Ok(()));
                    return Ok(ProcessOutcome::Stopped);
                }
                Some(WorkerCommand::Restart { service, reply }) => {
                    terminate_pty(pid, &mut status).await;
                    reader_task.abort();
                    writer_task.abort();
                    if attach_active {
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: false,
                        });
                    }
                    let _ = reply.send(Ok(()));
                    return Ok(ProcessOutcome::Restart { service });
                }
                Some(WorkerCommand::Attach { stream, replay }) => {
                    if attach_active {
                        drop(stream);
                    } else {
                        attach_active = true;
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: true,
                        });
                        let input = input_tx.clone();
                        let output = output_tx.subscribe();
                        let done = attach_done_tx.clone();
                        tokio::spawn(async move {
                            relay_attach(stream, input, output, replay).await;
                            let _ = done.send(());
                        });
                    }
                }
                None => {
                    terminate_pty(pid, &mut status).await;
                    reader_task.abort();
                    writer_task.abort();
                    if attach_active {
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: false,
                        });
                    }
                    return Ok(ProcessOutcome::Stopped);
                }
            }
        }
    }
}

struct PtySpawn {
    pid: u32,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    status: oneshot::Receiver<Result<u32, String>>,
}

async fn spawn_pty_process(service: &LoadedService) -> Result<PtySpawn, String> {
    let service = service.clone();
    tokio::task::spawn_blocking(move || {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| error.to_string())?;
        let mut command = CommandBuilder::new("/bin/sh");
        command.arg("-c");
        command.arg(&service.config.command);
        command.cwd(&service.directory);
        for (key, value) in &service.environment {
            command.env(key, value);
        }
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| error.to_string())?;
        let pid = child
            .process_id()
            .ok_or_else(|| "PTY child did not expose a pid".to_owned())?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| error.to_string())?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| error.to_string())?;
        let (status_tx, status_rx) = oneshot::channel();
        std::thread::spawn(move || {
            let status = child
                .wait()
                .map(|status| status.exit_code())
                .map_err(|error| error.to_string());
            let _ = status_tx.send(status);
        });
        Ok(PtySpawn {
            pid,
            reader,
            writer,
            status: status_rx,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn relay_attach(
    stream: UnixStream,
    input: mpsc::UnboundedSender<Vec<u8>>,
    mut output: broadcast::Receiver<Vec<u8>>,
    replay: Vec<u8>,
) {
    let (mut reader, mut writer) = stream.into_split();
    if !replay.is_empty() && writer.write_all(&replay).await.is_err() {
        return;
    }
    let mut buffer = [0_u8; 8192];
    loop {
        tokio::select! {
            result = reader.read(&mut buffer) => match result {
                Ok(0) | Err(_) => return,
                Ok(length) => {
                    if input.send(buffer[..length].to_vec()).is_err() {
                        return;
                    }
                }
            },
            result = output.recv() => match result {
                Ok(bytes) => {
                    if writer.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

async fn relay_readonly_attach(
    stream: UnixStream,
    mut output: broadcast::Receiver<Vec<u8>>,
    replay: Vec<u8>,
) {
    let (mut reader, mut writer) = stream.into_split();
    if !replay.is_empty() && writer.write_all(&replay).await.is_err() {
        return;
    }
    let mut input = [0_u8; 8192];
    loop {
        tokio::select! {
            result = reader.read(&mut input) => match result {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            },
            result = output.recv() => match result {
                Ok(bytes) => {
                    if writer.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

async fn terminate_and_reap(
    pid: u32,
    wait_task: &mut JoinHandle<std::io::Result<std::process::ExitStatus>>,
) {
    signal_pid(pid, Signal::SIGTERM);
    if timeout(TERMINATION_TIMEOUT, &mut *wait_task).await.is_err() {
        signal_pid(pid, Signal::SIGKILL);
        let _ = timeout(TERMINATION_TIMEOUT, &mut *wait_task).await;
    }
}

async fn terminate_pty(pid: u32, status: &mut oneshot::Receiver<Result<u32, String>>) {
    signal_pid(pid, Signal::SIGTERM);
    if timeout(TERMINATION_TIMEOUT, &mut *status).await.is_err() {
        signal_pid(pid, Signal::SIGKILL);
        let _ = timeout(TERMINATION_TIMEOUT, &mut *status).await;
    }
}

fn signal_pid(pid: u32, signal: Signal) {
    let _ = kill(Pid::from_raw(pid as i32), signal);
}

fn abort_reader(task: Option<JoinHandle<()>>) {
    if let Some(task) = task {
        task.abort();
    }
}

pub fn backoff(attempt: u32) -> Duration {
    let multiplier = 1_u64 << attempt.saturating_sub(1).min(7);
    let millis = BACKOFF_BASE
        .as_millis()
        .saturating_mul(u128::from(multiplier))
        .min(BACKOFF_MAX.as_millis());
    Duration::from_millis(millis as u64)
}

pub fn target_name(target: &Target) -> Option<&str> {
    match target {
        Target::Name(name) => Some(name),
        Target::Directory(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(backoff(1), Duration::from_millis(250));
        assert_eq!(backoff(2), Duration::from_millis(500));
        assert_eq!(backoff(8), Duration::from_secs(30));
        assert_eq!(backoff(100), Duration::from_secs(30));
    }
}
