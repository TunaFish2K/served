use crate::{client, editor, paths::ServedPaths};
use anyhow::{Context, Result};
use crossterm::{
    cursor::{MoveTo, Show},
    execute,
    terminal::{
        Clear as TerminalClear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
        disable_raw_mode, enable_raw_mode, size,
    },
};
use std::{
    io::{self, IsTerminal, Read, Write, stdout},
    os::fd::{AsFd, AsRawFd},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::interval,
};

pub async fn attach(
    paths: ServedPaths,
    name: Option<String>,
    stream: bool,
    no_stdin: bool,
) -> Result<()> {
    let interactive =
        !stream && !no_stdin && io::stdin().is_terminal() && io::stdout().is_terminal();
    let result = match name {
        Some(name) => client::attach(&paths, name.clone())
            .await
            .map(|session| (name, session)),
        None => {
            let directory = std::env::current_dir().context("read current directory")?;
            client::attach_current(&paths, directory).await
        }
    };
    let (service_name, session) = match result {
        Ok(session) => session,
        Err(error) => return handle_direct_attach_error(error, interactive).await,
    };
    let _screen = interactive.then(AttachScreen::enter).transpose()?;
    attach_session(&paths, service_name, session, interactive, !no_stdin).await
}

async fn handle_direct_attach_error(error: anyhow::Error, interactive: bool) -> Result<()> {
    let Some(unavailable) = error.downcast_ref::<client::AttachUnavailable>() else {
        return Err(error);
    };
    let warning = crash_warning(unavailable);
    let latest_log = unavailable.latest_log.clone();
    eprintln!("{warning}");
    let Some(path) = latest_log else {
        eprintln!("latest.log is unavailable; enable persist_logs or use served history --stdout");
        return Err(error);
    };
    eprintln!("latest log: {}", path.display());

    if interactive {
        eprint!("Open latest.log? [y/N] ");
        if let Err(prompt_error) = io::stderr().flush() {
            eprintln!("cannot show latest.log prompt: {prompt_error}");
            return Err(error);
        }
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer).is_ok() && is_affirmative(&answer) {
            if let Err(editor_error) = open_default_editor(&path).await {
                eprintln!("cannot open latest.log: {editor_error}");
            }
        }
    }

    Err(error)
}

pub(crate) fn crash_warning(unavailable: &client::AttachUnavailable) -> String {
    format!(
        "warning: service {:?} is not running after {} failures in {} seconds",
        unavailable.name, unavailable.recent_failures, unavailable.window_seconds
    )
}

fn is_affirmative(answer: &str) -> bool {
    matches!(answer.trim(), "y" | "Y")
}

async fn open_default_editor(path: &Path) -> Result<()> {
    let editor = editor::resolve(None)?;
    let status = editor::run(&editor, path).await?;
    editor::require_success(status)
}

struct AttachScreen;

impl AttachScreen {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable attach raw mode")?;
        let mut output = stdout();
        if let Err(error) = execute!(
            output,
            EnterAlternateScreen,
            TerminalClear(ClearType::All),
            MoveTo(0, 0),
            Show
        ) {
            disable_raw_mode().ok();
            return Err(error).context("enter attach alternate screen");
        }
        Ok(Self)
    }
}

impl Drop for AttachScreen {
    fn drop(&mut self) {
        disable_raw_mode().ok();
        let mut output = stdout();
        let _ = execute!(output, LeaveAlternateScreen, Show);
    }
}

struct ResizeController<'a> {
    paths: &'a ServedPaths,
    name: String,
    token: String,
    frame: Option<crate::protocol::Frame>,
    current_size: Option<(u16, u16)>,
    applied_size: Option<(u16, u16)>,
    retry_at: Instant,
    retry_delay: Duration,
}

impl<'a> ResizeController<'a> {
    const BASE_RETRY: Duration = Duration::from_millis(250);
    const MAX_RETRY: Duration = Duration::from_secs(5);

    fn new(paths: &'a ServedPaths, name: String, token: String) -> Self {
        Self {
            paths,
            name,
            token,
            frame: None,
            current_size: None,
            applied_size: None,
            retry_at: Instant::now(),
            retry_delay: Self::BASE_RETRY,
        }
    }

    async fn sync(&mut self) {
        if let Ok((cols, rows)) = size() {
            if cols > 0 && rows > 0 {
                self.current_size = Some((cols, rows));
            }
        }
        let Some((cols, rows)) = self.current_size else {
            return;
        };

        if self.frame.is_none() {
            if Instant::now() < self.retry_at {
                return;
            }
            match client::open_resize_control(self.paths).await {
                Ok(frame) => {
                    self.frame = Some(frame);
                    self.applied_size = None;
                    self.retry_delay = Self::BASE_RETRY;
                }
                Err(_) => {
                    self.schedule_retry();
                    return;
                }
            }
        }

        if self.applied_size == Some((cols, rows)) {
            return;
        }
        let result = match self.frame.as_mut() {
            Some(frame) => client::send_resize(frame, &self.name, &self.token, cols, rows).await,
            None => return,
        };
        match result {
            Ok(()) => {
                self.applied_size = Some((cols, rows));
                self.retry_delay = Self::BASE_RETRY;
            }
            Err(_) => {
                self.frame = None;
                self.schedule_retry();
            }
        }
    }

