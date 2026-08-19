use std::{
    collections::VecDeque,
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

use nix::{
    errno::Errno,
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
    time::{sleep, timeout},
};
use tracing::warn;

use super::{WorkerCommand, WorkerEvent, backoff};
use crate::{
    config::LoadedService,
    logs::{MEMORY_LOG_LIMIT, attach_snapshot_from_bytes},
    protocol::HandoffStream,
};

const TERMINATION_TIMEOUT: Duration = Duration::from_secs(5);

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

#[derive(Debug, Clone, Copy)]
struct ExitOutcome {
    code: Option<i32>,
    success: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachMode {
    Interactive,
    ReadOnly,
}

#[derive(Debug)]
struct OutputHub {
    history: Mutex<VecDeque<u8>>,
    output: broadcast::Sender<Vec<u8>>,
}

impl OutputHub {
    fn new() -> Self {
        let (output, _) = broadcast::channel(64);
        Self {
            history: Mutex::new(VecDeque::with_capacity(MEMORY_LOG_LIMIT.min(8192))),
            output,
        }
    }

    fn publish(&self, bytes: Vec<u8>) {
        let mut history = self.history.lock().expect("output cache mutex poisoned");
        history.extend(bytes.iter().copied());
        while history.len() > MEMORY_LOG_LIMIT {
            history.pop_front();
        }
        let _ = self.output.send(bytes);
    }

    fn subscribe_with_snapshot(&self) -> (Vec<u8>, broadcast::Receiver<Vec<u8>>) {
        let history = self.history.lock().expect("output cache mutex poisoned");
        let raw: Vec<u8> = history.iter().copied().collect();
        let replay = attach_snapshot_from_bytes(&raw);
        let output = self.output.subscribe();
        (replay, output)
    }
}

struct ProcessSession {
    pid: u32,
    pgid: u32,
    mode: AttachMode,
    exit: oneshot::Receiver<Result<ExitOutcome, String>>,
    output: Arc<OutputHub>,
    input: Option<mpsc::UnboundedSender<Vec<u8>>>,
    pty: Option<Box<dyn MasterPty + Send>>,
    io_tasks: Vec<JoinHandle<()>>,
}

impl ProcessSession {
    fn abort_io(&mut self) {
        for task in self.io_tasks.drain(..) {
            task.abort();
        }
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let Some(pty) = self.pty.as_ref() else {
            return Ok(());
        };
        pty.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| error.to_string())
    }
}

pub(super) async fn run_service(
    mut service: LoadedService,
    manager_environment: std::collections::BTreeMap<String, String>,
    events: mpsc::Sender<WorkerEvent>,
    mut commands: mpsc::Receiver<WorkerCommand>,
) {
    let mut attempt = 0_u32;

    loop {
        let name = service.config.name.clone();
        let _ = events
            .send(WorkerEvent::Starting {
                name: name.clone(),
                persist_logs: service.config.persist_logs,
            })
            .await;
        let process = supervise_process(
            service.clone(),
            manager_environment.clone(),
            events.clone(),
            &mut commands,
        )
        .await;

        let outcome = match process {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = events
                    .send(WorkerEvent::Failed {
                        name: name.clone(),
                        error,
                    })
                    .await;
                if !service.config.restart.should_restart(false) {
                    let _ = events.send(WorkerEvent::Stopped { name }).await;
                    return;
                }
                attempt = attempt.saturating_add(1);
                let delay = backoff(attempt);
                let _ = events
                    .send(WorkerEvent::Restarting {
                        name: service.config.name.clone(),
                        delay,
                    })
                    .await;
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
                let _ = events.send(WorkerEvent::Stopped { name }).await;
                return;
            }
            ProcessOutcome::Restart { service: next } => {
                attempt = 0;
                service = next;
            }
            ProcessOutcome::Exited { code, success } => {
                let _ = events
                    .send(WorkerEvent::Exited {
                        name: name.clone(),
                        code,
                        success,
                    })
                    .await;
                if !service.config.restart.should_restart(success) {
                    let _ = events.send(WorkerEvent::Stopped { name }).await;
                    return;
                }
                attempt = attempt.saturating_add(1);
                let delay = backoff(attempt);
                let _ = events
                    .send(WorkerEvent::Restarting {
                        name: name.clone(),
                        delay,
                    })
                    .await;
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
            Some(WorkerCommand::Attach { stream }) => {
                drop(stream);
                BackoffOutcome::Timer
            }
            Some(WorkerCommand::Resize { reply, .. }) => {
                let _ = reply.send(Ok(()));
                BackoffOutcome::Timer
            }
            None => BackoffOutcome::Stop,
        }
    }
}

async fn supervise_process(
    service: LoadedService,
    manager_environment: std::collections::BTreeMap<String, String>,
    events: mpsc::Sender<WorkerEvent>,
    commands: &mut mpsc::Receiver<WorkerCommand>,
) -> Result<ProcessOutcome, String> {
    let mut session = if service.config.tty {
        spawn_pty_session(&service, events.clone()).await?
    } else {
        spawn_pipe_session(&service, manager_environment, events.clone())?
    };
    let name = service.config.name.clone();
    let _ = events
        .send(WorkerEvent::Started {
            name: name.clone(),
            pid: session.pid,
            tty: session.mode == AttachMode::Interactive,
        })
        .await;

    let mut attach_count = 0_usize;
    let (attach_done_tx, mut attach_done_rx) = mpsc::unbounded_channel::<()>();

    loop {
        tokio::select! {
            result = &mut session.exit => {
                session.abort_io();
                deactivate_attaches(&events, &name, &mut attach_count).await;
                let outcome = result
                    .map_err(|error| error.to_string())?
                    .map_err(|error| error.to_string())?;
                return Ok(ProcessOutcome::Exited {
                    code: outcome.code,
                    success: outcome.success,
                });
            }
            command = commands.recv() => match command {
                Some(WorkerCommand::Stop { reply }) => {
                    match terminate_process_group(session.pgid, &mut session.exit).await {
                        Ok(()) => {
                            session.abort_io();
                            deactivate_attaches(&events, &name, &mut attach_count).await;
                            let _ = reply.send(Ok(()));
                            return Ok(ProcessOutcome::Stopped);
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                Some(WorkerCommand::Restart { service, reply }) => {
                    match terminate_process_group(session.pgid, &mut session.exit).await {
                        Ok(()) => {
                            session.abort_io();
                            deactivate_attaches(&events, &name, &mut attach_count).await;
                            let _ = reply.send(Ok(()));
                            return Ok(ProcessOutcome::Restart { service });
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                Some(WorkerCommand::Attach { stream }) => {
                    if session.mode == AttachMode::Interactive && attach_count > 0 {
                        drop(stream);
                        continue;
                    }
                    if attach_count == 0 {
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: true,
                        }).await;
                    }
                    attach_count += 1;
                    let (replay, output) = session.output.subscribe_with_snapshot();
                    let done = attach_done_tx.clone();
                    if let Some(input) = session.input.clone() {
                        tokio::spawn(async move {
                            relay_interactive_attach(stream, input, output, replay).await;
                            let _ = done.send(());
                        });
                    } else {
                        tokio::spawn(async move {
                            relay_readonly_attach(stream, output, replay).await;
                            let _ = done.send(());
                        });
                    }
                }
                Some(WorkerCommand::Resize { cols, rows, reply }) => {
                    let result = session.resize(cols, rows);
                    if let Err(error) = &result {
                        warn!(service = %name, %error, cols, rows, "cannot resize PTY");
                    }
                    let _ = reply.send(result);
                }
                None => {
                    terminate_process_group(session.pgid, &mut session.exit).await?;
                    session.abort_io();
                    deactivate_attaches(&events, &name, &mut attach_count).await;
                    return Ok(ProcessOutcome::Stopped);
                }
            },
            Some(_) = attach_done_rx.recv(), if attach_count > 0 => {
                attach_count -= 1;
                if attach_count == 0 {
                    let _ = events.send(WorkerEvent::AttachChanged {
                        name: name.clone(),
                        active: false,
                    }).await;
                }
            }
        }
    }
}

async fn deactivate_attaches(
    events: &mpsc::Sender<WorkerEvent>,
    name: &str,
    attach_count: &mut usize,
) {
    if *attach_count == 0 {
        return;
    }
    *attach_count = 0;
    let _ = events
        .send(WorkerEvent::AttachChanged {
            name: name.to_owned(),
            active: false,
        })
        .await;
}

fn spawn_pipe_session(
    service: &LoadedService,
    manager_environment: std::collections::BTreeMap<String, String>,
    events: mpsc::Sender<WorkerEvent>,
) -> Result<ProcessSession, String> {
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
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let pid = child
        .id()
        .ok_or_else(|| "managed shell did not expose a pid".to_owned())?;
    let output = Arc::new(OutputHub::new());
    let mut io_tasks = Vec::new();
    if let Some(reader) = child.stdout.take() {
        io_tasks.push(spawn_pipe_reader(
            reader,
            service.config.name.clone(),
            events.clone(),
            output.clone(),
        ));
    }
    if let Some(reader) = child.stderr.take() {
        io_tasks.push(spawn_pipe_reader(
            reader,
            service.config.name.clone(),
            events,
            output.clone(),
        ));
    }
    let (exit_tx, exit) = oneshot::channel();
    tokio::spawn(async move {
        let result = child
            .wait()
            .await
            .map(|status| ExitOutcome {
                code: status.code(),
                success: status.success(),
            })
            .map_err(|error| error.to_string());
        let _ = exit_tx.send(result);
    });
    Ok(ProcessSession {
        pid,
        pgid: pid,
        mode: AttachMode::ReadOnly,
        exit,
        output,
        input: None,
        pty: None,
        io_tasks,
    })
}

fn spawn_pipe_reader<R>(
    mut reader: R,
    name: String,
    events: mpsc::Sender<WorkerEvent>,
    output: Arc<OutputHub>,
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
                    output.publish(bytes.clone());
                    if events
                        .send(WorkerEvent::Output {
                            name: name.clone(),
                            bytes,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    })
}

struct PtyParts {
    pid: u32,
    pgid: u32,
    master: Box<dyn MasterPty + Send>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    exit: oneshot::Receiver<Result<ExitOutcome, String>>,
}

async fn spawn_pty_session(
    service: &LoadedService,
    events: mpsc::Sender<WorkerEvent>,
) -> Result<ProcessSession, String> {
    let parts = spawn_pty_process(service).await?;
    let output = Arc::new(OutputHub::new());
    let reader_output = output.clone();
    let reader_events = events;
    let reader_name = service.config.name.clone();
    let reader_task = tokio::task::spawn_blocking(move || {
        let mut reader = parts.reader;
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(length) => {
                    let bytes = buffer[..length].to_vec();
                    reader_output.publish(bytes.clone());
                    if reader_events
                        .blocking_send(WorkerEvent::Output {
                            name: reader_name.clone(),
                            bytes,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    });
    let (input, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer_task = tokio::task::spawn_blocking(move || {
        let mut writer = parts.writer;
        while let Some(bytes) = input_rx.blocking_recv() {
            if writer.write_all(&bytes).is_err() {
                return;
            }
            let _ = writer.flush();
        }
    });
    Ok(ProcessSession {
        pid: parts.pid,
        pgid: parts.pgid,
        mode: AttachMode::Interactive,
        exit: parts.exit,
        output,
        input: Some(input),
        pty: Some(parts.master),
        io_tasks: vec![reader_task, writer_task],
    })
}

async fn spawn_pty_process(service: &LoadedService) -> Result<PtyParts, String> {
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
        let pgid = pair
            .master
            .process_group_leader()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| "PTY child did not expose a process group".to_owned())?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| error.to_string())?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| error.to_string())?;
        let (exit_tx, exit) = oneshot::channel();
        std::thread::spawn(move || {
            let result = child
                .wait()
                .map(|status| {
                    let code = status.exit_code();
                    ExitOutcome {
                        code: Some(code as i32),
                        success: code == 0,
                    }
                })
                .map_err(|error| error.to_string());
            let _ = exit_tx.send(result);
        });
        Ok(PtyParts {
            pid,
            pgid,
            master: pair.master,
            reader,
            writer,
            exit,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn relay_interactive_attach(
    stream: HandoffStream,
    input: mpsc::UnboundedSender<Vec<u8>>,
    mut output: broadcast::Receiver<Vec<u8>>,
    replay: Vec<u8>,
) {
    let (mut reader, mut writer) = tokio::io::split(stream);
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
    stream: HandoffStream,
    mut output: broadcast::Receiver<Vec<u8>>,
    replay: Vec<u8>,
) {
    let (mut reader, mut writer) = tokio::io::split(stream);
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

async fn terminate_process_group(
    pgid: u32,
    wait: &mut oneshot::Receiver<Result<ExitOutcome, String>>,
) -> Result<(), String> {
    signal_process_group(pgid, Signal::SIGTERM)?;
    if wait_for_exit(wait, TERMINATION_TIMEOUT).await? {
        return Ok(());
    }

    signal_process_group(pgid, Signal::SIGKILL)?;
    if wait_for_exit(wait, TERMINATION_TIMEOUT).await? {
        return Ok(());
    }
    Err(format!("process group {pgid} did not exit after SIGKILL"))
}

async fn wait_for_exit(
    wait: &mut oneshot::Receiver<Result<ExitOutcome, String>>,
    duration: Duration,
) -> Result<bool, String> {
    match timeout(duration, wait).await {
        Ok(Ok(Ok(_))) => Ok(true),
        Ok(Ok(Err(error))) => Err(format!("wait for service process: {error}")),
        Ok(Err(error)) => Err(format!("wait for service process: {error}")),
        Err(_) => Ok(false),
    }
}

fn signal_process_group(pgid: u32, signal: Signal) -> Result<(), String> {
    let pgid = i32::try_from(pgid).map_err(|_| format!("process group id is too large: {pgid}"))?;
    match kill(Pid::from_raw(-pgid), signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(format!("send {signal} to process group {pgid}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_hub_pairs_snapshot_with_the_following_live_output() {
        let hub = OutputHub::new();
        hub.publish(b"before\n".to_vec());

        let (snapshot, mut receiver) = hub.subscribe_with_snapshot();
        hub.publish(b"after\n".to_vec());

        assert_eq!(snapshot, b"before\r\n");
        assert_eq!(receiver.try_recv().expect("live output"), b"after\n");
    }
}
