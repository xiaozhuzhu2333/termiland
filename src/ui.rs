use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;

pub fn draw(f: &mut Frame, app: &App) {
    let [left, center, right] = Layout::horizontal([
        Constraint::Length(app.config.ui.left_width),
        Constraint::Min(10),
        Constraint::Length(app.config.ui.right_width),
    ])
    .areas(f.area());

    placeholder(f, left, "目录", "M3 · 文件浏览");
    placeholder(f, center, "终端", "M1 · PTY Shell");
    islands(f, right, app);
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

fn islands(f: &mut Frame, area: Rect, app: &App) {
    let islands = &app.config.islands;
    let mut lines = vec![Line::from(format!(
        "岛 {}/{}",
        islands.items.len(),
        islands.max
    ))];
    if islands.items.is_empty() {
        lines.push(Line::from("未配置（参考 config.example.toml）"));
    } else {
        for island in &islands.items {
            let title = island.name.as_deref().unwrap_or(island.command.as_str());
            let mode = if island.live { "实时" } else { "触发" };
            let height = island
                .height
                .map(|h| format!("{h} 行"))
                .unwrap_or_else(|| "平分".to_string());
            lines.push(Line::from(format!("{mode} · {title} · {height}")));
        }
    }
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(Block::bordered().title("岛")),
        area,
    );
}
