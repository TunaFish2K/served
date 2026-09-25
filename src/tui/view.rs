use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::model::{HistoryView, MainUi, Notice, Page, Reader, ServiceAction, Tone};
use crate::protocol::{HistoryRecord, ServiceInfo, ServiceKind, ServiceState};

pub(super) fn colors_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
}

fn tone_style(tone: Tone) -> Style {
    if !colors_enabled() {
        return Style::default();
    }
    Style::default().fg(match tone {
        Tone::Accent => Color::Cyan,
        Tone::Success => Color::Green,
        Tone::Warning => Color::Yellow,
        Tone::Error => Color::Red,
    })
}

fn muted() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}
fn selected_style() -> Style {
    Style::default()
        .fg(Color::Reset)
        .bg(Color::Reset)
        .add_modifier(Modifier::REVERSED)
}

pub(super) fn danger_style(selected: bool) -> Style {
    if selected && colors_enabled() {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Red)
            .remove_modifier(Modifier::REVERSED)
    } else if selected {
        selected_style()
    } else {
        tone_style(Tone::Error)
    }
}

pub(super) fn usable(area: Rect) -> bool {
    area.width >= 40 && area.height >= 10
}
/// Key/description pairs keep styling and cell-width measurement in one place.
pub(super) type Footer = &'static [(&'static str, &'static str)];
pub(super) const SERVICES: Footer = &[("enter", "actions"), ("?", "help"), ("esc/q", "quit")];
pub(super) const EMPTY: Footer = &[("?", "help"), ("esc/q", "quit")];
pub(super) const ACTIONS: Footer = &[("enter", "select"), ("?", "help"), ("esc/q", "back")];
pub(super) const CONFIRM: Footer = &[("enter", "select"), ("?", "help"), ("esc/q", "cancel")];
pub(super) const HISTORY: Footer = &[("enter", "open"), ("?", "help"), ("esc/q", "back")];
pub(super) const CONTENT: Footer = &[("↑↓", "scroll"), ("?", "help"), ("esc/q", "back")];
pub(super) const HELP: Footer = &[("↑↓", "scroll"), ("?/esc/q", "back")];
pub(super) const ERROR: Footer = &[("↑↓", "scroll"), ("esc/q", "back")];
pub(super) const CRASH: Footer = &[("enter/y", "open log"), ("n/esc", "cancel")];

fn inner_width(area: Rect) -> u16 {
    area.width
        .saturating_sub(if area.width < 80 { 2 } else { 4 })
}

fn footer_width(footer: Footer) -> usize {
    footer
        .iter()
        .map(|(key, description)| key.width() + 1 + description.width())
        .sum::<usize>()
        + footer.len().saturating_sub(1) * 3
}

fn footer_rows(width: u16, footer: Footer) -> u16 {
    if usize::from(width) >= footer_width(footer) + 2 + 20 {
        1
    } else {
        2
    }
}

fn work_width(area: Rect, items: Option<usize>) -> u16 {
    if items.is_some() {
        inner_width(area).min(76)
    } else {
        inner_width(area)
    }
}

pub(super) fn body_rows(area: Rect, footer: Footer, items: Option<usize>) -> usize {
    let available = usize::from(
        area.height
            .saturating_sub(5 + footer_rows(work_width(area, items), footer)),
    )
    .max(1);
    items.map_or(available, |count| count.max(6).min(available))
}

pub(super) fn main_rows(area: Rect, ui: &MainUi) -> usize {
    let items = if ui.help.is_some() || ui.message.is_some() {
        None
    } else {
        Some(match ui.page {
            Page::Services => ui.services.len(),
            _ => 6,
        })
    };
    body_rows(area, main_footer(ui), items)
}

