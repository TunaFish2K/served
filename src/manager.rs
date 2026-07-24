use std::{
    collections::{BTreeMap, HashMap},
    fs,
    os::unix::fs::{FileTypeExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
};
use tracing::{info, warn};

use crate::{
    config::{LoadedService, RestartPolicy, load_service, manager_environment},
    logs::LogStore,
    paths::ServedPaths,
    protocol::{
        Request, Response, ServiceInfo, ServiceState, Target, framed, receive_json, send_json,
    },
    worker::{WorkerCommand, WorkerEvent, spawn_service},
};

pub enum ManagerCommand {
    Request {
        request: Request,
        reply: oneshot::Sender<std::result::Result<Response, String>>,
    },
    PrepareAttach {
        name: String,
        reply: oneshot::Sender<std::result::Result<String, String>>,
    },
    Attach {
        name: String,
        token: String,
        stream: UnixStream,
    },
}

struct ManagedService {
    definition: LoadedService,
    state: ServiceState,
    pid: Option<u32>,
    worker: Option<mpsc::Sender<WorkerCommand>>,
    attach_active: bool,
    attach_token: Option<String>,
    logs: LogStore,
}

struct ManagerState {
    paths: ServedPaths,
    base_environment: BTreeMap<String, String>,
    events: mpsc::UnboundedSender<WorkerEvent>,
    services: HashMap<String, ManagedService>,
}

impl ManagerState {
    fn new(
        paths: ServedPaths,
        base_environment: BTreeMap<String, String>,
        events: mpsc::UnboundedSender<WorkerEvent>,
    ) -> Self {
        Self {
            paths,
            base_environment,
            events,
            services: HashMap::new(),
        }
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
                    } else if let Err(error) = self.start_loaded(service) {
                        warn!(service = %name, %error, "cannot start enabled service");
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

    fn start_loaded(&mut self, service: LoadedService) -> Result<()> {
        let name = service.config.name.clone();
        if let Some(existing) = self.services.get(&name) {
            if existing.worker.is_some() {
                bail!("service {name:?} is already running under the manager")
            }
        }
        let worker = spawn_service(
            service.clone(),
            self.base_environment.clone(),
            self.events.clone(),
        );
        if let Some(existing) = self.services.get_mut(&name) {
            existing.definition = service;
            existing.state = ServiceState::Starting;
            existing.pid = None;
            existing.worker = Some(worker);
            existing.attach_active = false;
            existing.attach_token = None;
            return Ok(());
        }
        let log_directory = self.paths.logs_dir().join(&name);
        self.services.insert(
            name,
            ManagedService {
                definition: service,
                state: ServiceState::Starting,
                pid: None,
                worker: Some(worker),
                attach_active: false,
                attach_token: None,
                logs: LogStore::new(log_directory),
            },
        );
        Ok(())
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
            Request::HistoryList { target } => self.history_list(target),
            Request::HistoryChunk {
                target,
                id,
                offset,
                limit,
            } => self.history_chunk(target, id, offset, limit),
        }
    }

    fn history_list(&self, target: Target) -> std::result::Result<Response, String> {
        let (name, _) = self.resolve_target(&target)?;
        let service = self
            .services
            .get(&name)
            .ok_or_else(|| format!("service {name:?} is not enabled"))?;
        Ok(Response::HistoryList {
            service: name,
            records: service.logs.records(),
        })
    }

    fn history_chunk(
        &self,
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
        let chunk = service
            .logs
            .read_chunk(&id, offset, limit)
            .map_err(|error| format!("read history {id:?}: {error}"))?;
        Ok(Response::HistoryChunk {
            service: name,
            id,
            offset,
            next_offset: chunk.next_offset,
            total: chunk.total,
            eof: chunk.eof,
            content: chunk.content,
        })
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
        self.start_loaded(service)
            .map_err(|error| error.to_string())?;
        info!(path = %directory.display(), "service enabled");
        Ok(Response::Ok)
    }

    async fn disable(&mut self, target: Target) -> std::result::Result<Response, String> {
        let (name, _directory) = self.resolve_target(&target)?;
        let link = self.paths.registry_dir().join(&name);
        let worker = self
            .services
            .get(&name)
            .and_then(|service| service.worker.clone());
        if let Some(worker) = worker {
            let (reply, result) = stop_worker(worker).await;
            result?;
            reply?;
        }
        fs::remove_file(&link).map_err(|error| format!("remove enable link: {error}"))?;
        self.services.remove(&name);
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
        let worker = self
            .services
            .get(&name)
            .and_then(|managed| managed.worker.clone());
        if let Some(worker) = worker {
            let (reply, result) = restart_worker(worker, service.clone()).await;
            result?;
            reply?;
            if let Some(managed) = self.services.get_mut(&name) {
                managed.definition = service;
                managed.state = ServiceState::Starting;
                managed.pid = None;
            }
        } else {
            self.start_loaded(service)
                .map_err(|error| error.to_string())?;
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

    fn prepare_attach(&mut self, name: &str) -> std::result::Result<String, String> {
        let Some(service) = self.services.get_mut(name) else {
            return Err(format!("service {name:?} is not enabled"));
        };
        if service.worker.is_none() || !matches!(service.state, ServiceState::Running) {
            return Err(format!("service {name:?} is not running"));
        }
        if service.definition.config.tty && service.attach_active {
            return Err(format!("service {name:?} already has an attach client"));
        }
        if service.definition.config.tty {
            service.attach_active = true;
            let token = new_attach_token();
            service.attach_token = Some(token.clone());
            return Ok(token);
        }
        Ok(new_attach_token())
    }

    async fn handle_attach(&mut self, name: String, token: String, stream: UnixStream) {
        let Some(service) = self.services.get(&name) else {
            return;
        };
        let tty = service.definition.config.tty;
        let Some(worker) = service.worker.clone() else {
            if tty {
                if let Some(service) = self.services.get_mut(&name) {
                    service.attach_active = false;
                    service.attach_token = None;
                }
            }
            return;
        };
        if worker.send(WorkerCommand::Attach { stream }).await.is_err() && tty {
            if let Some(service) = self.services.get_mut(&name) {
                if service.attach_token.as_deref() == Some(token.as_str()) {
                    service.attach_active = false;
                    service.attach_token = None;
                }
            }
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
        let Some(service) = self.services.get(&name) else {
            return Err(format!("service {name:?} is not enabled"));
        };
        if service.worker.is_none() || !matches!(service.state, ServiceState::Running) {
            return Err(format!("service {name:?} is not running"));
        }
        if !service.definition.config.tty
            || !service.definition.config.sync_rows_cols
            || !service.attach_active
            || service.attach_token.as_deref() != Some(token.as_str())
        {
            return Ok(Response::Ok);
        }
        let worker = service
            .worker
            .clone()
            .expect("running service must have a worker");
        let (reply, receiver) = oneshot::channel();
        worker
            .send(WorkerCommand::Resize { cols, rows, reply })
            .await
            .map_err(|_| "service worker is no longer available".to_owned())?;
        receiver
            .await
            .map_err(|_| "service worker stopped during resize".to_owned())?
            .map_err(|error| format!("cannot resize service PTY: {error}"))?;
        Ok(Response::Ok)
    }

    fn handle_event(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::Starting { name, persist_logs } => {
                if let Some(service) = self.services.get_mut(&name) {
                    service.state = ServiceState::Starting;
                    service.pid = None;
                    for warning in service.logs.begin_run(persist_logs) {
                        warn!(service = %name, %warning, "log history degraded");
                    }
                }
            }
            WorkerEvent::Started { name, pid, tty: _ } => {
                if let Some(service) = self.services.get_mut(&name) {
                    service.state = ServiceState::Running;
                    service.pid = Some(pid);
                }
            }
            WorkerEvent::Output { name, bytes } => {
                if let Some(service) = self.services.get_mut(&name) {
                    if let Some(warning) = service.logs.append(&bytes) {
                        warn!(service = %name, %warning, "log history degraded");
                    }
                }
            }
            WorkerEvent::Exited {
                name,
                code: _,
                success,
            } => {
                if let Some(service) = self.services.get_mut(&name) {
                    service.pid = None;
                    service.attach_active = false;
                    service.attach_token = None;
                    service.state = if service.definition.config.restart.should_restart(success) {
                        ServiceState::Restarting
                    } else {
                        ServiceState::Stopped
                    };
                }
            }
            WorkerEvent::Restarting { name, delay: _ } => {
                if let Some(service) = self.services.get_mut(&name) {
                    service.state = ServiceState::Restarting;
                    service.pid = None;
                    service.attach_active = false;
                    service.attach_token = None;
                }
            }
            WorkerEvent::Stopped { name } => {
                if let Some(service) = self.services.get_mut(&name) {
                    service.state = ServiceState::Stopped;
                    service.pid = None;
                    service.worker = None;
                    service.attach_active = false;
                    service.attach_token = None;
                }
            }
            WorkerEvent::Failed { name, error } => {
                warn!(service = %name, %error, "service worker failure");
                if let Some(service) = self.services.get_mut(&name) {
                    service.state = ServiceState::Failed;
                    service.pid = None;
                    service.attach_active = false;
                    service.attach_token = None;
                }
            }
            WorkerEvent::AttachChanged { name, active } => {
                if let Some(service) = self.services.get_mut(&name) {
                    service.attach_active = active;
                    if !active {
                        service.attach_token = None;
                    }
                }
            }
        }
    }

    fn list_services(&self) -> Vec<ServiceInfo> {
        let mut services: Vec<_> = self
            .services
            .values()
            .map(|service| ServiceInfo {
                name: service.definition.config.name.clone(),
                directory: service.definition.directory.display().to_string(),
                state: service.state.clone(),
                pid: service.pid,
                tty: service.definition.config.tty,
                restart: restart_name(service.definition.config.restart).to_owned(),
                persist_logs: service.definition.config.persist_logs,
                attach_active: service.attach_active,
                output_tail: service.logs.output_tail(),
            })
            .collect();
        services.sort_by(|left, right| left.name.cmp(&right.name));
        services
    }
}

async fn stop_worker(
    worker: mpsc::Sender<WorkerCommand>,
) -> (
    std::result::Result<(), String>,
    std::result::Result<(), String>,
) {
    let (reply, receiver) = oneshot::channel();
    if worker.send(WorkerCommand::Stop { reply }).await.is_err() {
        return (
            Ok(()),
            Err("service worker is no longer available".to_owned()),
        );
    }
    match receiver.await {
        Ok(result) => (result, Ok(())),
        Err(error) => (Ok(()), Err(error.to_string())),
    }
}

async fn restart_worker(
    worker: mpsc::Sender<WorkerCommand>,
    service: LoadedService,
) -> (
    std::result::Result<(), String>,
    std::result::Result<(), String>,
) {
    let (reply, receiver) = oneshot::channel();
    if worker
        .send(WorkerCommand::Restart { service, reply })
        .await
        .is_err()
    {
        return (
            Ok(()),
            Err("service worker is no longer available".to_owned()),
        );
    }
    match receiver.await {
        Ok(result) => (result, Ok(())),
        Err(error) => (Ok(()), Err(error.to_string())),
    }
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

fn new_attach_token() -> String {
    format!("{:032x}", rand::random::<u128>())
}

fn restart_name(policy: RestartPolicy) -> &'static str {
    match policy {
        RestartPolicy::Never => "never",
        RestartPolicy::OnFailure => "on-failure",
        RestartPolicy::Always => "always",
    }
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
    let log_directory = paths.logs_dir();
    if let Err(error) = fs::create_dir_all(&log_directory) {
        warn!(path = %log_directory.display(), %error, "cannot create log directory; persistent logs will fall back to memory");
    } else {
        if let Err(error) = fs::set_permissions(&log_directory, fs::Permissions::from_mode(0o700)) {
            warn!(path = %log_directory.display(), %error, "cannot restrict served log directory");
        }
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

    let (events, mut event_receiver) = mpsc::unbounded_channel();
    let (commands, mut command_receiver) = mpsc::channel(64);
    let mut state = ManagerState::new(paths, manager_environment(), events);
    state.restore_enabled().await;
    info!("served manager is ready");

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
                ManagerCommand::PrepareAttach { name, reply } => {
                    let _ = reply.send(state.prepare_attach(&name));
                }
                ManagerCommand::Attach {
                    name,
                    token,
                    stream,
                } => state.handle_attach(name, token, stream).await,
            },
            Some(event) = event_receiver.recv() => state.handle_event(event),
            else => break,
        }
    }
    let _ = fs::remove_file(socket_path);
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
        if let Request::Attach { name } = request {
            let (reply, receiver) = oneshot::channel();
            commands
                .send(ManagerCommand::PrepareAttach {
                    name: name.clone(),
                    reply,
                })
                .await
                .context("prepare attach request")?;
            let token = match receiver.await.context("receive attach response")? {
                Ok(token) => token,
                Err(message) => {
                    send_json(&mut frame, &Response::Error { message }).await?;
                    return Ok(());
                }
            };
            send_json(
                &mut frame,
                &Response::Attach {
                    token: token.clone(),
                },
            )
            .await?;
            let stream = frame.into_inner();
            commands
                .send(ManagerCommand::Attach {
                    name,
                    token,
                    stream,
                })
                .await
                .context("send attach request to manager")?;
            return Ok(());
        }
        let (reply, receiver) = oneshot::channel();
        commands
            .send(ManagerCommand::Request { request, reply })
            .await
            .context("send command to manager")?;
        let response = receiver.await.context("receive manager response")?;
        let response = match response {
            Ok(response) => response,
            Err(message) => Response::Error { message },
        };
        send_json(&mut frame, &response).await?;
    }
}

pub async fn dispatch(socket_path: &Path, request: Request) -> Result<Response> {
    crate::protocol::request(socket_path, request).await
}