    fn schedule_retry(&mut self) {
        self.retry_at = Instant::now() + self.retry_delay;
        self.retry_delay = self.retry_delay.saturating_mul(2).min(Self::MAX_RETRY);
    }
}

/// Cancellation is reported to the executable after terminal guards are dropped.
#[derive(Debug, thiserror::Error)]
#[error("attach interrupted by signal {0}")]
pub struct Interrupted(pub i32);

struct Input {
    stopped: Arc<AtomicBool>,
    receiver: tokio::sync::mpsc::Receiver<io::Result<Vec<u8>>>,
}

impl Input {
    fn start() -> Self {
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = stopped.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        // Tokio stdin uses an uncancellable blocking read and can keep runtime
        // shutdown waiting forever. Poll before reading on a bounded worker.
        std::thread::spawn(move || {
            let mut input = io::stdin();
            while !flag.load(Ordering::Acquire) {
                let mut descriptors = [nix::poll::PollFd::new(
                    input.as_fd(),
                    nix::poll::PollFlags::POLLIN,
                )];
                match nix::poll::poll(&mut descriptors, 100u16) {
                    Ok(0) => continue,
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(error) => {
                        let _ = sender.blocking_send(Err(error.into()));
                        break;
                    }
                    Ok(_) => {}
                }
                if flag.load(Ordering::Acquire) {
                    break;
                }
                let mut bytes = vec![0; 8192];
                let result = input.read(&mut bytes).map(|length| {
                    bytes.truncate(length);
                    bytes
                });
                let done = result.as_ref().map_or(true, Vec::is_empty);
                if sender.blocking_send(result).is_err() || done {
                    break;
                }
            }
        });
        Self { stopped, receiver }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

pub(crate) async fn attach_session(
    paths: &ServedPaths,
    name: String,
    session: client::AttachSession,
    interactive: bool,
    read_stdin: bool,
) -> Result<()> {
    let client::AttachSession { stream, token } = session;
    let (mut socket_read, mut socket_write) = tokio::io::split(stream);
    let mut input = read_stdin.then(Input::start);
    let mut stdout = Output::new()?;
    let mut output = [0_u8; 8192];
    let mut resize = ResizeController::new(paths, name, token);
    if interactive {
        resize.sync().await;
    }
    let mut resize_tick = interval(Duration::from_millis(250));
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let transfer = async {
        loop {
            tokio::select! {
                value = async { input.as_mut().unwrap().receiver.recv().await }, if input.is_some() => {
                    match value.transpose()? {
                        Some(bytes) if !bytes.is_empty() => {
                            if interactive && bytes.contains(&0x03) { return Ok(()); }
                            socket_write.write_all(&bytes).await?;
                        }
                        _ => {
                            if interactive { return Ok(()); }
                            // Do not half-close: existing runners treat socket EOF as detach.
                            input = None;
                        }
                    }
                }
                count = socket_read.read(&mut output) => {
                    let count = count?;
                    if count == 0 { return Ok(()); }
                    if let Err(error) = stdout.write_all(&output[..count]).await {
                        if error.kind() == io::ErrorKind::BrokenPipe { return Ok(()); }
                        return Err(error.into());
                    }
                }
                _ = resize_tick.tick(), if interactive => resize.sync().await,
            }
        }
    };
    tokio::select! {
        result = transfer => result,
        _ = interrupt.recv() => Err(Interrupted(2).into()),
        _ = terminate.recv() => Err(Interrupted(15).into()),
    }
}

// Nonblocking writes let signals interrupt a full downstream pipe. Restore the
// inherited open-file flags so a caller's terminal is not left nonblocking.
struct Output {
    file: std::fs::File,
    ready: Option<tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>>,
    flags: nix::fcntl::OFlag,
}

impl Output {
    fn new() -> io::Result<Self> {
        use nix::fcntl::{FcntlArg, OFlag, fcntl};
        let fd = io::stdout().as_fd().try_clone_to_owned()?;
        let file = std::fs::File::from(fd);
        let flags = OFlag::from_bits_truncate(fcntl(file.as_raw_fd(), FcntlArg::F_GETFL)?);
        let mut output = Self {
            file,
            ready: None,
            flags,
        };
        if !output.file.metadata()?.is_file() {
            fcntl(
                output.file.as_raw_fd(),
                FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK),
            )?;
            output.ready =
                match tokio::io::unix::AsyncFd::new(output.file.as_fd().try_clone_to_owned()?) {
                    Ok(ready) => Some(ready),
                    // Some character devices, including /dev/null, cannot be polled.
                    Err(error) if error.raw_os_error() == Some(nix::libc::EPERM) => None,
                    Err(error) => return Err(error),
                };
        }
        Ok(output)
    }

    async fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        let Some(ready) = &self.ready else {
            return self.file.write_all(bytes);
        };
        while !bytes.is_empty() {
            let mut guard = ready.writable().await?;
            match guard
                .try_io(|fd| nix::unistd::write(fd.get_ref(), bytes).map_err(io::Error::from))
            {
                Ok(Ok(0)) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(Ok(count)) => bytes = &bytes[count..],
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => {}
            }
        }
        Ok(())
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = nix::fcntl::fcntl(
            self.file.as_raw_fd(),
            nix::fcntl::FcntlArg::F_SETFL(self.flags),
        );
    }
}