pub(super) fn main_footer(ui: &MainUi) -> Footer {
    if ui.help.is_some() {
        HELP
    } else if ui.message.is_some() {
        ERROR
    } else {
        match ui.page {
            Page::Services if ui.services.is_empty() => EMPTY,
            Page::Services => SERVICES,
            Page::Actions { .. } => ACTIONS,
            Page::ConfirmDisable { .. } => CONFIRM,
        }
    }
}

struct PageAreas {
    body: Rect,
    detail: Rect,
}

/// All managed screens share this borderless frame. Attach owns the raw terminal.
fn page(
    frame: &mut Frame<'_>,
    title: &str,
    count: &str,
    footer: Footer,
    items: Option<usize>,
    tone: Tone,
) -> Option<PageAreas> {
    let area = frame.area();
    if !usable(area) {
        frame.render_widget(Paragraph::new("Resize to 40x10\nesc/q back/quit"), area);
        return None;
    }
    let margin = if area.width < 80 { 1 } else { 2 };
    let width = work_width(area, items);
    let body = Rect::new(
        area.x + margin,
        area.y + 3,
        width,
        body_rows(area, footer, items) as u16,
    );
    let footer_area = Rect::new(body.x, body.bottom() + 1, width, footer_rows(width, footer));
    let count = if count.is_empty() {
        String::new()
    } else {
        format!(" · {count}")
    };
    let title = clip(
        title,
        usize::from(width).saturating_sub(count.width()),
        false,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(title, tone_style(tone).add_modifier(Modifier::BOLD)),
            Span::styled(&count, muted()),
        ])),
        Rect::new(body.x, area.y + 1, width, 1),
    );
    if items.is_some()
        && (footer == SERVICES || footer == ACTIONS || (footer == EMPTY && !count.is_empty()))
    {
        frame.render_widget(
            Paragraph::new("─".repeat(usize::from(width))).style(muted()),
            Rect::new(body.x, body.bottom(), width, 1),
        );
    }
    let width = footer_width(footer) as u16;
    let keys = Rect::new(body.right() - width, footer_area.bottom() - 1, width, 1);
    frame.render_widget(Paragraph::new(footer_line(footer)), keys);
    Some(PageAreas {
        body,
        detail: Rect::new(
            body.x,
            footer_area.y,
            if footer_area.height == 1 {
                body.width - width - 2
            } else {
                body.width
            },
            1,
        ),
    })
}

/// Measure terminal cells and retain whole grapheme clusters, including combining marks.
pub(super) fn clip(text: &str, width: usize, tail: bool) -> String {
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    if text.width() <= width {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    let graphemes: Vec<_> = text.graphemes(true).collect();
    let mut kept = Vec::new();
    let mut used = 1;
    let iter: Box<dyn Iterator<Item = &&str>> = if tail {
        Box::new(graphemes.iter().rev())
    } else {
        Box::new(graphemes.iter())
    };
    for part in iter {
        if used + part.width() > width {
            break;
        }
        used += part.width();
        kept.push(*part);
    }
    if tail {
        kept.reverse();
        format!("…{}", kept.concat())
    } else {
        format!("{}…", kept.concat())
    }
}

pub(super) fn wrapped(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for source in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for part in source.graphemes(true) {
            let part = if part == "\t" { "    " } else { part };
            if part.chars().any(char::is_control) {
                continue;
            }
            let cells = part.width();
            if used + cells > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push_str(part);
            used += cells;
        }
        lines.push(line);
    }
    lines
}

fn list(frame: &mut Frame<'_>, area: Rect, items: Vec<ListItem<'_>>, selected: usize) -> ListState {
    list_with_highlight(frame, area, items, selected, selected_style())
}

fn list_with_highlight(
    frame: &mut Frame<'_>,
    area: Rect,
    items: Vec<ListItem<'_>>,
    selected: usize,
    highlight: Style,
) -> ListState {
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected.min(items.len() - 1)));
    }
    // Reserve the same two-cell inset for every row, outside the highlight.
    let inset = area.width.min(2);
    frame.render_stateful_widget(
        List::new(items).highlight_style(highlight),
        Rect {
            x: area.x + inset,
            width: area.width - inset,
            ..area
        },
        &mut state,
    );
    state
}

