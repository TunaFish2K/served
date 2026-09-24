use std::{
    io::{self, IsTerminal, Write, stdout},
    path::Path,
    time::{Duration, Instant},
};

use crate::{
    client, editor,
    logs::DEFAULT_CHUNK_LIMIT,
    paths::ServedPaths,
    protocol::{Request, Response, Target},
};
use anyhow::{Context, Result, bail};
use crossterm::{
    cursor::{MoveTo, Show},
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{
        Clear as TerminalClear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
        disable_raw_mode, enable_raw_mode, size,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::interval,
};

mod model;
mod view;

use model::{
    CrashLogPrompt, CrashPromptAction, HistoryView, Intent, LifecycleAction, MainUi,
    PendingLifecycleAction, Reader, ServiceAction, crash_prompt_action,
};
use view::{draw_history_content, draw_history_list, draw_main, draw_reader};

#[cfg(test)]
use crate::protocol::{ServiceInfo, ServiceKind, ServiceState};
#[cfg(test)]
use view::history_position;

pub async fn attach(paths: ServedPaths, name: Option<String>) -> Result<()> {
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
        Err(error) => return handle_direct_attach_error(error).await,
    };
    let _screen = AttachScreen::enter()?;
    attach_session(&paths, service_name, session).await
}

async fn handle_direct_attach_error(error: anyhow::Error) -> Result<()> {
    let Some(unavailable) = error.downcast_ref::<client::AttachUnavailable>() else {
        return Err(error);
    };
    let warning = crash_warning(unavailable);
    let latest_log = unavailable.latest_log.clone();
    eprintln!("{warning}");
    let Some(path) = latest_log else {
        eprintln!("latest.log is unavailable; enable persist_logs or use the TUI history browser");
        return Err(error);
    };
    eprintln!("latest log: {}", path.display());

    if io::stdin().is_terminal() && io::stdout().is_terminal() {
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

fn crash_warning(unavailable: &client::AttachUnavailable) -> String {
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

pub async fn run(paths: ServedPaths) -> Result<()> {
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend).context("create terminal")?;
    enable_raw_mode().context("enable terminal raw mode")?;
    execute!(terminal.backend_mut(), EnterAlternateScreen).context("enter alternate screen")?;
    let result = run_loop(&mut terminal, paths).await;
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    result
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    paths: ServedPaths,
) -> Result<()> {
    let mut ui = MainUi {
        unavailable: Some("Connecting to manager".to_owned()),
        ..MainUi::default()
    };
    let mut refresh: Option<tokio::task::JoinHandle<Result<Response>>> = None;
    let mut crash_prompt: Option<CrashLogPrompt> = None;
    let mut pending_action: Option<PendingLifecycleAction> = None;
    let mut exit_when_idle = false;

    loop {
        if pending_action
            .as_ref()
            .is_some_and(PendingLifecycleAction::is_finished)
        {
            let outcome = pending_action
                .take()
                .expect("finished lifecycle action")
                .finish()
                .await;
            if outcome.succeeded {
                if exit_when_idle {
                    return Ok(());
                }
                ui.notice = Some((outcome.notice, Instant::now() + Duration::from_secs(3)));
            } else {
                ui.help = None;
                ui.message = Some(Reader::new("Operation failed", outcome.notice));
            }
            exit_when_idle = false;
        }
        if refresh
            .as_ref()
            .is_some_and(tokio::task::JoinHandle::is_finished)
        {
            match refresh.take().expect("finished refresh").await {
                Ok(Ok(Response::Services { services })) => ui.refresh(services),
                Ok(Ok(response)) => {
                    ui.unavailable = Some(format!("unexpected manager response: {response:?}"))
                }
                Ok(Err(error)) => ui.unavailable = Some(error.to_string()),
                Err(error) => ui.unavailable = Some(format!("refresh failed: {error}")),
            }
        }
        if pending_action.is_none() && refresh.is_none() {
            let paths = paths.clone();
            refresh = Some(tokio::spawn(async move {
                tokio::time::timeout(
                    Duration::from_secs(2),
                    client::request(&paths, Request::List),
                )
                .await
                .context("manager did not respond within two seconds")?
            }));
        }
        let progress = pending_action
            .as_ref()
            .map(|action| action.progress_notice(exit_when_idle))
            .unwrap_or_default();
        terminal.draw(|frame| {
            if crash_prompt.is_some() {
                if let Some(message) = &mut ui.message {
                    draw_reader(frame, message, view::CRASH);
                }
            } else {
                draw_main(frame, &mut ui, &progress);
            }
        })?;
        if !event::poll(Duration::from_millis(250)).context("poll terminal event")? {
            continue;
        }
        let Event::Key(key) = event::read().context("read terminal event")? else {
            continue;
        };
        if key.kind == event::KeyEventKind::Release {
            continue;
        }
        let area = terminal.size()?;
        let area = ratatui::layout::Rect::new(0, 0, area.width, area.height);
        let rows = view::body_rows(
            area,
            if crash_prompt.is_some() {
                view::CRASH
            } else {
                view::main_footer(&ui)
            },
        );
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if pending_action.is_some() {
                exit_when_idle = true;
                continue;
            }
            return Ok(());
        }
        if !view::usable(area) && !matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            continue;
        }
        if let Some(prompt) = crash_prompt.take() {
            match if key.code == KeyCode::Char('q') {
                CrashPromptAction::Cancel
            } else {
                crash_prompt_action(&key.code)
            } {
                CrashPromptAction::Open => {
                    ui.message = Some(Reader::new(
                        "Attach unavailable",
                        match open_editor_in_tui(terminal, &prompt.path).await {
                            Ok(()) => prompt.warning,
                            Err(error) => {
                                format!("{}\nCannot open latest.log: {error}", prompt.warning)
                            }
                        },
                    ));
                }
                CrashPromptAction::Cancel => ui.message = None,
                CrashPromptAction::Ignore => {
                    if let Some(message) = &mut ui.message {
                        message.key(key.code, rows);
                    }
                    crash_prompt = Some(prompt);
                }
            }
            continue;
        }
        match ui.key(key.code, pending_action.is_some(), rows) {
            Intent::None => {}
            Intent::Quit => {
                if pending_action.is_some() {
                    exit_when_idle = true;
                } else {
                    return Ok(());
                }
            }
            Intent::Act(action, name) => {
                if let Some(lifecycle) = action.lifecycle() {
                    ui.notice = None;
                    if let Some(refresh) = refresh.take() {
                        refresh.abort();
                    }
                    pending_action = Some(start_lifecycle_action(&paths, lifecycle, name));
                    continue;
                }
                match action {
                    ServiceAction::Attach => {
                        let result = match client::attach(&paths, name.clone()).await {
                            Ok(session) => attach_in_tui(terminal, &paths, name, session).await,
                            Err(error) => Err(error),
                        };
                        if let Err(error) = result {
                            if let Some(unavailable) =
                                error.downcast_ref::<client::AttachUnavailable>()
                            {
                                let warning = crash_warning(unavailable);
                                if let Some(path) = unavailable.latest_log.clone() {
                                    ui.message = Some(Reader::new(
                                        "Attach unavailable",
                                        format!("{warning}\n\nOpen latest.log?"),
                                    ));
                                    crash_prompt = Some(CrashLogPrompt { warning, path });
                                } else {
                                    ui.message = Some(Reader::new(
                                        "Attach unavailable",
                                        format!(
                                            "{warning}\n\nNo latest.log. Use history or enable persist_logs."
                                        ),
                                    ));
                                }
                            } else {
                                ui.message = Some(Reader::new("Attach failed", error.to_string()));
                            }
                        }
                    }
                    ServiceAction::History => {
                        if let Err(error) = history_in_tui(terminal, &paths, &name).await {
                            ui.message = Some(Reader::new("History failed", error.to_string()));
                        }
                    }
                    _ => unreachable!("lifecycle actions handled above"),
                }
            }
        }
    }
}

