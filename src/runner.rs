use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::Result;
use tokio::sync::{mpsc, oneshot, watch};
use tracing::warn;

use crate::{
    ipc::HandoffStream,
    logs::LogStore,
    process,
    runner_protocol::{
        LaunchSpec, RunnerHistoryRecord, RunnerMetadata, RunnerRequest, RunnerResponse,
        RunnerServiceState as ServiceState, RunnerStatus,
    },
    worker::{WORKER_EVENT_CAPACITY, WorkerCommand, WorkerEvent, spawn_service},
};

const CRASH_WINDOW: Duration = Duration::from_secs(60);
const CRASH_THRESHOLD: usize = 3;

mod server;

pub use server::run;

enum RunnerCommand {
    Request {
        request: RunnerRequest,
        reply: oneshot::Sender<std::result::Result<RunnerResponse, String>>,
    },
    PrepareAttach {
        reply: oneshot::Sender<std::result::Result<String, AttachError>>,
    },
    AttachStream {
        token: String,
        stream: HandoffStream,
    },
    Exit,
}

#[derive(Debug)]
enum AttachError {
    Message(String),
    CrashLoop {
        name: String,
        recent_failures: u32,
        latest_log: Option<String>,
    },
}

#[derive(Debug, Default)]
struct FailureTracker {
    failures: VecDeque<Instant>,
}

impl FailureTracker {
    fn record(&mut self, now: Instant) {
        self.prune(now);
        self.failures.push_back(now);
    }

    fn recent_count(&mut self, now: Instant) -> usize {
        self.prune(now);
        self.failures.len()
    }

    fn prune(&mut self, now: Instant) {
        while self
            .failures
            .front()
            .is_some_and(|failure| now.saturating_duration_since(*failure) >= CRASH_WINDOW)
        {
            self.failures.pop_front();
        }
    }
}

struct RunnerState {
    name: String,
    metadata_path: PathBuf,
    spec: Option<LaunchSpec>,
    logs: Option<LogStore>,
    worker: Option<mpsc::Sender<WorkerCommand>>,
    state: ServiceState,
    pid: Option<u32>,
    pid_start_time: Option<u64>,
    attach_active: bool,
    attach_token: Option<String>,
    failures: FailureTracker,
    events: mpsc::Receiver<WorkerEvent>,
    worker_task: Option<tokio::task::JoinHandle<()>>,
    manually_stopped: bool,
    status_updates: watch::Sender<RunnerStatus>,
}

impl RunnerState {
    fn new(
        name: String,
        socket_path: PathBuf,
        status_updates: watch::Sender<RunnerStatus>,
    ) -> Self {
        let metadata_path = socket_path.with_file_name("runner.json");
        let (_, events) = mpsc::channel(WORKER_EVENT_CAPACITY);
        Self {
            name,
            metadata_path,
            spec: None,
            logs: None,
            worker: None,
            state: ServiceState::Stopped,
            pid: None,
            pid_start_time: None,
            attach_active: false,
            attach_token: None,
            failures: FailureTracker::default(),
            events,
            worker_task: None,
            manually_stopped: false,
            status_updates,
        }
    }