fn text_page(
    frame: &mut Frame<'_>,
    title: &str,
    lines: Vec<Line<'static>>,
    scroll: &mut usize,
    footer: Footer,
    detail: &str,
    tone: Tone,
) {
    let Some(areas) = page(frame, title, "", footer, None, tone) else {
        return;
    };
    *scroll = (*scroll).min(lines.len().saturating_sub(usize::from(areas.body.height)));
    let total = lines.len();
    let visible: Vec<_> = lines
        .into_iter()
        .skip(*scroll)
        .take(usize::from(areas.body.height))
        .collect();
    frame.render_widget(Paragraph::new(visible), areas.body);
    frame.render_widget(
        Paragraph::new(if detail.is_empty() {
            if total > usize::from(areas.body.height) {
                format!("{}/{}", *scroll + 1, total)
            } else {
                String::new()
            }
        } else {
            clip(detail, usize::from(areas.detail.width), false)
        })
        .style(muted()),
        areas.detail,
    );
}

fn content_width(frame: &Frame<'_>) -> usize {
    usize::from(inner_width(frame.area()))
}

fn help_lines(content: &str, width: usize) -> Vec<Line<'static>> {
    let key_width = content
        .lines()
        .filter_map(|line| line.split_once("  "))
        .map(|(key, _)| key.width())
        .max()
        .unwrap_or(0);
    let indent = (key_width + 4).min(width.saturating_sub(1));
    let mut lines = Vec::new();
    for line in content.lines() {
        if let Some((key, description)) = line.split_once("  ") {
            for (index, part) in wrapped(description, width.saturating_sub(indent).max(1))
                .into_iter()
                .enumerate()
            {
                let prefix = if index == 0 {
                    format!(
                        "  {key}{}",
                        " ".repeat(indent.saturating_sub(key.width() + 2))
                    )
                } else {
                    " ".repeat(indent)
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, tone_style(Tone::Accent)),
                    Span::raw(part),
                ]));
            }
        } else {
            lines.extend(wrapped(line, width).into_iter().map(Line::from));
        }
    }
    lines
}

fn footer_line(footer: Footer) -> Line<'static> {
    key_hints(footer, "   ")
}

pub(super) fn key_hints(footer: Footer, gap: &'static str) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, (key, description)) in footer.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(gap));
        }
        spans.push(Span::styled(*key, tone_style(Tone::Accent)));
        spans.push(Span::styled(format!(" {description}"), muted()));
    }
    Line::from(spans)
}

pub(super) fn draw_help(
    frame: &mut Frame<'_>,
    title: &str,
    content: &str,
    scroll: &mut usize,
    footer: Footer,
) {
    let area = frame.area();
    if !usable(area) {
        frame.render_widget(Paragraph::new("Resize to 40x10\nesc back"), area);
        return;
    }
    let margin = if area.width < 80 { 1 } else { 2 };
    let width = inner_width(area);
    let x = area.x + margin;
    let lines = help_lines(content, usize::from(width));
    let total = lines.len();
    let capacity = usize::from(area.height - 6);
    *scroll = (*scroll).min(total.saturating_sub(capacity));
    let visible: Vec<_> = lines.into_iter().skip(*scroll).take(capacity).collect();
    let height = visible.len() as u16;
    frame.render_widget(
        Paragraph::new(clip(title, usize::from(width), false))
            .style(tone_style(Tone::Accent).add_modifier(Modifier::BOLD)),
        Rect::new(x, area.y + 1, width, 1),
    );
    frame.render_widget(
        Paragraph::new(visible),
        Rect::new(x, area.y + 3, width, height),
    );
    // Help key rows start two cells inside the reading area.
    let footer_y = area.y + 4 + height;
    let key_width = footer_width(footer) as u16;
    frame.render_widget(
        Paragraph::new(footer_line(footer)),
        Rect::new(x + 2, footer_y, key_width, 1),
    );
    if total > capacity {
        let available = width.saturating_sub(2 + key_width + 2);
        let position = clip(
            &format!("{}/{}", *scroll + 1, total),
            usize::from(available),
            true,
        );
        let position_width = position.width() as u16;
        frame.render_widget(
            Paragraph::new(position).style(muted()),
            Rect::new(x + width - position_width, footer_y, position_width, 1),
        );
    }
}

