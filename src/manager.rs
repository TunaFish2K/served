use std::{
    collections::HashMap,
    fs,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt, symlink},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::{
    io::copy_bidirectional,
    net::{UnixListener, UnixStream},
    signal::unix::{SignalKind, signal},
    sync::{mpsc, oneshot},
    time::{interval, sleep},
};
use tracing::{info, warn};

use crate::{
    config::{LoadedService, load_service, manager_environment},
    paths::ServedPaths,
    process,
    protocol::{
        HandoffStream, Request, Response, ServiceInfo, ServiceState, Target, framed, into_handoff,
        receive_json, send_json,
    },
    runner_protocol::{LaunchSpec, RunnerMetadata, RunnerRequest, RunnerResponse, RunnerStatus},
};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const RUNNER_START_ATTEMPTS: usize = 100;
const RUNNER_START_DELAY: Duration = Duration::from_millis(20);

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
    Handoff,
}

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
}

struct ManagerState {
    paths: ServedPaths,
    base_environment: std::collections::BTreeMap<String, String>,
    services: HashMap<String, ManagedService>,
}

impl ManagerState {
    fn new(
        paths: ServedPaths,
        base_environment: std::collections::BTreeMap<String, String>,
    ) -> Self {
        Self {
            paths,
            base_environment,
            services: HashMap::new(),
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
        self.services.insert(
            name,
            ManagedService {
                definition: service,
                runner_socket,
                status,
            },
        );
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

    async fn reconcile(&mut self) {
        let names: Vec<_> = self.services.keys().cloned().collect();
        for name in names {
            let Some(current) = self.services.get(&name) else {
                continue;
            };
            let definition = current.definition.clone();
            let socket = current.runner_socket.clone();
            match self.fetch_runner_status(&socket, &name).await {
                Ok(status) => {
                    if let Some(service) = self.services.get_mut(&name) {
                        service.status = status;
                    }
                }
                Err(error) => match self.ensure_service(definition).await {
                    Ok(()) => warn!(service = %name, "recreated unavailable runner"),
                    Err(recovery_error) => {
                        warn!(service = %name, %error, %recovery_error, "runner unavailable");
                        if let Some(service) = self.services.get_mut(&name) {
                            service.status.state = ServiceState::Failed;
                            service.status.pid = None;
                            service.status.attach_active = false;
                        }
                    }
                },
            }
        }
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
            Request::List => {
                self.reconcile().await;
                Ok(Response::Services {
                    services: self.list_services(),
                })
            }
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
            Request::ManagerShutdown | Request::ManagerHandoff => {
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
                records,
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
            let _ = fs::remove_file(&link);
            return Err(error.to_string());
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
        self.services.remove(&name);
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
        self.services.insert(
            name.clone(),
            ManagedService {
                definition: service,
                runner_socket: socket,
                status,
            },
        );
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
        if !matches!(status.state, ServiceState::Running) || status.pid.is_none() {
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
        self.services.clear();
        first_error.map_or(Ok(()), Err)
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
                state: service.status.state.clone(),
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

pub async fn run_daemon(paths: ServedPaths) -> Result<()> {
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

    let (commands, mut command_receiver) = mpsc::channel(64);
    let mut state = ManagerState::new(paths.clone(), manager_environment());
    state.restore_enabled().await;
    info!("served manager is ready");

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
    let mut reconcile_tick = interval(RECONCILE_INTERVAL);
    let mut handoff = false;

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
                ManagerCommand::Handoff => {
                    handoff = true;
                    break;
                }
            },
            _ = reconcile_tick.tick() => state.reconcile().await,
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
    if handoff {
        reexec_manager().context("handoff manager process")?;
    }
    Ok(())
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
            Request::ManagerHandoff => {
                send_json(&mut frame, &Response::Ok).await?;
                commands
                    .send(ManagerCommand::Handoff)
                    .await
                    .context("request manager handoff")?;
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

fn reexec_manager() -> Result<()> {
    let binary = std::env::current_exe().context("resolve current served binary")?;
    let error = Command::new(binary).arg("daemon").exec();
    Err(error).context("exec new served daemon")
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
    let previous_generation = fs::read_to_string(paths.manager_generation()).ok();
    match crate::protocol::request(&paths.socket_path(), Request::ManagerHandoff).await? {
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
