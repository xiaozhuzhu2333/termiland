use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, Focus, LeftPage};
use crate::config::{CommandsConfig, Island, JumpConfig, UiConfig};

fn focus_border(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
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

    let screen = app.term.screen();
    let (rows, cols) = screen.size();
    let selection = app.selection();
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

    if app.pty.is_alive() && app.scroll() == 0 {
        let (cursor_row, cursor_col) = screen.cursor_position();
        if cursor_row < inner.height && cursor_col < inner.width {
            f.set_cursor_position((inner.x + cursor_col, inner.y + cursor_row));
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
        tab("跳转", page == LeftPage::Jump),
        Span::raw(" "),
        tab("命令", page == LeftPage::Commands),
    ])
}

fn islands_column(f: &mut Frame, area: Rect, app: &App) {
    if app.islands.is_empty() {
        placeholder(
            f,
            area,
            "岛",
            "未配置（参考 config.example.toml）",
            focus_border(false),
        );
        return;
    }
    let constraints: Vec<Constraint> = app
        .islands
        .iter()
        .map(|i| match i.height {
            Some(h) => Constraint::Length(h.max(3)),
            None => Constraint::Fill(1),
        })
        .collect();
    let areas = Layout::vertical(constraints).split(area);
    for (index, (island, &island_area)) in app.islands.iter().zip(areas.iter()).enumerate() {
        render_island(f, island_area, island, app.focus() == Focus::Island(index));
    }
}

fn render_island(f: &mut Frame, area: Rect, island: &Island, focused: bool) {
    let title = island
        .name
        .clone()
        .unwrap_or_else(|| island.command.clone());
    let (toggle, color) = if island.live {
        ("◉ 实时", Color::Green)
    } else {
        ("○ 触发", Color::DarkGray)
    };
    let block = Block::bordered()
        .border_style(focus_border(focused))
        .title(title)
        .title_top(Line::from(Span::styled(toggle, Style::new().fg(color))).right_aligned());
    let body = Paragraph::new(Span::styled(
        format!("M4 · {}", island.command),
        Style::new().fg(Color::DarkGray),
    ));
    f.render_widget(body.block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        use crate::config::{CommandsConfig, Config, Island, IslandsConfig, JumpConfig, UiConfig};

        let config = Config {
            ui: UiConfig {
                left_width: 22,
                right_width: 36,
            },
            islands: IslandsConfig {
                max: 3,
                items: vec![
                    Island {
                        name: Some("history".to_owned()),
                        command: "tail -n 30 $HISTFILE".to_owned(),
                        height: Some(12),
                        live: false,
                    },
                    Island {
                        name: Some("top".to_owned()),
                        command: "top".to_owned(),
                        height: Some(20),
                        live: true,
                    },
                ],
            },
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