fn start_lifecycle_action(
    paths: &ServedPaths,
    action: LifecycleAction,
    name: String,
) -> PendingLifecycleAction {
    let request = match action {
        LifecycleAction::Start => Request::Start {
            target: Target::Name(name.clone()),
        },
        LifecycleAction::Stop => Request::Stop {
            target: Target::Name(name.clone()),
        },
        LifecycleAction::Disable => Request::Disable {
            target: Target::Name(name.clone()),
        },
        LifecycleAction::Restart => Request::Restart {
            target: Target::Name(name.clone()),
        },
    };
    let paths = paths.clone();
    PendingLifecycleAction::spawn(action, name, async move {
        client::expect_ok(&paths, request).await
    })
}

async fn history_in_tui(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    paths: &ServedPaths,
    name: &str,
) -> Result<()> {
    let response = client::request(
        paths,
        Request::HistoryList {
            target: Target::Name(name.to_owned()),
        },
    )
    .await?;
    let Response::HistoryList { records, .. } = response else {
        bail!("unexpected manager response")
    };
    let mut selected = 0_usize;
    let mut view = None;
    let mut help: Option<Reader> = None;
    let mut message: Option<Reader> = None;

    loop {
        terminal.draw(|frame| {
            if let Some(help) = &mut help {
                draw_reader(frame, help, view::HELP);
            } else if let Some(message) = &mut message {
                draw_reader(frame, message, view::ERROR);
            } else if let Some(history_view) = view.as_ref() {
                draw_history_content(frame, name, history_view);
            } else {
                draw_history_list(frame, name, &records, selected);
            }
        })?;
        if !event::poll(Duration::from_millis(250)).context("poll history terminal event")? {
            continue;
        }
        let Event::Key(key) = event::read().context("read history terminal event")? else {
            continue;
        };

        if key.kind == event::KeyEventKind::Release {
            continue;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(());
        }
        let size = terminal.size()?;
        let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        let rows = view::body_rows(
            area,
            if help.is_some() {
                view::HELP
            } else if message.is_some() {
                view::ERROR
            } else if view.is_some() {
                view::CONTENT
            } else {
                view::HISTORY
            },
        );
        if !view::usable(area) && !matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            continue;
        }
        if let Some(reader) = &mut help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q' | '?')) {
                help = None;
            } else {
                reader.key(key.code, rows);
            }
            continue;
        }
        if let Some(reader) = &mut message {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                message = None;
            } else {
                reader.key(key.code, rows);
            }
            continue;
        }
        if key.code == KeyCode::Char('?') {
            help = Some(Reader::new(
                "History / help",
                if view.is_some() {
                    "Up/Down, j/k  Scroll logical lines\nPgUp/PgDn  Page\ng/Home  First line\nG/End  Last line\nEsc/q  Back to history\n\nPosition counts logical lines, not wrapped rows."
                } else {
                    "Up/Down, j/k  Select run\nEnter  Read output\nEsc/q  Back to service\n\ndisk: persistent log\nmemory: bounded in-memory history"
                },
            ));
            continue;
        }
        if let Some(history_view) = view.as_mut() {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => view = None,
                KeyCode::Up | KeyCode::Char('k') => {
                    history_view.scroll = history_view.scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    history_view.scroll = history_view.scroll.saturating_add(1);
                    if let Err(error) = load_history_if_needed(paths, name, history_view).await {
                        message = Some(Reader::new("History failed", error.to_string()));
                    }
                    clamp_history_scroll(history_view);
                }
                KeyCode::PageUp => {
                    history_view.scroll = history_view.scroll.saturating_sub(rows as u64);
                }
                KeyCode::PageDown => {
                    history_view.scroll = history_view.scroll.saturating_add(rows as u64);
                    if let Err(error) = load_history_if_needed(paths, name, history_view).await {
                        message = Some(Reader::new("History failed", error.to_string()));
                    }
                    clamp_history_scroll(history_view);
                }
                KeyCode::Home | KeyCode::Char('g') => history_view.scroll = 0,
                KeyCode::End | KeyCode::Char('G') => {
                    while !history_view.eof {
                        if let Err(error) = load_history_chunk(paths, name, history_view).await {
                            message = Some(Reader::new("History failed", error.to_string()));
                            break;
                        }
                    }
                    history_view.scroll = history_view.total_lines.saturating_sub(1);
                }
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                if !records.is_empty() {
                    selected = (selected + 1).min(records.len() - 1);
                }
            }
            KeyCode::Enter => {
                if let Some(record) = records.get(selected) {
                    let mut history_view = HistoryView::new(record.id.clone());
                    match load_history_chunk(paths, name, &mut history_view).await {
                        Ok(()) => view = Some(history_view),
                        Err(error) => {
                            message = Some(Reader::new("History failed", error.to_string()))
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

async fn load_history_if_needed(
    paths: &ServedPaths,
    name: &str,
    view: &mut HistoryView,
) -> Result<()> {
    let loaded_lines = view.content.lines().count() as u64;
    if !view.eof && view.scroll.saturating_add(12) >= loaded_lines {
        load_history_chunk(paths, name, view).await?;
    }
    Ok(())
}

async fn load_history_chunk(paths: &ServedPaths, name: &str, view: &mut HistoryView) -> Result<()> {
    if view.eof {
        return Ok(());
    }
    let response = client::request(
        paths,
        Request::HistoryChunk {
            target: Target::Name(name.to_owned()),
            id: view.id.clone(),
            offset: view.offset,
            limit: DEFAULT_CHUNK_LIMIT,
        },
    )
    .await?;
    let Response::HistoryChunk {
        next_offset,
        total_lines,
        eof,
        content,
        ..
    } = response
    else {
        bail!("unexpected manager response")
    };
    if next_offset <= view.offset && !eof {
        bail!("history reader made no progress")
    }
    view.content.push_str(&content);
    view.offset = next_offset;
    view.total_lines = total_lines;
    view.eof = eof;
    clamp_history_scroll(view);
    Ok(())
}

fn clamp_history_scroll(view: &mut HistoryView) {
    if view.total_lines == 0 {
        view.scroll = 0;
    } else {
        view.scroll = view.scroll.min(view.total_lines.saturating_sub(1));
    }
}

async fn attach_in_tui(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    paths: &ServedPaths,
    name: String,
    session: client::AttachSession,
) -> Result<()> {
    clear_attach_screen(terminal)?;
    let attach_result = attach_session(paths, name, session).await;
    let restore_result = clear_attach_screen(terminal);
    if let Err(error) = attach_result {
        restore_result?;
        return Err(error);
    }
    restore_result
}

async fn open_editor_in_tui(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    path: &Path,
) -> Result<()> {
    let editor = editor::resolve(None)?;
    disable_raw_mode().context("disable terminal raw mode for editor")?;
    if let Err(error) = execute!(terminal.backend_mut(), LeaveAlternateScreen, Show) {
        enable_raw_mode().ok();
        return Err(error).context("leave alternate screen for editor");
    }

    let editor_result = editor::run(&editor, path).await;
    let restore_result = restore_tui_after_editor(terminal);
    restore_result?;
    editor::require_success(editor_result?)
}

fn restore_tui_after_editor(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        TerminalClear(ClearType::All),
        MoveTo(0, 0),
        Show
    )
    .context("re-enter alternate screen after editor")?;
    enable_raw_mode().context("restore terminal raw mode after editor")?;
    terminal.clear().context("redraw TUI after editor")?;
    Ok(())
}

fn clear_attach_screen(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    terminal.clear().context("clear attach screen")?;
    execute!(terminal.backend_mut(), MoveTo(0, 0), Show).context("reset attach cursor")?;
    Ok(())
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

async fn attach_session(
    paths: &ServedPaths,
    name: String,
    session: client::AttachSession,
) -> Result<()> {
    let client::AttachSession { stream, token } = session;
    let (mut socket_read, mut socket_write) = tokio::io::split(stream);
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut input = [0_u8; 8192];
    let mut output = [0_u8; 8192];
    let mut resize = ResizeController::new(paths, name, token);
    resize.sync().await;
    let mut resize_tick = interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            count = stdin.read(&mut input) => {
                let count = count?;
                if count == 0 || input_requests_detach(&input[..count]) {
                    return Ok(());
                }
                socket_write.write_all(&input[..count]).await?;
            }
            count = socket_read.read(&mut output) => {
                let count = count?;
                if count == 0 {
                    return Ok(());
                }
                stdout.write_all(&output[..count]).await?;
                stdout.flush().await?;
            }
            _ = resize_tick.tick() => resize.sync().await,
        }
    }
}

fn input_requests_detach(input: &[u8]) -> bool {
    input.contains(&0x03)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn service_info(tty: bool) -> ServiceInfo {
        ServiceInfo {
            config_file: None,
            name: "api".to_owned(),
            directory: "/tmp/api".to_owned(),
            kind: ServiceKind::Enabled,
            state: ServiceState::Running,
            pid: Some(42),
            tty,
            restart: "never".to_owned(),
            persist_logs: false,
            attach_active: false,
            output_tail: "ready".to_owned(),
        }
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        use unicode_width::UnicodeWidthStr;
        let buffer = terminal.backend().buffer();
        let mut result = String::new();
        for y in 0..buffer.area.height {
            let mut x = 0;
            while x < buffer.area.width {
                let symbol = buffer[(x, y)].symbol();
                result.push_str(symbol);
                x += symbol.width().max(1) as u16;
            }
            result.push('\n');
        }
        result
    }

    #[test]
    fn history_position_is_one_based_and_clamped() {
        assert_eq!(history_position(0, 0), (0, 0));
        assert_eq!(history_position(0, 3), (1, 3));
        assert_eq!(history_position(1, 3), (2, 3));
        assert_eq!(history_position(99, 3), (3, 3));
    }

    #[test]
    fn ctrl_c_is_the_attach_detach_byte() {
        assert!(input_requests_detach(b"output\x03"));
        assert!(!input_requests_detach(b"output\x1d"));
        assert!(!input_requests_detach(b"output"));
    }

    #[tokio::test]
    async fn lifecycle_action_starts_without_waiting_and_reports_success() {
        let (release, waiting) = tokio::sync::oneshot::channel();
        let pending =
            PendingLifecycleAction::spawn(LifecycleAction::Disable, "api".to_owned(), async move {
                waiting.await.expect("release lifecycle action");
                Ok(())
            });

        assert!(!pending.is_finished());
        assert_eq!(pending.progress_notice(false), "disabling api...");
        assert_eq!(
            pending.progress_notice(true),
            "disabling api...; quitting when complete"
        );

        release.send(()).expect("finish lifecycle action");
        let outcome = tokio::time::timeout(Duration::from_secs(1), pending.finish())
            .await
            .expect("lifecycle action completion");
        assert!(outcome.succeeded);
        assert_eq!(outcome.notice, "disabled api");
    }

    #[tokio::test]
    async fn lifecycle_action_failure_keeps_the_error_context() {
        let pending =
            PendingLifecycleAction::spawn(LifecycleAction::Restart, "api".to_owned(), async {
                Err(anyhow::anyhow!("runner unavailable"))
            });

        let outcome = pending.finish().await;
        assert!(!outcome.succeeded);
        assert_eq!(outcome.notice, "restart api: runner unavailable");
    }

    #[test]
    fn attach_log_prompt_accepts_only_explicit_yes() {
        assert!(is_affirmative("y\n"));
        assert!(is_affirmative("Y"));
        assert!(!is_affirmative(""));
        assert!(!is_affirmative("n\n"));
        assert!(!is_affirmative("yes"));
    }

    #[test]
    fn tui_crash_prompt_uses_enter_as_yes_and_escape_as_no() {
        assert_eq!(
            crash_prompt_action(&KeyCode::Enter),
            CrashPromptAction::Open
        );
        assert_eq!(
            crash_prompt_action(&KeyCode::Char('y')),
            CrashPromptAction::Open
        );
        assert_eq!(
            crash_prompt_action(&KeyCode::Esc),
            CrashPromptAction::Cancel
        );
        assert_eq!(
            crash_prompt_action(&KeyCode::Char('j')),
            CrashPromptAction::Ignore
        );
    }

    #[test]
    fn borderless_layout_adapts_and_preserves_service_states() {
        for (width, height) in [(40, 10), (80, 24), (120, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut ui = MainUi::default();
            ui.refresh(vec![service_info(true)]);
            terminal
                .draw(|frame| draw_main(frame, &mut ui, ""))
                .unwrap();
            let text = buffer_text(&terminal);
            for expected in [
                "served",
                "api",
                "running",
                "enabled",
                "enter actions",
                "? help",
                "q quit",
            ] {
                assert!(
                    text.contains(expected),
                    "missing {expected} at {width}x{height}: {text}"
                );
            }
            assert!(!text.contains("tips:"));
            assert!(!text.contains("ready"));
            assert!(
                !text
                    .chars()
                    .any(|c| matches!(c, '│' | '─' | '┌' | '┐' | '└' | '┘'))
            );
            let buffer = terminal.backend().buffer();
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|cell| cell.modifier.contains(ratatui::style::Modifier::REVERSED))
            );
            assert!(
                buffer
                    .content
                    .iter()
                    .all(|cell| cell.bg == ratatui::style::Color::Reset)
            );
        }
    }

    #[test]
    fn scrolling_long_unicode_lists_keeps_status_visible_after_resize() {
        let mut ui = MainUi::default();
        ui.refresh(
            (0..50)
                .map(|i| {
                    let mut service = service_info(false);
                    service.name = format!("{i:02}-服务名称很长很长很长很长");
                    service.directory =
                        format!("/very/long/path/{}/project-end", "项目/".repeat(40));
                    service.state = ServiceState::Failed;
                    service
                })
                .collect(),
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        for _ in 0..49 {
            ui.key(KeyCode::Down, false, 32);
        }
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        terminal.backend_mut().resize(40, 10);
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("49-服务"));
        assert!(text.contains("failed"));
        assert!(text.contains("project-end"));
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.fg == ratatui::style::Color::Red)
        );
        ui.key(KeyCode::Enter, false, 2);
        ui.key(KeyCode::End, false, 2);
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        assert!(buffer_text(&terminal).contains("enabled"));
        assert!(matches!(ui.page, model::Page::Actions { selected: 5, .. }));
        assert!(buffer_text(&terminal).contains("> Disable"));
    }

    #[test]
    fn actions_help_confirmation_and_errors_preserve_context() {
        use model::Page;
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut ui = MainUi::default();
        ui.refresh(vec![service_info(true)]);
        assert_eq!(ui.key(KeyCode::Enter, false, 2), Intent::None);
        assert_eq!(
            ui.key(KeyCode::Enter, false, 2),
            Intent::Act(ServiceAction::Attach, "api".into())
        );
        for _ in 0..5 {
            ui.key(KeyCode::Down, false, 2);
        }
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        assert!(buffer_text(&terminal).contains("Disable"));
        ui.key(KeyCode::Char('?'), false, 2);
        ui.key(KeyCode::PageDown, false, 2);
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        ui.key(KeyCode::Char('?'), false, 2);
        assert!(matches!(ui.page, Page::Actions { selected: 5, .. }));
        ui.key(KeyCode::Enter, false, 2);
        assert!(matches!(
            ui.page,
            Page::ConfirmDisable { confirm: false, .. }
        ));
        assert_eq!(ui.key(KeyCode::Enter, false, 2), Intent::None);
        assert!(matches!(ui.page, Page::Actions { selected: 5, .. }));
        ui.key(KeyCode::Enter, false, 2);
        ui.key(KeyCode::Down, false, 2);
        assert_eq!(
            ui.key(KeyCode::Enter, false, 2),
            Intent::Act(ServiceAction::Disable, "api".into())
        );
        ui.key(KeyCode::Enter, false, 2);
        ui.key(KeyCode::Down, false, 2);
        ui.message = Some(Reader::new("Failed", "error detail\n".repeat(30)));
        ui.key(KeyCode::End, false, 2);
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        assert!(ui.message.as_ref().unwrap().scroll > 0);
        ui.key(KeyCode::Esc, false, 2);
        assert!(matches!(ui.page, Page::Actions { selected: 1, .. }));
    }

    #[test]
    fn refresh_preserves_identity_and_disconnection_blocks_actions() {
        let a = service_info(false);
        let mut b = a.clone();
        b.name = "worker".into();
        let mut ui = MainUi::default();
        ui.refresh(vec![a.clone(), b.clone()]);
        ui.key(KeyCode::Down, false, 10);
        ui.refresh(vec![b.clone(), a.clone()]);
        assert_eq!(ui.services[ui.selected].name, "worker");
        ui.unavailable = Some("connection refused".into());
        for action in ServiceAction::ALL {
            assert_eq!(ui.key(KeyCode::Char(action.key()), false, 10), Intent::None);
        }
        ui.key(KeyCode::Enter, false, 10);
        assert_eq!(ui.key(KeyCode::Enter, false, 10), Intent::None);
        ui.refresh(vec![b, a.clone()]);
        assert_eq!(
            ui.key(KeyCode::Enter, false, 10),
            Intent::Act(ServiceAction::Attach, "worker".into())
        );
        ui.refresh(vec![a]);
        assert!(matches!(ui.page, model::Page::Services));
        ui.refresh(vec![]);
        assert_eq!(ui.selected, 0);
        assert_eq!(ui.key(KeyCode::Char('s'), false, 10), Intent::None);
    }

    #[test]
    fn pending_operations_allow_navigation_help_and_quit_but_no_new_action() {
        let mut ui = MainUi::default();
        ui.refresh(vec![service_info(false)]);
        for action in ServiceAction::ALL {
            assert_eq!(ui.key(KeyCode::Char(action.key()), true, 10), Intent::None);
        }
        ui.key(KeyCode::Enter, true, 10);
        assert_eq!(ui.key(KeyCode::Enter, true, 10), Intent::None);
        ui.key(KeyCode::Char('?'), true, 10);
        assert!(
            ui.help
                .as_ref()
                .unwrap()
                .content
                .contains("Operation in progress")
        );
        ui.key(KeyCode::Esc, true, 10);
        ui.key(KeyCode::Esc, true, 10);
        assert_eq!(ui.key(KeyCode::Char('q'), true, 10), Intent::Quit);
    }

    #[test]
    fn main_render_shows_lifecycle_progress_and_expires_success() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut ui = MainUi::default();
        ui.refresh(vec![service_info(false)]);
        terminal
            .draw(|frame| draw_main(frame, &mut ui, "stopping api..."))
            .unwrap();
        assert!(buffer_text(&terminal).contains("stopping api..."));
        let now = Instant::now();
        ui.notice = Some(("stopped api".into(), now + Duration::from_secs(3)));
        assert_eq!(ui.notice(now), "stopped api");
        assert_eq!(ui.notice(now + Duration::from_secs(3)), "");
    }

    // Optional artifacts contain the actual TestBackend cells, not a separate mockup.
    fn export_page(terminal: &Terminal<TestBackend>, name: &str) {
        let Ok(directory) = std::env::var("SERVED_TUI_PREVIEWS") else {
            return;
        };
        let buffer = terminal.backend().buffer();
        let cells: Vec<_> = buffer
            .content
            .iter()
            .map(|cell| {
                serde_json::json!({
                    "text": cell.symbol(),
                    "dim": cell.modifier.contains(ratatui::style::Modifier::DIM),
                    "reverse": cell.modifier.contains(ratatui::style::Modifier::REVERSED),
                    "bold": cell.modifier.contains(ratatui::style::Modifier::BOLD),
                })
            })
            .collect();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(std::path::Path::new(&directory).join(format!("{name}-{}.json", buffer.area.width)),
            serde_json::to_vec(&serde_json::json!({"width":buffer.area.width,"height":buffer.area.height,"cells":cells})).unwrap()).unwrap();
    }

    #[test]
    fn actual_pages_follow_the_footer_contract() {
        use model::Page;
        for width in (40..=62).chain([80, 120]) {
            let mut terminal =
                Terminal::new(TestBackend::new(width, if width < 80 { 10 } else { 24 })).unwrap();
            let mut ui = MainUi::default();
            let mut service = service_info(false);
            service.directory = "/projects/api".into();
            ui.refresh(vec![service]);
            for (name, footer, keys) in [
                ("main", view::SERVICES, "enter actions   ? help   q quit"),
                (
                    "actions",
                    view::ACTIONS,
                    "enter select   ? help   esc/q back",
                ),
                (
                    "confirm",
                    view::CONFIRM,
                    "enter select   ? help   esc/q cancel",
                ),
            ] {
                ui.page = match name {
                    "actions" => Page::Actions {
                        selected: 0,
                        scroll: 0,
                    },
                    "confirm" => Page::ConfirmDisable {
                        confirm: false,
                        menu: None,
                    },
                    _ => Page::Services,
                };
                terminal
                    .draw(|frame| draw_main(frame, &mut ui, ""))
                    .unwrap();
                let baseline = buffer_text(&terminal);
                assert!(baseline.contains(keys));
                let lines: Vec<_> = baseline.lines().collect();
                let height = terminal.backend().buffer().area.height;
                let rows = view::body_rows(ratatui::layout::Rect::new(0, 0, width, height), footer);
                let body = lines[3..3 + rows].join("\n");
                assert!(!body.contains("/projects"));
                assert!(!body.contains("enabled"));
                assert!(lines[3 + rows..].join("\n").contains("enabled"));
                if name == "confirm" {
                    assert!(body.contains("Stops and unregisters service."));
                    assert!(body.contains("> Cancel"));
                    assert!(body.contains("Disable"));
                }
                export_page(&terminal, name);
                let footer_line = lines.iter().position(|line| line.contains(keys)).unwrap();
                for state in ["progress", "success", "offline", "recovered"] {
                    ui.unavailable = (state == "offline").then(|| "offline".into());
                    ui.notice = (state == "success").then(|| {
                        (
                            "started api".into(),
                            Instant::now() + Duration::from_secs(3),
                        )
                    });
                    terminal
                        .draw(|frame| {
                            draw_main(
                                frame,
                                &mut ui,
                                if state == "progress" {
                                    "starting api..."
                                } else {
                                    ""
                                },
                            )
                        })
                        .unwrap();
                    let text = buffer_text(&terminal);
                    let updated: Vec<_> = text.lines().collect();
                    assert_eq!(
                        unicode_width::UnicodeWidthStr::width(
                            &updated[footer_line][..updated[footer_line].find(keys).unwrap()]
                        ),
                        unicode_width::UnicodeWidthStr::width(
                            &lines[footer_line][..lines[footer_line].find(keys).unwrap()]
                        )
                    );
                    assert_eq!(updated[3..3 + rows], lines[3..3 + rows]);
                    assert_eq!(view::main_footer(&ui), footer);
                    let expected = match state {
                        "progress" => "starting api...",
                        "success" => "started api",
                        "offline" => "Manager",
                        _ => "enabled",
                    };
                    assert_eq!(
                        text.matches(expected).count(),
                        1,
                        "{name}/{state}/{width}: {text}"
                    );
                    if state == "offline" {
                        assert_eq!(ui.key(KeyCode::Char('s'), false, rows), Intent::None);
                        export_page(&terminal, &format!("{name}-offline"));
                    }
                    if state == "progress" {
                        export_page(&terminal, &format!("{name}-progress"));
                    }
                }
            }
            ui.page = Page::Actions {
                selected: 5,
                scroll: 0,
            };
            ui.key(KeyCode::Char('?'), false, 3);
            assert!(
                ui.help
                    .as_ref()
                    .unwrap()
                    .content
                    .contains("Directory: /projects/api")
            );
            terminal
                .draw(|frame| draw_main(frame, &mut ui, ""))
                .unwrap();
            assert!(buffer_text(&terminal).contains("?/esc/q back"));
            export_page(&terminal, "help");
            ui.key(KeyCode::Char('?'), false, 3);
            assert!(matches!(ui.page, Page::Actions { selected: 5, .. }));
            ui.message = Some(Reader::new("Error", "An operation failed.\n".repeat(40)));
            terminal
                .draw(|frame| draw_main(frame, &mut ui, ""))
                .unwrap();
            assert!(buffer_text(&terminal).contains("esc/q back"));
            export_page(&terminal, "error");
            terminal
                .draw(|frame| draw_reader(frame, ui.message.as_mut().unwrap(), view::CRASH))
                .unwrap();
            assert!(buffer_text(&terminal).contains("enter/y open log   n/esc cancel"));
            export_page(&terminal, "crash");
            let records = vec![crate::protocol::HistoryRecord {
                id: "latest".into(),
                bytes: 123,
                current: true,
                persisted: true,
            }];
            terminal
                .draw(|frame| draw_history_list(frame, "api", &records, 0))
                .unwrap();
            assert!(buffer_text(&terminal).contains("enter open   ? help   esc/q back"));
            export_page(&terminal, "history");
            let mut history = HistoryView::new("latest".into());
            history.content = "log output\n".repeat(40);
            history.total_lines = 40;
            terminal
                .draw(|frame| draw_history_content(frame, "api", &history))
                .unwrap();
            assert!(buffer_text(&terminal).contains("1/40"));
            export_page(&terminal, "logs");
            terminal
                .draw(|frame| draw_main(frame, &mut MainUi::default(), ""))
                .unwrap();
            assert!(buffer_text(&terminal).contains("served enable"));
            export_page(&terminal, "empty");
        }
    }

    #[test]
    fn action_paging_keeps_selection_visible_and_full_details_readable() {
        use model::Page;
        let mut ui = MainUi::default();
        let mut service = service_info(false);
        service.directory = format!("/{}END", "项目/".repeat(80));
        service.name = format!("{}END", "长名称".repeat(20));
        let full_path = service.directory.clone();
        let full_name = service.name.clone();
        ui.refresh(vec![service]);
        ui.key(KeyCode::Enter, false, 3);
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        for width in [40, 60, 120, 40] {
            terminal.backend_mut().resize(width, 10);
            let rows = view::body_rows(ratatui::layout::Rect::new(0, 0, width, 10), view::ACTIONS);
            for (key, expected) in [
                (KeyCode::Home, 0),
                (KeyCode::PageDown, rows.min(5)),
                (KeyCode::End, 5),
                (KeyCode::PageUp, 5usize.saturating_sub(rows)),
            ] {
                ui.key(key, false, rows);
                terminal
                    .draw(|frame| draw_main(frame, &mut ui, ""))
                    .unwrap();
                assert!(matches!(ui.page, Page::Actions { selected, .. } if selected == expected));
                assert!(
                    buffer_text(&terminal)
                        .contains(&format!("> {}", ServiceAction::ALL[expected].label()))
                );
            }
        }
        ui.key(KeyCode::Char('?'), false, 3);
        let help = ui.help.as_ref().unwrap();
        assert!(help.content.contains(&full_path));
        assert!(help.content.contains(&full_name));
        assert!(help.content.contains("Type: enabled"));
        ui.key(KeyCode::End, false, 3);
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        assert!(ui.help.as_ref().unwrap().scroll > 0);
    }

    #[test]
    fn footer_preserves_path_suffix_and_service_kind() {
        for kind in [
            crate::protocol::ServiceKind::Enabled,
            crate::protocol::ServiceKind::Temporary,
        ] {
            for width in [40, 54, 80, 120] {
                let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
                let mut ui = MainUi::default();
                let mut service = service_info(false);
                service.kind = kind;
                service.directory = format!("/{}终e\u{301}", "项目/".repeat(40));
                ui.refresh(vec![service]);
                terminal
                    .draw(|frame| draw_main(frame, &mut ui, ""))
                    .unwrap();
                let text = buffer_text(&terminal);
                let label = if kind == crate::protocol::ServiceKind::Enabled {
                    "enabled"
                } else {
                    "temporary"
                };
                assert!(text.contains(&format!("终e\u{301} · {label}")), "{text}");
                assert!(text.contains("enter actions   ? help   q quit"));
            }
        }
    }

    #[test]
    fn history_render_shows_logical_position_and_contextual_help() {
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut history = HistoryView::new("latest".into());
        history.content = "first\nsecond\nthird".into();
        history.total_lines = 3;
        history.scroll = 1;
        terminal
            .draw(|frame| draw_history_content(frame, "api", &history))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("second"));
        assert!(text.contains("2/3"));
        assert!(text.contains("? help"));
        assert!(text.contains("esc/q back"));
        history.scroll = 0;
        terminal
            .draw(|frame| draw_history_content(frame, "api", &history))
            .unwrap();
        assert!(!buffer_text(&terminal).contains("1/3"));
        history.content = "长文本".repeat(40);
        history.total_lines = 1;
        terminal
            .draw(|frame| draw_history_content(frame, "api", &history))
            .unwrap();
        assert!(buffer_text(&terminal).contains("1/1"));
    }
}