pub(super) fn draw_reader(frame: &mut Frame<'_>, reader: &mut Reader, footer: Footer) {
    if footer == HELP {
        draw_help(
            frame,
            &reader.title,
            &reader.content,
            &mut reader.scroll,
            footer,
        );
        return;
    }
    let lines = wrapped(&reader.content, content_width(frame))
        .into_iter()
        .map(Line::from)
        .collect();
    text_page(
        frame,
        &reader.title,
        lines,
        &mut reader.scroll,
        footer,
        "",
        reader.tone,
    );
}

pub(super) fn draw_main(frame: &mut Frame<'_>, ui: &mut MainUi, progress: &str) {
    if let Some(help) = &mut ui.help {
        draw_reader(frame, help, HELP);
        return;
    }
    if let Some(message) = &mut ui.message {
        draw_reader(frame, message, ERROR);
        return;
    }
    let viewport = (frame.area().width, frame.area().height);
    if ui.viewport != viewport {
        if let Page::Actions { selected, scroll } = &mut ui.page {
            *scroll = selected
                .saturating_sub(body_rows(frame.area(), ACTIONS, Some(6)).saturating_sub(1));
        }
        ui.viewport = viewport;
    }
    let progress_notice = (!progress.is_empty()).then(|| Notice::new(progress, Tone::Warning));
    let notice = progress_notice
        .as_ref()
        .or_else(|| ui.notice(std::time::Instant::now()))
        .cloned();
    let service = ui.services.get(ui.selected);
    match &mut ui.page {
        Page::Services => draw_services(
            frame,
            &ui.services,
            ui.selected,
            ui.initial_loading,
            ui.unavailable.is_some(),
            notice.as_ref(),
        ),
        Page::Actions { selected, scroll } => {
            let Some(service) = service else {
                return;
            };
            let Some(areas) = page(
                frame,
                &format!("{} / actions", service.name),
                "",
                ACTIONS,
                Some(6),
                Tone::Accent,
            ) else {
                return;
            };
            frame.render_widget(
                Paragraph::new(clip(
                    &display_path(&service.directory),
                    usize::from(areas.body.width),
                    true,
                ))
                .style(muted()),
                Rect::new(areas.body.x, areas.body.y - 1, areas.body.width, 1),
            );
            let rows = usize::from(areas.body.height);
            *scroll = (*scroll)
                .min(*selected)
                .max(selected.saturating_sub(rows.saturating_sub(1)))
                .min(ServiceAction::ALL.len().saturating_sub(rows));
            let lines: Vec<Line> = ServiceAction::ALL
                .iter()
                .enumerate()
                .skip(*scroll)
                .take(rows)
                .map(|(i, action)| {
                    Line::from(vec![
                        Span::styled(
                            "  ",
                            Style::default()
                                .fg(Color::Reset)
                                .bg(Color::Reset)
                                .remove_modifier(Modifier::REVERSED),
                        ),
                        Span::styled(
                            format!("{:<10} ", action.label()),
                            if i != *selected && *action == ServiceAction::Disable {
                                tone_style(Tone::Error)
                            } else {
                                Style::default()
                            },
                        ),
                        Span::styled(
                            action.key().to_string(),
                            if i == *selected {
                                Style::default()
                            } else {
                                tone_style(Tone::Accent)
                            },
                        ),
                    ])
                    .style(if i == *selected {
                        if *action == ServiceAction::Disable {
                            danger_style(true)
                        } else {
                            selected_style()
                        }
                    } else {
                        Style::default()
                    })
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), areas.body);
            draw_service_detail(
                frame,
                areas.detail,
                Some(service),
                ui.unavailable.is_some(),
                notice.as_ref(),
            );
        }

        Page::ConfirmDisable { confirm, .. } => {
            let Some(service) = service else {
                return;
            };
            let Some(areas) = page(
                frame,
                &format!("{} / disable", service.name),
                "",
                CONFIRM,
                Some(6),
                Tone::Accent,
            ) else {
                return;
            };
            frame.render_widget(
                Paragraph::new("Stops and unregisters service.").style(tone_style(Tone::Error)),
                Rect {
                    height: 1,
                    ..areas.body
                },
            );
            let items = vec![
                ListItem::new("Cancel"),
                ListItem::new("Disable").style(danger_style(false)),
            ];
            list_with_highlight(
                frame,
                Rect {
                    y: areas.body.y + 1,
                    height: areas.body.height - 1,
                    width: 9,
                    ..areas.body
                },
                items,
                usize::from(*confirm),
                if *confirm {
                    danger_style(true)
                } else {
                    selected_style()
                },
            );
            draw_service_detail(
                frame,
                areas.detail,
                Some(service),
                ui.unavailable.is_some(),
                notice.as_ref(),
            );
        }
    }
}

