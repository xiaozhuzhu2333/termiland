use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, LeftPage};
use crate::config::{CommandsConfig, Island, JumpConfig};

pub fn draw(f: &mut Frame, app: &App) {
    let [left, center, right] = Layout::horizontal([
        Constraint::Length(app.config.ui.left_width),
        Constraint::Min(10),
        Constraint::Length(app.config.ui.right_width),
    ])
    .areas(f.area());

    placeholder(f, center, "终端", "M1 · PTY Shell");
    left_column(f, left, app);
    islands_column(f, right, app);
}

fn placeholder(f: &mut Frame, area: Rect, title: &str, text: &str) {
    let block = Block::bordered().title(title);
    f.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .block(block),
        area,
    );
}

fn left_column(f: &mut Frame, area: Rect, app: &App) {
    let [dir, panel] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area);
    placeholder(f, dir, "目录", "M3 · 文件浏览");
    paged_panel(f, panel, app);
}

fn paged_panel(f: &mut Frame, area: Rect, app: &App) {
    let text = match app.left_page {
        LeftPage::Jump => jump_page(&app.config.jump),
        LeftPage::Commands => commands_page(&app.config.commands),
    };
    let block = Block::bordered().title(page_tabs(app.left_page));
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
    let islands = &app.config.islands;
    if islands.items.is_empty() {
        placeholder(f, area, "岛", "未配置（参考 config.example.toml）");
        return;
    }
    let constraints: Vec<Constraint> = islands
        .items
        .iter()
        .map(|i| match i.height {
            Some(h) => Constraint::Length(h.max(3)),
            None => Constraint::Fill(1),
        })
        .collect();
    let areas = Layout::vertical(constraints).split(area);
    for (island, &island_area) in islands.items.iter().zip(areas.iter()) {
        render_island(f, island_area, island);
    }
}

fn render_island(f: &mut Frame, area: Rect, island: &Island) {
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
        .title(title)
        .title_top(Line::from(Span::styled(toggle, Style::new().fg(color))).right_aligned());
    let body = Paragraph::new(Span::styled(
        format!("M4 · {}", island.command),
        Style::new().fg(Color::DarkGray),
    ));
    f.render_widget(body.block(block), area);
}
