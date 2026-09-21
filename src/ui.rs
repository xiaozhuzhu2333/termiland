use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, Focus, LeftPage, Selection};
use crate::config::{CommandsConfig, JumpConfig, UiConfig};
use crate::island::IslandState;

fn focus_border(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

const TAB_LABELS: [&str; 2] = ["跳转", "命令"];

fn display_width(s: &str) -> u16 {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

pub fn paged_tab_hit(panel: Rect, column: u16, row: u16) -> Option<LeftPage> {
    if row != panel.y || column <= panel.x || column >= panel.x + panel.width {
        return None;
    }
    let mut x = panel.x + 1;
    for (index, label) in TAB_LABELS.iter().enumerate() {
        let width = display_width(label) + 2;
        if column >= x && column < x + width {
            return Some(if index == 0 {
                LeftPage::Jump
            } else {
                LeftPage::Commands
            });
        }
        x += width + 1;
    }
    None
}

pub fn right_pane_rect(area: Rect, config: &UiConfig) -> Rect {
    let [_, _, right] = Layout::horizontal([
        Constraint::Length(config.left_width),
        Constraint::Min(10),
        Constraint::Length(config.right_width),
    ])
    .areas(area);
    right
}

pub fn paged_panel_rect(area: Rect, config: &UiConfig) -> Rect {
    let [left, _, _] = Layout::horizontal([
        Constraint::Length(config.left_width),
        Constraint::Min(10),
        Constraint::Length(config.right_width),
    ])
    .areas(area);
    let [_, panel] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(left);
    panel
}

pub fn island_layout(area: Rect, heights: &[Option<u16>]) -> Vec<Rect> {
    if heights.is_empty() {
        return Vec::new();
    }
    let constraints: Vec<Constraint> = heights
        .iter()
        .map(|h| match h {
            Some(h) => Constraint::Length((*h).max(3)),
            None => Constraint::Fill(1),
        })
        .collect();
    Layout::vertical(constraints).split(area).to_vec()
}

pub fn island_inner(rect: Rect) -> Rect {
    Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    }
}

pub fn island_output_inner(rect: Rect) -> Rect {
    let inner = island_inner(rect);
    Rect {
        height: inner.height.saturating_sub(1),
        ..inner
    }
}

pub fn terminal_pane_inner(area: Rect, config: &UiConfig) -> Rect {
    let [_, center, _] = Layout::horizontal([
        Constraint::Length(config.left_width),
        Constraint::Min(10),
        Constraint::Length(config.right_width),
    ])
    .areas(area);
    Block::bordered().inner(center)
}

pub fn draw(f: &mut Frame, app: &App) {
    let [left, center, right] = Layout::horizontal([
        Constraint::Length(app.config.ui.left_width),
        Constraint::Min(10),
        Constraint::Length(app.config.ui.right_width),
    ])
    .areas(f.area());

    terminal_pane(f, center, app);
    left_column(f, left, app);
    islands_column(f, right, app);
}

fn placeholder(f: &mut Frame, area: Rect, title: &str, text: &str, border: Style) {
    let block = Block::bordered().border_style(border).title(title);
    f.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .block(block),
        area,
    );
}

