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
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
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
    let tip = TIPS.choose(&mut thread_rng()).copied().unwrap_or(TIPS[0]);
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
    let result = editor_loop(&mut terminal, &mut fields, config.tty, config.restart);
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
) -> Result<(bool, RestartPolicy)> {
    let mut selected = 0_usize;
    loop {
        terminal.draw(|frame| {
            let areas = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Length(5),
                    Constraint::Length(5),
                    Constraint::Min(6),
                    Constraint::Length(2),
                ])
                .split(frame.area());
            frame.render_widget(
                Paragraph::new(format!("tty: {}  restart: {}", tty, restart_name(restart))).block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(".served editor"),
                ),
                areas[0],
            );
            for (index, field) in fields.iter().enumerate() {
                let mut field = field.clone();
                if index == selected {
                    field.set_cursor_line_style(Style::default().bg(Color::DarkGray));
                }
                frame.render_widget(&field, areas[index + 1]);
            }
            frame.render_widget(Paragraph::new(""), areas[4]);
        })?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => return Ok((tty, restart)),
                KeyCode::Char('t') => tty = !tty,
                KeyCode::Char('r') => restart = next_restart(restart),
                KeyCode::Char('c') => bail!("editor cancelled"),
                _ => {}
            }
            continue;
        }
        match key.code {
            KeyCode::Esc => bail!("editor cancelled"),
            KeyCode::Tab => selected = (selected + 1) % fields.len(),
            _ => {
                fields[selected].input(Input::from(key));
            }
        }
    }
}

fn next_restart(policy: RestartPolicy) -> RestartPolicy {
    match policy {
        RestartPolicy::Never => RestartPolicy::OnFailure,
        RestartPolicy::OnFailure => RestartPolicy::Always,
        RestartPolicy::Always => RestartPolicy::Never,
    }
}

fn restart_name(policy: RestartPolicy) -> &'static str {
    match policy {
        RestartPolicy::Never => "never",
        RestartPolicy::OnFailure => "on-failure",
        RestartPolicy::Always => "always",
    }
}
