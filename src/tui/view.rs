use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use super::model::HistoryView;
use crate::protocol::{HistoryRecord, ServiceInfo, ServiceState};

pub(super) fn draw_main(
    frame: &mut Frame<'_>,
    services: &[ServiceInfo],
    selected: usize,
    tip: &str,
    notice: &str,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
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

    frame.render_widget(Paragraph::new(notice).wrap(Wrap { trim: false }), areas[2]);
    frame.render_widget(Paragraph::new(format!("tips: {tip}")), areas[3]);
    frame.render_widget(
        Paragraph::new(main_footer(services, selected)).wrap(Wrap { trim: false }),
        areas[4],
    );
}

pub(super) fn main_footer(services: &[ServiceInfo], selected: usize) -> String {
    let navigation = "up/down/j/k move";
    if services.get(selected).is_none() {
        return format!("{navigation}   q/Esc quit");
    }
    format!("{navigation}   r restart   d disable   a attach   h history   q/Esc quit")
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

pub(super) fn history_position(scroll: u64, total_lines: u64) -> (u64, u64) {
    if total_lines == 0 {
        (0, 0)
    } else {
        (scroll.min(total_lines.saturating_sub(1)) + 1, total_lines)
    }
}

pub(super) fn draw_history_list(
    frame: &mut Frame<'_>,
    name: &str,
    records: &[HistoryRecord],
    selected: usize,
    tip: &str,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(frame.area());
    let title = Paragraph::new(format!("history: {name}"))
        .block(Block::default().borders(Borders::ALL).title("served"));
    frame.render_widget(title, areas[0]);
    let items = records.iter().map(|record| {
        ListItem::new(format!(
            "{:<28} {:>10} bytes  {}",
            record.id,
            record.bytes,
            if record.persisted { "disk" } else { "memory" }
        ))
    });
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("runs"))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut state = ListState::default();
    if !records.is_empty() {
        state.select(Some(selected.min(records.len() - 1)));
    }
    frame.render_stateful_widget(list, areas[1], &mut state);
    frame.render_widget(Paragraph::new(format!("tips: {tip}")), areas[2]);
    frame.render_widget(
        Paragraph::new("up/down/j/k move   Enter open   Esc/q back").wrap(Wrap { trim: false }),
        areas[3],
    );
}

pub(super) fn draw_history_content(
    frame: &mut Frame<'_>,
    name: &str,
    view: &HistoryView,
    tip: &str,
) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(frame.area());
    frame.render_widget(
        Paragraph::new(format!("history: {name} / {}", view.id))
            .block(Block::default().borders(Borders::ALL).title("served")),
        areas[0],
    );
    frame.render_widget(
        Paragraph::new(view.content.as_str())
            .block(Block::default().borders(Borders::ALL).title("output"))
            .scroll((view.scroll.min(u16::MAX as u64) as u16, 0))
            .wrap(Wrap { trim: false }),
        areas[1],
    );
    let (position, total_lines) = history_position(view.scroll, view.total_lines);
    frame.render_widget(
        Paragraph::new(format!("{position}/{total_lines}")),
        areas[2],
    );
    frame.render_widget(Paragraph::new(format!("tips: {tip}")), areas[3]);
    frame.render_widget(
        Paragraph::new("up/down/j/k scroll   PgUp/PgDn page   g/G top/end   Esc/q back")
            .wrap(Wrap { trim: false }),
        areas[4],
    );
}