fn terminal_pane(f: &mut Frame, area: Rect, app: &App) {
    let status = if let Some(text) = app.copy_notice_text() {
        Line::from(text)
    } else if let Some(count) = app.selection_chars() {
        Line::from(format!("选中 {count} 字符"))
    } else if app.scroll() > 0 {
        Line::from(vec![
            Span::styled(
                format!("↑ {} 行", app.scroll()),
                Style::new().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(
                "↓ 底部",
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
        ])
    } else if !app.pty.is_alive() && app.pty.is_finished() {
        Line::from("已退出")
    } else if !app.pty.is_alive() {
        Line::from("已退出 · 排空中")
    } else {
        Line::from("")
    };
    let block = Block::bordered()
        .border_style(focus_border(app.focus() == Focus::Terminal))
        .title("终端")
        .title_top(status.right_aligned())
        .title_bottom(
            Line::from(Span::styled(
                "Ctrl+Q 退出 · F2 岛栏 · F1 切页",
                Style::new().fg(Color::DarkGray),
            ))
            .right_aligned(),
        );
    let inner = block.inner(area);
    f.render_widget(block, area);

    render_screen(f, inner, app.term.screen(), app.selection());

    if app.pty.is_alive() && app.focus() == Focus::Terminal && app.scroll() == 0 {
        let (cursor_row, cursor_col) = app.term.screen().cursor_position();
        if cursor_row < inner.height && cursor_col < inner.width {
            f.set_cursor_position((inner.x + cursor_col, inner.y + cursor_row));
        }
    }
}

fn render_screen(f: &mut Frame, inner: Rect, screen: &vt100::Screen, selection: Option<Selection>) {
    let (rows, cols) = screen.size();
    for row in 0..rows.min(inner.height) {
        for col in 0..cols.min(inner.width) {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            if cell.is_wide() && col + 1 >= inner.width {
                continue;
            }
            let symbol = if cell.contents().is_empty() {
                " "
            } else {
                cell.contents()
            };
            let style = if selection.is_some_and(|s| s.contains(row, col)) {
                cell_style(cell).add_modifier(Modifier::REVERSED)
            } else {
                cell_style(cell)
            };
            f.buffer_mut()[(inner.x + col, inner.y + row)]
                .set_symbol(symbol)
                .set_style(style);
        }
    }
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::new()
        .fg(map_color(cell.fgcolor()))
        .bg(map_color(cell.bgcolor()));
    let mut modifier = Modifier::empty();
    if cell.bold() {
        modifier |= Modifier::BOLD;
    }
    if cell.italic() {
        modifier |= Modifier::ITALIC;
    }
    if cell.underline() {
        modifier |= Modifier::UNDERLINED;
    }
    if cell.inverse() {
        modifier |= Modifier::REVERSED;
    }
    if !modifier.is_empty() {
        style = style.add_modifier(modifier);
    }
    style
}

fn map_color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn left_column(f: &mut Frame, area: Rect, app: &App) {
    let [dir, panel] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area);
    let border = focus_border(false);
    placeholder(f, dir, "目录", "M3 · 文件浏览", border);
    paged_panel(f, panel, app, border);
}

fn paged_panel(f: &mut Frame, area: Rect, app: &App, border: Style) {
    let text = match app.left_page {
        LeftPage::Jump => jump_page(&app.config.jump),
        LeftPage::Commands => commands_page(&app.config.commands),
    };
    let block = Block::bordered()
        .border_style(border)
        .title(page_tabs(app.left_page));
    f.render_widget(Paragraph::new(text).block(block), area);
}

fn jump_page(jump: &JumpConfig) -> Text<'static> {
    if jump.bookmarks.is_empty() {
        return Text::from("未配置收藏目录（[jump].bookmarks）");
    }
    Text::from(
        jump.bookmarks
            .iter()
            .map(|b| Line::from(format!("→ {b}")))
            .collect::<Vec<_>>(),
    )
}

fn commands_page(commands: &CommandsConfig) -> Text<'static> {
    if commands.items.is_empty() {
        return Text::from("未配置常用命令（[[commands.items]]）");
    }
    Text::from(
        commands
            .items
            .iter()
            .map(|c| {
                let name = c.name.as_deref().unwrap_or(c.command.as_str());
                Line::from(format!("{name} · {}", c.command))
            })
            .collect::<Vec<_>>(),
    )
}

fn page_tabs(page: LeftPage) -> Line<'static> {
    let active = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let inactive = Style::new().fg(Color::DarkGray);
    let tab = |label: &str, on: bool| {
        let style = if on { active } else { inactive };
        Span::styled(format!("[{label}]"), style)
    };
    Line::from(vec![
        tab(TAB_LABELS[0], page == LeftPage::Jump),
        Span::raw(" "),
        tab(TAB_LABELS[1], page == LeftPage::Commands),
    ])
}

fn islands_column(f: &mut Frame, area: Rect, app: &App) {
    if app.islands.is_empty() {
        placeholder(f, area, "岛", "无岛", focus_border(false));
        return;
    }
    let heights: Vec<Option<u16>> = app.islands.iter().map(|i| i.height).collect();
    let areas = island_layout(area, &heights);
    for (index, (island, &island_area)) in app.islands.iter().zip(areas.iter()).enumerate() {
        render_island(f, island_area, island, app.focus() == Focus::Island(index));
    }
}

