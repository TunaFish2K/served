use std::{
    fs,
    io::{self, stdout},
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use rand::{seq::SliceRandom, thread_rng};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
use tui_textarea::{Input, TextArea};

use crate::{
    client,
    config::{CONFIG_FILE, ENV_FILE, RestartPolicy, ServiceConfig},
    paths::ServedPaths,
    protocol::{Request, Response, ServiceInfo, ServiceState, Target},
};

const TIPS: &[&str] = &[
    "one directory, one .served.json, one working directory",
    "the manager starts enabled services after a user-session restart",
    "tty:false keeps output as pipes and disables attach",
    "restart validates the new files before stopping the old process",
    "service output is intentionally kept in memory only",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorField {
    Name,
    Command,
    Tty,
    Restart,
    Env,
}

impl EditorField {
    fn next(self) -> Self {
        match self {
            Self::Name => Self::Command,
            Self::Command => Self::Tty,
            Self::Tty => Self::Restart,
            Self::Restart => Self::Env,
            Self::Env => Self::Name,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Name => Self::Env,
            Self::Command => Self::Name,
            Self::Tty => Self::Command,
            Self::Restart => Self::Tty,
            Self::Env => Self::Restart,
        }
    }

    fn text_index(self) -> Option<usize> {
        match self {
            Self::Name => Some(0),
            Self::Command => Some(1),
            Self::Env => Some(2),
            Self::Tty | Self::Restart => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Command => "command",
            Self::Tty => "TTY",
            Self::Restart => "restart",
            Self::Env => ENV_FILE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorPopup {
    Tty { selected: bool },
    Restart { selected: RestartPolicy },
}

impl EditorPopup {
    fn open(field: EditorField, tty: bool, restart: RestartPolicy) -> Option<Self> {
        match field {
            EditorField::Tty => Some(Self::Tty { selected: tty }),
            EditorField::Restart => Some(Self::Restart { selected: restart }),
            EditorField::Name | EditorField::Command | EditorField::Env => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Tty { .. } => "select TTY",
            Self::Restart { .. } => "select restart policy",
        }
    }

    fn option_count(self) -> usize {
        match self {
            Self::Tty { .. } => 2,
            Self::Restart { .. } => 3,
        }
    }

    fn option_label(self, index: usize) -> &'static str {
        match self {
            Self::Tty { .. } => match index {
                0 => "Enabled",
                _ => "Disabled",
            },
            Self::Restart { .. } => restart_name(restart_from_index(index)),
        }
    }

    fn selected_index(self) -> usize {
        match self {
            Self::Tty { selected } => usize::from(!selected),
            Self::Restart { selected } => restart_index(selected),
        }
    }

    fn move_up(&mut self) {
        match self {
            Self::Tty { selected } => *selected = !*selected,
            Self::Restart { selected } => *selected = previous_restart(*selected),
        }
    }

    fn move_down(&mut self) {
        match self {
            Self::Tty { selected } => *selected = !*selected,
            Self::Restart { selected } => *selected = next_restart(*selected),
        }
    }

    fn apply(self, tty: &mut bool, restart: &mut RestartPolicy) {
        match self {
            Self::Tty { selected } => *tty = selected,
            Self::Restart { selected } => *restart = selected,
        }
    }
}

fn random_tip() -> &'static str {
    TIPS.choose(&mut thread_rng()).copied().unwrap_or(TIPS[0])
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
                notice = format!("manager unavailable: {error}");
            }
            Ok(response) => {
                notice = format!("unexpected manager response: {response:?}");
            }
        }

        terminal.draw(|frame| draw(frame, &services, selected, tip, &notice))?;
        if !event::poll(Duration::from_millis(250)).context("poll terminal event")? {
            continue;
        }
        let Event::Key(key) = event::read().context("read terminal event")? else {
            continue;
        };
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
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
                if let Some(service) = services.get(selected).filter(|service| service.tty) {
                    match client::attach(&paths, service.name.clone()).await {
                        Ok(stream) => {
                            notice = "".to_owned();
                            attach_session(stream).await?;
                        }
                        Err(error) => notice = format!("attach: {error}"),
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

fn draw(frame: &mut Frame<'_>, services: &[ServiceInfo], selected: usize, tip: &str, notice: &str) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(7),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(frame.area());

    let title = Paragraph::new(Line::from("served"))
        .block(Block::default().borders(Borders::ALL).title("manager"));
    frame.render_widget(title, areas[0]);

    let items: Vec<ListItem> = services
        .iter()
        .map(|service| {
            let state = state_name(&service.state);
            ListItem::new(format!(
                "{:<18} {:<11} {}",
                service.name, state, service.directory
            ))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("enabled services"),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut list_state = ListState::default();
    if !services.is_empty() {
        list_state.select(Some(selected.min(services.len() - 1)));
    }
    frame.render_stateful_widget(list, areas[1], &mut list_state);

    let output = services
        .get(selected)
        .map(|service| service.output_tail.as_str())
        .unwrap_or("No service selected");
    frame.render_widget(
        Paragraph::new(output)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("recent output"),
            )
            .wrap(Wrap { trim: false }),
        areas[2],
    );
    frame.render_widget(Paragraph::new(notice), areas[3]);
    frame.render_widget(Paragraph::new(format!("tips: {tip}")), areas[4]);
    frame.render_widget(
        Paragraph::new(main_footer(services, selected)).wrap(Wrap { trim: false }),
        areas[5],
    );
}

fn main_footer(services: &[ServiceInfo], selected: usize) -> String {
    let navigation = "up/down/j/k move";
    let Some(service) = services.get(selected) else {
        return format!("{navigation}   q/Esc quit");
    };
    let attach = if service.tty {
        "a attach"
    } else {
        "a attach unavailable"
    };
    format!("{navigation}   r restart   d disable   {attach}   q/Esc quit")
}

fn state_name(state: &ServiceState) -> &'static str {
    match state {
        ServiceState::Starting => "starting",
        ServiceState::Running => "running",
        ServiceState::Restarting => "restarting",
        ServiceState::Stopped => "stopped",
        ServiceState::Failed => "failed",
    }
}

async fn attach_session(stream: UnixStream) -> Result<()> {
    let (mut socket_read, mut socket_write) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut input = [0_u8; 8192];
    let mut output = [0_u8; 8192];
    loop {
        tokio::select! {
            count = stdin.read(&mut input) => {
                let count = count?;
                if count == 0 || input[..count].contains(&0x1d) {
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
        }
    }
}

pub fn edit_current(directory: &Path) -> Result<()> {
    let config_path = directory.join(CONFIG_FILE);
    let env_path = directory.join(ENV_FILE);
    let config = if config_path.is_file() {
        serde_json::from_slice::<ServiceConfig>(&fs::read(&config_path)?)
            .context("parse existing .served.json")?
    } else {
        ServiceConfig::template(directory)
    };
    let env = if env_path.is_file() {
        fs::read_to_string(&env_path).context("read .env")?
    } else {
        String::new()
    };
    let mut fields = vec![
        TextArea::new(vec![config.name]),
        TextArea::new(vec![config.command]),
        TextArea::new(if env.is_empty() {
            vec![String::new()]
        } else {
            env.lines().map(ToOwned::to_owned).collect()
        }),
    ];
    for (index, field) in fields.iter_mut().enumerate() {
        field.set_block(Block::default().borders(Borders::ALL).title(match index {
            0 => "name",
            1 => "command",
            _ => ENV_FILE,
        }));
    }

    let mut terminal =
        Terminal::new(CrosstermBackend::new(stdout())).context("create editor terminal")?;
    enable_raw_mode().context("enable editor raw mode")?;
    execute!(terminal.backend_mut(), EnterAlternateScreen).context("enter editor screen")?;
    let result = editor_loop(
        &mut terminal,
        &mut fields,
        config.tty,
        config.restart,
        random_tip(),
    );
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    let (tty, restart) = match result {
        Ok(value) => value,
        Err(error) if error.to_string() == "editor cancelled" => return Ok(()),
        Err(error) => return Err(error),
    };

    let config = ServiceConfig {
        name: fields[0].lines().join("\n").trim().to_owned(),
        command: fields[1].lines().join("\n"),
        tty,
        restart,
    };
    config.validate().map_err(|error| anyhow::anyhow!(error))?;
    fs::create_dir_all(directory).context("create service directory")?;
    fs::write(
        config_path,
        format!("{}\n", serde_json::to_string_pretty(&config)?),
    )?;
    fs::write(env_path, format!("{}\n", fields[2].lines().join("\n")))?;
    Ok(())
}

fn editor_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    fields: &mut [TextArea<'static>],
    mut tty: bool,
    mut restart: RestartPolicy,
    tip: &str,
) -> Result<(bool, RestartPolicy)> {
    let mut selected = EditorField::Name;
    let mut popup = None;
    loop {
        terminal.draw(|frame| draw_editor(frame, fields, tty, restart, selected, popup, tip))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            bail!("editor cancelled");
        }

        if let Some(current_popup) = popup.as_mut() {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => current_popup.move_up(),
                KeyCode::Down | KeyCode::Char('j') => current_popup.move_down(),
                KeyCode::Enter => {
                    if let Some(current_popup) = popup.take() {
                        current_popup.apply(&mut tty, &mut restart);
                    }
                }
                KeyCode::Esc => popup = None,
                _ => {}
            }
            continue;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('s') {
                return Ok((tty, restart));
            }
            continue;
        }

        match key.code {
            KeyCode::Esc => bail!("editor cancelled"),
            KeyCode::BackTab => selected = selected.previous(),
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                selected = selected.previous();
            }
            KeyCode::Tab => selected = selected.next(),
            KeyCode::Enter => {
                if let Some(current_popup) = EditorPopup::open(selected, tty, restart) {
                    popup = Some(current_popup);
                } else if let Some(index) = selected.text_index() {
                    fields[index].input(Input::from(key));
                }
            }
            _ => {
                if let Some(index) = selected.text_index() {
                    fields[index].input(Input::from(key));
                }
            }
        }
    }
}

fn draw_editor(
    frame: &mut Frame<'_>,
    fields: &[TextArea<'static>],
    tty: bool,
    restart: RestartPolicy,
    selected: EditorField,
    popup: Option<EditorPopup>,
    tip: &str,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(frame.area());

    frame.render_widget(
        Paragraph::new("edit .served.json and .env").block(
            Block::default()
                .borders(Borders::ALL)
                .title(".served editor"),
        ),
        areas[0],
    );

    for (index, field) in fields.iter().enumerate().take(2) {
        let field_kind = if index == 0 {
            EditorField::Name
        } else {
            EditorField::Command
        };
        let mut field = field.clone();
        let block_style = if selected == field_kind {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        field.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(block_style)
                .title(field_kind.title()),
        );
        if selected == field_kind {
            field.set_cursor_line_style(Style::default().bg(Color::DarkGray));
        }
        frame.render_widget(&field, areas[index + 1]);
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            tty_name(tty),
            option_style(selected == EditorField::Tty),
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(choice_border_style(selected == EditorField::Tty))
                .title(EditorField::Tty.title()),
        ),
        areas[3],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            restart_name(restart),
            option_style(selected == EditorField::Restart),
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(choice_border_style(selected == EditorField::Restart))
                .title(EditorField::Restart.title()),
        ),
        areas[4],
    );

    if let Some(field) = fields.get(2) {
        let mut field = field.clone();
        let block_style = if selected == EditorField::Env {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        };
        field.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(block_style)
                .title(EditorField::Env.title()),
        );
        if selected == EditorField::Env {
            field.set_cursor_line_style(Style::default().bg(Color::DarkGray));
        }
        frame.render_widget(&field, areas[5]);
    }

    frame.render_widget(Paragraph::new(format!("tips: {tip}")), areas[6]);
    frame.render_widget(
        Paragraph::new(editor_footer(selected, popup.is_some())).wrap(Wrap { trim: false }),
        areas[7],
    );

    if let Some(popup) = popup {
        draw_popup(frame, popup);
    }
}

fn draw_popup(frame: &mut Frame<'_>, popup: EditorPopup) {
    let area = centered_rect(frame.area(), 34, popup.option_count() as u16 + 2);
    frame.render_widget(Clear, area);
    let items: Vec<ListItem> = (0..popup.option_count())
        .map(|index| ListItem::new(popup.option_label(index)))
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(popup.title()))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(Some(popup.selected_index()));
    frame.render_stateful_widget(list, area, &mut state);
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn option_style(selected: bool) -> Style {
    if selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

fn choice_border_style(selected: bool) -> Style {
    if selected {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    }
}

fn tty_name(tty: bool) -> &'static str {
    if tty { "Enabled" } else { "Disabled" }
}

fn editor_footer(selected: EditorField, popup_open: bool) -> String {
    if popup_open {
        "up/down/j/k choose   Enter apply   Esc close".to_owned()
    } else if matches!(selected, EditorField::Tty | EditorField::Restart) {
        "Tab next   Shift-Tab previous   Enter choose   Ctrl-S save   Esc/Ctrl-C cancel".to_owned()
    } else {
        "Tab next   Shift-Tab previous   Ctrl-S save   Esc/Ctrl-C cancel".to_owned()
    }
}

fn next_restart(policy: RestartPolicy) -> RestartPolicy {
    match policy {
        RestartPolicy::Never => RestartPolicy::OnFailure,
        RestartPolicy::OnFailure => RestartPolicy::Always,
        RestartPolicy::Always => RestartPolicy::Never,
    }
}

fn previous_restart(policy: RestartPolicy) -> RestartPolicy {
    match policy {
        RestartPolicy::Never => RestartPolicy::Always,
        RestartPolicy::OnFailure => RestartPolicy::Never,
        RestartPolicy::Always => RestartPolicy::OnFailure,
    }
}

fn restart_index(policy: RestartPolicy) -> usize {
    match policy {
        RestartPolicy::Never => 0,
        RestartPolicy::OnFailure => 1,
        RestartPolicy::Always => 2,
    }
}

fn restart_from_index(index: usize) -> RestartPolicy {
    match index % 3 {
        0 => RestartPolicy::Never,
        1 => RestartPolicy::OnFailure,
        _ => RestartPolicy::Always,
    }
}

fn restart_name(policy: RestartPolicy) -> &'static str {
    match policy {
        RestartPolicy::Never => "never",
        RestartPolicy::OnFailure => "on-failure",
        RestartPolicy::Always => "always",
    }
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
            restart: restart_name(RestartPolicy::Never).to_owned(),
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

    fn editor_fields() -> Vec<TextArea<'static>> {
        vec![
            TextArea::new(vec!["api".to_owned()]),
            TextArea::new(vec!["./run.sh".to_owned()]),
            TextArea::new(vec!["PORT=8080".to_owned()]),
        ]
    }

    #[test]
    fn editor_focus_follows_visual_order_and_wraps() {
        assert_eq!(EditorField::Name.next(), EditorField::Command);
        assert_eq!(EditorField::Command.next(), EditorField::Tty);
        assert_eq!(EditorField::Tty.next(), EditorField::Restart);
        assert_eq!(EditorField::Restart.next(), EditorField::Env);
        assert_eq!(EditorField::Env.next(), EditorField::Name);

        assert_eq!(EditorField::Name.previous(), EditorField::Env);
        assert_eq!(EditorField::Env.previous(), EditorField::Restart);
        assert_eq!(EditorField::Restart.previous(), EditorField::Tty);
    }

    #[test]
    fn popup_selection_is_staged_until_apply() {
        let mut tty = true;
        let mut restart = RestartPolicy::Never;
        let mut tty_popup = EditorPopup::open(EditorField::Tty, tty, restart).expect("TTY popup");
        tty_popup.move_down();
        assert_eq!(
            tty_popup.option_label(tty_popup.selected_index()),
            "Disabled"
        );
        assert!(tty);

        let mut restart_popup =
            EditorPopup::open(EditorField::Restart, tty, restart).expect("restart popup");
        restart_popup.move_down();
        restart_popup.move_down();
        assert_eq!(restart_popup.selected_index(), 2);
        restart_popup.move_up();
        restart_popup.apply(&mut tty, &mut restart);
        assert!(tty);
        assert_eq!(restart, RestartPolicy::OnFailure);
    }

    #[test]
    fn main_footer_describes_available_actions() {
        assert_eq!(main_footer(&[], 0), "up/down/j/k move   q/Esc quit");

        let tty_services = vec![service_info(true)];
        let tty_footer = main_footer(&tty_services, 0);
        assert!(tty_footer.contains("r restart"));
        assert!(tty_footer.contains("d disable"));
        assert!(tty_footer.contains("a attach"));
        assert!(!tty_footer.contains("unavailable"));

        let pipe_services = vec![service_info(false)];
        assert!(main_footer(&pipe_services, 0).contains("a attach unavailable"));
    }

    #[test]
    fn main_render_keeps_tip_and_contextual_footer() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let services = vec![service_info(true)];
        terminal
            .draw(|frame| draw(frame, &services, 0, "render tip", ""))
            .expect("draw");

        let text = buffer_text(&terminal);
        assert!(text.contains("tips: render tip"));
        assert!(text.contains("r restart"));
        assert!(text.contains("a attach"));
        assert!(text.contains("q/Esc quit"));
    }

    #[test]
    fn editor_render_shows_fields_and_popup_options() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let fields = editor_fields();
        terminal
            .draw(|frame| {
                draw_editor(
                    frame,
                    &fields,
                    false,
                    RestartPolicy::OnFailure,
                    EditorField::Restart,
                    Some(EditorPopup::Restart {
                        selected: RestartPolicy::OnFailure,
                    }),
                    "editor tip",
                )
            })
            .expect("draw");

        let text = buffer_text(&terminal);
        assert!(text.contains("name"));
        assert!(text.contains("command"));
        assert!(text.contains("TTY"));
        assert!(text.contains("Disabled"));
        assert!(text.contains("restart"));
        assert!(text.contains("on-failure"));
        assert!(text.contains("always"));
        assert!(text.contains(".env"));
        assert!(text.contains("tips: editor tip"));
        assert!(text.contains("Enter apply"));
    }
}
