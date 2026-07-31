use std::{
    collections::VecDeque,
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
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

use crate::{
    config::LoadedService,
    logs::{MEMORY_LOG_LIMIT, attach_snapshot_from_bytes},
    protocol::{HandoffStream, Target},
};

const TERMINATION_TIMEOUT: Duration = Duration::from_secs(5);
const BACKOFF_BASE: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
pub const WORKER_EVENT_CAPACITY: usize = 256;

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
        stream: HandoffStream,
    },
    Resize {
        cols: u16,
        rows: u16,
        reply: oneshot::Sender<Result<(), String>>,
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

pub fn spawn_service(
    service: LoadedService,
    manager_environment: std::collections::BTreeMap<String, String>,
    events: mpsc::Sender<WorkerEvent>,
) -> mpsc::Sender<WorkerCommand> {
    let (commands, receiver) = mpsc::channel(16);
    tokio::spawn(run_service(service, manager_environment, events, receiver));
    commands
}

async fn run_service(
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

async fn run_pipe(
    service: LoadedService,
    manager_environment: std::collections::BTreeMap<String, String>,
    events: mpsc::Sender<WorkerEvent>,
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
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let pid = child
        .id()
        .ok_or_else(|| "managed shell did not expose a pid".to_owned())?;
    let output_hub = Arc::new(OutputHub::new());
    let stdout_task = child.stdout.take().map(|reader| {
        spawn_pipe_reader(
            reader,
            service.config.name.clone(),
            events.clone(),
            output_hub.clone(),
        )
    });
    let stderr_task = child.stderr.take().map(|reader| {
        spawn_pipe_reader(
            reader,
            service.config.name.clone(),
            events.clone(),
            output_hub.clone(),
        )
    });
    let name = service.config.name.clone();
    let _ = events
        .send(WorkerEvent::Started {
            name: name.clone(),
            pid,
            tty: false,
        })
        .await;
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
                    }).await;
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
                    match terminate_process_group(pid, &mut wait_task).await {
                        Ok(()) => {
                            abort_reader(stdout_task);
                            abort_reader(stderr_task);
                            if attach_count > 0 {
                                let _ = events.send(WorkerEvent::AttachChanged {
                                    name: name.clone(),
                                    active: false,
                                }).await;
                            }
                            let _ = reply.send(Ok(()));
                            return Ok(ProcessOutcome::Stopped);
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                Some(WorkerCommand::Restart { service, reply }) => {
                    match terminate_process_group(pid, &mut wait_task).await {
                        Ok(()) => {
                            abort_reader(stdout_task);
                            abort_reader(stderr_task);
                            if attach_count > 0 {
                                let _ = events.send(WorkerEvent::AttachChanged {
                                    name: name.clone(),
                                    active: false,
                                }).await;
                            }
                            let _ = reply.send(Ok(()));
                            return Ok(ProcessOutcome::Restart { service });
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                Some(WorkerCommand::Attach { stream }) => {
                    if attach_count == 0 {
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: true,
                        }).await;
                    }
                    attach_count += 1;
                    let (replay, output) = output_hub.subscribe_with_snapshot();
                    let done = attach_done_tx.clone();
                    tokio::spawn(async move {
                        relay_readonly_attach(stream, output, replay).await;
                        let _ = done.send(());
                    });
                }
                Some(WorkerCommand::Resize { reply, .. }) => {
                    let _ = reply.send(Ok(()));
                }
                None => {
                    terminate_process_group(pid, &mut wait_task).await?;
                    abort_reader(stdout_task);
                    abort_reader(stderr_task);
                    if attach_count > 0 {
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: false,
                        }).await;
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
                    }).await;
                }
            }
        }
    }
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

