use std::{
    io::{self, IsTerminal, Write, stdout},
    path::Path,
    time::{Duration, Instant},
};

use crate::{
    client,
    config::CONFIG_FILE,
    editor,
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
use rand::{seq::SliceRandom, thread_rng};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::interval,
};

mod model;
mod view;

use model::{CrashLogPrompt, CrashPromptAction, HistoryView, crash_prompt_action};
use view::{draw_history_content, draw_history_list, draw_main};

#[cfg(test)]
use crate::protocol::{ServiceInfo, ServiceState};
#[cfg(test)]
use view::{history_position, main_footer};

const TIPS: &[&str] = &[
    "one directory, one .served.json, one working directory",
    "the manager starts enabled services after a user-session restart",
    "tty:false services support read-only attach",
    "restart validates the new files before stopping the old process",
    "history keeps live output separate from attach",
    "persistent logs live below the XDG state directory",
];

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

fn random_tip() -> &'static str {
    TIPS.choose(&mut thread_rng()).copied().unwrap_or(TIPS[0])
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
    let mut selected = 0_usize;
    let mut services = Vec::new();
    let mut notice = String::new();
    let mut crash_prompt: Option<CrashLogPrompt> = None;
    let tip = random_tip();
    let current_directory = std::env::current_dir().ok();

    loop {
        match client::request(&paths, Request::List).await {
            Ok(Response::Services { services: latest }) => {
                services = latest;
                if !services.is_empty() {
                    selected = selected.min(services.len() - 1);
                }
                if let Some(directory) = &current_directory {
                    let has_local_config = directory.join(CONFIG_FILE).is_file();
                    let enabled = services
                        .iter()
                        .any(|service| Path::new(&service.directory) == directory.as_path());
                    if has_local_config && !enabled && notice.is_empty() {
                        notice = "enable your service to manage it here!".to_owned();
                    }
                }
            }
            Err(error) => {
                if crash_prompt.is_none() {
                    notice = format!("manager unavailable: {error}");
                }
            }
            Ok(response) => {
                if crash_prompt.is_none() {
                    notice = format!("unexpected manager response: {response:?}");
                }
            }
        }

        terminal.draw(|frame| draw_main(frame, &services, selected, tip, &notice))?;
        if !event::poll(Duration::from_millis(250)).context("poll terminal event")? {
            continue;
        }
        let Event::Key(key) = event::read().context("read terminal event")? else {
            continue;
        };
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(());
        }
        if let Some(prompt) = crash_prompt.take() {
            match crash_prompt_action(&key.code) {
                CrashPromptAction::Open => {
                    notice = match open_editor_in_tui(terminal, &prompt.path).await {
                        Ok(()) => prompt.warning,
                        Err(error) => {
                            format!("{}; cannot open latest.log: {error}", prompt.warning)
                        }
                    };
                }
                CrashPromptAction::Cancel => {
                    notice = prompt.warning;
                }
                CrashPromptAction::Ignore => crash_prompt = Some(prompt),
            }
            continue;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Down | KeyCode::Char('j') => {
                if !services.is_empty() {
                    selected = (selected + 1).min(services.len() - 1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                selected = selected.saturating_sub(1);
            }
            KeyCode::Char('r') => {
                if let Some(service) = services.get(selected) {
                    notice = command_notice(
                        client::expect_ok(
                            &paths,
                            Request::Restart {
                                target: Target::Name(service.name.clone()),
                            },
                        )
                        .await,
                        "restart",
                    );
                }
            }
            KeyCode::Char('d') => {
                if let Some(service) = services.get(selected) {
                    notice = command_notice(
                        client::expect_ok(
                            &paths,
                            Request::Disable {
                                target: Target::Name(service.name.clone()),
                            },
                        )
                        .await,
                        "disable",
                    );
                }
            }
            KeyCode::Char('a') => {
                if let Some(service) = services.get(selected) {
                    match client::attach(&paths, service.name.clone()).await {
                        Ok(session) => {
                            notice = "".to_owned();
                            attach_in_tui(terminal, &paths, service.name.clone(), session).await?;
                        }
                        Err(error) => {
                            if let Some(unavailable) =
                                error.downcast_ref::<client::AttachUnavailable>()
                            {
                                let warning = crash_warning(unavailable);
                                if let Some(path) = unavailable.latest_log.clone() {
                                    notice = format!("{warning}; open latest.log? [Y/n]");
                                    crash_prompt = Some(CrashLogPrompt { warning, path });
                                } else {
                                    notice = format!(
                                        "{warning}; latest.log unavailable, use h history or enable persist_logs"
                                    );
                                }
                            } else {
                                notice = format!("attach: {error}");
                            }
                        }
                    }
                }
            }
            KeyCode::Char('h') => {
                if let Some(service) = services.get(selected) {
                    match history_in_tui(terminal, &paths, &service.name, tip).await {
                        Ok(()) => notice.clear(),
                        Err(error) => notice = format!("history: {error}"),
                    }
                }
            }
            _ => {}
        }
    }
}