fn render_island(f: &mut Frame, area: Rect, island: &IslandState, focused: bool) {
    let title = if island.command.is_empty() {
        "岛".to_owned()
    } else {
        island.command.clone()
    };
    let (badge, badge_color) = if island.follow {
        ("◉ 跟随", Color::Green)
    } else {
        ("○ 单次", Color::DarkGray)
    };
    let mut badge_spans = Vec::new();
    if island.scroll > 0 {
        badge_spans.push(Span::styled(
            format!("↑ {}  ", island.scroll),
            Style::new().fg(Color::DarkGray),
        ));
    }
    badge_spans.push(Span::styled(badge, Style::new().fg(badge_color)));
    let mut block = Block::bordered()
        .border_style(focus_border(focused))
        .title(title)
        .title_top(Line::from(badge_spans).right_aligned());
    if focused {
        block = block.title_bottom(
            Line::from(Span::styled(
                "↑↓ 切换 · 回车执行 · Esc 返回",
                Style::new().fg(Color::DarkGray),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    if island.command.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "聚焦后输入命令 · 回车执行",
                Style::new().fg(Color::DarkGray),
            ))
            .alignment(Alignment::Center),
            inner,
        );
        return;
    }

    let [output, input] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    render_screen(f, output, island.parser.screen(), island.selection);
    let prompt = Line::from(vec![
        Span::styled(
            "> ",
            Style::new().fg(if focused {
                Color::Cyan
            } else {
                Color::DarkGray
            }),
        ),
        Span::raw(island.command.clone()),
    ]);
    f.render_widget(Paragraph::new(prompt), input);
    if focused {
        let cursor_x = input.x + 2 + display_width(&island.command);
        if cursor_x < input.x + input.width {
            f.set_cursor_position((cursor_x, input.y));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paged_tab_hit_matches_label_regions() {
        let panel = Rect::new(0, 5, 22, 10);
        assert_eq!(paged_tab_hit(panel, 2, 5), Some(LeftPage::Jump));
        assert_eq!(paged_tab_hit(panel, 6, 5), Some(LeftPage::Jump));
        assert_eq!(paged_tab_hit(panel, 9, 5), Some(LeftPage::Commands));
        assert_eq!(paged_tab_hit(panel, 13, 5), Some(LeftPage::Commands));
        assert_eq!(paged_tab_hit(panel, 18, 5), None);
        assert_eq!(paged_tab_hit(panel, 2, 6), None);
    }

    #[test]
    fn maps_vt100_colors_to_ratatui() {
        assert_eq!(map_color(vt100::Color::Default), Color::Reset);
        assert_eq!(map_color(vt100::Color::Idx(4)), Color::Indexed(4));
        assert_eq!(
            map_color(vt100::Color::Rgb(10, 20, 30)),
            Color::Rgb(10, 20, 30)
        );
    }

    #[test]
    fn computes_terminal_pane_inner() {
        let cfg = UiConfig {
            left_width: 22,
            right_width: 36,
        };
        let inner = terminal_pane_inner(Rect::new(0, 0, 120, 30), &cfg);
        assert_eq!((inner.height, inner.width), (28, 60));
        let inner = terminal_pane_inner(Rect::new(0, 0, 80, 24), &cfg);
        assert_eq!((inner.height, inner.width), (22, 20));
    }

    #[test]
    fn full_draw_keeps_cells_complete() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        use crate::app::App;
        use crate::config::{CommandsConfig, Config, IslandsConfig, JumpConfig, UiConfig};

        let config = Config {
            ui: UiConfig {
                left_width: 22,
                right_width: 36,
            },
            islands: IslandsConfig { max: 3 },
            jump: JumpConfig::default(),
            commands: CommandsConfig::default(),
        };

        let mut app = App::new(config).expect("app");
        let mut total = 0u64;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let out = app.pty.poll_output();
            total += out.len() as u64;
            app.term.process(&out);
            if total > 100 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();

        let buf = terminal.backend().buffer();
        let mut empties = Vec::new();
        for y in 0..30u16 {
            for x in 0..120u16 {
                if buf[(x, y)].symbol().is_empty() {
                    empties.push((x, y));
                }
            }
        }
        assert!(empties.is_empty(), "存在空 symbol 格子: {empties:?}");

        for y in [1u16, 5, 10, 13, 20, 28] {
            let border = buf[(84, y)].symbol();
            assert_eq!(border, "│", "y={y} 处岛栏左边框被破坏: {border:?}");
        }
    }
}