async fn run_pty(
    service: LoadedService,
    events: mpsc::Sender<WorkerEvent>,
    commands: &mut mpsc::Receiver<WorkerCommand>,
) -> Result<ProcessOutcome, String> {
    let spawned = spawn_pty_process(&service).await?;
    let pid = spawned.pid;
    let pgid = spawned.pgid;
    let name = service.config.name.clone();
    let master = spawned.master;
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let output_hub = Arc::new(OutputHub::new());
    let reader_output = output_hub.clone();
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
    let writer_task = tokio::task::spawn_blocking(move || {
        let mut writer = spawned.writer;
        while let Some(bytes) = input_rx.blocking_recv() {
            if writer.write_all(&bytes).is_err() {
                return;
            }
            let _ = writer.flush();
        }
    });
    let _ = events
        .send(WorkerEvent::Started {
            name: name.clone(),
            pid,
            tty: true,
        })
        .await;
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
                    }).await;
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
                }).await;
            }
            command = commands.recv() => match command {
                Some(WorkerCommand::Stop { reply }) => {
                    match terminate_process_group(pgid, &mut status).await {
                        Ok(()) => {
                            reader_task.abort();
                            writer_task.abort();
                            if attach_active {
                                let _ = events.send(WorkerEvent::AttachChanged {
                                    name: name.clone(),
                                    active: false,
                                }).await;
                            }
                            let _ = reply.send(Ok(()));
                            return Ok(ProcessOutcome::Stopped);
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                Some(WorkerCommand::Restart { service, reply }) => {
                    match terminate_process_group(pgid, &mut status).await {
                        Ok(()) => {
                            reader_task.abort();
                            writer_task.abort();
                            if attach_active {
                                let _ = events.send(WorkerEvent::AttachChanged {
                                    name: name.clone(),
                                    active: false,
                                }).await;
                            }
                            let _ = reply.send(Ok(()));
                            return Ok(ProcessOutcome::Restart { service });
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error));
                        }
                    }
                }
                Some(WorkerCommand::Attach { stream }) => {
                    if attach_active {
                        drop(stream);
                    } else {
                        attach_active = true;
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: true,
                        }).await;
                        let input = input_tx.clone();
                        let (replay, output) = output_hub.subscribe_with_snapshot();
                        let done = attach_done_tx.clone();
                        tokio::spawn(async move {
                            relay_attach(stream, input, output, replay).await;
                            let _ = done.send(());
                        });
                    }
                }
                Some(WorkerCommand::Resize { cols, rows, reply }) => {
                    let result = master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    }).map_err(|error| error.to_string());
                    if let Err(error) = &result {
                        warn!(service = %name, %error, cols, rows, "cannot resize PTY");
                    }
                    let _ = reply.send(result);
                }
                None => {
                    terminate_process_group(pgid, &mut status).await?;
                    reader_task.abort();
                    writer_task.abort();
                    if attach_active {
                        let _ = events.send(WorkerEvent::AttachChanged {
                            name: name.clone(),
                            active: false,
                        }).await;
                    }
                    return Ok(ProcessOutcome::Stopped);
                }
            }
        }
    }
}

struct PtySpawn {
    pid: u32,
    pgid: u32,
    master: Box<dyn MasterPty + Send>,
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
            pgid,
            master: pair.master,
            reader,
            writer,
            status: status_rx,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn relay_attach(
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

async fn terminate_process_group<W>(pgid: u32, wait: &mut W) -> Result<(), String>
where
    W: TerminationWait,
{
    signal_process_group(pgid, Signal::SIGTERM)?;
    if wait.wait_for_exit(TERMINATION_TIMEOUT).await? {
        return Ok(());
    }

    signal_process_group(pgid, Signal::SIGKILL)?;
    if wait.wait_for_exit(TERMINATION_TIMEOUT).await? {
        return Ok(());
    }
    Err(format!("process group {pgid} did not exit after SIGKILL"))
}

trait TerminationWait {
    fn wait_for_exit(
        &mut self,
        duration: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send + '_>>;
}

impl TerminationWait for JoinHandle<std::io::Result<std::process::ExitStatus>> {
    fn wait_for_exit(
        &mut self,
        duration: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send + '_>>
    {
        Box::pin(async move {
            match timeout(duration, &mut *self).await {
                Ok(result) => {
                    let result =
                        result.map_err(|error| format!("wait for service process: {error}"))?;
                    result.map_err(|error| format!("wait for service process: {error}"))?;
                    Ok(true)
                }
                Err(_) => Ok(false),
            }
        })
    }
}

impl TerminationWait for oneshot::Receiver<Result<u32, String>> {
    fn wait_for_exit(
        &mut self,
        duration: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send + '_>>
    {
        Box::pin(async move {
            match timeout(duration, &mut *self).await {
                Ok(Ok(Ok(_))) => Ok(true),
                Ok(Ok(Err(error))) => Err(format!("wait for PTY process: {error}")),
                Ok(Err(error)) => Err(format!("wait for PTY process: {error}")),
                Err(_) => Ok(false),
            }
        })
    }
}

fn signal_process_group(pgid: u32, signal: Signal) -> Result<(), String> {
    let pgid = i32::try_from(pgid).map_err(|_| format!("process group id is too large: {pgid}"))?;
    match kill(Pid::from_raw(-pgid), signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(format!("send {signal} to process group {pgid}: {error}")),
    }
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
