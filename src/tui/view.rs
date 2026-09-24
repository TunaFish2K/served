use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::model::{HistoryView, MainUi, Page, Reader, ServiceAction};
use crate::protocol::{HistoryRecord, ServiceInfo, ServiceKind, ServiceState};

fn muted() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}
fn selected_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

pub(super) fn usable(area: Rect) -> bool {
    area.width >= 40 && area.height >= 10
}
/// Key/description pairs keep styling and cell-width measurement in one place.
pub(super) type Footer = &'static [(&'static str, &'static str)];
pub(super) const SERVICES: Footer = &[("enter", "actions"), ("?", "help"), ("q", "quit")];
pub(super) const EMPTY: Footer = &[("?", "help"), ("q", "quit")];
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

fn footer_rows(area: Rect, footer: Footer) -> u16 {
    if usize::from(inner_width(area)) >= footer_width(footer) + 2 + 20 {
        1
    } else {
        2
    }
}

pub(super) fn body_rows(area: Rect, footer: Footer) -> usize {
    usize::from(area.height.saturating_sub(5 + footer_rows(area, footer))).max(1)
}

pub(super) fn main_footer(ui: &MainUi) -> Footer {
    if ui.help.is_some() {
        HELP
    } else if ui.message.is_some() {
        ERROR
    } else {
        match ui.page {
            Page::Services if ui.services.is_empty() || ui.unavailable.is_some() => EMPTY,
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
fn page(frame: &mut Frame<'_>, title: &str, count: &str, footer: Footer) -> Option<PageAreas> {
    let area = frame.area();
    if !usable(area) {
        frame.render_widget(Paragraph::new("Resize to 40x10\nq quit / Esc back"), area);
        return None;
    }
    let margin = if area.width < 80 { 1 } else { 2 };
    let inner = Rect::new(
        area.x + margin,
        area.y + 1,
        area.width - margin * 2,
        area.height - 2,
    );
    let regions = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(footer_rows(area, footer)),
        ])
        .split(inner);
    let count_width = count.width().min(usize::from(inner.width)) as u16;
    let title_width = inner
        .width
        .saturating_sub(count_width + u16::from(!count.is_empty()));
    frame.render_widget(
        Paragraph::new(clip(title, usize::from(title_width), false))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        Rect {
            width: title_width,
            ..regions[0]
        },
    );
    frame.render_widget(
        Paragraph::new(count).style(muted()),
        Rect {
            x: inner.right() - count_width,
            width: count_width,
            ..regions[0]
        },
    );
    let width = footer_width(footer) as u16;
    let footer_area = regions[4];
    let keys = Rect::new(inner.right() - width, footer_area.bottom() - 1, width, 1);
    let mut spans = Vec::new();
    for (index, (key, description)) in footer.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::raw(*key));
        spans.push(Span::styled(format!(" {description}"), muted()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), keys);
    Some(PageAreas {
        body: regions[2],
        detail: Rect::new(
            inner.x,
            footer_area.y,
            if footer_area.height == 1 {
                inner.width - width - 2
            } else {
                inner.width
            },
            1,
        ),
    })
}

/// Measure terminal cells and retain whole grapheme clusters, including combining marks.
fn clip(text: &str, width: usize, tail: bool) -> String {
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

fn wrapped(text: &str, width: usize) -> Vec<String> {
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

fn list(frame: &mut Frame<'_>, area: Rect, items: Vec<ListItem<'_>>, selected: usize) {
    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected.min(items.len() - 1)));
    }
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(selected_style())
            .highlight_symbol("> "),
        area,
        &mut state,
    );
}

fn text_page(
    frame: &mut Frame<'_>,
    title: &str,
    lines: Vec<Line<'static>>,
    scroll: &mut usize,
    footer: Footer,
    detail: &str,
) {
    let Some(areas) = page(frame, title, "", footer) else {
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

pub(super) fn draw_reader(frame: &mut Frame<'_>, reader: &mut Reader, footer: Footer) {
    let lines = wrapped(&reader.content, content_width(frame))
        .into_iter()
        .map(Line::from)
        .collect();
    text_page(frame, &reader.title, lines, &mut reader.scroll, footer, "");
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
            *scroll = selected.saturating_sub(body_rows(frame.area(), ACTIONS).saturating_sub(1));
        }
        ui.viewport = viewport;
    }
    let notice = if progress.is_empty() {
        ui.notice(std::time::Instant::now())
    } else {
        progress
    }
    .to_owned();
    let service = ui.services.get(ui.selected);
    match &mut ui.page {
        Page::Services => draw_services(
            frame,
            &ui.services,
            ui.selected,
            ui.unavailable.is_some(),
            &notice,
        ),
        Page::Actions { selected, scroll } => {
            let Some(service) = service else {
                return;
            };
            let mut lines: Vec<Line> = ServiceAction::ALL
                .iter()
                .enumerate()
                .map(|(i, action)| {
                    Line::from(format!(
                        "{} {:<10} {}",
                        if i == *selected { ">" } else { " " },
                        action.label(),
                        action.key()
                    ))
                    .style(if i == *selected {
                        selected_style()
                    } else {
                        Style::default()
                    })
                })
                .collect();
            lines.push(Line::default());
            let mut details = format!(
                "{}\n{}\n{}",
                service.name,
                service.directory,
                kind_name(&service.kind)
            );
            if ui.unavailable.is_some() {
                details.push_str("\nManager unavailable; stale data. Actions disabled.");
            }
            if !progress.is_empty() {
                details.push_str(&format!("\n{progress}"));
            }
            lines.extend(
                wrapped(&details, content_width(frame))
                    .into_iter()
                    .map(|s| Line::from(s).style(muted())),
            );
            text_page(
                frame,
                &format!("{} / actions", service.name),
                lines,
                scroll,
                ACTIONS,
                if ui.unavailable.is_some() {
                    "Manager unavailable; actions disabled"
                } else {
                    &notice
                },
            );
        }
        Page::ConfirmDisable { confirm, .. } => {
            let Some(service) = service else {
                return;
            };
            let Some(areas) = page(frame, &format!("{} / disable", service.name), "", CONFIRM)
            else {
                return;
            };
            let items = vec![ListItem::new("Cancel"), ListItem::new("Disable")];
            list(frame, areas.body, items, usize::from(*confirm));
            frame.render_widget(
                Paragraph::new(clip(
                    if ui.unavailable.is_some() {
                        "Manager unavailable; actions disabled"
                    } else {
                        "Stops service and removes registration."
                    },
                    usize::from(areas.detail.width),
                    false,
                ))
                .style(muted()),
                areas.detail,
            );
        }
    }
}