    async fn handle_request(
        &mut self,
        request: RunnerRequest,
    ) -> std::result::Result<RunnerResponse, String> {
        match request {
            RunnerRequest::Hello { .. } => {
                Err("runner handshake must be the first protocol message".to_owned())
            }
            RunnerRequest::Configure {
                spec,
                log_directory,
            } => {
                self.configure(spec, PathBuf::from(log_directory)).await?;
                Ok(RunnerResponse::Ok)
            }
            RunnerRequest::Restart {
                spec,
                log_directory,
            } => {
                self.restart(spec, PathBuf::from(log_directory)).await?;
                Ok(RunnerResponse::Ok)
            }
            RunnerRequest::StopService => {
                self.stop_service().await?;
                Ok(RunnerResponse::Ok)
            }
            RunnerRequest::StartService {
                spec,
                log_directory,
            } => {
                if !matches!(
                    self.state,
                    ServiceState::Starting | ServiceState::Running | ServiceState::Restarting
                ) {
                    self.restart(spec, PathBuf::from(log_directory)).await?;
                }
                Ok(RunnerResponse::Ok)
            }
            RunnerRequest::ConfigureStopped {
                spec,
                log_directory,
            } => {
                if self.spec.is_some() {
                    return Err("runner is already configured".to_owned());
                }
                self.set_spec(spec, PathBuf::from(log_directory));
                self.manually_stopped = true;
                self.write_metadata();
                Ok(RunnerResponse::Ok)
            }
            RunnerRequest::Stop => {
                self.stop().await?;
                Ok(RunnerResponse::Ok)
            }
            RunnerRequest::Status => Ok(RunnerResponse::Status {
                status: self.status(),
            }),
            RunnerRequest::WatchStatus => {
                Err("status watch requires its dedicated streaming path".to_owned())
            }
            RunnerRequest::Attach => Err("attach requires a raw socket handoff".to_owned()),
            RunnerRequest::Resize { token, cols, rows } => {
                self.resize(token, cols, rows).await?;
                Ok(RunnerResponse::Ok)
            }
            RunnerRequest::HistoryList => Ok(RunnerResponse::HistoryList {
                records: self
                    .logs
                    .as_ref()
                    .map(LogStore::records)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|record| RunnerHistoryRecord {
                        id: record.id,
                        bytes: record.bytes,
                        current: record.current,
                        persisted: record.persisted,
                    })
                    .collect(),
            }),
            RunnerRequest::HistoryChunk { id, offset, limit } => {
                let logs = self
                    .logs
                    .as_mut()
                    .ok_or_else(|| "runner has not been configured".to_owned())?;
                let chunk = logs
                    .read_chunk(&id, offset, limit)
                    .map_err(|error| format!("read history {id:?}: {error}"))?;
                Ok(RunnerResponse::HistoryChunk {
                    id,
                    offset,
                    next_offset: chunk.next_offset,
                    total: chunk.total,
                    total_lines: chunk.total_lines,
                    eof: chunk.eof,
                    content: chunk.content,
                })
            }
            RunnerRequest::Ping => Ok(RunnerResponse::Ok),
        }
    }

    async fn configure(&mut self, spec: LaunchSpec, log_directory: PathBuf) -> Result<(), String> {
        if self.manually_stopped || (self.spec.as_ref() == Some(&spec) && self.worker.is_some()) {
            return Ok(());
        }
        if self.spec.is_some() && self.worker.is_some() {
            self.restart(spec.clone(), log_directory).await?;
            return Ok(());
        }
        self.spec = Some(spec.clone());
        self.logs = Some(LogStore::with_limits(
            log_directory,
            spec.config.log_max_bytes,
            spec.config.log_max_files,
        ));
        self.spawn(spec)?;
        Ok(())
    }

    async fn restart(&mut self, spec: LaunchSpec, log_directory: PathBuf) -> Result<(), String> {
        self.finish_worker().await?;
        self.set_spec(spec.clone(), log_directory);
        self.spawn(spec)
    }

    fn set_spec(&mut self, spec: LaunchSpec, log_directory: PathBuf) {
        if let Some(logs) = self.logs.as_mut() {
            logs.set_limits(spec.config.log_max_bytes, spec.config.log_max_files);
        } else {
            self.logs = Some(LogStore::with_limits(
                log_directory,
                spec.config.log_max_bytes,
                spec.config.log_max_files,
            ));
        }
        self.spec = Some(spec);
    }

    // Each worker generation owns a separate event channel. Drain the old generation
    // while waiting for termination so bounded output cannot block stop/restart.
    async fn finish_worker(&mut self) -> Result<(), String> {
        let Some(mut task) = self.worker_task.take() else {
            if self.pid.is_some() {
                return Err("worker unavailable; cannot confirm service termination".to_owned());
            }
            return Ok(());
        };
        let worker = self.worker.clone();
        let result = {
            let completion = async {
                if let Some(worker) = worker {
                    let (reply, receiver) = oneshot::channel();
                    if worker.send(WorkerCommand::Stop { reply }).await.is_ok() {
                        // A natural exit can drop the reply. Only a successful task
                        // join below establishes that this is a completed worker.
                        if let Ok(result) = receiver.await {
                            result?;
                        }
                    }
                }
                (&mut task)
                    .await
                    .map_err(|error| format!("join service worker: {error}"))
            };
            tokio::pin!(completion);
            loop {
                tokio::select! {
                    result = &mut completion => break result,
                    Some(event) = self.events.recv() => self.handle_event(event),
                }
            }
        };
        if let Err(error) = result {
            if !task.is_finished() {
                self.worker_task = Some(task);
            }
            return Err(error);
        }
        while let Ok(event) = self.events.try_recv() {
            self.handle_event(event);
        }
        self.worker = None;
        Ok(())
    }

    fn spawn(&mut self, spec: LaunchSpec) -> Result<(), String> {
        let service = spec.into_loaded().map_err(|error| error.to_string())?;
        self.state = ServiceState::Starting;
        self.pid = None;
        self.pid_start_time = None;
        self.attach_active = false;
        self.attach_token = None;
        self.manually_stopped = false;
        let (events, receiver) = mpsc::channel(WORKER_EVENT_CAPACITY);
        let (worker, task) = spawn_service(service, BTreeMap::new(), events);
        self.events = receiver;
        self.worker = Some(worker);
        self.worker_task = Some(task);
        Ok(())
    }

    async fn stop_service(&mut self) -> Result<(), String> {
        self.finish_worker().await?;
        self.manually_stopped = true;
        self.state = ServiceState::Stopped;
        self.pid = None;
        self.pid_start_time = None;
        self.attach_active = false;
        self.attach_token = None;
        self.write_metadata();
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), String> {
        self.stop_service().await?;
        self.remove_metadata();
        Ok(())
    }

    fn prepare_attach(&mut self) -> std::result::Result<String, AttachError> {
        let Some(spec) = self.spec.as_ref() else {
            return Err(AttachError::Message("runner is not configured".to_owned()));
        };
        if self.worker.is_none() || !matches!(self.state, ServiceState::Running) {
            let recent_failures = self.failures.recent_count(Instant::now());
            if recent_failures >= CRASH_THRESHOLD {
                return Err(AttachError::CrashLoop {
                    name: self.name.clone(),
                    recent_failures: recent_failures.min(u32::MAX as usize) as u32,
                    latest_log: self
                        .logs
                        .as_ref()
                        .and_then(LogStore::latest_log_path)
                        .map(|path| path.display().to_string()),
                });
            }
            return Err(AttachError::Message(format!(
                "service {:?} is not running",
                self.name
            )));
        }
        if spec.config.tty && self.attach_active {
            return Err(AttachError::Message(format!(
                "service {:?} already has an attach client",
                self.name
            )));
        }
        let token = format!("{:032x}", rand::random::<u128>());
        if spec.config.tty {
            self.attach_active = true;
            self.attach_token = Some(token.clone());
        }
        Ok(token)
    }

    async fn attach_stream(&mut self, token: String, stream: HandoffStream) {
        let Some(worker) = self.worker.clone() else {
            self.reset_attach(&token);
            return;
        };
        if worker.send(WorkerCommand::Attach { stream }).await.is_err() {
            self.reset_attach(&token);
            self.publish_status();
        }
    }

    async fn resize(&mut self, token: String, cols: u16, rows: u16) -> Result<(), String> {
        if cols == 0 || rows == 0 {
            return Err("resize dimensions must be greater than zero".to_owned());
        }
        let Some(spec) = self.spec.as_ref() else {
            return Err("runner is not configured".to_owned());
        };
        if !spec.config.tty
            || !spec.config.sync_rows_cols
            || !self.attach_active
            || self.attach_token.as_deref() != Some(token.as_str())
        {
            return Ok(());
        }
        let Some(worker) = self.worker.clone() else {
            return Err("service worker is no longer available".to_owned());
        };
        let (reply, receiver) = oneshot::channel();
        worker
            .send(WorkerCommand::Resize { cols, rows, reply })
            .await
            .map_err(|_| "service worker is no longer available".to_owned())?;
        receiver
            .await
            .map_err(|_| "service worker stopped during resize".to_owned())?
            .map_err(|error| format!("cannot resize service PTY: {error}"))
    }

    fn reset_attach(&mut self, token: &str) {
        if self.attach_token.as_deref() == Some(token) {
            self.attach_active = false;
            self.attach_token = None;
        }
    }

    fn status(&mut self) -> RunnerStatus {
        let (tty, restart, persist_logs) = self
            .spec
            .as_ref()
            .map(|spec| {
                (
                    spec.config.tty,
                    restart_name(spec.config.restart).to_owned(),
                    spec.config.persist_logs,
                )
            })
            .unwrap_or((false, "never".to_owned(), false));
        let recent_failures = self.failures.recent_count(Instant::now());
        RunnerStatus {
            supports_start_stop: true,
            manually_stopped: self.manually_stopped,
            name: self.name.clone(),
            runner_pid: std::process::id(),
            state: self.state.clone(),
            pid: self.pid,
            pid_start_time: self.pid_start_time,
            tty,
            restart,
            persist_logs,
            attach_active: self.attach_active,
            output_tail: self
                .logs
                .as_ref()
                .map(LogStore::output_tail)
                .unwrap_or_default(),
            recent_failures: recent_failures.min(u32::MAX as usize) as u32,
            window_seconds: CRASH_WINDOW.as_secs(),
            latest_log: self
                .logs
                .as_ref()
                .and_then(LogStore::latest_log_path)
                .map(|path| path.display().to_string()),
            spec: self.spec.clone().map(Box::new),
        }
    }

    fn publish_status(&mut self) {
        let status = self.status();
        self.status_updates.send_if_modified(|current| {
            if *current == status {
                return false;
            }
            *current = status;
            true
        });
    }

    fn handle_event(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::Starting { persist_logs, .. } => {
                self.state = ServiceState::Starting;
                self.pid = None;
                self.pid_start_time = None;
                if let Some(logs) = self.logs.as_mut() {
                    for warning in logs.begin_run(persist_logs) {
                        warn!(service = %self.name, %warning, "log history degraded");
                    }
                }
                self.write_metadata();
            }
            WorkerEvent::Started { pid, .. } => {
                self.state = ServiceState::Running;
                self.pid = Some(pid);
                self.pid_start_time = process::start_time(pid);
                self.write_metadata();
            }
            WorkerEvent::Output { bytes, .. } => {
                if let Some(logs) = self.logs.as_mut() {
                    if let Some(warning) = logs.append(&bytes) {
                        warn!(service = %self.name, %warning, "log history degraded");
                    }
                }
            }
            WorkerEvent::Exited { success, .. } => {
                if !success {
                    self.failures.record(Instant::now());
                }
                self.pid = None;
                self.pid_start_time = None;
                self.attach_active = false;
                self.attach_token = None;
                let should_restart = self
                    .spec
                    .as_ref()
                    .is_some_and(|spec| spec.config.restart.should_restart(success));
                self.state = if should_restart {
                    ServiceState::Restarting
                } else {
                    ServiceState::Stopped
                };
                self.write_metadata();
            }
            WorkerEvent::Restarting { .. } => {
                self.state = ServiceState::Restarting;
                self.pid = None;
                self.pid_start_time = None;
                self.attach_active = false;
                self.attach_token = None;
                self.write_metadata();
            }
            WorkerEvent::Stopped { .. } => {
                self.state = ServiceState::Stopped;
                self.pid = None;
                self.pid_start_time = None;
                self.worker = None;
                self.attach_active = false;
                self.attach_token = None;
                self.write_metadata();
            }
            WorkerEvent::Failed { error, .. } => {
                warn!(service = %self.name, %error, "service worker failure");
                self.failures.record(Instant::now());
                self.state = ServiceState::Failed;
                self.pid = None;
                self.pid_start_time = None;
                self.attach_active = false;
                self.attach_token = None;
                self.write_metadata();
            }
            WorkerEvent::AttachChanged { active, .. } => {
                self.attach_active = active;
                if !active {
                    self.attach_token = None;
                }
            }
        }
        self.publish_status();
    }

    fn write_metadata(&self) {
        let metadata = RunnerMetadata {
            name: self.name.clone(),
            runner_pid: std::process::id(),
            runner_start_time: process::start_time(std::process::id()),
            service_pid: self.pid,
            service_start_time: self.pid_start_time,
        };
        match serde_json::to_vec_pretty(&metadata)
            .ok()
            .and_then(|bytes| fs::write(&self.metadata_path, bytes).ok())
        {
            Some(()) => {
                let _ = fs::set_permissions(&self.metadata_path, fs::Permissions::from_mode(0o600));
            }
            None => warn!(path = %self.metadata_path.display(), "cannot write runner metadata"),
        }
    }

    fn remove_metadata(&self) {
        let _ = fs::remove_file(&self.metadata_path);
    }
}

