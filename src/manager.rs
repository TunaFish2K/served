use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::{sync::mpsc, task::JoinHandle, time::sleep};
use tracing::{info, warn};

use serde::{Deserialize, Serialize};

use crate::{
    config::{LoadedService, RestartPolicy, ServiceConfig, load_service},
    paths::ServedPaths,
    process,
    protocol::{
        HandoffStream, HistoryRecord, Request, Response, RunSpec, ServiceInfo, ServiceKind,
        ServiceState as PublicServiceState, Target, into_handoff, receive_json, send_json,
    },
    runner_protocol::{
        LaunchSpec, RunnerMetadata, RunnerRequest, RunnerResponse, RunnerServiceState, RunnerStatus,
    },
};

const RUNNER_START_ATTEMPTS: usize = 100;
const RUNNER_START_DELAY: Duration = Duration::from_millis(20);
const TRANSIENT_DEFINITION_VERSION: u32 = 1;
pub const SUPERVISOR_RELINQUISH_EXIT_CODE: i32 = 75;

#[derive(Debug, Serialize, Deserialize)]
struct TransientDefinition {
    version: u32,
    spec: LaunchSpec,
}

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
    kind: ServiceKind,
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

    async fn restore_services(&mut self) {
        self.restore_enabled().await;
        self.restore_transients().await;
        self.cleanup_orphan_runners().await;
    }

    async fn restore_enabled(&mut self) {
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
                    } else if let Err(error) =
                        self.ensure_service(service, ServiceKind::Enabled).await
                    {
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

    async fn restore_transients(&mut self) {
        let entries = match fs::read_dir(self.paths.runners_dir()) {
            Ok(entries) => entries,
            Err(error) => {
                warn!(%error, "cannot scan transient service definitions");
                return;
            }
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if validate_target_name(&name).is_err() {
                continue;
            }
            let path = self.paths.transient_definition(&name);
            if !path.is_file() {
                continue;
            }
            if self.services.contains_key(&name) {
                warn!(service = %name, "enabled service overrides conflicting transient definition");
                remove_transient_files(&self.paths, &name);
                continue;
            }
            let definition = match read_transient_definition(&path) {
                Ok(definition) => definition,
                Err(error) => {
                    warn!(service = %name, %error, "discarding invalid transient definition");
                    self.cleanup_transient_runner(&name).await;
                    continue;
                }
            };
            let service = match definition.spec.clone().into_loaded() {
                Ok(service) => service,
                Err(error) => {
                    warn!(service = %name, %error, "discarding invalid transient launch spec");
                    self.cleanup_transient_runner(&name).await;
                    continue;
                }
            };
            if service.config.name != name
                || service.config.validate().is_err()
                || fs::canonicalize(&service.directory).ok().as_ref() != Some(&service.directory)
                || self
                    .services
                    .values()
                    .any(|managed| managed.definition.directory == service.directory)
            {
                warn!(service = %name, "discarding inconsistent transient definition");
                self.cleanup_transient_runner(&name).await;
                continue;
            }
            if !runner_identity_alive(&self.paths.runner_metadata(&name)) {
                warn!(service = %name, "discarding transient definition without a live runner");
                self.cleanup_transient_runner(&name).await;
                continue;
            }
            match self
                .fetch_runner_status(&self.paths.runner_socket(&name), &name)
                .await
            {
                Ok(status) if status.spec.as_ref() == Some(&definition.spec) => {
                    if let Err(error) = self.ensure_service(service, ServiceKind::Temporary).await {
                        warn!(service = %name, %error, "cannot restore transient service");
                    }
                }
                Ok(_) => {
                    warn!(service = %name, "discarding transient definition that does not match its runner");
                    self.cleanup_transient_runner(&name).await;
                }
                Err(error) => {
                    warn!(service = %name, %error, "live transient runner is temporarily unavailable");
                }
            }
        }
    }

    async fn ensure_service(
        &mut self,
        service: LoadedService,
        kind: ServiceKind,
    ) -> Result<(), String> {
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
                kind,
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
            Request::Run { spec } => self.run_temporary(spec).await,
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
        let (name, _, _) = self.resolve_target(&target)?;
        let service = self
            .services
            .get(&name)
            .ok_or_else(|| format!("service {name:?} is not managed"))?;
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
        let (name, _, _) = self.resolve_target(&target)?;
        let service = self
            .services
            .get(&name)
            .ok_or_else(|| format!("service {name:?} is not managed"))?;
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
        if self
            .services
            .values()
            .any(|managed| managed.definition.directory == service.directory)
        {
            return Err(format!(
                "service directory {} is already managed",
                service.directory.display()
            ));
        }
        let link = self.paths.registry_dir().join(&service.config.name);
        if fs::symlink_metadata(&link).is_ok() {
            return Err(format!(
                "service name {:?} is already enabled",
                service.config.name
            ));
        }
        if self
            .paths
            .transient_definition(&service.config.name)
            .exists()
        {
            return Err(format!(
                "service name {:?} is already managed",
                service.config.name
            ));
        }
        symlink(&service.directory, &link)
            .map_err(|error| format!("create enable link {}: {error}", link.display()))?;
        if let Err(error) = self.ensure_service(service, ServiceKind::Enabled).await {
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

    async fn run_temporary(&mut self, spec: RunSpec) -> std::result::Result<Response, String> {
        let restart = RestartPolicy::parse(&spec.restart)
            .ok_or_else(|| format!("invalid restart policy {:?}", spec.restart))?;
        if spec.argv.first().is_none_or(String::is_empty) {
            return Err("temporary service program must not be empty".to_owned());
        }
        let directory = PathBuf::from(&spec.directory);
        if !directory.exists() {
            return Err(format!(
                "service directory does not exist: {}",
                directory.display()
            ));
        }
        if !directory.is_dir() {
            return Err(format!(
                "service directory is not a directory: {}",
                directory.display()
            ));
        }
        let directory = fs::canonicalize(directory).map_err(|error| error.to_string())?;
        let config = ServiceConfig {
            name: spec.name,
            command: shell_command(&spec.argv),
            tty: spec.tty,
            sync_rows_cols: spec.sync_rows_cols,
            restart,
            persist_logs: spec.persist_logs,
            log_max_bytes: spec.log_max_bytes,
            log_max_files: spec.log_max_files,
            env: spec.env,
        };
        config.validate().map_err(|error| error.to_string())?;
        if self.services.contains_key(&config.name)
            || fs::symlink_metadata(self.paths.registry_dir().join(&config.name)).is_ok()
            || self.paths.transient_definition(&config.name).exists()
        {
            return Err(format!("service name {:?} is already managed", config.name));
        }
        if self
            .services
            .values()
            .any(|managed| managed.definition.directory == directory)
        {
            return Err(format!(
                "service directory {} is already managed",
                directory.display()
            ));
        }
        let mut environment = self.base_environment.clone();
        for (key, value) in &config.env {
            environment.insert(key.clone(), value.clone());
        }
        let service = LoadedService {
            directory,
            config,
            environment,
        };
        let name = service.config.name.clone();
        let launch = LaunchSpec::from_loaded(&service);
        write_transient_definition(&self.paths, &name, &launch)?;
        if let Err(error) = self.ensure_service(service, ServiceKind::Temporary).await {
            self.cleanup_transient_runner(&name).await;
            return Err(error);
        }
        info!(service = %name, "temporary service started");
        Ok(Response::Ok)
    }

    async fn disable(&mut self, target: Target) -> std::result::Result<Response, String> {
        let (name, _, kind) = self.resolve_target(&target)?;
        let socket = self
            .services
            .get(&name)
            .map(|service| service.runner_socket.clone())
            .ok_or_else(|| format!("service {name:?} is not managed"))?;
        stop_runner(&socket, &name).await?;
        match kind {
            ServiceKind::Enabled => {
                let link = self.paths.registry_dir().join(&name);
                fs::remove_file(&link).map_err(|error| format!("remove enable link: {error}"))?;
            }
            ServiceKind::Temporary => remove_transient_files(&self.paths, &name),
        }
        if let Some(service) = self.services.remove(&name) {
            service.watcher.abort();
        }
        let _ = fs::remove_dir(self.paths.runner_dir(&name));
        info!(service = %name, "service removed");
        Ok(Response::Ok)
    }

    async fn restart(&mut self, target: Target) -> std::result::Result<Response, String> {
        let (name, directory, kind) = self.resolve_target(&target)?;
        let service = match kind {
            ServiceKind::Enabled => {
                let service = load_service(&directory, &self.base_environment)
                    .map_err(|error| error.to_string())?;
                if service.config.name != name {
                    return Err(format!(
                        "restart cannot rename enabled service from {name:?} to {:?}; disable it first",
                        service.config.name
                    ));
                }
                service
            }
            ServiceKind::Temporary => self
                .services
                .get(&name)
                .map(|service| service.definition.clone())
                .ok_or_else(|| format!("service {name:?} is not managed"))?,
        };
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
                kind,
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

    fn resolve_target(
        &self,
        target: &Target,
    ) -> std::result::Result<(String, PathBuf, ServiceKind), String> {
        match target {
            Target::Name(name) => {
                validate_target_name(name)?;
                let service = self
                    .services
                    .get(name)
                    .ok_or_else(|| format!("service {name:?} is not managed"))?;
                Ok((
                    name.clone(),
                    service.definition.directory.clone(),
                    service.kind,
                ))
            }
            Target::Directory(directory) => {
                let directory = fs::canonicalize(directory).map_err(|error| error.to_string())?;
                self.services
                    .iter()
                    .find(|(_, service)| service.definition.directory == directory)
                    .map(|(name, service)| (name.clone(), directory.clone(), service.kind))
                    .ok_or_else(|| {
                        format!("no managed service for directory {}", directory.display())
                    })
            }
        }
    }

    async fn prepare_attach(
        &mut self,
        name: String,
    ) -> std::result::Result<AttachSetup, PrepareAttachError> {
        let service = self.services.get(&name).ok_or_else(|| {
            PrepareAttachError::Message(format!("service {name:?} is not managed"))
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
            .ok_or_else(|| format!("service {name:?} is not managed"))?;
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
            .map(|(name, service)| (name.clone(), service.runner_socket.clone(), service.kind))
            .collect();
        let mut first_error = None;
        for (name, socket, kind) in services {
            if let Err(error) = stop_runner(&socket, &name).await {
                warn!(service = %name, %error, "cannot stop runner during manager shutdown");
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
            if kind == ServiceKind::Temporary {
                remove_transient_files(&self.paths, &name);
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
                let kind = service.kind;
                match self.ensure_service(definition, kind).await {
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
            if self.services.contains_key(&name)
                || fs::symlink_metadata(self.paths.registry_dir().join(&name)).is_ok()
                || self.paths.transient_definition(&name).is_file()
            {
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
                kind: service.kind,
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

    async fn cleanup_transient_runner(&self, name: &str) {
        let socket = self.paths.runner_socket(name);
        if socket.exists() || runner_identity_alive(&self.paths.runner_metadata(name)) {
            if let Err(error) = stop_runner(&socket, name).await {
                warn!(service = %name, %error, "cannot stop discarded transient runner");
                if runner_identity_alive(&self.paths.runner_metadata(name)) {
                    return;
                }
            }
        }
        remove_transient_files(&self.paths, name);
        remove_stale_runner_files(&self.paths, name);
        let _ = fs::remove_dir(self.paths.runner_dir(name));
    }
}

fn shell_command(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| format!("'{}'", argument.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn write_transient_definition(
    paths: &ServedPaths,
    name: &str,
    spec: &LaunchSpec,
) -> Result<(), String> {
    let directory = paths.runner_dir(name);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("create transient runtime directory: {error}"))?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("restrict transient runtime directory: {error}"))?;
    let path = paths.transient_definition(name);
    let temporary = path.with_extension("json.tmp");
    let definition = TransientDefinition {
        version: TRANSIENT_DEFINITION_VERSION,
        spec: spec.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&definition)
        .map_err(|error| format!("encode transient definition: {error}"))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| {
            format!(
                "write transient definition {}: {error}",
                temporary.display()
            )
        })?;
    file.write_all(&bytes).map_err(|error| {
        format!(
            "write transient definition {}: {error}",
            temporary.display()
        )
    })?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("restrict transient definition: {error}"))?;
    fs::rename(&temporary, &path)
        .map_err(|error| format!("publish transient definition {}: {error}", path.display()))?;
    Ok(())
}

fn read_transient_definition(path: &Path) -> Result<TransientDefinition, String> {
    let definition: TransientDefinition = serde_json::from_slice(
        &fs::read(path)
            .map_err(|error| format!("read transient definition {}: {error}", path.display()))?,
    )
    .map_err(|error| format!("decode transient definition {}: {error}", path.display()))?;
    if definition.version != TRANSIENT_DEFINITION_VERSION {
        return Err(format!(
            "unsupported transient definition version {}",
            definition.version
        ));
    }
    Ok(definition)
}

fn remove_transient_files(paths: &ServedPaths, name: &str) {
    let path = paths.transient_definition(name);
    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(path.with_extension("json.tmp"));
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
            remove_transient_files(paths, &name);
            remove_stale_runner_files(paths, &name);
            let _ = fs::remove_dir(paths.runner_dir(&name));
            continue;
        }
        if let Err(error) = stop_runner(&socket, &name).await {
            if !runner_identity_alive(&metadata) {
                remove_stale_runner_files(paths, &name);
                remove_transient_files(paths, &name);
                let _ = fs::remove_dir(paths.runner_dir(&name));
                continue;
            }
            warn!(service = %name, %error, "cannot stop runner during fallback shutdown");
            if first_error.is_none() {
                first_error = Some(error);
            }
            continue;
        }
        remove_transient_files(paths, &name);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn shell_command_preserves_argv_without_shell_expansion() {
        let command = shell_command(&[
            "printf".to_owned(),
            "<%s>|<%s>|<%s>|<%s>".to_owned(),
            "hello world".to_owned(),
            "single'quote".to_owned(),
            "$HOME".to_owned(),
            "; echo injected".to_owned(),
        ]);
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .output()
            .expect("execute quoted argv");
        assert!(output.status.success());
        assert_eq!(
            output.stdout,
            b"<hello world>|<single'quote>|<$HOME>|<; echo injected>"
        );
    }

    #[test]
    fn transient_definition_round_trips_with_private_permissions() {
        let root = tempdir().expect("tempdir");
        let paths = ServedPaths::from_home(root.path());
        let service = LoadedService {
            directory: root.path().to_path_buf(),
            config: ServiceConfig {
                name: "temporary".to_owned(),
                command: "'true'".to_owned(),
                tty: true,
                sync_rows_cols: true,
                restart: RestartPolicy::Never,
                persist_logs: false,
                log_max_bytes: 1024,
                log_max_files: 3,
                env: Default::default(),
            },
            environment: Default::default(),
        };
        let spec = LaunchSpec::from_loaded(&service);

        write_transient_definition(&paths, "temporary", &spec).expect("write definition");
        let path = paths.transient_definition("temporary");
        let restored = read_transient_definition(&path).expect("read definition");

        assert_eq!(restored.spec, spec);
        assert_eq!(
            fs::metadata(path)
                .expect("definition metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