fn draw_services(
    frame: &mut Frame<'_>,
    services: &[ServiceInfo],
    selected: usize,
    stale: bool,
    notice: &str,
) {
    let count = if stale {
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
    let footer = if services.is_empty() || stale {
        EMPTY
    } else {
        SERVICES
    };
    let Some(areas) = page(frame, "served", &count, footer) else {
        return;
    };
    if services.is_empty() {
        let message = if stale {
            "Manager unavailable. Retrying…"
        } else {
            "No services. Use served enable,\nor served run -- <command>."
        };
        frame.render_widget(Paragraph::new(message).style(muted()), areas.body);
    } else {
        let name_width = usize::from(areas.body.width.saturating_sub(14));
        let items = services
            .iter()
            .map(|service| {
                let name = clip(&service.name, name_width, false);
                let spacing = " ".repeat(name_width.saturating_sub(name.width()) + 1);
                let style = match service.state {
                    ServiceState::Failed => Style::default().fg(Color::Red),
                    ServiceState::Starting | ServiceState::Restarting => {
                        Style::default().fg(Color::Yellow)
                    }
                    _ => Style::default(),
                };
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{name}{spacing}")),
                    Span::styled(state_name(&service.state), style),
                ]))
            })
            .collect();
        list(frame, areas.body, items, selected);
    }
    let detail = if stale {
        clip(
            "Manager unavailable; stale data. ? help",
            usize::from(areas.detail.width),
            false,
        )
    } else if !notice.is_empty() {
        clip(notice, usize::from(areas.detail.width), false)
    } else if let Some(service) = services.get(selected) {
        let kind = kind_name(&service.kind);
        let path = std::env::var("HOME")
            .ok()
            .and_then(|home| {
                if service.directory == home {
                    Some("~".to_owned())
                } else {
                    service
                        .directory
                        .strip_prefix(&format!("{home}/"))
                        .map(|rest| format!("~/{rest}"))
                }
            })
            .unwrap_or_else(|| service.directory.clone());
        format!(
            "{} · {kind}",
            clip(
                &path,
                usize::from(areas.detail.width).saturating_sub(kind.len() + 3),
                true
            )
        )
    } else {
        String::new()
    };
    frame.render_widget(Paragraph::new(detail).style(muted()), areas.detail);
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
    ) else {
        return;
    };
    if records.is_empty() {
        frame.render_widget(
            Paragraph::new("No history available.").style(muted()),
            areas.body,
        );
    }
    let items = records
        .iter()
        .map(|record| {
            let suffix = format!(
                "{} B {}",
                record.bytes,
                if record.persisted { "disk" } else { "memory" }
            );
            let width = usize::from(areas.body.width).saturating_sub(suffix.width() + 3);
            let id = clip(&record.id, width, false);
            ListItem::new(format!(
                "{id}{} {suffix}",
                " ".repeat(width.saturating_sub(id.width()))
            ))
        })
        .collect();
    list(frame, areas.body, items, selected);
    if let Some(record) = records.get(selected) {
        frame.render_widget(
            Paragraph::new(clip(&record.id, usize::from(areas.detail.width), false)).style(muted()),
            areas.detail,
        );
    }
}

pub(super) fn draw_history_content(frame: &mut Frame<'_>, name: &str, history: &HistoryView) {
    let Some(areas) = page(frame, &format!("{name} / {}", history.id), "", CONTENT) else {
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
                        let areas = page(frame, "Title", "", footer).unwrap();
                        assert_eq!(usize::from(areas.body.height), body_rows(area, footer));
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
    fn empty_and_tiny_screens_have_recovery_instructions() {
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut ui = MainUi::default();
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        assert!(rendered(&terminal).contains("served enable"));
        assert!(rendered(&terminal).contains("served run"));
        terminal.backend_mut().resize(30, 8);
        terminal
            .draw(|frame| draw_main(frame, &mut ui, ""))
            .unwrap();
        assert!(rendered(&terminal).contains("Resize to 40x10"));
        assert!(rendered(&terminal).contains("q quit"));
        assert_eq!(
            ui.key(crossterm::event::KeyCode::Char('q'), false, 1),
            super::super::model::Intent::Quit
        );
    }
}
