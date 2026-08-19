use std::{
    collections::HashMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::{sync::mpsc, task::JoinHandle, time::sleep};
use tracing::{info, warn};

use crate::{
    config::{LoadedService, load_service},
    paths::ServedPaths,
    process,
    protocol::{
        HandoffStream, HistoryRecord, Request, Response, ServiceInfo,
        ServiceState as PublicServiceState, Target, into_handoff, receive_json, send_json,
    },
    runner_protocol::{
        LaunchSpec, RunnerMetadata, RunnerRequest, RunnerResponse, RunnerServiceState, RunnerStatus,
    },
};

const RUNNER_START_ATTEMPTS: usize = 100;
const RUNNER_START_DELAY: Duration = Duration::from_millis(20);
pub const SUPERVISOR_RELINQUISH_EXIT_CODE: i32 = 75;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonExit {
    Stopped,
    Relinquished,
}

mod daemon;
mod watcher;

pub use daemon::run_daemon;

struct AttachSetup {
    token: String,
    runner_stream: HandoffStream,
}

#[derive(Debug)]
enum PrepareAttachError {
    Message(String),
    CrashLoop {
        name: String,
        recent_failures: u32,
        latest_log: Option<String>,
    },
}

struct ManagedService {
    definition: LoadedService,
    runner_socket: PathBuf,
    status: RunnerStatus,
    watcher_generation: u64,
    watcher: JoinHandle<()>,
}

struct ManagerState {
    paths: ServedPaths,
    base_environment: std::collections::BTreeMap<String, String>,
    services: HashMap<String, ManagedService>,
    runner_updates: mpsc::Sender<RunnerUpdate>,
    next_watcher_generation: u64,
}

use watcher::RunnerUpdate;

impl ManagerState {
    fn new(
        paths: ServedPaths,
        base_environment: std::collections::BTreeMap<String, String>,
        runner_updates: mpsc::Sender<RunnerUpdate>,
    ) -> Self {
        Self {
            paths,
            base_environment,
            services: HashMap::new(),
            runner_updates,
            next_watcher_generation: 1,
        }
    }

    async fn restore_enabled(&mut self) {
        self.cleanup_orphan_runners().await;
        let directory = self.paths.registry_dir();
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                warn!(path = %directory.display(), %error, "cannot scan enabled registry");
                return;
            }
        };
        for entry in entries.flatten() {
            let link = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let target = match fs::canonicalize(&link) {
                Ok(target) => target,
                Err(error) => {
                    warn!(service = %name, %error, "ignoring broken enable link");
                    continue;
                }
            };
            match load_service(&target, &self.base_environment) {
                Ok(service) if service.config.name == name => {
                    if self.services.contains_key(&name) {
                        warn!(service = %name, "ignoring duplicate enabled service");
                    } else if let Err(error) = self.ensure_service(service).await {
                        warn!(service = %name, %error, "cannot restore enabled service");
                    }
                }
                Ok(service) => warn!(
                    link = %name,
                    config_name = %service.config.name,
                    "enable link name does not match service config"
                ),
                Err(error) => warn!(service = %name, %error, "ignoring invalid enabled service"),
            }
        }
    }

    async fn ensure_service(&mut self, service: LoadedService) -> Result<(), String> {
        let name = service.config.name.clone();
        validate_target_name(&name)?;
        let runner_socket = self.ensure_runner_socket(&name).await?;
        let spec = LaunchSpec::from_loaded(&service);
        let log_directory = self.paths.logs_dir().join(&name);
        match crate::runner_protocol::request(
            &runner_socket,
            &name,
            RunnerRequest::Configure {
                spec,
                log_directory: log_directory.display().to_string(),
            },
        )
        .await
        .map_err(|error| error.to_string())?
        {
            RunnerResponse::Ok => {}
            response => {
                return Err(format!(
                    "unexpected runner configure response: {response:?}"
                ));
            }
        }
        let status = self.fetch_runner_status(&runner_socket, &name).await?;
        let watcher_generation = self.next_watcher_generation;
        self.next_watcher_generation = self.next_watcher_generation.wrapping_add(1).max(1);
        let watcher = watcher::spawn(
            runner_socket.clone(),
            name.clone(),
            watcher_generation,
            self.runner_updates.clone(),
        );
        if let Some(previous) = self.services.insert(
            name,
            ManagedService {
                definition: service,
                runner_socket,
                status,
                watcher_generation,
                watcher,
            },
        ) {
            previous.watcher.abort();
        }
        Ok(())
    }

    async fn ensure_runner_socket(&self, name: &str) -> Result<PathBuf, String> {
        let runner_dir = self.paths.runner_dir(name);
        fs::create_dir_all(&runner_dir).map_err(|error| {
            format!("create runner directory {}: {error}", runner_dir.display())
        })?;
        fs::set_permissions(&runner_dir, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "restrict runner directory {}: {error}",
                runner_dir.display()
            )
        })?;
        let socket = self.paths.runner_socket(name);

        if crate::runner_protocol::connect(&socket, name).await.is_ok() {
            return Ok(socket);
        }

        if runner_identity_alive(&self.paths.runner_metadata(name)) {
            return Err(format!(
                "runner for service {name:?} is alive but its control socket is unavailable"
            ));
        }
        remove_stale_runner_files(&self.paths, name);
        let binary = std::env::current_exe().map_err(|error| error.to_string())?;
        Command::new(binary)
            .arg("runner")
            .arg("--name")
            .arg(name)
            .arg("--socket")
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("spawn runner for {name:?}: {error}"))?;

        for _ in 0..RUNNER_START_ATTEMPTS {
            if crate::runner_protocol::connect(&socket, name).await.is_ok() {
                return Ok(socket);
            }
            sleep(RUNNER_START_DELAY).await;
        }
        Err(format!("runner for service {name:?} did not become ready"))
    }

    async fn fetch_runner_status(&self, socket: &Path, name: &str) -> Result<RunnerStatus, String> {
        match crate::runner_protocol::request(socket, name, RunnerRequest::Status)
            .await
            .map_err(|error| error.to_string())?
        {
            RunnerResponse::Status { status } => Ok(status),
            response => Err(format!("unexpected runner status response: {response:?}")),
        }
    }

    async fn handle_request(&mut self, request: Request) -> std::result::Result<Response, String> {
        match request {
            Request::Hello { .. } => Err("handshake must be the first protocol message".to_owned()),
            Request::List => Ok(Response::Services {
                services: self.list_services(),
            }),
            Request::Enable { directory } => self.enable(PathBuf::from(directory)).await,
            Request::Disable { target } => self.disable(target).await,
            Request::Restart { target } => self.restart(target).await,
            Request::Attach { .. } => Err("attach requires a raw socket handoff".to_owned()),
            Request::Resize {
                name,
                token,
                cols,
                rows,
            } => self.resize(name, token, cols, rows).await,
            Request::HistoryList { target } => self.history_list(target).await,
            Request::HistoryChunk {
                target,
                id,
                offset,
                limit,
            } => self.history_chunk(target, id, offset, limit).await,
            Request::ManagerShutdown
            | Request::ManagerHandoff { .. }
            | Request::ManagerRelinquish => {
                Err("manager lifecycle requests must use their dedicated control path".to_owned())
            }
        }
    }

    async fn history_list(&mut self, target: Target) -> std::result::Result<Response, String> {
        let (name, _) = self.resolve_target(&target)?;
        let service = self
            .services
            .get(&name)
            .ok_or_else(|| format!("service {name:?} is not enabled"))?;
        match crate::runner_protocol::request(
            &service.runner_socket,
            &name,
            RunnerRequest::HistoryList,
        )
        .await
        .map_err(|error| error.to_string())?
        {
            RunnerResponse::HistoryList { records } => Ok(Response::HistoryList {
                service: name,
                records: records
                    .into_iter()
                    .map(|record| HistoryRecord {
                        id: record.id,
                        bytes: record.bytes,
                        current: record.current,
                        persisted: record.persisted,
                    })
                    .collect(),
            }),
            response => Err(format!("unexpected runner history response: {response:?}")),
        }
    }

    async fn history_chunk(
        &mut self,
        target: Target,
        id: String,
        offset: u64,
        limit: u32,
    ) -> std::result::Result<Response, String> {
        let (name, _) = self.resolve_target(&target)?;
        let service = self
            .services
            .get(&name)
            .ok_or_else(|| format!("service {name:?} is not enabled"))?;
        match crate::runner_protocol::request(
            &service.runner_socket,
            &name,
            RunnerRequest::HistoryChunk {
                id: id.clone(),
                offset,
                limit,
            },
        )
        .await
        .map_err(|error| error.to_string())?
        {
            RunnerResponse::HistoryChunk {
                id,
                offset,
                next_offset,
                total,
                total_lines,
                eof,
                content,
            } => Ok(Response::HistoryChunk {
                service: name,
                id,
                offset,
                next_offset,
                total,
                total_lines,
                eof,
                content,
            }),
            response => Err(format!("unexpected runner history response: {response:?}")),
        }
    }

    async fn enable(&mut self, directory: PathBuf) -> std::result::Result<Response, String> {
        let service =
            load_service(&directory, &self.base_environment).map_err(|error| error.to_string())?;
        let link = self.paths.registry_dir().join(&service.config.name);
        if fs::symlink_metadata(&link).is_ok() {
            return Err(format!(
                "service name {:?} is already enabled",
                service.config.name
            ));
        }
        symlink(&service.directory, &link)
            .map_err(|error| format!("create enable link {}: {error}", link.display()))?;
        if let Err(error) = self.ensure_service(service).await {
            return match fs::remove_file(&link) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(format!(
                    "{error}; rollback failed removing enable link {}: {rollback_error}",
                    link.display()
                )),
            };
        }
        info!(path = %directory.display(), "service enabled");
        Ok(Response::Ok)
    }

    async fn disable(&mut self, target: Target) -> std::result::Result<Response, String> {
        let (name, _) = self.resolve_target(&target)?;
        let socket = self
            .services
            .get(&name)
            .map(|service| service.runner_socket.clone())
            .ok_or_else(|| format!("service {name:?} is not enabled"))?;
        stop_runner(&socket, &name).await?;
        let link = self.paths.registry_dir().join(&name);
        fs::remove_file(&link).map_err(|error| format!("remove enable link: {error}"))?;
        if let Some(service) = self.services.remove(&name) {
            service.watcher.abort();
        }
        let _ = fs::remove_dir(self.paths.runner_dir(&name));
        info!(service = %name, "service disabled");
        Ok(Response::Ok)
    }

    async fn restart(&mut self, target: Target) -> std::result::Result<Response, String> {
        let (name, directory) = self.resolve_target(&target)?;
        let service =
            load_service(&directory, &self.base_environment).map_err(|error| error.to_string())?;
        if service.config.name != name {
            return Err(format!(
                "restart cannot rename enabled service from {name:?} to {:?}; disable it first",
                service.config.name
            ));
        }
        let socket = self.ensure_runner_socket(&name).await?;
        let spec = LaunchSpec::from_loaded(&service);
        let response = crate::runner_protocol::request(
            &socket,
            &name,
            RunnerRequest::Restart {
                spec,
                log_directory: self.paths.logs_dir().join(&name).display().to_string(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;
        if !matches!(response, RunnerResponse::Ok) {
            return Err(format!("unexpected runner restart response: {response:?}"));
        }
        let status = self.fetch_runner_status(&socket, &name).await?;
        let watcher_generation = self.next_watcher_generation;
        self.next_watcher_generation = self.next_watcher_generation.wrapping_add(1).max(1);
        let watcher = watcher::spawn(
            socket.clone(),
            name.clone(),
            watcher_generation,
            self.runner_updates.clone(),
        );
        if let Some(previous) = self.services.insert(
            name.clone(),
            ManagedService {
                definition: service,
                runner_socket: socket,
                status,
                watcher_generation,
                watcher,
            },
        ) {
            previous.watcher.abort();
        }
        info!(service = %name, "service restarted");
        Ok(Response::Ok)
    }

    fn resolve_target(&self, target: &Target) -> std::result::Result<(String, PathBuf), String> {
        match target {
            Target::Name(name) => {
                validate_target_name(name)?;
                let link = self.paths.registry_dir().join(name);
                if fs::symlink_metadata(&link).is_err() {
                    return Err(format!("service {name:?} is not enabled"));
                }
                let directory = fs::canonicalize(link).map_err(|error| error.to_string())?;
                Ok((name.clone(), directory))
            }
            Target::Directory(directory) => {
                let directory = fs::canonicalize(directory).map_err(|error| error.to_string())?;
                let service = load_service(&directory, &self.base_environment)
                    .map_err(|error| error.to_string())?;
                let link = self.paths.registry_dir().join(&service.config.name);
                let linked = fs::canonicalize(&link).map_err(|error| error.to_string())?;
                if linked != directory {
                    return Err(format!("service {:?} is not enabled", service.config.name));
                }
                Ok((service.config.name, directory))
            }
        }
    }

    async fn prepare_attach(
        &mut self,
        name: String,
    ) -> std::result::Result<AttachSetup, PrepareAttachError> {
        let service = self.services.get(&name).ok_or_else(|| {
            PrepareAttachError::Message(format!("service {name:?} is not enabled"))
        })?;
        let socket = service.runner_socket.clone();
        let status = self
            .fetch_runner_status(&socket, &name)
            .await
            .map_err(|error| {
                PrepareAttachError::Message(format!("cannot query service runner: {error}"))
            })?;
        if let Some(service) = self.services.get_mut(&name) {
            service.status = status.clone();
        }
        if !matches!(status.state, RunnerServiceState::Running) || status.pid.is_none() {
            if status.recent_failures >= 3 {
                return Err(PrepareAttachError::CrashLoop {
                    name,
                    recent_failures: status.recent_failures,
                    latest_log: status.latest_log,
                });
            }
            return Err(PrepareAttachError::Message(format!(
                "service {:?} is not running",
                name
            )));
        }

        let mut frame = crate::runner_protocol::connect(&socket, &name)
            .await
            .map_err(|error| PrepareAttachError::Message(error.to_string()))?;
        send_json(&mut frame, &RunnerRequest::Attach)
            .await
            .map_err(|error| PrepareAttachError::Message(error.to_string()))?;
        let response = receive_json::<RunnerResponse>(&mut frame)
            .await
            .map_err(|error| PrepareAttachError::Message(error.to_string()))?;
        match response {
            RunnerResponse::Attach { token } => Ok(AttachSetup {
                token,
                runner_stream: into_handoff(frame)
                    .map_err(|error| PrepareAttachError::Message(error.to_string()))?,
            }),
            RunnerResponse::AttachUnavailable {
                name,
                recent_failures,
                window_seconds: _,
                latest_log,
            } => Err(PrepareAttachError::CrashLoop {
                name,
                recent_failures,
                latest_log,
            }),
            RunnerResponse::Error { message } => Err(PrepareAttachError::Message(message)),
            response => Err(PrepareAttachError::Message(format!(
                "unexpected runner attach response: {response:?}"
            ))),
        }
    }

    async fn resize(
        &mut self,
        name: String,
        token: String,
        cols: u16,
        rows: u16,
    ) -> std::result::Result<Response, String> {
        if cols == 0 || rows == 0 {
            return Err("resize dimensions must be greater than zero".to_owned());
        }
        let service = self
            .services
            .get(&name)
            .ok_or_else(|| format!("service {name:?} is not enabled"))?;
        match crate::runner_protocol::request(
            &service.runner_socket,
            &name,
            RunnerRequest::Resize { token, cols, rows },
        )
        .await
        .map_err(|error| error.to_string())?
        {
            RunnerResponse::Ok => Ok(Response::Ok),
            response => Err(format!("unexpected runner resize response: {response:?}")),
        }
    }

    async fn stop_all(&mut self) -> std::result::Result<(), String> {
        let services: Vec<_> = self
            .services
            .iter()
            .map(|(name, service)| (name.clone(), service.runner_socket.clone()))
            .collect();
        let mut first_error = None;
        for (name, socket) in services {
            if let Err(error) = stop_runner(&socket, &name).await {
                warn!(service = %name, %error, "cannot stop runner during manager shutdown");
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        for service in self.services.drain().map(|(_, service)| service) {
            service.watcher.abort();
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn handle_runner_update(&mut self, update: RunnerUpdate) {
        match update {
            RunnerUpdate::Status {
                name,
                generation,
                status,
            } => {
                let Some(service) = self.services.get_mut(&name) else {
                    return;
                };
                if service.watcher_generation == generation {
                    service.status = *status;
                }
            }
            RunnerUpdate::Unavailable {
                name,
                generation,
                error,
            } => {
                let Some(service) = self.services.get(&name) else {
                    return;
                };
                if service.watcher_generation != generation {
                    return;
                }
                let definition = service.definition.clone();
                match self.ensure_service(definition).await {
                    Ok(()) => warn!(service = %name, "recreated unavailable runner"),
                    Err(recovery_error) => {
                        warn!(service = %name, %error, %recovery_error, "runner unavailable");
                        if let Some(service) = self.services.get_mut(&name) {
                            service.status.state = RunnerServiceState::Failed;
                            service.status.pid = None;
                            service.status.attach_active = false;
                        }
                    }
                }
            }
        }
    }

    async fn cleanup_orphan_runners(&self) {
        let Ok(entries) = fs::read_dir(self.paths.runners_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if validate_target_name(&name).is_err() {
                continue;
            }
            if fs::symlink_metadata(self.paths.registry_dir().join(&name)).is_ok() {
                continue;
            }
            if let Err(error) = stop_runner(&self.paths.runner_socket(&name), &name).await {
                warn!(service = %name, %error, "cannot clean orphan runner");
                remove_stale_runner_files(&self.paths, &name);
            }
            let _ = fs::remove_dir(self.paths.runner_dir(&name));
        }
    }

    fn list_services(&self) -> Vec<ServiceInfo> {
        let mut services: Vec<_> = self
            .services
            .values()
            .map(|service| ServiceInfo {
                name: service.definition.config.name.clone(),
                directory: service.definition.directory.display().to_string(),
                state: public_service_state(&service.status.state),
                pid: service.status.pid,
                tty: service.definition.config.tty,
                restart: service.status.restart.clone(),
                persist_logs: service.status.persist_logs,
                attach_active: service.status.attach_active,
                output_tail: service.status.output_tail.clone(),
            })
            .collect();
        services.sort_by(|left, right| left.name.cmp(&right.name));
        services
    }
}

fn public_service_state(state: &RunnerServiceState) -> PublicServiceState {
    match state {
        RunnerServiceState::Starting => PublicServiceState::Starting,
        RunnerServiceState::Running => PublicServiceState::Running,
        RunnerServiceState::Restarting => PublicServiceState::Restarting,
        RunnerServiceState::Stopped => PublicServiceState::Stopped,
        RunnerServiceState::Failed => PublicServiceState::Failed,
    }
}

async fn stop_runner(socket: &Path, name: &str) -> Result<(), String> {
    match crate::runner_protocol::request(socket, name, RunnerRequest::Stop)
        .await
        .map_err(|error| error.to_string())?
    {
        RunnerResponse::Ok => Ok(()),
        response => Err(format!("unexpected runner stop response: {response:?}")),
    }
}

async fn stop_all_runners(paths: &ServedPaths) -> Result<(), String> {
    let entries = match fs::read_dir(paths.runners_dir()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("scan runner directory: {error}")),
    };
    let mut first_error = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if validate_target_name(&name).is_err() {
            continue;
        }
        let socket = paths.runner_socket(&name);
        let metadata = paths.runner_metadata(&name);
        if !socket.exists() && !runner_identity_alive(&metadata) {
            remove_stale_runner_files(paths, &name);
            let _ = fs::remove_dir(paths.runner_dir(&name));
            continue;
        }
        if let Err(error) = stop_runner(&socket, &name).await {
            if !runner_identity_alive(&metadata) {
                remove_stale_runner_files(paths, &name);
                let _ = fs::remove_dir(paths.runner_dir(&name));
                continue;
            }
            warn!(service = %name, %error, "cannot stop runner during fallback shutdown");
            if first_error.is_none() {
                first_error = Some(error);
            }
            continue;
        }
        for _ in 0..RUNNER_START_ATTEMPTS {
            if !socket.exists() {
                break;
            }
            sleep(RUNNER_START_DELAY).await;
        }
        let _ = fs::remove_dir(paths.runner_dir(&name));
    }
    first_error.map_or(Ok(()), Err)
}

fn runner_identity_alive(metadata_path: &Path) -> bool {
    let Ok(bytes) = fs::read(metadata_path) else {
        return false;
    };
    let Ok(metadata) = serde_json::from_slice::<RunnerMetadata>(&bytes) else {
        return false;
    };
    process::matches(metadata.runner_pid, metadata.runner_start_time)
        || metadata
            .service_pid
            .zip(metadata.service_start_time)
            .is_some_and(|(pid, start)| process::matches(pid, Some(start)))
}

fn remove_stale_runner_files(paths: &ServedPaths, name: &str) {
    let _ = fs::remove_file(paths.runner_socket(name));
    let _ = fs::remove_file(paths.runner_metadata(name));
}

fn validate_target_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name
            .chars()
            .any(|character| !character.is_ascii_alphanumeric() && !"._-".contains(character))
    {
        return Err(format!("invalid service name {name:?}"));
    }
    Ok(())
}

pub async fn request_shutdown(paths: ServedPaths) -> Result<()> {
    match crate::protocol::request(&paths.socket_path(), Request::ManagerShutdown).await {
        Ok(Response::Ok) => Ok(()),
        Ok(Response::Error { message }) => stop_all_runners(&paths).await.map_err(|runner_error| {
            anyhow::anyhow!("manager shutdown failed: {message}; fallback failed: {runner_error}")
        }),
        Ok(response) => bail!("unexpected manager shutdown response: {response:?}"),
        Err(manager_error) => stop_all_runners(&paths).await.map_err(|runner_error| {
            anyhow::anyhow!(
                "manager is unavailable: {manager_error}; fallback shutdown failed: {runner_error}"
            )
        }),
    }
}

pub async fn request_handoff(paths: ServedPaths) -> Result<()> {
    let executable = std::env::current_exe().context("resolve handoff executable")?;
    let executable = executable
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("handoff executable path is not valid UTF-8"))?
        .to_owned();
    let previous_generation = fs::read_to_string(paths.manager_generation()).ok();
    match crate::protocol::request(&paths.socket_path(), Request::ManagerHandoff { executable })
        .await?
    {
        Response::Ok => {}
        response => bail!("unexpected manager handoff response: {response:?}"),
    }
    for _ in 0..RUNNER_START_ATTEMPTS {
        let current_generation = fs::read_to_string(paths.manager_generation()).ok();
        let replaced = match (&previous_generation, &current_generation) {
            (Some(previous), Some(current)) => previous != current,
            (None, Some(_)) => true,
            _ => false,
        };
        if replaced {
            if let Ok(Response::Services { .. }) =
                crate::protocol::request(&paths.socket_path(), Request::List).await
            {
                return Ok(());
            }
        }
        sleep(RUNNER_START_DELAY).await;
    }
    Err(anyhow::anyhow!(
        "new manager did not become ready after handoff"
    ))
}

pub async fn request_relinquish(paths: ServedPaths) -> Result<()> {
    match crate::protocol::request(&paths.socket_path(), Request::ManagerRelinquish).await? {
        Response::Ok => {}
        response => bail!("unexpected manager relinquish response: {response:?}"),
    }
    for _ in 0..RUNNER_START_ATTEMPTS {
        if !paths.socket_path().exists() && !paths.manager_generation().exists() {
            return Ok(());
        }
        sleep(RUNNER_START_DELAY).await;
    }
    Err(anyhow::anyhow!(
        "manager did not release its socket after relinquish"
    ))
}