fn initial_status(name: &str) -> RunnerStatus {
    RunnerStatus {
        supports_start_stop: true,
        manually_stopped: false,
        name: name.to_owned(),
        runner_pid: std::process::id(),
        state: ServiceState::Stopped,
        pid: None,
        pid_start_time: None,
        tty: false,
        restart: "never".to_owned(),
        persist_logs: false,
        attach_active: false,
        output_tail: String::new(),
        recent_failures: 0,
        window_seconds: CRASH_WINDOW.as_secs(),
        latest_log: None,
        spec: None,
    }
}

fn restart_name(policy: crate::config::RestartPolicy) -> &'static str {
    match policy {
        crate::config::RestartPolicy::Never => "never",
        crate::config::RestartPolicy::OnFailure => "on-failure",
        crate::config::RestartPolicy::Always => "always",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_stop_retains_worker_control_and_does_not_mark_manual_stop() {
        let directory = tempfile::tempdir().unwrap();
        let (updates, _) = watch::channel(initial_status("api"));
        let mut state = RunnerState::new(
            "api".to_owned(),
            directory.path().join("runner.sock"),
            updates,
        );
        let (commands, mut receiver) = mpsc::channel(2);
        state.worker = Some(commands);
        state.state = ServiceState::Running;
        state.pid = Some(42);
        state.worker_task = Some(tokio::spawn(async move {
            if let Some(WorkerCommand::Stop { reply }) = receiver.recv().await {
                reply.send(Err("termination failed".to_owned())).unwrap();
            }
            if let Some(WorkerCommand::Stop { reply }) = receiver.recv().await {
                reply.send(Ok(())).unwrap();
            }
        }));
        assert_eq!(
            state.stop_service().await.unwrap_err(),
            "termination failed"
        );
        assert!(state.worker.is_some());
        assert!(state.worker_task.is_some());
        assert!(!state.manually_stopped);
        assert_eq!(state.pid, Some(42));
        state.stop_service().await.unwrap();
        assert!(state.manually_stopped);
        assert!(state.worker.is_none());
        assert!(state.pid.is_none());
    }

    #[tokio::test]
    async fn stop_drains_bounded_events_and_joins_a_worker_that_drops_its_reply() {
        let directory = tempfile::tempdir().unwrap();
        let (updates, _) = watch::channel(initial_status("api"));
        let mut state = RunnerState::new(
            "api".to_owned(),
            directory.path().join("runner.sock"),
            updates,
        );
        let (commands, mut receiver) = mpsc::channel(2);
        let (events, event_receiver) = mpsc::channel(1);
        state.events = event_receiver;
        state.worker = Some(commands);
        state.worker_task = Some(tokio::spawn(async move {
            let request = receiver.recv().await.unwrap();
            for _ in 0..10 {
                events
                    .send(WorkerEvent::Started {
                        name: "api".to_owned(),
                        pid: 42,
                        tty: false,
                    })
                    .await
                    .unwrap();
            }
            drop(request); // Natural exit won the race with the command.
        }));
        tokio::time::timeout(Duration::from_secs(2), state.stop_service())
            .await
            .unwrap()
            .unwrap();
        assert!(state.worker_task.is_none());
        assert_eq!(state.state, ServiceState::Stopped);
        assert!(state.pid.is_none());
        assert!(state.events.try_recv().is_err());
    }

    #[test]
    fn status_watchers_are_not_notified_when_the_value_is_unchanged() {
        let (status_updates, statuses) = watch::channel(initial_status("api"));
        let mut state = RunnerState::new(
            "api".to_owned(),
            PathBuf::from("/tmp/api.sock"),
            status_updates,
        );

        state.publish_status();
        assert!(!statuses.has_changed().expect("status channel open"));

        state.state = ServiceState::Running;
        state.pid = Some(42);
        state.publish_status();
        assert!(statuses.has_changed().expect("status channel open"));
    }
}
