use std::{
    io::{self, stdout},
    path::Path,
    time::{Duration, Instant},
};

use crate::{
    attach::{attach_session, crash_warning},
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
        disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};

mod model;
mod view;

use model::{
    CrashLogPrompt, CrashPromptAction, HistoryView, Intent, LifecycleAction, MainUi,
    PendingLifecycleAction, Reader, ServiceAction, crash_prompt_action,
};
use model::{Notice, Tone};
use view::{draw_history_content, draw_history_list, draw_main, draw_reader};

#[cfg(test)]
use crate::protocol::{ServiceInfo, ServiceKind, ServiceState};
#[cfg(test)]
use view::history_position;

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
                ui.notice = Some((
                    Notice::new(outcome.notice, Tone::Success),
                    Instant::now() + Duration::from_secs(3),
                ));
            } else {
                ui.help = None;
                ui.message = Some(Reader::error("Operation failed", outcome.notice));
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
        let rows = if crash_prompt.is_some() {
            view::body_rows(area, view::CRASH, None)
        } else {
            view::main_rows(area, &ui)
        };
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
                    ui.message = Some(Reader::error(
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
                                    ui.message = Some(Reader::error(
                                        "Attach unavailable",
                                        format!("{warning}\n\nOpen latest.log?"),
                                    ));
                                    crash_prompt = Some(CrashLogPrompt { warning, path });
                                } else {
                                    ui.message = Some(Reader::error(
                                        "Attach unavailable",
                                        format!(
                                            "{warning}\n\nNo latest.log. Use history or enable persist_logs."
                                        ),
                                    ));
                                }
                            } else {
                                ui.message =
                                    Some(Reader::error("Attach failed", error.to_string()));
                            }
                        }
                    }
                    ServiceAction::History => {
                        if let Err(error) = history_in_tui(terminal, &paths, &name).await {
                            ui.message = Some(Reader::error("History failed", error.to_string()));
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
            if help.is_none() && message.is_none() && view.is_none() {
                Some(records.len())
            } else {
                None
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
                        message = Some(Reader::error("History failed", error.to_string()));
                    }
                    clamp_history_scroll(history_view);
                }
                KeyCode::PageUp => {
                    history_view.scroll = history_view.scroll.saturating_sub(rows as u64);
                }
                KeyCode::PageDown => {
                    history_view.scroll = history_view.scroll.saturating_add(rows as u64);
                    if let Err(error) = load_history_if_needed(paths, name, history_view).await {
                        message = Some(Reader::error("History failed", error.to_string()));
                    }
                    clamp_history_scroll(history_view);
                }
                KeyCode::Home | KeyCode::Char('g') => history_view.scroll = 0,
                KeyCode::End | KeyCode::Char('G') => {
                    while !history_view.eof {
                        if let Err(error) = load_history_chunk(paths, name, history_view).await {
                            message = Some(Reader::error("History failed", error.to_string()));
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
                            message = Some(Reader::error("History failed", error.to_string()))
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
    let attach_result = attach_session(paths, name, session, true, true, false).await;
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

    fn assert_selected(terminal: &Terminal<TestBackend>, expected: &str) {
        use ratatui::style::{Color, Modifier};
        let buffer = terminal.backend().buffer();
        let margin = if buffer.area.width < 80 { 1 } else { 2 };
        let mut selected = Vec::new();
        for y in 3..buffer.area.height {
            if buffer[(margin + 2, y)]
                .modifier
                .contains(Modifier::REVERSED)
            {
                for x in margin..margin + 2 {
                    let cell = &buffer[(x, y)];
                    assert_eq!(cell.symbol(), " ");
                    assert_eq!(cell.fg, Color::Reset);
                    assert!(!cell.modifier.contains(Modifier::REVERSED));
                }
            }
            let text: String = (margin + 2..buffer.area.width)
                .filter(|&x| buffer[(x, y)].modifier.contains(Modifier::REVERSED))
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            if !text.trim().is_empty() {
                selected.push(text.trim().to_owned());
            }
        }
        assert_eq!(selected.len(), 1, "{selected:?}");
        assert!(selected[0].starts_with(expected), "{selected:?}");
    }

    #[test]
    fn history_position_is_one_based_and_clamped() {
        assert_eq!(history_position(0, 0), (0, 0));
        assert_eq!(history_position(0, 3), (1, 3));
        assert_eq!(history_position(1, 3), (2, 3));
        assert_eq!(history_position(99, 3), (3, 3));
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
                .any(|cell| cell.fg
                    == if view::colors_enabled() {
                        ratatui::style::Color::Red
                    } else {
                        ratatui::style::Color::Reset
                    })
        );
        ui.key(KeyCode::Enter, false, 2);
        ui.key(KeyCode::End, false, 2);
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        assert!(buffer_text(&terminal).contains("enabled"));
        assert!(matches!(ui.page, model::Page::Actions { selected: 5, .. }));
        assert_selected(&terminal, "Disable");
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
        ui.message = Some(Reader::error("Failed", "error detail\n".repeat(30)));
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
        ui.notice = Some((
            Notice::new("stopped api", Tone::Success),
            now + Duration::from_secs(3),
        ));
        assert_eq!(ui.notice(now).unwrap().text, "stopped api");
        assert!(ui.notice(now + Duration::from_secs(3)).is_none());
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
                "fg": format!("{:?}", cell.fg),
                "bg": format!("{:?}", cell.bg),
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
            let mut terminal = Terminal::new(TestBackend::new(
                width,
                match width {
                    60 => 16,
                    80 => 24,
                    120 => 40,
                    _ => 10,
                },
            ))
            .unwrap();
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
                let rows = view::main_rows(ratatui::layout::Rect::new(0, 0, width, height), &ui);
                let body = lines[3..3 + rows].join("\n");
                assert!(!body.contains("/projects"));
                assert!(!body.contains("enabled"));
                assert!(lines[3 + rows..].join("\n").contains("enabled"));
                if name == "confirm" {
                    assert!(body.contains("Stops and unregisters service."));
                    assert_selected(&terminal, "Cancel");
                    assert!(body.contains("Disable"));
                }
                export_page(&terminal, name);
                let footer_line = lines.iter().position(|line| line.contains(keys)).unwrap();
                for state in ["progress", "success", "offline", "recovered"] {
                    ui.unavailable = (state == "offline").then(|| "offline".into());
                    ui.notice = (state == "success").then(|| {
                        (
                            Notice::new("started api", Tone::Success),
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
            ui.message = Some(Reader::error("Error", "An operation failed.\n".repeat(40)));
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
            assert_selected(&terminal, "latest");
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
            let rows = view::body_rows(
                ratatui::layout::Rect::new(0, 0, width, 10),
                view::ACTIONS,
                Some(6),
            );
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
                assert_selected(&terminal, ServiceAction::ALL[expected].label());
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
    fn compact_lists_grow_then_scroll_without_stretching_to_window_width() {
        for (width, height) in [(40, 10), (60, 16), (80, 24), (120, 40)] {
            for count in [0usize, 1, 6, 7, 50] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let mut ui = MainUi::default();
                ui.refresh(
                    (0..count)
                        .map(|i| {
                            let mut service = service_info(false);
                            service.name = format!("service-{i:02}");
                            service
                        })
                        .collect(),
                );
                ui.selected = count.saturating_sub(1);
                terminal
                    .draw(|frame| draw_main(frame, &mut ui, ""))
                    .unwrap();
                let text = buffer_text(&terminal);
                let lines: Vec<_> = text.lines().collect();
                let footer_y = lines
                    .iter()
                    .position(|line| line.contains("q quit"))
                    .unwrap();
                if height >= 24 {
                    assert_eq!(footer_y, 4 + count.clamp(6, (height - 6) as usize));
                }
                if count > 0 {
                    assert_selected(&terminal, &format!("service-{:02}", count - 1));
                }
                assert!(lines[1].contains(&format!("served · {count} service")));
                if width == 120 {
                    let buffer = terminal.backend().buffer();
                    assert!(
                        (0..height).all(|y| (78..width).all(|x| buffer[(x, y)].symbol() == " "))
                    );
                }
                export_page(&terminal, &format!("services-{count}"));
            }
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut reader = Reader::new("Help", "row\n".repeat(100));
            terminal
                .draw(|frame| draw_reader(frame, &mut reader, view::HELP))
                .unwrap();
            let text = buffer_text(&terminal);
            assert!(
                text.lines()
                    .nth(height as usize - 2)
                    .unwrap()
                    .contains("?/esc/q back")
            );
        }
    }

    #[test]
    fn semantic_colors_and_no_color_preserve_text_and_selection() {
        use ratatui::style::{Color, Modifier};
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut ui = MainUi::default();
        ui.refresh(
            [
                ServiceState::Running,
                ServiceState::Starting,
                ServiceState::Restarting,
                ServiceState::Failed,
                ServiceState::Stopped,
            ]
            .into_iter()
            .enumerate()
            .map(|(i, state)| {
                let mut service = service_info(false);
                service.name = format!("service-{i}");
                service.state = state;
                service
            })
            .collect(),
        );
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let color = |expected| {
            if view::colors_enabled() {
                expected
            } else {
                Color::Reset
            }
        };
        assert_eq!(buffer[(2, 1)].fg, color(Color::Cyan));
        assert_eq!(buffer[(2, 3)].symbol(), " ");
        assert_eq!(buffer[(2, 3)].fg, Color::Reset);
        assert!(!buffer[(2, 3)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(2, 3)].modifier.contains(Modifier::REVERSED));
        assert!(buffer[(4, 3)].modifier.contains(Modifier::REVERSED));
        assert!(!buffer[(15, 3)].modifier.contains(Modifier::REVERSED));
        for (row, expected) in [
            Color::Green,
            Color::Yellow,
            Color::Yellow,
            Color::Red,
            Color::Reset,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(buffer[(15, 3 + row as u16)].fg, color(expected));
        }
        assert!(buffer.content.iter().all(|cell| cell.bg == Color::Reset));
        if !view::colors_enabled() {
            assert!(buffer.content.iter().all(|cell| cell.fg == Color::Reset));
        }
        export_page(&terminal, "states");
        for (name, notice, progress, expected) in [
            (
                "success",
                Some(Notice::new("started api", Tone::Success)),
                "",
                Color::Green,
            ),
            ("progress", None, "starting api...", Color::Yellow),
        ] {
            ui.notice = notice.map(|notice| (notice, Instant::now() + Duration::from_secs(3)));
            terminal
                .draw(|frame| draw_main(frame, &mut ui, progress))
                .unwrap();
            assert_eq!(terminal.backend().buffer()[(2, 10)].fg, color(expected));
            export_page(&terminal, name);
        }
        ui.notice = None;
        ui.unavailable = Some("offline".into());
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(2, 10)].fg,
            color(Color::Yellow)
        );
        ui.unavailable = None;
        ui.key(KeyCode::Char('d'), false, 6);
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(2, 3)].fg, color(Color::Red));
        ui.message = Some(Reader::error("Failed", "Diagnostic details"));
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        assert_eq!(terminal.backend().buffer()[(2, 1)].fg, color(Color::Red));
        assert_eq!(terminal.backend().buffer()[(2, 3)].fg, Color::Reset);
        if !view::colors_enabled() {
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset)
            );
        }
    }

    #[test]
    fn selection_changes_style_without_moving_list_text() {
        use model::Page;
        for width in [40, 80, 120] {
            let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
            let mut ui = MainUi::default();
            let mut first = service_info(false);
            first.name = "项目名称很长而且包含组合字符e\u{301}".repeat(3);
            let mut second = first.clone();
            second.name.push('2');
            ui.refresh(vec![first, second]);
            for page in [
                Page::Services,
                Page::Actions {
                    selected: 0,
                    scroll: 0,
                },
                Page::ConfirmDisable {
                    confirm: false,
                    menu: None,
                },
            ] {
                ui.page = page;
                terminal
                    .draw(|frame| draw_main(frame, &mut ui, ""))
                    .unwrap();
                let before = buffer_text(&terminal);
                ui.key(KeyCode::Down, false, 3);
                terminal
                    .draw(|frame| draw_main(frame, &mut ui, ""))
                    .unwrap();
                assert_eq!(before, buffer_text(&terminal));
                let expected = match ui.page {
                    Page::Services => "项",
                    Page::Actions { .. } => "Start",
                    Page::ConfirmDisable { .. } => "Disable",
                };
                assert_selected(&terminal, expected);
            }
            let records = (0..2)
                .map(|i| crate::protocol::HistoryRecord {
                    id: format!("record-{i}"),
                    bytes: 123,
                    current: false,
                    persisted: true,
                })
                .collect::<Vec<_>>();
            terminal
                .draw(|frame| draw_history_list(frame, "api", &records, 0))
                .unwrap();
            let before = buffer_text(&terminal);
            terminal
                .draw(|frame| draw_history_list(frame, "api", &records, 1))
                .unwrap();
            // Only the selected ID in the footer changes; list text stays in place.
            assert_eq!(
                before.lines().take(5).collect::<Vec<_>>(),
                buffer_text(&terminal).lines().take(5).collect::<Vec<_>>()
            );
            assert_selected(&terminal, "record-1");
        }
    }

    #[test]
    fn main_keeps_status_next_to_names_like_actions() {
        use ratatui::style::Modifier;
        for width in [40, 80, 120] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            let mut ui = MainUi::default();
            ui.refresh(vec![service_info(false)]);
            terminal
                .draw(|frame| draw_main(frame, &mut ui, ""))
                .unwrap();
            let margin = if width < 80 { 1 } else { 2 };
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(margin + 13, 3)].symbol(), "r");
            assert!(
                !buffer[(width - margin - 1, 3)]
                    .modifier
                    .contains(Modifier::REVERSED)
            );
            ui.key(KeyCode::Enter, false, 18);
            terminal
                .draw(|frame| draw_main(frame, &mut ui, ""))
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(margin + 13, 3)].symbol(), "a");
            assert!(
                !buffer[(width - margin - 1, 3)]
                    .modifier
                    .contains(Modifier::REVERSED)
            );
        }
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
