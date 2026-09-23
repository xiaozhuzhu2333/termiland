use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, DirSelection, Focus, LeftPage, NoticeKind, Selection};
use crate::config::UiConfig;
use crate::island::IslandState;

fn focus_border(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    }
}

const TAB_LABELS: [&str; 2] = ["跳转", "命令"];

pub(crate) fn display_width(s: &str) -> u16 {
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

pub fn paged_add_zone(panel: Rect) -> (u16, u16) {
    (panel.x + panel.width - 4, panel.y)
}

pub fn paged_add_hit(panel: Rect, column: u16, row: u16) -> bool {
    if panel.width < 16 {
        return false;
    }
    let (x, y) = paged_add_zone(panel);
    row == y && column >= x && column < x + 3
}

pub fn paged_record_hit(
    panel: Rect,
    column: u16,
    row: u16,
    records_len: usize,
    offset: u16,
    input_active: bool,
) -> Option<(usize, bool)> {
    let view = paged_records_view(panel, input_active, records_len);
    if row >= view.area.y
        && row < view.area.y + view.area.height
        && column >= view.area.x
        && column < view.area.x + view.area.width
    {
        if view.show_bar(records_len) && column == view.area.x + view.area.width - 1 {
            return None;
        }
        let index = offset as usize + (row - view.area.y) as usize;
        if index < records_len {
            return Some((index, column - view.area.x <= 1));
        }
    }
    None
}

pub const EMPTY_RIGHT_WIDTH: u16 = 8;

pub fn right_pane_width(config: &UiConfig, islands_empty: bool) -> u16 {
    if islands_empty {
        EMPTY_RIGHT_WIDTH
    } else {
        config.right_width
    }
}

fn pane_areas(area: Rect, left_width: u16, right_width: u16) -> [Rect; 3] {
    Layout::horizontal([
        Constraint::Length(left_width),
        Constraint::Min(10),
        Constraint::Length(right_width),
    ])
    .areas(area)
}

pub fn right_pane_rect(area: Rect, left_width: u16, right_width: u16) -> Rect {
    pane_areas(area, left_width, right_width)[2]
}

pub fn paged_panel_rect(area: Rect, left_width: u16, right_width: u16) -> Rect {
    let [left, _, _] = pane_areas(area, left_width, right_width);
    let [_, panel] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(left);
    panel
}

pub fn dir_pane_rect(area: Rect, left_width: u16, right_width: u16) -> Rect {
    let [left, _, _] = pane_areas(area, left_width, right_width);
    let [dir, _] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(left);
    dir
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

pub fn islands_body_and_bar(right: Rect) -> (Rect, Rect) {
    let bar = Rect {
        x: right.x,
        y: right.y + right.height.saturating_sub(1),
        width: right.width,
        height: 1,
    };
    let body = Rect {
        height: right.height.saturating_sub(1),
        ..right
    };
    (body, bar)
}

pub fn add_button_zone(bar: Rect) -> (u16, u16) {
    let x = bar.x + bar.width / 2 - display_width("[+]") / 2;
    (x, 3)
}

pub fn island_inner(rect: Rect) -> Rect {
    bordered_inner(rect)
}

pub fn bordered_inner(rect: Rect) -> Rect {
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

pub fn terminal_pane_inner(area: Rect, left_width: u16, right_width: u16) -> Rect {
    let center = pane_areas(area, left_width, right_width)[1];
    Block::bordered().inner(center)
}

pub fn draw(f: &mut Frame, app: &App) {
    let right_width = right_pane_width(&app.config.ui, app.islands.is_empty());
    let [left, center, right] = pane_areas(f.area(), app.config.ui.left_width, right_width);

    terminal_pane(f, center, app);
    left_column(f, left, app);
    islands_column(f, right, app);
}

fn placeholder(f: &mut Frame, area: Rect, title: &str, text: &str, border: Style) {
    let block = Block::bordered().border_style(border).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let centered = Rect {
        y: inner.y + inner.height / 2,
        height: 1,
        ..inner
    };
    f.render_widget(Paragraph::new(text).alignment(Alignment::Center), centered);
}

fn terminal_pane(f: &mut Frame, area: Rect, app: &App) {
    let status = if let Some((text, kind)) = app.notice_text() {
        let style = match kind {
            NoticeKind::Warn => Style::new().fg(Color::Red),
            NoticeKind::Info => Style::new(),
            NoticeKind::Highlight => Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        };
        Line::from(Span::styled(text, style))
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
                "Ctrl+Q 退出 · F1 切页 · F2 岛栏 · F3 加岛 · F4 收藏",
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
    dir_pane(f, dir, app);
    paged_panel(f, panel, app, focus_border(false));
}

pub fn dir_pane_lines(area: Rect, app: &App) -> Vec<(String, Style)> {
    let inner = bordered_inner(area);
    let mut lines = Vec::new();
    if inner.width < 2 || inner.height < 2 {
        return lines;
    }
    lines.push((
        truncate_tail(&app.dir.path.to_string_lossy(), inner.width),
        Style::new().fg(Color::DarkGray),
    ));
    if inner.height < 3 {
        return lines;
    }
    lines.push((String::new(), Style::new()));
    if let Some(err) = &app.dir.error {
        lines.push((format!("读取失败: {err}"), Style::new().fg(Color::Red)));
        return lines;
    }
    let visible = inner.height - 2;
    let total = app.dir.total();
    if total == 0 {
        lines.push(("空目录".to_owned(), Style::new().fg(Color::DarkGray)));
        return lines;
    }
    let offset = app.dir.clamped_offset(visible);
    let show_bar = total > visible;
    let text_width = inner.width - u16::from(show_bar);
    for i in 0..visible {
        let Some(entry) = app.dir.entries.get((offset + i) as usize) else {
            break;
        };
        let style = if entry.is_dir {
            Style::new().fg(Color::Cyan)
        } else if entry.hidden {
            Style::new().fg(Color::DarkGray)
        } else {
            Style::new()
        };
        lines.push((truncate_entry(&entry.name, entry.is_dir, text_width), style));
    }
    lines
}

fn dir_pane(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered()
        .border_style(focus_border(false))
        .title("目录");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 2 || inner.height < 2 {
        return;
    }

    let lines = dir_pane_lines(area, app);
    for (i, (text, style)) in lines.iter().enumerate() {
        f.render_widget(
            Paragraph::new(Span::styled(text.clone(), *style)),
            Rect {
                y: inner.y + i as u16,
                height: 1,
                ..inner
            },
        );
    }

    let total = app.dir.total();
    let visible = inner.height.saturating_sub(2);
    if total > visible {
        let offset = app.dir.clamped_offset(visible);
        let bar_x = inner.x + inner.width - 1;
        let list_y = inner.y + 2;
        for y in list_y..list_y + visible {
            f.buffer_mut()[(bar_x, y)]
                .set_symbol("│")
                .set_style(Style::new().fg(Color::DarkGray));
        }
        let track = visible;
        let thumb_h = ((u32::from(track) * u32::from(visible)) / u32::from(total))
            .max(1)
            .min(u32::from(track)) as u16;
        let max_off = total - visible;
        let thumb_y = if max_off == 0 {
            list_y
        } else {
            list_y + (u32::from(offset) * u32::from(track - thumb_h) / u32::from(max_off)) as u16
        };
        for dy in 0..thumb_h {
            f.buffer_mut()[(bar_x, thumb_y + dy)]
                .set_symbol("▐")
                .set_style(Style::new().fg(Color::Cyan));
        }
    }

    match app.dir_selection() {
        Some(DirSelection::Rect(sel)) => {
            for row in 0..inner.height {
                for col in 0..inner.width {
                    if sel.contains(row, col) {
                        let cell = &mut f.buffer_mut()[(inner.x + col, inner.y + row)];
                        cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
                    }
                }
            }
        }
        Some(DirSelection::Path) => reverse_row(f, inner, 0, &lines),
        Some(DirSelection::Entry { name, .. }) => {
            let visible = inner.height.saturating_sub(2);
            let offset = app.dir.clamped_offset(visible);
            if let Some(index) = app.dir.entries.iter().position(|e| e.name == *name)
                && index >= offset as usize
                && index < (offset + visible) as usize
            {
                reverse_row(f, inner, 2 + index as u16 - offset, &lines);
            }
        }
        None => {}
    }
}

fn reverse_row(f: &mut Frame, inner: Rect, row: u16, lines: &[(String, Style)]) {
    let Some((text, _)) = lines.get(row as usize) else {
        return;
    };
    for col in 0..display_width(text).min(inner.width) {
        let cell = &mut f.buffer_mut()[(inner.x + col, inner.y + row)];
        cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
    }
}

fn truncate_head(text: &str, width: u16) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(text) <= width {
        return text.to_owned();
    }
    if width == 1 {
        return "…".to_owned();
    }
    let mut out = String::new();
    let mut used = 0u16;
    for ch in text.chars() {
        let cw = if ch.is_ascii() { 1 } else { 2 };
        if used + cw > width - 1 {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

fn truncate_middle(text: &str, width: u16) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(text) <= width {
        return text.to_owned();
    }
    if width < 5 {
        return truncate_head(text, width);
    }
    let budget = width - 1;
    let head_budget = budget * 2 / 3;
    let tail_budget = budget - head_budget;
    let mut head = String::new();
    let mut used = 0u16;
    for ch in text.chars() {
        let cw = if ch.is_ascii() { 1 } else { 2 };
        if used + cw > head_budget {
            break;
        }
        head.push(ch);
        used += cw;
    }
    let mut tail = Vec::new();
    let mut used = 0u16;
    for ch in text.chars().rev() {
        let cw = if ch.is_ascii() { 1 } else { 2 };
        if used + cw > tail_budget {
            break;
        }
        tail.push(ch);
        used += cw;
    }
    tail.reverse();
    format!("{head}…{}", tail.into_iter().collect::<String>())
}

fn truncate_entry(name: &str, is_dir: bool, width: u16) -> String {
    if width == 0 {
        return String::new();
    }
    let suffix = if is_dir { "/" } else { "" };
    if display_width(name) + suffix.len() as u16 <= width {
        return format!("{name}{suffix}");
    }
    truncate_head(name, width)
}

fn truncate_tail(text: &str, width: u16) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(text) <= width {
        return text.to_owned();
    }
    if width == 1 {
        return "…".to_owned();
    }
    let mut tail = Vec::new();
    let mut used = 0u16;
    for ch in text.chars().rev() {
        let cw = if ch.is_ascii() { 1 } else { 2 };
        if used + cw > width - 1 {
            break;
        }
        tail.push(ch);
        used += cw;
    }
    tail.push('…');
    tail.reverse();
    tail.into_iter().collect()
}

pub struct PagedRecordsView {
    pub area: Rect,
    pub text_width: u16,
}

impl PagedRecordsView {
    pub fn visible(&self) -> u16 {
        self.area.height
    }

    pub fn show_bar(&self, records_len: usize) -> bool {
        records_len > self.area.height as usize
    }
}

pub fn paged_records_view(panel: Rect, input_active: bool, records_len: usize) -> PagedRecordsView {
    let inner = bordered_inner(panel);
    let area = Rect {
        height: inner.height.saturating_sub(u16::from(input_active)),
        ..inner
    };
    let view = PagedRecordsView {
        area,
        text_width: 0,
    };
    let show_bar = view.show_bar(records_len);
    PagedRecordsView {
        area,
        text_width: area.width.saturating_sub(2 + u16::from(show_bar)),
    }
}

pub fn clamp_record_offset(offset: u16, records_len: usize, visible: u16) -> u16 {
    let max = records_len
        .saturating_sub(visible as usize)
        .min(u16::MAX as usize);
    offset.min(max as u16)
}

fn paged_panel(f: &mut Frame, area: Rect, app: &App, border: Style) {
    let block = Block::bordered()
        .border_style(border)
        .title(page_tabs(app.left_page));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 2 || inner.height < 1 {
        return;
    }
    let records_len = app.paged_records_len();
    let input_active = app.panel_input().is_some();
    let view = paged_records_view(area, input_active, records_len);
    let offset = clamp_record_offset(app.panel_offset(), records_len, view.visible());
    f.render_widget(
        Paragraph::new(Text::from(paged_panel_lines(app, offset, &view))),
        view.area,
    );

    if view.show_bar(records_len) {
        let bar_x = view.area.x + view.area.width - 1;
        let track = view.area.height;
        for y in view.area.y..view.area.y + track {
            f.buffer_mut()[(bar_x, y)]
                .set_symbol("│")
                .set_style(Style::new().fg(Color::DarkGray));
        }
        let total = records_len.min(u16::MAX as usize) as u16;
        let thumb_h = ((u32::from(track) * u32::from(track)) / u32::from(total))
            .max(1)
            .min(u32::from(track)) as u16;
        let max_off = total - track;
        let thumb_y = if max_off == 0 {
            view.area.y
        } else {
            view.area.y
                + (u32::from(offset) * u32::from(track - thumb_h) / u32::from(max_off)) as u16
        };
        for dy in 0..thumb_h {
            f.buffer_mut()[(bar_x, thumb_y + dy)]
                .set_symbol("▐")
                .set_style(Style::new().fg(Color::Cyan));
        }
    }

    if let Some(text) = app.panel_input() {
        let y = inner.y + inner.height - 1;
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::new().fg(Color::Cyan)),
                Span::raw(text.to_owned()),
                Span::styled("▎", Style::new().fg(Color::Cyan)),
            ])),
            Rect {
                y,
                height: 1,
                ..inner
            },
        );
    }

    if area.width >= 16 {
        let (x, y) = paged_add_zone(area);
        f.render_widget(
            Paragraph::new(Span::styled(
                "[+]",
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
            Rect {
                x,
                y,
                width: 3,
                height: 1,
            },
        );
    }
}

fn paged_panel_lines(app: &App, offset: u16, view: &PagedRecordsView) -> Vec<Line<'static>> {
    let records: Vec<String> = match app.left_page {
        LeftPage::Jump => app.config.jump.bookmarks.clone(),
        LeftPage::Commands => app
            .config
            .commands
            .items
            .iter()
            .map(|c| match &c.name {
                Some(name) => format!("{name} · {}", c.command),
                None => c.command.clone(),
            })
            .collect(),
    };
    if records.is_empty() {
        let hint = match app.left_page {
            LeftPage::Jump => "F4 添加当前路径",
            LeftPage::Commands => "F4 添加当前命令",
        };
        return vec![Line::from(Span::styled(
            hint,
            Style::new().fg(Color::DarkGray),
        ))];
    }
    records
        .into_iter()
        .enumerate()
        .skip(offset as usize)
        .take(view.visible() as usize)
        .map(|(index, text)| {
            let (marker, marker_style) = if app.armed_record() == Some(index) {
                (
                    "× ",
                    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
                )
            } else {
                ("→ ", Style::new().fg(Color::DarkGray))
            };
            Line::from(vec![
                Span::styled(marker, marker_style),
                Span::raw(truncate_middle(&text, view.text_width)),
            ])
        })
        .collect()
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
    let (body, bar) = islands_body_and_bar(area);
    if app.islands.is_empty() {
        placeholder(f, body, "岛", "无岛", focus_border(false));
    } else {
        let heights: Vec<Option<u16>> = app.islands.iter().map(|i| i.height).collect();
        let areas = island_layout(body, &heights);
        for (index, (island, &island_area)) in app.islands.iter().zip(areas.iter()).enumerate() {
            render_island(f, island_area, island, app.focus() == Focus::Island(index));
        }
    }
    let bar_line = Line::from(Span::styled(
        "[+]",
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    ));
    f.render_widget(Paragraph::new(bar_line).alignment(Alignment::Center), bar);
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
    badge_spans.push(Span::raw(" "));
    badge_spans.push(Span::styled("×", Style::new().fg(Color::Red)));
    let mut block = Block::bordered()
        .border_style(focus_border(focused))
        .title(title)
        .title_top(Line::from(badge_spans).right_aligned());
    if focused {
        block = block.title_bottom(
            Line::from(Span::styled(
                "↑↓ 切换 · 回车执行",
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
    fn paged_add_hit_matches_zone() {
        let panel = Rect::new(0, 5, 22, 10);
        assert!(paged_add_hit(panel, 18, 5));
        assert!(paged_add_hit(panel, 20, 5));
        assert!(!paged_add_hit(panel, 17, 5));
        assert!(!paged_add_hit(panel, 21, 5));
        assert!(!paged_add_hit(panel, 18, 6));
        assert!(
            !paged_add_hit(Rect::new(0, 5, 12, 10), 8, 5),
            "窄面板不显示 [+]"
        );
    }

    #[test]
    fn paged_record_hit_maps_rows_and_arrow_zone() {
        let panel = Rect::new(0, 5, 22, 10);
        assert_eq!(paged_record_hit(panel, 1, 6, 3, 0, false), Some((0, true)));
        assert_eq!(paged_record_hit(panel, 2, 6, 3, 0, false), Some((0, true)));
        assert_eq!(paged_record_hit(panel, 3, 6, 3, 0, false), Some((0, false)));
        assert_eq!(paged_record_hit(panel, 5, 7, 3, 0, false), Some((1, false)));
        assert_eq!(paged_record_hit(panel, 1, 8, 3, 0, false), Some((2, true)));
        assert_eq!(
            paged_record_hit(panel, 1, 9, 3, 0, false),
            None,
            "超出条目数的行不可命中"
        );
        assert_eq!(
            paged_record_hit(panel, 1, 6, 3, 2, false),
            Some((2, true)),
            "偏移后首行对应绝对索引 2"
        );
        assert_eq!(
            paged_record_hit(panel, 3, 13, 10, 0, false),
            Some((7, false)),
            "可见区最后一行可命中"
        );
        assert_eq!(
            paged_record_hit(panel, 1, 14, 10, 0, false),
            None,
            "超出可见高度的行不可命中"
        );
        assert_eq!(
            paged_record_hit(panel, 1, 13, 10, 0, true),
            None,
            "输入栏占用最底行时该行不可命中"
        );
        assert_eq!(
            paged_record_hit(panel, 1, 12, 10, 0, true),
            Some((6, true)),
            "输入栏打开时倒数第二行仍可命中"
        );
        assert_eq!(
            paged_record_hit(panel, 20, 6, 10, 0, false),
            None,
            "滚动条所在列不可命中"
        );
        assert_eq!(
            paged_record_hit(panel, 3, 6, 10, 0, false),
            Some((0, false)),
            "滚动条之外的列正常命中"
        );
        assert_eq!(
            paged_record_hit(panel, 1, 5, 3, 0, false),
            None,
            "边框行不是条目"
        );
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
        let inner = terminal_pane_inner(Rect::new(0, 0, 120, 30), cfg.left_width, cfg.right_width);
        assert_eq!((inner.height, inner.width), (28, 60));
        let inner = terminal_pane_inner(Rect::new(0, 0, 80, 24), cfg.left_width, cfg.right_width);
        assert_eq!((inner.height, inner.width), (22, 20));
    }

    #[test]
    fn dir_pane_lines_insert_blank_separator() {
        use crate::app::App;
        use crate::config::Config;
        use crate::dirpane::Entry;

        let mut app = App::new(Config::default()).expect("app");
        app.dir.entries = vec![Entry {
            name: "alpha".to_owned(),
            is_dir: true,
            hidden: false,
        }];

        let lines = dir_pane_lines(Rect::new(0, 0, 22, 20), &app);
        assert_eq!(lines[1].0, "", "路径行下应为空白分隔行");
        assert_eq!(lines[2].0, "alpha/", "条目应从分隔行之后开始");
    }

    #[test]
    fn paged_panel_lines_truncates_long_records() {
        use crate::app::App;
        use crate::config::Config;

        let mut app = App::new(Config::default()).expect("app");
        app.config.jump.bookmarks =
            vec!["/very/long/directory/name/that/cannot/fit/in/pane".to_owned()];

        let view = paged_records_view(Rect::new(0, 0, 22, 12), false, 1);
        let lines = paged_panel_lines(&app, 0, &view);
        let text = lines[0].spans[1].content.to_string();
        assert_eq!(
            text, "/very/long/…n/pane",
            "中段截断应同时保留头尾，便于区分前缀相同的长记录"
        );
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
            save_path: None,
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

        for y in [1u16, 5, 10, 13, 20, 27] {
            let border = buf[(84, y)].symbol();
            assert_eq!(border, "│", "y={y} 处岛栏左边框被破坏: {border:?}");
        }

        let (bx, _) = add_button_zone(Rect::new(84, 29, 36, 1));
        assert_eq!(buf[(bx, 29)].symbol(), "[", "+ 按钮应渲染在命中区起点");
        assert_eq!(buf[(bx + 1, 29)].symbol(), "+");
        assert_eq!(buf[(bx + 2, 29)].symbol(), "]");
    }
}