fn draw_services(
    frame: &mut Frame<'_>,
    services: &[ServiceInfo],
    selected: usize,
    initial_loading: bool,
    stale: bool,
    notice: Option<&Notice>,
) {
    let count = if initial_loading {
        String::new()
    } else if stale {
        "stale".to_owned()
    } else {
        format!(
            "{} {}",
            services.len(),
            if services.len() == 1 {
                "service"
            } else {
                "services"
            }
        )
    };
    let footer = if services.is_empty() { EMPTY } else { SERVICES };
    let Some(areas) = page(
        frame,
        "served",
        &count,
        footer,
        Some(services.len()),
        Tone::Accent,
    ) else {
        return;
    };
    if initial_loading {
        return;
    }
    if services.is_empty() {
        let message = if stale {
            "Manager unavailable. Retrying…"
        } else {
            "No services. Use served enable,\nor served run -- <command>."
        };
        frame.render_widget(
            Paragraph::new(message).style(if stale {
                tone_style(Tone::Warning)
            } else {
                muted()
            }),
            areas.body,
        );
    } else {
        let name_width = services
            .iter()
            .map(|service| service.name.width())
            .max()
            .unwrap_or(0)
            .clamp(10, 24)
            .min(usize::from(areas.body.width.saturating_sub(13)));
        let items = services
            .iter()
            .map(|service| {
                let name = clip(&service.name, name_width, false);
                let spacing = " ".repeat(name_width.saturating_sub(name.width()) + 1);
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{name}{spacing}")),
                    Span::styled(state_name(&service.state), state_style(&service.state)),
                ]))
            })
            .collect();
        // Keep names and states together, like the compact action menu.
        let state = list(
            frame,
            Rect {
                width: (name_width + 13) as u16,
                ..areas.body
            },
            items,
            selected,
        );
        // List highlights its full width; leave padding after the status plain.
        if let Some(service) = services.get(selected) {
            let row = selected.saturating_sub(state.offset());
            if row < usize::from(areas.body.height) {
                let y = areas.body.y + row as u16;
                let end = areas.body.x + (name_width + 3) as u16;
                for x in end + state_name(&service.state).width() as u16..end + 10 {
                    frame.buffer_mut()[(x, y)]
                        .modifier
                        .remove(Modifier::REVERSED);
                }
            }
        }
    }
    draw_service_detail(frame, areas.detail, services.get(selected), stale, notice);
}