fn command_notice(result: Result<()>, action: &str) -> String {
    match result {
        Ok(()) => format!("{action} requested"),
        Err(error) => format!("{action}: {error}"),
    }
}

async fn history_in_tui(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    paths: &ServedPaths,
    name: &str,
    tip: &str,
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

    loop {
        if let Some(history_view) = view.as_ref() {
            terminal.draw(|frame| draw_history_content(frame, name, history_view, tip))?;
        } else {
            terminal.draw(|frame| draw_history_list(frame, name, &records, selected, tip))?;
        }
        if !event::poll(Duration::from_millis(250)).context("poll history terminal event")? {
            continue;
        }
        let Event::Key(key) = event::read().context("read history terminal event")? else {
            continue;
        };

        if let Some(history_view) = view.as_mut() {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => view = None,
                KeyCode::Up | KeyCode::Char('k') => {
                    history_view.scroll = history_view.scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    history_view.scroll = history_view.scroll.saturating_add(1);
                    load_history_if_needed(paths, name, history_view).await?;
                    clamp_history_scroll(history_view);
                }
                KeyCode::PageUp => {
                    history_view.scroll = history_view.scroll.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    history_view.scroll = history_view.scroll.saturating_add(10);
                    load_history_if_needed(paths, name, history_view).await?;
                    clamp_history_scroll(history_view);
                }
                KeyCode::Home | KeyCode::Char('g') => history_view.scroll = 0,
                KeyCode::End | KeyCode::Char('G') => {
                    while !history_view.eof {
                        load_history_chunk(paths, name, history_view).await?;
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
                    load_history_chunk(paths, name, &mut history_view).await?;
                    view = Some(history_view);
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
            name: "api".to_owned(),
            directory: "/tmp/api".to_owned(),
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
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn history_position_is_one_based_and_clamped() {
        assert_eq!(history_position(0, 0), (0, 0));
        assert_eq!(history_position(0, 3), (1, 3));
        assert_eq!(history_position(1, 3), (2, 3));
        assert_eq!(history_position(99, 3), (3, 3));
    }

    #[test]
    fn main_footer_describes_available_actions() {
        assert_eq!(main_footer(&[], 0), "up/down/j/k move   q/Esc quit");

        let tty_services = vec![service_info(true)];
        let tty_footer = main_footer(&tty_services, 0);
        assert!(tty_footer.contains("r restart"));
        assert!(tty_footer.contains("d disable"));
        assert!(tty_footer.contains("a attach"));
        assert!(tty_footer.contains("h history"));
        assert!(!tty_footer.contains("unavailable"));

        let pipe_services = vec![service_info(false)];
        assert!(main_footer(&pipe_services, 0).contains("a attach"));
    }

    #[test]
    fn ctrl_c_is_the_attach_detach_byte() {
        assert!(input_requests_detach(b"output\x03"));
        assert!(!input_requests_detach(b"output\x1d"));
        assert!(!input_requests_detach(b"output"));
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
    fn main_render_keeps_tip_and_contextual_footer() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let services = vec![service_info(true)];
        terminal
            .draw(|frame| draw_main(frame, &services, 0, "render tip", ""))
            .expect("draw");

        let text = buffer_text(&terminal);
        assert!(text.contains("tips: render tip"));
        assert!(text.contains("r restart"));
        assert!(text.contains("a attach"));
        assert!(text.contains("q/Esc quit"));
        assert!(!text.contains("recent output"));
        assert!(!text.contains("ready"));
    }

    #[test]
    fn history_render_shows_position_tip_and_footer() {
        let backend = TestBackend::new(100, 16);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let mut view = HistoryView::new("latest".to_owned());
        view.content = "first\nsecond\nthird".to_owned();
        view.total_lines = 3;
        view.scroll = 1;
        terminal
            .draw(|frame| draw_history_content(frame, "api", &view, "history tip"))
            .expect("draw");

        let text = buffer_text(&terminal);
        assert!(text.contains("2/3"));
        assert!(text.contains("tips: history tip"));
        assert!(text.contains("Esc/q back"));
    }
}