fn display_path(directory: &str) -> String {
    std::env::var("HOME")
        .ok()
        .and_then(|home| {
            if directory == home {
                Some("~".to_owned())
            } else {
                directory
                    .strip_prefix(&format!("{home}/"))
                    .map(|rest| format!("~/{rest}"))
            }
        })
        .unwrap_or_else(|| directory.to_owned())
}

fn draw_service_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    service: Option<&ServiceInfo>,
    stale: bool,
    notice: Option<&Notice>,
) {
    let detail = if stale {
        clip(
            "Manager unavailable; stale data. ? help",
            usize::from(area.width),
            false,
        )
    } else if let Some(notice) = notice {
        clip(&notice.text, usize::from(area.width), false)
    } else if let Some(service) = service {
        let kind = kind_name(&service.kind);
        let path = display_path(&service.directory);
        format!(
            "{} · {kind}",
            clip(
                &path,
                usize::from(area.width).saturating_sub(kind.len() + 3),
                true
            )
        )
    } else {
        String::new()
    };
    let style = if stale {
        tone_style(Tone::Warning)
    } else if let Some(notice) = notice {
        tone_style(notice.tone)
    } else {
        muted()
    };
    frame.render_widget(Paragraph::new(detail).style(style), area);
}

fn state_style(state: &ServiceState) -> Style {
    match state {
        ServiceState::Running => tone_style(Tone::Success),
        ServiceState::Starting | ServiceState::Restarting => tone_style(Tone::Warning),
        ServiceState::Failed => tone_style(Tone::Error),
        ServiceState::Stopped => Style::default(),
    }
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
fn kind_name(kind: &ServiceKind) -> &'static str {
    match kind {
        ServiceKind::Enabled => "enabled",
        ServiceKind::Temporary => "temporary",
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
) {
    let Some(areas) = page(
        frame,
        &format!("{name} / history"),
        &format!("{} runs", records.len()),
        HISTORY,
        Some(records.len()),
        Tone::Accent,
    ) else {
        return;
    };
    if records.is_empty() {
        frame.render_widget(
            Paragraph::new("No history available.").style(muted()),
            areas.body,
        );
    }
    let suffix_width = records
        .iter()
        .map(|record| {
            format!(
                "{} B {}",
                record.bytes,
                if record.persisted { "disk" } else { "memory" }
            )
            .width()
        })
        .max()
        .unwrap_or(0);
    let id_width = records
        .iter()
        .map(|record| record.id.width())
        .max()
        .unwrap_or(0)
        .clamp(10, 24)
        .min(usize::from(areas.body.width).saturating_sub(suffix_width + 3));
    let items = records
        .iter()
        .map(|record| {
            let suffix = format!(
                "{} B {}",
                record.bytes,
                if record.persisted { "disk" } else { "memory" }
            );
            let width = id_width;
            let id = clip(&record.id, width, false);
            ListItem::new(format!(
                "{id}{} {suffix}",
                " ".repeat(width.saturating_sub(id.width()))
            ))
        })
        .collect();
    list(
        frame,
        Rect {
            width: (id_width + suffix_width + 3).min(usize::from(areas.body.width)) as u16,
            ..areas.body
        },
        items,
        selected,
    );
    if let Some(record) = records.get(selected) {
        frame.render_widget(
            Paragraph::new(clip(&record.id, usize::from(areas.detail.width), false)).style(muted()),
            areas.detail,
        );
    }
}

pub(super) fn draw_history_content(frame: &mut Frame<'_>, name: &str, history: &HistoryView) {
    let Some(areas) = page(
        frame,
        &format!("{name} / {}", history.id),
        "",
        CONTENT,
        None,
        Tone::Accent,
    ) else {
        return;
    };
    // Preserve logical-line scrolling and avoid Paragraph's u16 scroll truncation.
    let lines: Vec<Line> = history
        .content
        .lines()
        .skip(usize::try_from(history.scroll).unwrap_or(usize::MAX))
        .flat_map(|line| wrapped(line, usize::from(areas.body.width)))
        .take(usize::from(areas.body.height))
        .map(Line::from)
        .collect();
    frame.render_widget(Paragraph::new(lines), areas.body);
    let (position, total) = history_position(history.scroll, history.total_lines);
    let fits = history.scroll == 0
        && total <= u64::from(areas.body.height)
        && history
            .content
            .lines()
            .flat_map(|line| wrapped(line, usize::from(areas.body.width)))
            .take(usize::from(areas.body.height) + 1)
            .count()
            <= usize::from(areas.body.height);
    frame.render_widget(
        Paragraph::new(if fits {
            String::new()
        } else {
            format!("{position}/{total}")
        })
        .style(muted()),
        areas.detail,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn rendered(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn footers_align_and_match_paging_height_at_every_transition() {
        for footer in [
            SERVICES, EMPTY, ACTIONS, CONFIRM, HISTORY, CONTENT, HELP, ERROR, CRASH,
        ] {
            let threshold = footer_width(footer) as u16 + 24;
            for width in [40, (threshold - 1).max(40), threshold.max(40), 80, 120] {
                let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
                terminal
                    .draw(|frame| {
                        let area = frame.area();
                        let areas = page(frame, "Title", "", footer, None, Tone::Accent).unwrap();
                        assert_eq!(
                            usize::from(areas.body.height),
                            body_rows(area, footer, None)
                        );
                        assert_eq!(areas.detail.y, if width < threshold { 7 } else { 8 });
                        assert!(areas.detail.width >= 20);
                        frame.render_widget(Paragraph::new("detail").style(muted()), areas.detail);
                    })
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let margin = if width < 80 { 1 } else { 2 };
                let start = width - margin - footer_width(footer) as u16;
                let expected = footer
                    .iter()
                    .map(|(k, d)| format!("{k} {d}"))
                    .collect::<Vec<_>>()
                    .join("   ");
                let actual: String = (start..width - margin)
                    .map(|x| buffer[(x, 8)].symbol())
                    .collect();
                assert_eq!(actual, expected);
                assert!(!buffer[(start, 8)].modifier.contains(Modifier::DIM));
                let description = start + footer[0].0.width() as u16 + 1;
                assert!(buffer[(description, 8)].modifier.contains(Modifier::DIM));
            }
        }
    }

    #[test]
    fn help_footer_follows_content_and_aligns_with_keys() {
        for (width, height) in [(40, 10), (80, 24), (120, 40)] {
            for count in [3, 100] {
                for footer in [HELP, &[("esc", "back")][..]] {
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    let content = (0..count)
                        .map(|i| format!("key  Row {i}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let mut scroll = usize::MAX;
                    terminal
                        .draw(|frame| draw_help(frame, "Help", &content, &mut scroll, footer))
                        .unwrap();
                    let visible = count.min(usize::from(height - 6));
                    let y = 4 + visible as u16;
                    let x = if width < 80 { 3 } else { 4 };
                    let expected = footer
                        .iter()
                        .map(|(k, d)| format!("{k} {d}"))
                        .collect::<Vec<_>>()
                        .join("   ");
                    let buf = terminal.backend().buffer();
                    let actual: String = (x..x + expected.width() as u16)
                        .map(|col| buf[(col, y)].symbol())
                        .collect();
                    assert_eq!(actual, expected);
                    assert_eq!(buf[(x, 3)].symbol(), "k");
                    assert!((0..width).all(|col| buf[(col, y - 1)].symbol() == " "));
                    assert!(
                        (y + 1..height)
                            .all(|row| (0..width).all(|col| buf[(col, row)].symbol() == " "))
                    );
                    assert_eq!(scroll, count - visible);
                    if count > visible {
                        let position = format!("{}/{count}", scroll + 1);
                        let margin = if width < 80 { 1 } else { 2 };
                        let start = width - margin - position.width() as u16;
                        assert!(start >= x + expected.width() as u16 + 2);
                        let actual: String = (start..width - margin)
                            .map(|col| buf[(col, y)].symbol())
                            .collect();
                        assert_eq!(actual, position);
                    }
                }
            }
        }
    }

    #[test]
    fn feedback_keeps_footer_and_body_in_place() {
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let mut reader = Reader::new("Reader", "row\n".repeat(40));
        for detail in [
            "",
            "starting api...",
            "Manager unavailable; actions disabled",
        ] {
            terminal
                .draw(|frame| {
                    text_page(
                        frame,
                        "Reader",
                        vec![Line::from("row"); 40],
                        &mut reader.scroll,
                        ACTIONS,
                        detail,
                        Tone::Accent,
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(2, 6)].symbol(), "r");
            assert_eq!(buffer[(2, 7)].symbol(), " ");
            assert_eq!(buffer[(78 - footer_width(ACTIONS) as u16, 8)].symbol(), "e");
        }
    }

    #[test]
    fn truncation_preserves_graphemes_and_path_suffixes() {
        assert_eq!(clip("服务名称", 5, false), "服务…");
        assert_eq!(clip("/very/long/项目", 7, true), "…g/项目");
        assert_eq!(clip("e\u{301}long", 2, false), "e\u{301}…");
        assert_eq!(clip("anything", 0, false), "");
        assert_eq!(wrapped("中文abcd", 4), vec!["中文", "abcd"]);
    }

    #[test]
    fn long_messages_are_fully_reachable_and_resize_clamps_scroll() {
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut reader = Reader::new("Error", format!("{}\nEND", "诊断消息".repeat(200)));
        reader.key(crossterm::event::KeyCode::End, 2);
        terminal
            .draw(|frame| draw_reader(frame, &mut reader, &[("esc/q", "back")]))
            .unwrap();
        assert!(rendered(&terminal).contains("END"));
        let old = reader.scroll;
        terminal.backend_mut().resize(120, 40);
        terminal
            .draw(|frame| draw_reader(frame, &mut reader, &[("esc/q", "back")]))
            .unwrap();
        assert!(reader.scroll < old);
        assert!(rendered(&terminal).contains("END"));
    }

    #[test]
    fn help_keys_are_colored_and_wrapped_without_losing_text() {
        for width in [38, 76, 116] {
            let lines = help_lines(
                "ctrl+c  Quit; wait for pending action\n\nconnection refused",
                width,
            );
            let text: String = lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .map(|span| span.content.as_ref())
                .collect();
            assert!(text.contains("ctrl+c"));
            assert!(text.contains("connection refused"));
            assert!(lines.iter().all(|line| line.width() <= width));
            assert_eq!(
                lines[0].spans[0].style.fg,
                if colors_enabled() {
                    Some(Color::Cyan)
                } else {
                    None
                }
            );
            assert_eq!(lines[0].spans[1].style.fg, None);
        }
    }

    #[test]
    fn empty_and_tiny_screens_have_recovery_instructions() {
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut ui = MainUi::default();
        for width in [40, 80, 120] {
            terminal.backend_mut().resize(width, 10);
            terminal
                .draw(|frame| draw_main(frame, &mut ui, ""))
                .unwrap();
            assert!(rendered(&terminal).contains("served enable"));
            assert!(rendered(&terminal).contains("served run"));
            assert!(rendered(&terminal).contains("? help   esc/q quit"));
        }
        terminal.backend_mut().resize(30, 8);
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        assert!(rendered(&terminal).contains("Resize to 40x10"));
        assert!(rendered(&terminal).contains("esc/q back/quit"));
        for key in [
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyCode::Char('q'),
        ] {
            assert_eq!(ui.key(key, false, 1), super::super::model::Intent::Quit);
        }
    }
}
