use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;

use crate::config::{CommandItem, Config};
use crate::dirpane::DirPane;
use crate::island::{IslandState, sanitize_resize_boundary};
use crate::keys;
use crate::pty;
use crate::ui;

const SCROLLBACK_LEN: usize = 10_000;
const SCROLL_STEP: i32 = 3;
const NOTICE: Duration = Duration::from_secs(3);
const TRIGGER_SETTLE: Duration = Duration::from_millis(300);
const DIR_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[cfg(windows)]
const LINE_SEP: &str = "\r\n";
#[cfg(not(windows))]
const LINE_SEP: &str = "\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Warn,
    Highlight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Terminal,
    Island(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IslandHit {
    Body,
    Toggle,
    Remove,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    start: (u16, u16),
    end: (u16, u16),
}

impl Selection {
    pub fn normalized(&self) -> ((u16, u16), (u16, u16)) {
        if self.start <= self.end {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    pub fn contains(&self, row: u16, col: u16) -> bool {
        let ((r1, c1), (r2, c2)) = self.normalized();
        if r1 == r2 {
            row == r1 && col >= c1 && col <= c2
        } else if row == r1 {
            col >= c1
        } else if row == r2 {
            col <= c2
        } else {
            row > r1 && row < r2
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirSelection {
    Rect(Selection),
    Entry { name: String, is_dir: bool },
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftPage {
    Jump,
    Commands,
}

impl LeftPage {
    fn next(self) -> Self {
        match self {
            LeftPage::Jump => LeftPage::Commands,
            LeftPage::Commands => LeftPage::Jump,
        }
    }
}

pub struct App {
    pub config: Config,
    pub left_page: LeftPage,
    pub pty: pty::PtySession,
    pub term: vt100::Parser,
    pub islands: Vec<IslandState>,
    pub dir: DirPane,
    pty_size: (u16, u16),
    pane_inner: Rect,
    layout_area: Rect,
    island_areas: Vec<Rect>,
    add_bar: Rect,
    paged_panel: Rect,
    dir_pane_rect: Rect,
    scroll: u16,
    selection: Option<Selection>,
    dir_selection: Option<DirSelection>,
    notice: Option<(String, Instant, NoticeKind)>,
    focus: Focus,
    armed_record: Option<usize>,
    panel_input: Option<String>,
    panel_offset: u16,
    island_path_edit: Option<(usize, String)>,
    trigger_pending: bool,
    last_output_at: Instant,
    last_dir_poll: Instant,
    dir_mtime: Option<SystemTime>,
    should_quit: bool,
}

fn sane_area(width: u16, height: u16) -> Rect {
    if width >= 2 && height >= 2 {
        Rect::new(0, 0, width, height)
    } else {
        Rect::new(0, 0, 80, 24)
    }
}

impl App {
    pub fn new(config: Config) -> Result<Self> {
        let area = crossterm::terminal::size()
            .map(|(width, height)| sane_area(width, height))
            .unwrap_or_else(|_| sane_area(80, 24));
        let pane_inner = ui::terminal_pane_inner(
            area,
            config.ui.left_width,
            ui::right_pane_width(&config.ui, false),
        );
        let (rows, cols) = (pane_inner.height.max(2), pane_inner.width.max(2));
        let pty = pty::PtySession::spawn(rows, cols)?;
        let term = vt100::Parser::new(rows, cols, SCROLLBACK_LEN);
        let mut app = Self {
            config,
            left_page: LeftPage::Jump,
            pty,
            term,
            islands: vec![IslandState::empty(10, 20)],
            dir: DirPane::load(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            ),
            pty_size: (rows, cols),
            pane_inner,
            layout_area: area,
            island_areas: Vec::new(),
            add_bar: Rect::default(),
            paged_panel: Rect::default(),
            dir_pane_rect: Rect::default(),
            scroll: 0,
            selection: None,
            dir_selection: None,
            notice: None,
            focus: Focus::Terminal,
            armed_record: None,
            panel_input: None,
            panel_offset: 0,
            island_path_edit: None,
            trigger_pending: false,
            last_output_at: Instant::now(),
            last_dir_poll: Instant::now(),
            dir_mtime: None,
            should_quit: false,
        };
        app.refresh_layout(area)?;
        Ok(app)
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.should_quit {
            self.sync_layout(terminal)?;
            let output = self.pty.poll_output();
            if !output.is_empty() {
                self.last_output_at = Instant::now();
                self.term.process(&output);
                let (row, col) = self.term.screen().cursor_position();
                self.pty.set_cursor_position(row, col);
            }
            for island in &mut self.islands {
                island.poll_output();
            }
            self.maybe_trigger_live();
            self.poll_dir_pane();
            terminal.draw(|f| ui::draw(f, self))?;
            self.handle_events()?;
        }
        Ok(())
    }

    fn maybe_trigger_live(&mut self) {
        if !self.trigger_pending || self.last_output_at.elapsed() < TRIGGER_SETTLE {
            return;
        }
        self.trigger_pending = false;
        let cwds: Vec<PathBuf> = (0..self.islands.len())
            .map(|index| self.island_working_dir(index))
            .collect();
        for (island, cwd) in self.islands.iter_mut().zip(cwds) {
            if island.follow && island.armed {
                island.execute(&cwd);
            }
        }
    }

    fn poll_dir_pane(&mut self) {
        if self.last_dir_poll.elapsed() < DIR_POLL_INTERVAL {
            return;
        }
        self.last_dir_poll = Instant::now();
        if let Some(cwd) = self.shell_cwd()
            && cwd != self.dir.path
        {
            self.dir = DirPane::load(cwd);
            self.dir_mtime = dir_mtime(&self.dir.path);
            self.prune_dir_selection(true);
            return;
        }
        let mtime = dir_mtime(&self.dir.path);
        if mtime.is_some() && mtime != self.dir_mtime {
            self.dir_mtime = mtime;
            self.dir.reload();
            self.prune_dir_selection(false);
        }
    }

    fn prune_dir_selection(&mut self, cwd_changed: bool) {
        let keep = match self.dir_selection.as_ref() {
            Some(DirSelection::Entry { name, .. }) => {
                self.dir.entries.iter().any(|e| e.name == *name)
            }
            Some(DirSelection::Path) => !cwd_changed,
            Some(DirSelection::Rect(_)) => false,
            None => false,
        };
        if !keep {
            self.dir_selection = None;
        }
    }

    #[cfg(target_os = "linux")]
    fn shell_cwd(&self) -> Option<PathBuf> {
        let pid = self.pty.child_pid()?;
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    #[cfg(not(target_os = "linux"))]
    fn shell_cwd(&self) -> Option<PathBuf> {
        None
    }

    fn sync_layout(&mut self, terminal: &DefaultTerminal) -> Result<()> {
        let size = terminal.size()?;
        let area = sane_area(size.width, size.height);
        if area == self.layout_area && self.island_areas.len() == self.islands.len() {
            return Ok(());
        }
        self.refresh_layout(area)
    }

    fn refresh_layout(&mut self, area: Rect) -> Result<()> {
        self.layout_area = area;
        let left = self.config.ui.left_width;
        let right = ui::right_pane_width(&self.config.ui, self.islands.is_empty());
        self.pane_inner = ui::terminal_pane_inner(area, left, right);
        self.relayout_islands(area, left, right);
        self.paged_panel = ui::paged_panel_rect(area, left, right);
        self.dir_pane_rect = ui::dir_pane_rect(area, left, right);
        let size = (self.pane_inner.height.max(2), self.pane_inner.width.max(2));
        if self.pty_size != size {
            self.pty_size = size;
            self.pty.resize(size.0, size.1)?;
            sanitize_resize_boundary(&mut self.term, size.1);
            self.term.screen_mut().set_size(size.0, size.1);
        }
        Ok(())
    }

    fn relayout_islands(&mut self, area: Rect, left: u16, right: u16) {
        let (body, bar) = ui::islands_body_and_bar(ui::right_pane_rect(area, left, right));
        self.add_bar = bar;
        let heights: Vec<Option<u16>> = self.islands.iter().map(|i| i.height).collect();
        self.island_areas = ui::island_layout(body, &heights);
        for (island, &rect) in self.islands.iter_mut().zip(self.island_areas.iter()) {
            let output = ui::island_output_inner(rect);
            island.resize(output.height.max(1), output.width.max(1));
        }
    }

    fn add_island(&mut self) -> Result<()> {
        if self.islands.len() >= self.config.islands.max {
            self.notify(
                format!("已达岛上限 {}", self.config.islands.max),
                NoticeKind::Warn,
            );
            return Ok(());
        }
        self.islands.push(crate::island::IslandState::empty(10, 20));
        self.refresh_layout(self.layout_area)?;
        self.focus = Focus::Island(self.islands.len() - 1);
        Ok(())
    }

    fn remove_island(&mut self, index: usize) -> Result<()> {
        if index >= self.islands.len() {
            return Ok(());
        }
        self.islands.remove(index);
        self.island_path_edit = None;
        self.refresh_layout(self.layout_area)?;
        self.focus = match self.focus {
            Focus::Island(i) if i == index => Focus::Terminal,
            Focus::Island(i) if i > index => Focus::Island(i - 1),
            other => other,
        };
        Ok(())
    }

    fn remove_focused_island(&mut self) -> Result<()> {
        if let Focus::Island(i) = self.focus {
            self.remove_island(i)?;
        }
        Ok(())
    }

    fn notify(&mut self, text: String, kind: NoticeKind) {
        self.notice = Some((text, Instant::now(), kind));
    }

    pub fn scroll(&self) -> u16 {
        self.scroll
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub fn dir_selection(&self) -> Option<&DirSelection> {
        self.dir_selection.as_ref()
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn armed_record(&self) -> Option<usize> {
        self.armed_record
    }

    pub fn panel_input(&self) -> Option<&str> {
        self.panel_input.as_deref()
    }

    pub fn panel_offset(&self) -> u16 {
        self.panel_offset
    }

    pub fn island_path_edit(&self) -> Option<(usize, &str)> {
        self.island_path_edit
            .as_ref()
            .map(|(i, t)| (*i, t.as_str()))
    }

    pub fn paged_records_len(&self) -> usize {
        match self.left_page {
            LeftPage::Jump => self.config.jump.bookmarks.len(),
            LeftPage::Commands => self.config.commands.items.len(),
        }
    }

    pub fn selection_chars(&self) -> Option<usize> {
        self.selection
            .map(|sel| selection_text(&self.term, &sel).chars().count())
    }

    pub fn notice_text(&self) -> Option<(String, NoticeKind)> {
        self.notice
            .as_ref()
            .and_then(|(text, at, kind)| (at.elapsed() < NOTICE).then(|| (text.clone(), *kind)))
    }

    fn handle_events(&mut self) -> Result<()> {
        if !event::poll(Duration::from_millis(50))? {
            return Ok(());
        }
        let mut input = Vec::new();
        loop {
            match event::read()? {
                Event::Key(key) => self.handle_key(key, &mut input)?,
                Event::Paste(text) => {
                    let line: String = text.chars().filter(|c| *c != '\r' && *c != '\n').collect();
                    if let Some((_, buffer)) = &mut self.island_path_edit {
                        buffer.push_str(&line);
                    } else if self.panel_input.is_some() {
                        if let Some(buffer) = &mut self.panel_input {
                            buffer.push_str(&line);
                        }
                    } else {
                        match self.focus {
                            Focus::Island(_) => {
                                self.edit_focused_command(Some(&line));
                            }
                            Focus::Terminal => input.extend_from_slice(&keys::paste_bytes(&text)),
                        }
                    }
                }
                Event::Mouse(mouse) => self.handle_mouse(mouse)?,
                _ => {}
            }
            if self.should_quit || !event::poll(Duration::ZERO)? {
                break;
            }
        }
        if !input.is_empty() {
            self.send_terminal_input(&input)?;
        }
        Ok(())
    }

    fn send_terminal_input(&mut self, input: &[u8]) -> Result<()> {
        if input.contains(&b'\r') {
            self.trigger_pending = true;
            self.last_output_at = Instant::now();
        }
        self.reset_scroll();
        self.selection = None;
        self.pty.write_input(input)
    }

    fn handle_key(&mut self, key: KeyEvent, input: &mut Vec<u8>) -> Result<()> {
        if key.kind == KeyEventKind::Release {
            return Ok(());
        }
        let is_press = key.kind == KeyEventKind::Press;
        if is_press && self.island_path_edit.is_some() {
            self.handle_island_path_key(key);
            return Ok(());
        }
        if is_press && self.panel_input.is_some() {
            self.handle_panel_input_key(key);
            return Ok(());
        }
        if is_press
            && key.code == KeyCode::Char('q')
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.should_quit = true;
        } else if is_press && key.code == KeyCode::F(1) {
            self.left_page = self.left_page.next();
            self.armed_record = None;
            self.panel_offset = 0;
        } else if is_press && key.code == KeyCode::F(2) {
            self.cycle_focus();
        } else if is_press && key.code == KeyCode::F(3) {
            self.add_island()?;
        } else if is_press && key.code == KeyCode::F(4) {
            self.add_panel_record()?;
        } else if is_press
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && self.has_any_selection()
        {
            self.copy_active_selection();
        } else {
            match self.focus {
                Focus::Terminal => self.handle_terminal_key(key, is_press, input),
                Focus::Island(index) => {
                    if is_press
                        && matches!(key.code, KeyCode::Tab | KeyCode::BackTab | KeyCode::Esc)
                    {
                        self.focus = Focus::Terminal;
                    } else if is_press && key.code == KeyCode::Up {
                        self.move_island_focus(-1);
                    } else if is_press && key.code == KeyCode::Down {
                        self.move_island_focus(1);
                    } else if is_press && key.code == KeyCode::End && key.modifiers.is_empty() {
                        if let Some(island) = self.islands.get_mut(index) {
                            island.reset_scroll();
                        }
                    } else if is_press && key.code == KeyCode::Delete && key.modifiers.is_empty() {
                        self.remove_focused_island()?;
                    } else if is_press && key.code == KeyCode::Enter && key.modifiers.is_empty() {
                        self.execute_focused_island();
                    } else if is_press && key.code == KeyCode::Backspace && key.modifiers.is_empty()
                    {
                        self.edit_focused_command(None);
                    } else if is_press
                        && let KeyCode::Char(c) = key.code
                        && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
                    {
                        self.edit_focused_command(Some(&c.to_string()));
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_terminal_key(&mut self, key: KeyEvent, is_press: bool, input: &mut Vec<u8>) {
        if is_press && key.code == KeyCode::End && key.modifiers.is_empty() && self.scroll > 0 {
            self.reset_scroll();
        } else if let Some(bytes) = keys::encode(&key) {
            input.extend_from_slice(&bytes);
        }
    }

    fn has_any_selection(&self) -> bool {
        self.selection.is_some()
            || self.dir_selection.is_some()
            || self.islands.iter().any(|i| i.selection.is_some())
    }

    fn copy_active_selection(&mut self) {
        if let Some(sel) = self.selection {
            self.copy_text(selection_text(&self.term, &sel));
            return;
        }
        for island in &self.islands {
            if let Some(sel) = island.selection {
                self.copy_text(selection_text(&island.parser, &sel));
                return;
            }
        }
        if let Some(sel) = &self.dir_selection {
            let text = self.dir_selection_text(sel);
            self.copy_text(text);
        }
    }

    fn dir_selection_text(&self, sel: &DirSelection) -> String {
        match sel {
            DirSelection::Rect(rect) => {
                let lines: Vec<String> = ui::dir_pane_lines(self.dir_pane_rect, self)
                    .into_iter()
                    .map(|(text, _)| text)
                    .collect();
                lines_selection_text(&lines, rect)
            }
            DirSelection::Entry { name, is_dir } => {
                let mut text = name.clone();
                if *is_dir {
                    text.push('/');
                }
                text
            }
            DirSelection::Path => self.dir.path.to_string_lossy().into_owned(),
        }
    }

    fn copy_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if crate::clipboard::copy(&text) {
            let count = text.chars().count();
            self.notify(format!("已复制 {count} 字符"), NoticeKind::Info);
        } else {
            self.notify(
                "复制失败（内容过大或环境不支持）".to_owned(),
                NoticeKind::Warn,
            );
        }
    }

    fn cycle_focus(&mut self) {
        let count = self.islands.len();
        if count == 0 {
            return;
        }
        self.focus = match self.focus {
            Focus::Terminal => Focus::Island(0),
            Focus::Island(i) if i + 1 < count => Focus::Island(i + 1),
            Focus::Island(_) => Focus::Terminal,
        };
    }

    fn move_island_focus(&mut self, delta: i32) {
        if let Focus::Island(i) = self.focus {
            let count = self.islands.len() as i32;
            let target = (i as i32 + delta).clamp(0, count - 1);
            self.focus = Focus::Island(target as usize);
        }
    }

    fn edit_focused_command(&mut self, text: Option<&str>) {
        if let Focus::Island(i) = self.focus
            && let Some(island) = self.islands.get_mut(i)
        {
            match text {
                Some(t) => island.command.push_str(t),
                None => {
                    island.command.pop();
                    if island.command.is_empty() {
                        island.clear();
                    }
                }
            }
            island.selection = None;
            island.reset_scroll();
        }
    }

    fn execute_focused_island(&mut self) {
        if let Focus::Island(i) = self.focus {
            let cwd = self.island_working_dir(i);
            if let Some(island) = self.islands.get_mut(i) {
                island.execute(&cwd);
            }
        }
    }

    fn island_cwd(&self) -> PathBuf {
        self.shell_cwd().unwrap_or_else(|| self.dir.path.clone())
    }

    fn panel_input_row_hit(&self, column: u16, row: u16) -> bool {
        let inner = ui::bordered_inner(self.paged_panel);
        inner.height > 0
            && row == inner.y + inner.height - 1
            && column >= inner.x
            && column < inner.x + inner.width
    }

    fn handle_panel_input_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
        } else if key.code == KeyCode::Enter && key.modifiers.is_empty() {
            self.confirm_panel_input();
        } else if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            self.panel_input = None;
        } else if key.code == KeyCode::Backspace && key.modifiers.is_empty() {
            if let Some(text) = &mut self.panel_input {
                text.pop();
            }
        } else if let KeyCode::Char(c) = key.code
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
            && let Some(text) = &mut self.panel_input
        {
            text.push(c);
        }
    }

    fn confirm_panel_input(&mut self) {
        let Some(raw) = self.panel_input.take() else {
            return;
        };
        let text = raw.trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.push_panel_record(text);
    }

    fn handle_island_path_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
        } else if key.code == KeyCode::Enter && key.modifiers.is_empty() {
            self.confirm_island_path();
        } else if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            self.island_path_edit = None;
        } else if key.code == KeyCode::Backspace && key.modifiers.is_empty() {
            if let Some((_, text)) = &mut self.island_path_edit {
                text.pop();
            }
        } else if let KeyCode::Char(c) = key.code
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
            && let Some((_, text)) = &mut self.island_path_edit
        {
            text.push(c);
        }
    }

    fn confirm_island_path(&mut self) {
        let Some((index, raw)) = self.island_path_edit.take() else {
            return;
        };
        let text = raw.trim().to_owned();
        if text.is_empty() {
            if let Some(island) = self.islands.get_mut(index)
                && island.custom_path.take().is_some()
            {
                self.notify("已恢复默认路径".to_owned(), NoticeKind::Info);
            }
            return;
        }
        let expanded = expand_tilde(&text);
        if !expanded.is_absolute() {
            self.notify(
                "路径需为绝对路径（可用 ~ 开头）".to_owned(),
                NoticeKind::Warn,
            );
            self.island_path_edit = Some((index, raw));
            return;
        }
        if !expanded.is_dir() {
            self.notify(
                format!("路径不存在: {}", expanded.display()),
                NoticeKind::Warn,
            );
            self.island_path_edit = Some((index, raw));
            return;
        }
        if let Some(island) = self.islands.get_mut(index) {
            island.custom_path = Some(expanded.to_string_lossy().into_owned());
        }
        self.notify(
            format!("已指定路径 {}", expanded.display()),
            NoticeKind::Info,
        );
    }

    fn island_working_dir(&self, index: usize) -> PathBuf {
        self.islands
            .get(index)
            .and_then(|island| island.custom_path.clone())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.island_cwd())
    }

    fn push_panel_record(&mut self, text: String) {
        let duplicate = match self.left_page {
            LeftPage::Jump => self.config.jump.bookmarks.contains(&text),
            LeftPage::Commands => self.config.commands.items.iter().any(|c| c.command == text),
        };
        if duplicate {
            self.notify("已存在，未重复添加".to_owned(), NoticeKind::Warn);
            return;
        }
        self.notify(format!("已添加 {text}"), NoticeKind::Info);
        match self.left_page {
            LeftPage::Jump => self.config.jump.bookmarks.push(text),
            LeftPage::Commands => self.config.commands.items.push(CommandItem {
                name: None,
                command: text,
            }),
        }
        let len = self.paged_records_len();
        let view = ui::paged_records_view(self.paged_panel, false, len);
        self.panel_offset = ui::clamp_record_offset(u16::MAX, len, view.visible());
        self.save_config();
    }

    fn panel_record_text(&self, index: usize) -> Option<String> {
        match self.left_page {
            LeftPage::Jump => self.config.jump.bookmarks.get(index).cloned(),
            LeftPage::Commands => self
                .config
                .commands
                .items
                .get(index)
                .map(|c| c.command.clone()),
        }
    }

    fn panel_record_payload(&self, index: usize) -> Option<String> {
        let text = self.panel_record_text(index)?;
        Some(match self.left_page {
            LeftPage::Jump => format!("cd {text}"),
            LeftPage::Commands => text,
        })
    }

    fn execute_panel_record(&mut self, index: usize) -> Result<()> {
        let Some(payload) = self.panel_record_payload(index) else {
            return Ok(());
        };
        let mut bytes = terminal_clear_line_bytes(&self.term);
        bytes.extend_from_slice(payload.as_bytes());
        self.focus = Focus::Terminal;
        self.send_terminal_input(&bytes)
    }

    fn add_panel_record(&mut self) -> Result<()> {
        match self.left_page {
            LeftPage::Jump => {
                let path = self.dir.path.to_string_lossy().into_owned();
                self.push_panel_record(path);
            }
            LeftPage::Commands => {
                let command = self.current_command();
                if command.is_empty() {
                    self.notify("当前命令为空".to_owned(), NoticeKind::Warn);
                    return Ok(());
                }
                self.push_panel_record(command);
            }
        }
        Ok(())
    }

    fn delete_panel_record(&mut self, index: usize) {
        let exists = match self.left_page {
            LeftPage::Jump => index < self.config.jump.bookmarks.len(),
            LeftPage::Commands => index < self.config.commands.items.len(),
        };
        if !exists {
            return;
        }
        match self.left_page {
            LeftPage::Jump => {
                self.config.jump.bookmarks.remove(index);
            }
            LeftPage::Commands => {
                self.config.commands.items.remove(index);
            }
        }
        self.notify("已删除".to_owned(), NoticeKind::Info);
        self.save_config();
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.notify(format!("配置保存失败: {e}"), NoticeKind::Warn);
        }
    }

    fn current_command(&self) -> String {
        strip_prompt(&terminal_line_before_cursor(&self.term))
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_mouse(SCROLL_STEP, mouse.column, mouse.row),
            MouseEventKind::ScrollDown => self.scroll_mouse(-SCROLL_STEP, mouse.column, mouse.row),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((index, _)) = self.island_path_edit {
                    let on_input_row = self.island_path_input_hit(index, mouse.column, mouse.row);
                    let on_close = self.island_path_close_hit(index, mouse.column, mouse.row);
                    if on_close || !on_input_row {
                        self.island_path_edit = None;
                    }
                    return Ok(());
                }
                if self.panel_input.is_some() {
                    if !self.panel_input_row_hit(mouse.column, mouse.row) {
                        self.panel_input = None;
                    }
                    return Ok(());
                }
                let was_armed = self.armed_record.take();
                if let Some((index, hit)) = self.island_hit(mouse.column, mouse.row) {
                    match hit {
                        IslandHit::Remove => self.remove_island(index)?,
                        IslandHit::Toggle => {
                            self.clear_all_selections();
                            self.focus = Focus::Island(index);
                            self.toggle_island_follow(index);
                        }
                        IslandHit::Path => {
                            self.clear_all_selections();
                            self.focus = Focus::Island(index);
                            let text = self.islands[index].custom_path.clone().unwrap_or_default();
                            self.island_path_edit = Some((index, text));
                        }
                        IslandHit::Body => {
                            self.clear_all_selections();
                            self.focus = Focus::Island(index);
                            if let Some(pos) = self.island_cell_at(index, mouse.column, mouse.row) {
                                self.islands[index].selection = Some(Selection {
                                    start: pos,
                                    end: pos,
                                });
                            }
                        }
                    }
                } else if self.add_button_hit(mouse.column, mouse.row) {
                    self.add_island()?;
                } else if self.jump_button_hit(mouse.column, mouse.row) {
                    self.reset_scroll();
                } else if ui::paged_add_hit(self.paged_panel, mouse.column, mouse.row) {
                    self.panel_input = Some(String::new());
                } else if let Some(page) =
                    ui::paged_tab_hit(self.paged_panel, mouse.column, mouse.row)
                {
                    self.left_page = page;
                    self.armed_record = None;
                    self.panel_offset = 0;
                } else if let Some((index, on_arrow)) = ui::paged_record_hit(
                    self.paged_panel,
                    mouse.column,
                    mouse.row,
                    self.paged_records_len(),
                    self.clamped_panel_offset(),
                    self.panel_input.is_some(),
                ) {
                    if on_arrow && was_armed == Some(index) {
                        self.delete_panel_record(index);
                    } else {
                        self.armed_record = Some(index);
                        self.execute_panel_record(index)?;
                    }
                } else if let Some(pos) = self.dir_cell_at(mouse.column, mouse.row) {
                    self.clear_all_selections();
                    self.dir_selection = Some(DirSelection::Rect(Selection {
                        start: pos,
                        end: pos,
                    }));
                } else if let Some(pos) = self.term_cell_at(mouse.column, mouse.row) {
                    self.clear_all_selections();
                    self.focus = Focus::Terminal;
                    self.selection = Some(Selection {
                        start: pos,
                        end: pos,
                    });
                } else {
                    self.clear_all_selections();
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.selection.is_some() {
                    if let Some(pos) = self.term_cell_at(mouse.column, mouse.row)
                        && let Some(sel) = &mut self.selection
                    {
                        sel.end = pos;
                    }
                } else if let Some(index) = self.islands.iter().position(|i| i.selection.is_some())
                    && let Some(pos) = self.island_cell_at(index, mouse.column, mouse.row)
                    && let Some(sel) = &mut self.islands[index].selection
                {
                    sel.end = pos;
                } else if let Some(pos) = self.dir_cell_at(mouse.column, mouse.row)
                    && let Some(DirSelection::Rect(sel)) = &mut self.dir_selection
                {
                    sel.end = pos;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(sel) = self.selection
                    && sel.start == sel.end
                {
                    self.selection = None;
                }
                let dir_click = match self.dir_selection {
                    Some(DirSelection::Rect(sel)) if sel.start == sel.end => Some(sel.start),
                    _ => None,
                };
                if let Some(anchor) = dir_click {
                    self.dir_selection = self.pick_dir_selection(anchor);
                }
                for island in &mut self.islands {
                    if let Some(sel) = &mut island.selection
                        && sel.start == sel.end
                    {
                        island.selection = None;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn scroll_mouse(&mut self, delta: i32, column: u16, row: u16) {
        if let Some((index, _)) = self.island_hit(column, row) {
            self.islands[index].scroll_by(delta);
        } else if self.in_dir_pane(column, row) {
            let before = self.dir.offset;
            self.dir.scroll_by(-delta);
            if self.dir.offset != before
                && matches!(self.dir_selection, Some(DirSelection::Rect(_)))
            {
                self.dir_selection = None;
            }
        } else if self.in_paged_pane(column, row) {
            let total = self.paged_records_len() as i32;
            self.panel_offset = (self.panel_offset as i32 - delta).clamp(0, total) as u16;
        } else {
            self.scroll_by(delta);
        }
    }

    fn in_dir_pane(&self, column: u16, row: u16) -> bool {
        let r = self.dir_pane_rect;
        column >= r.x && column < r.x + r.width && row >= r.y && row < r.y + r.height
    }

    fn in_paged_pane(&self, column: u16, row: u16) -> bool {
        let r = self.paged_panel;
        column >= r.x && column < r.x + r.width && row >= r.y && row < r.y + r.height
    }

    fn clamped_panel_offset(&self) -> u16 {
        let len = self.paged_records_len();
        let view = ui::paged_records_view(self.paged_panel, self.panel_input.is_some(), len);
        ui::clamp_record_offset(self.panel_offset, len, view.visible())
    }

    fn clear_all_selections(&mut self) {
        self.selection = None;
        self.dir_selection = None;
        for island in &mut self.islands {
            island.selection = None;
        }
    }

    fn dir_cell_at(&self, column: u16, row: u16) -> Option<(u16, u16)> {
        let inner = ui::bordered_inner(self.dir_pane_rect);
        if column >= inner.x
            && column < inner.x + inner.width
            && row >= inner.y
            && row < inner.y + inner.height
        {
            Some((row - inner.y, column - inner.x))
        } else {
            None
        }
    }

    fn pick_dir_selection(&mut self, anchor: (u16, u16)) -> Option<DirSelection> {
        let (row, _col) = anchor;
        let inner = ui::bordered_inner(self.dir_pane_rect);
        if row == 0 {
            let path = self.dir.path.to_string_lossy().into_owned();
            if ui::display_width(&path) > inner.width {
                self.notify(path, NoticeKind::Highlight);
            }
            return Some(DirSelection::Path);
        }
        if row == 1 {
            return None;
        }
        let visible = inner.height.saturating_sub(2);
        let offset = self.dir.clamped_offset(visible);
        let index = offset as usize + (row as usize - 2);
        let entry = self.dir.entries.get(index)?;
        let name = entry.name.clone();
        let is_dir = entry.is_dir;
        let show_bar = self.dir.total() > visible;
        let text_width = inner.width - u16::from(show_bar);
        if ui::display_width(&name) + u16::from(is_dir) > text_width {
            let mut text = name.clone();
            if is_dir {
                text.push('/');
            }
            self.notify(text, NoticeKind::Highlight);
        }
        Some(DirSelection::Entry { name, is_dir })
    }

    fn island_cell_at(&self, index: usize, column: u16, row: u16) -> Option<(u16, u16)> {
        let rect = self.island_areas.get(index)?;
        let output = ui::island_output_inner(*rect);
        if column >= output.x
            && column < output.x + output.width
            && row >= output.y
            && row < output.y + output.height
        {
            Some((row - output.y, column - output.x))
        } else {
            None
        }
    }

    fn island_path_input_hit(&self, index: usize, column: u16, row: u16) -> bool {
        self.island_areas.get(index).is_some_and(|&area| {
            let r = ui::island_path_input_row(area);
            column >= r.x && column < r.x + r.width && row >= r.y && row < r.y + r.height
        })
    }

    fn island_path_close_hit(&self, index: usize, column: u16, row: u16) -> bool {
        self.island_areas.get(index).is_some_and(|&area| {
            let r = ui::island_path_close_zone(area);
            column >= r.x && column < r.x + r.width && row >= r.y && row < r.y + r.height
        })
    }

    fn island_hit(&self, column: u16, row: u16) -> Option<(usize, IslandHit)> {
        for (index, area) in self.island_areas.iter().enumerate() {
            if column >= area.x
                && column < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
            {
                let hit = if row == area.y && column + 3 >= area.x + area.width {
                    IslandHit::Remove
                } else if row == area.y && column + 11 >= area.x + area.width {
                    IslandHit::Toggle
                } else if row == area.y && column + 21 >= area.x + area.width {
                    IslandHit::Path
                } else {
                    IslandHit::Body
                };
                return Some((index, hit));
            }
        }
        None
    }

    fn add_button_hit(&self, column: u16, row: u16) -> bool {
        if row != self.add_bar.y {
            return false;
        }
        let (x, w) = ui::add_button_zone(self.add_bar);
        column >= x && column < x + w
    }

    fn toggle_island_follow(&mut self, index: usize) {
        if let Some(island) = self.islands.get_mut(index) {
            island.toggle_follow();
        }
    }

    fn jump_button_hit(&self, column: u16, row: u16) -> bool {
        if self.scroll == 0 {
            return false;
        }
        let pane = self.pane_inner;
        let corner = pane.x + pane.width;
        let border_row = pane.y.saturating_sub(1);
        row == border_row && column + 8 >= corner && column <= corner
    }

    fn term_cell_at(&self, column: u16, row: u16) -> Option<(u16, u16)> {
        let pane = self.pane_inner;
        if column >= pane.x
            && column < pane.x + pane.width
            && row >= pane.y
            && row < pane.y + pane.height
        {
            Some((row - pane.y, column - pane.x))
        } else {
            None
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let screen = self.term.screen_mut();
        let before = screen.scrollback();
        let target = (before as i32 + delta).max(0) as usize;
        screen.set_scrollback(target);
        let after = screen.scrollback();
        self.scroll = after as u16;
        if after != before {
            self.selection = None;
        }
    }

    fn reset_scroll(&mut self) {
        if self.scroll > 0 {
            self.selection = None;
        }
        self.term.screen_mut().set_scrollback(0);
        self.scroll = 0;
    }
}

fn selection_text(term: &vt100::Parser, sel: &Selection) -> String {
    let ((r1, c1), (r2, c2)) = sel.normalized();
    let screen = term.screen();
    let (_, cols) = screen.size();
    let mut out = String::new();
    for row in r1..=r2 {
        let from = if row == r1 { c1 } else { 0 };
        let to = if row == r2 {
            c2.saturating_add(1)
        } else {
            cols
        };
        let mut line = String::new();
        for col in from..to.min(cols) {
            if let Some(cell) = screen.cell(row, col)
                && !cell.is_wide_continuation()
            {
                line.push_str(cell.contents());
            }
        }
        out.push_str(line.trim_end());
        if row != r2 {
            out.push_str(LINE_SEP);
        }
    }
    out
}

fn dir_mtime(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn terminal_line_before_cursor(term: &vt100::Parser) -> String {
    let screen = term.screen();
    let (row, col) = screen.cursor_position();
    screen
        .contents_between(0, 0, row, col)
        .rsplit('\n')
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn expand_tilde(text: &str) -> PathBuf {
    let Some(rest) = text.strip_prefix('~') else {
        return PathBuf::from(text);
    };
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match home {
        Some(home) => PathBuf::from(home).join(rest.trim_start_matches('/')),
        None => PathBuf::from(text),
    }
}

fn terminal_clear_line_bytes(term: &vt100::Parser) -> Vec<u8> {
    let left = terminal_line_before_cursor(term).chars().count();
    let screen = term.screen();
    let (row, col) = screen.cursor_position();
    let (_, cols) = screen.size();
    let mut right = 0usize;
    for c in col..cols {
        if let Some(cell) = screen.cell(row, c)
            && !cell.is_wide_continuation()
            && !cell.contents().is_empty()
        {
            right += 1;
        }
    }
    let mut bytes = vec![0x7f; left];
    for _ in 0..right {
        bytes.extend_from_slice(b"\x1b[3~");
    }
    bytes
}

fn strip_prompt(line: &str) -> String {
    let markers = [">", "$ ", "# ", "% "];
    let mut cut = None;
    for marker in markers {
        if let Some(pos) = line.find(marker) {
            let end = pos + marker.len();
            cut = Some(cut.map_or(end, |c: usize| c.min(end)));
        }
    }
    let cut = cut.unwrap_or(0);
    line[cut..].trim().to_owned()
}

fn char_index_at(line: &str, col: u16) -> usize {
    let mut used = 0u16;
    for (idx, ch) in line.chars().enumerate() {
        if used >= col {
            return idx;
        }
        used += if ch.is_ascii() { 1 } else { 2 };
    }
    line.chars().count()
}

fn lines_selection_text(lines: &[String], sel: &Selection) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let ((r1, c1), (r2, c2)) = sel.normalized();
    let last = (lines.len() - 1) as u16;
    let r2 = r2.min(last);
    let mut out = String::new();
    for row in r1..=r2 {
        if row > last {
            break;
        }
        let line = &lines[row as usize];
        let start = if row == r1 {
            char_index_at(line, c1)
        } else {
            0
        };
        let end = if row == r2 {
            char_index_at(line, c2.saturating_add(1))
        } else {
            line.chars().count()
        };
        let slice: String = line
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect();
        out.push_str(slice.trim_end());
        if row != r2 {
            out.push_str(LINE_SEP);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_contains_and_normalizes() {
        let sel = Selection {
            start: (2, 5),
            end: (0, 1),
        };
        assert_eq!(sel.normalized(), ((0, 1), (2, 5)));
        assert!(!sel.contains(0, 0));
        assert!(sel.contains(0, 1));
        assert!(sel.contains(1, 9));
        assert!(sel.contains(2, 3));
        assert!(!sel.contains(2, 6));

        let single = Selection {
            start: (1, 2),
            end: (1, 4),
        };
        assert!(single.contains(1, 3));
        assert!(!single.contains(1, 5));
    }

    #[test]
    fn extracts_multiline_selection() {
        let mut term = vt100::Parser::new(5, 10, 0);
        term.process(b"hello\r\nfoo bar\r\nbaz");
        let sel = Selection {
            start: (0, 1),
            end: (1, 3),
        };
        assert_eq!(selection_text(&term, &sel), format!("ello{LINE_SEP}foo"));

        let whole = Selection {
            start: (0, 0),
            end: (2, 2),
        };
        assert_eq!(
            selection_text(&term, &whole),
            format!("hello{LINE_SEP}foo bar{LINE_SEP}baz")
        );
    }

    #[test]
    fn scroll_tracks_scrollback() {
        let mut app = App::new(Config::default()).expect("app");
        for i in 0..200 {
            let line = format!("line {i}\r\n");
            app.term.process(line.as_bytes());
        }
        app.scroll_by(10);
        assert_eq!(app.scroll(), 10);
        app.scroll_by(-4);
        assert_eq!(app.scroll(), 6);
        app.scroll_by(100_000);
        assert!(app.scroll() > 100, "scrollback clamp failed");
        app.reset_scroll();
        assert_eq!(app.scroll(), 0);
    }

    fn press_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn app_with_islands() -> App {
        let mut app = App::new(Config::default()).expect("app");
        app.islands.push(crate::island::IslandState::empty(10, 36));
        app.island_areas = vec![Rect::new(50, 0, 36, 15), Rect::new(50, 15, 36, 15)];
        app
    }

    #[test]
    fn island_path_badge_opens_editor_and_confirms() {
        let mut app = app_with_islands();
        let dir = std::env::temp_dir();

        click_at(&mut app, 70, 0);
        let (index, text) = app
            .island_path_edit
            .clone()
            .expect("点击路径徽标应打开编辑");
        assert_eq!(index, 0);
        assert_eq!(text, "", "默认状态预填为空");
        assert_eq!(app.focus, Focus::Island(0), "点击徽标应聚焦该岛");

        for c in dir.to_string_lossy().chars() {
            app.handle_key(
                press_key(KeyCode::Char(c), KeyModifiers::NONE),
                &mut Vec::new(),
            )
            .unwrap();
        }
        app.handle_key(
            press_key(KeyCode::Enter, KeyModifiers::NONE),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(app.island_path_edit.is_none(), "确认后编辑应关闭");
        assert_eq!(
            app.islands[0].custom_path.as_deref(),
            Some(dir.to_string_lossy().as_ref()),
            "确认后应保存指定路径"
        );
        let (_, kind) = app.notice_text().expect("确认应有提示");
        assert_eq!(kind, NoticeKind::Info);
        assert_eq!(app.island_working_dir(0), dir, "执行目录应使用指定路径");

        click_at(&mut app, 70, 0);
        let (_, text) = app.island_path_edit.clone().expect("再点徽标应重新编辑");
        assert_eq!(text, dir.to_string_lossy(), "重新编辑应预填当前路径");
    }

    #[test]
    fn island_path_editor_validates_and_cancels() {
        let mut app = app_with_islands();
        let dir = std::env::temp_dir();
        app.islands[0].custom_path = Some(dir.to_string_lossy().into_owned());

        click_at(&mut app, 70, 0);
        for _ in 0..(dir.to_string_lossy().chars().count()) {
            app.handle_key(
                press_key(KeyCode::Backspace, KeyModifiers::NONE),
                &mut Vec::new(),
            )
            .unwrap();
        }
        for c in "relative/path".chars() {
            app.handle_key(
                press_key(KeyCode::Char(c), KeyModifiers::NONE),
                &mut Vec::new(),
            )
            .unwrap();
        }
        app.handle_key(
            press_key(KeyCode::Enter, KeyModifiers::NONE),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(app.island_path_edit.is_some(), "相对路径应留在输入框");
        let (_, kind) = app.notice_text().expect("校验失败应有提示");
        assert_eq!(kind, NoticeKind::Warn);

        app.handle_key(press_key(KeyCode::Esc, KeyModifiers::NONE), &mut Vec::new())
            .unwrap();
        assert!(app.island_path_edit.is_none(), "Esc 应取消编辑");
        assert_eq!(
            app.islands[0].custom_path.as_deref(),
            Some(dir.to_string_lossy().as_ref()),
            "取消编辑应保留原路径"
        );

        click_at(&mut app, 70, 0);
        click_at(&mut app, 5, 5);
        assert!(app.island_path_edit.is_none(), "点击输入框外应取消编辑");
        assert!(
            app.islands[0].custom_path.is_some(),
            "取消编辑不应清除原路径"
        );

        click_at(&mut app, 70, 0);
        click_at(&mut app, 63, 1);
        assert!(app.island_path_edit.is_some(), "点击输入框所在行应保持编辑");

        click_at(&mut app, 83, 1);
        assert!(app.island_path_edit.is_none(), "点击 × 应关闭输入框");
        assert!(app.islands[0].custom_path.is_some(), "× 关闭应保留原路径");

        click_at(&mut app, 70, 0);
        let len = app
            .island_path_edit
            .as_ref()
            .map(|(_, t)| t.chars().count())
            .unwrap_or_default();
        for _ in 0..len {
            app.handle_key(
                press_key(KeyCode::Backspace, KeyModifiers::NONE),
                &mut Vec::new(),
            )
            .unwrap();
        }
        app.handle_key(
            press_key(KeyCode::Enter, KeyModifiers::NONE),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(
            app.islands[0].custom_path.is_none(),
            "空回车应清除指定路径回到默认"
        );
        assert!(app.notice_text().is_some(), "恢复默认应有提示");
    }

    #[test]
    fn island_path_editor_is_modal() {
        let mut app = app_with_islands();
        click_at(&mut app, 70, 0);

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(4), KeyModifiers::NONE), &mut input)
            .unwrap();
        app.handle_key(press_key(KeyCode::F(1), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert!(input.is_empty(), "编辑期间按键不应发给 shell");
        assert_eq!(app.left_page, LeftPage::Jump, "编辑期间 F1 应被吞掉");
        assert!(
            app.config.jump.bookmarks.is_empty(),
            "编辑期间 F4 不应触发收藏"
        );
        assert!(
            app.islands[0].command.is_empty(),
            "编辑期间字符不应进入岛命令"
        );

        app.handle_key(
            press_key(KeyCode::Char('/'), KeyModifiers::NONE),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            app.island_path_edit.as_ref().map(|(_, t)| t.clone()),
            Some("/".to_owned()),
            "字符应进入路径输入框"
        );
    }

    #[test]
    fn expand_tilde_resolves_home() {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap();
        assert_eq!(expand_tilde("~/data"), PathBuf::from(&home).join("data"));
        assert_eq!(expand_tilde("~"), PathBuf::from(&home));
        assert_eq!(expand_tilde("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_tilde("relative"), PathBuf::from("relative"));
    }

    #[test]
    fn f2_cycles_focus_through_islands() {
        let mut app = app_with_islands();
        let mut input = Vec::new();

        assert_eq!(app.focus(), Focus::Terminal);
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Island(0));
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Island(1));
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Terminal);
        assert!(input.is_empty());

        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        app.handle_key(press_key(KeyCode::Tab, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Terminal);
    }

    #[test]
    fn f2_with_single_island_round_trips() {
        let mut app = App::new(Config::default()).expect("app");
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Island(0));
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Terminal);
    }

    #[test]
    fn island_input_executes_command() {
        let mut app = app_with_islands();
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Island(0));

        for ch in "echo island-run-ok".chars() {
            app.handle_key(press_key(KeyCode::Char(ch), KeyModifiers::NONE), &mut input)
                .unwrap();
        }
        assert_eq!(app.islands[0].command, "echo island-run-ok");
        assert!(input.is_empty(), "岛内输入不应进入 shell");

        app.handle_key(
            press_key(KeyCode::Backspace, KeyModifiers::NONE),
            &mut input,
        )
        .unwrap();
        assert_eq!(app.islands[0].command, "echo island-run-o");
        app.handle_key(
            press_key(KeyCode::Char('k'), KeyModifiers::NONE),
            &mut input,
        )
        .unwrap();

        app.handle_key(press_key(KeyCode::Enter, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert!(app.islands[0].session.is_some(), "回车应启动岛进程");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.islands[0].session.is_some() && std::time::Instant::now() < deadline {
            app.islands[0].poll_output();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            app.islands[0]
                .parser
                .screen()
                .contents()
                .contains("island-run-ok"),
            "岛屏幕应包含命令输出"
        );
        assert!(input.is_empty());
    }

    #[test]
    fn terminal_focus_sends_tab_to_shell() {
        let mut app = App::new(Config::default()).expect("app");
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::Tab, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Terminal);
        assert_eq!(input, b"\t".to_vec());
    }

    #[test]
    fn clicking_island_focuses_and_toggles() {
        let mut app = app_with_islands();
        app.island_areas = vec![Rect::new(50, 0, 36, 12), Rect::new(50, 12, 36, 12)];

        let body_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 60,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(body_click).unwrap();
        assert_eq!(app.focus(), Focus::Island(0));
        assert!(!app.islands[0].follow, "点击岛体不应拨动开关");

        let toggle_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 78,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(toggle_click).unwrap();
        assert_eq!(app.focus(), Focus::Island(0));
        assert!(app.islands[0].follow);
        assert!(!app.islands[1].follow, "只拨动被点击的岛");

        app.handle_mouse(toggle_click).unwrap();
        assert!(!app.islands[0].follow, "再次点击应拨回");
    }

    #[test]
    fn close_button_removes_island() {
        let mut app = app_with_islands();
        app.island_areas = vec![Rect::new(50, 0, 36, 15), Rect::new(50, 15, 36, 15)];
        let close_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 85,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(close_click).unwrap();
        assert_eq!(app.islands.len(), 1, "× 应删除岛");
        assert_eq!(app.focus(), Focus::Terminal, "非聚焦删除后焦点不变");
    }

    #[test]
    fn add_button_click_adds_island() {
        let mut app = app_with_islands();
        app.refresh_layout(app.layout_area).unwrap();

        let off_button = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: app.add_bar.x,
            row: app.add_bar.y,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(off_button).unwrap();
        assert_eq!(app.islands.len(), 2, "按钮外的提示栏区域不应新增");

        let (bx, _) = ui::add_button_zone(app.add_bar);
        let on_button = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: bx + 1,
            row: app.add_bar.y,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(on_button).unwrap();
        assert_eq!(app.islands.len(), 3, "点击 + 按钮应新增岛");
        assert_eq!(app.focus(), Focus::Island(2), "新岛应被聚焦");

        app.handle_mouse(on_button).unwrap();
        assert_eq!(app.islands.len(), 3, "达上限后点击不再新增");
        assert!(
            app.notice_text()
                .is_some_and(|(t, kind)| t.contains("上限") && kind == NoticeKind::Warn)
        );
    }

    #[test]
    fn clicking_terminal_returns_focus() {
        let mut app = app_with_islands();
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Island(0));

        app.island_areas = Vec::new();
        app.pane_inner = Rect::new(10, 1, 40, 20);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 30,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(click).unwrap();
        assert_eq!(app.focus(), Focus::Terminal);
        assert!(app.selection.is_some());
    }

    #[test]
    fn island_focus_navigates_with_arrows() {
        let mut app = app_with_islands();
        let mut input = Vec::new();

        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Island(0));

        app.handle_key(press_key(KeyCode::Up, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Island(0), "顶部应钳位");

        app.handle_key(press_key(KeyCode::Down, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Island(1));
        app.handle_key(press_key(KeyCode::Down, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Island(1), "底部应钳位");

        app.handle_key(press_key(KeyCode::Up, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Island(0));

        app.handle_key(
            press_key(KeyCode::Char('a'), KeyModifiers::NONE),
            &mut input,
        )
        .unwrap();
        assert!(input.is_empty(), "岛聚焦时按键不应进入 shell");

        app.handle_key(press_key(KeyCode::Esc, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Terminal);
    }

    #[test]
    fn clearing_command_resets_island() {
        let mut app = app_with_islands();
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        for ch in "echo island-reset-marker".chars() {
            app.handle_key(press_key(KeyCode::Char(ch), KeyModifiers::NONE), &mut input)
                .unwrap();
        }
        app.handle_key(press_key(KeyCode::Enter, KeyModifiers::NONE), &mut input)
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.islands[0].session.is_some() && std::time::Instant::now() < deadline {
            app.islands[0].poll_output();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            app.islands[0]
                .parser
                .screen()
                .contents()
                .contains("island-reset-marker")
        );

        for _ in 0.."echo island-reset-marker".len() {
            app.handle_key(
                press_key(KeyCode::Backspace, KeyModifiers::NONE),
                &mut input,
            )
            .unwrap();
        }
        assert!(app.islands[0].command.is_empty());
        assert!(app.islands[0].session.is_none());

        app.handle_key(
            press_key(KeyCode::Char('a'), KeyModifiers::NONE),
            &mut input,
        )
        .unwrap();
        assert!(
            !app.islands[0]
                .parser
                .screen()
                .contents()
                .contains("island-reset-marker")
        );
        assert_eq!(app.islands[0].command, "a");
    }

    #[test]
    fn wheel_over_island_scrolls_it() {
        let mut app = app_with_islands();
        for i in 0..100 {
            app.islands[0]
                .parser
                .process(format!("line{i}\r\n").as_bytes());
            app.term.process(format!("tline{i}\r\n").as_bytes());
        }
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 60,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(wheel).unwrap();
        assert_eq!(app.islands[0].scroll, 3);
        assert_eq!(app.scroll(), 0, "主终端不应被岛内滚动影响");

        let outside = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 30,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(outside).unwrap();
        assert!(app.scroll() > 0, "岛外滚动应作用于主终端");
    }

    #[test]
    fn island_mouse_selection_and_drag() {
        let mut app = app_with_islands();
        app.islands[0].parser.process(b"hello island");
        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 52,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(down).unwrap();
        assert_eq!(app.focus(), Focus::Island(0));
        assert!(app.islands[0].selection.is_some());

        let drag = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 55,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(drag).unwrap();
        let sel = app.islands[0].selection.expect("拖动后选区应存在");
        assert_eq!(sel.normalized(), ((1, 1), (1, 4)));
    }

    #[test]
    fn follow_island_runs_command() {
        let mut app = app_with_islands();
        app.islands[0].follow = true;
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        for ch in "echo live-island-ok".chars() {
            app.handle_key(press_key(KeyCode::Char(ch), KeyModifiers::NONE), &mut input)
                .unwrap();
        }
        app.handle_key(press_key(KeyCode::Enter, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert!(app.islands[0].session.is_some(), "岛应通过 PTY 执行");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.islands[0].session.is_some() && std::time::Instant::now() < deadline {
            app.islands[0].poll_output();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            app.islands[0]
                .parser
                .screen()
                .contents()
                .contains("live-island-ok")
        );
        assert!(app.islands[0].armed);
    }

    #[test]
    fn terminal_activity_triggers_armed_follow_islands() {
        let mut app = app_with_islands();
        app.islands[0].follow = true;
        app.islands[0].armed = true;
        app.islands[0].command = "echo trigger-test".to_owned();
        app.islands[1].follow = true;
        app.islands[1].armed = false;
        app.islands[1].command = "echo unarmed".to_owned();

        app.trigger_pending = true;
        app.last_output_at = Instant::now() - Duration::from_millis(400);
        app.maybe_trigger_live();
        assert!(!app.trigger_pending, "触发后应清除待触发标记");
        assert!(app.islands[0].session.is_some(), "已武装的跟随岛应重新执行");
        assert!(
            app.islands[1].session.is_none(),
            "未武装（未回车执行过）的岛不应被触发"
        );

        app.trigger_pending = true;
        app.last_output_at = Instant::now();
        app.maybe_trigger_live();
        assert!(app.trigger_pending, "输出未安静时不应触发");
    }

    #[test]
    fn enter_in_terminal_arms_trigger() {
        let mut app = app_with_islands();
        app.send_terminal_input(b"dir\r").unwrap();
        assert!(app.trigger_pending, "含回车的输入应标记待触发");
        app.trigger_pending = false;
        app.send_terminal_input(b"abc").unwrap();
        assert!(!app.trigger_pending, "无回车的输入不应标记");
    }

    #[test]
    fn mode_toggle_preserves_island_state() {
        let mut app = app_with_islands();
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        for ch in "ping -t 127.0.0.1".chars() {
            app.handle_key(press_key(KeyCode::Char(ch), KeyModifiers::NONE), &mut input)
                .unwrap();
        }
        app.handle_key(press_key(KeyCode::Enter, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert!(app.islands[0].session.is_some());
        app.islands[0].poll_output();
        assert!(
            app.islands[0]
                .session
                .as_ref()
                .is_some_and(|s| s.is_alive()),
            "ping 应持续运行"
        );

        assert!(!app.islands[0].follow);
        app.toggle_island_follow(0);
        assert!(app.islands[0].follow, "拨到跟随");
        assert!(app.islands[0].session.is_some(), "拨动不应杀进程");
        assert_eq!(app.islands[0].command, "ping -t 127.0.0.1");
        assert!(app.islands[0].armed, "拨动不应解除武装");

        app.toggle_island_follow(0);
        assert!(!app.islands[0].follow, "拨回单次");
        assert!(app.islands[0].session.is_some());
    }

    #[test]
    fn f3_adds_island_and_focuses_it() {
        let mut app = App::new(Config::default()).expect("app");
        assert_eq!(app.islands.len(), 1);
        assert_eq!(app.island_areas.len(), 1);

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(3), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.islands.len(), 2, "F3 应新增岛");
        assert_eq!(app.island_areas.len(), 2, "岛区域应同步重排");
        assert_eq!(app.focus(), Focus::Island(1), "新岛应被聚焦");

        let output = ui::island_output_inner(app.island_areas[1]);
        let (rows, cols) = app.islands[1].parser.screen().size();
        assert_eq!(
            (rows, cols),
            (output.height, output.width),
            "新岛尺寸应匹配区域"
        );
        let first = ui::island_output_inner(app.island_areas[0]);
        let (r0, c0) = app.islands[0].parser.screen().size();
        assert_eq!((r0, c0), (first.height, first.width), "现有岛应随重排缩放");

        app.handle_key(press_key(KeyCode::F(3), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.islands.len(), 3);
        app.handle_key(press_key(KeyCode::F(3), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.islands.len(), 3, "达到上限后不应再增");
        assert!(
            app.notice_text()
                .is_some_and(|(t, kind)| t.contains("上限") && kind == NoticeKind::Warn),
            "达到上限应提示"
        );
    }

    #[test]
    fn del_removes_focused_island() {
        let mut app = app_with_islands();
        app.islands[0].command = "ping -t 127.0.0.1".to_owned();
        let cwd = app.island_cwd();
        app.islands[0].execute(&cwd);
        assert!(app.islands[0].session.is_some(), "岛内应有运行中进程");

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.focus(), Focus::Island(0));
        app.handle_key(press_key(KeyCode::Delete, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.islands.len(), 1, "应删除聚焦岛");
        assert_eq!(app.focus(), Focus::Terminal, "删除聚焦岛后应回中栏");
        assert_eq!(app.island_areas.len(), 1);
        assert!(input.is_empty(), "岛聚焦时 Del 不应进 shell");
    }

    #[test]
    fn remove_island_shifts_focus_index() {
        let mut app = App::new(Config::default()).expect("app");
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(3), KeyModifiers::NONE), &mut input)
            .unwrap();
        app.handle_key(press_key(KeyCode::F(3), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.islands.len(), 3);

        app.focus = Focus::Island(2);
        app.remove_island(1).unwrap();
        assert_eq!(app.islands.len(), 2);
        assert_eq!(app.focus(), Focus::Island(1), "后方岛焦点索引应左移");
        assert_eq!(app.island_areas.len(), 2);
    }

    #[test]
    fn remove_last_island_then_readd() {
        let mut app = App::new(Config::default()).expect("app");
        app.remove_island(0).unwrap();
        assert!(app.islands.is_empty());
        assert_eq!(app.focus(), Focus::Terminal);

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(3), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.islands.len(), 1);
        assert_eq!(app.focus(), Focus::Island(0));
    }

    #[test]
    fn del_in_terminal_goes_to_shell() {
        let mut app = App::new(Config::default()).expect("app");
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::Delete, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(input, b"\x1b[3~".to_vec(), "终端聚焦时 Del 应发给 shell");
    }

    #[test]
    fn empty_islands_narrow_right_pane() {
        let mut app = App::new(Config::default()).expect("app");
        let wide = app.pane_inner.width;
        assert!(wide > 0);

        app.remove_island(0).unwrap();
        assert!(app.islands.is_empty());
        let narrow_pane = app.pane_inner.width;
        assert!(
            narrow_pane > wide,
            "删光岛后终端应变宽：{wide} → {narrow_pane}"
        );
        let expected = wide + (Config::default().ui.right_width - ui::EMPTY_RIGHT_WIDTH);
        assert_eq!(narrow_pane, expected);
        assert_eq!(app.pty_size.1, narrow_pane, "PTY 应同步新宽度");

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(3), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.pane_inner.width, wide, "加岛后终端应恢复原宽");
    }

    #[test]
    fn shrink_with_wide_char_at_boundary_no_panic() {
        let mut parser = vt100::Parser::new(2, 10, 0);
        parser.process("     \u{4e2d}".as_bytes());
        assert!(
            parser.screen().cell(0, 5).unwrap().is_wide(),
            "宽字符应占 5-6 列"
        );

        crate::island::sanitize_resize_boundary(&mut parser, 6);
        parser.screen_mut().set_size(2, 6);
        parser.process(b"xabcdef");
        assert!(
            !parser.screen().cell(0, 5).unwrap().is_wide(),
            "新边界列的悬空宽字符应已被空格覆盖"
        );
    }

    #[test]
    fn sanitize_skips_when_not_shrinking() {
        let mut parser = vt100::Parser::new(2, 10, 0);
        parser.process("     \u{4e2d}".as_bytes());
        let before = parser.screen().contents();
        crate::island::sanitize_resize_boundary(&mut parser, 10);
        crate::island::sanitize_resize_boundary(&mut parser, 12);
        assert_eq!(parser.screen().contents(), before, "不收窄时净化应为无操作");
    }

    #[test]
    fn wheel_over_dir_pane_scrolls_it() {
        let mut app = App::new(Config::default()).expect("app");
        app.dir.entries = (0..100)
            .map(|i| crate::dirpane::Entry {
                name: format!("f{i}"),
                is_dir: false,
                hidden: false,
            })
            .collect();
        app.dir_pane_rect = Rect::new(0, 0, 22, 15);
        for i in 0..100 {
            app.term.process(format!("tline{i}\r\n").as_bytes());
        }

        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(wheel).unwrap();
        assert_eq!(app.dir.offset, 0, "已在顶部时滚轮向上不应下移列表");

        let wheel_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(wheel_down).unwrap();
        assert!(app.dir.offset > 0, "滚轮向下应向列表后方滚动");
        assert_eq!(app.scroll(), 0, "主终端不应被影响");

        let outside = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 30,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(outside).unwrap();
        assert!(app.scroll() > 0, "目录栏外滚动应作用于主终端");
    }

    #[cfg(target_os = "linux")]
    fn align_shell_cwd(app: &mut App, dir: &std::path::Path) {
        app.send_terminal_input(format!("cd {}\r", dir.to_string_lossy()).as_bytes())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.dir.path != dir && Instant::now() < deadline {
            let _ = app.pty.poll_output();
            app.last_dir_poll = Instant::now() - DIR_POLL_INTERVAL;
            app.poll_dir_pane();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(app.dir.path, dir, "shell 应已 cd 到测试目录");
    }

    #[test]
    fn dir_pane_refreshes_on_mtime_change() {
        let base = std::env::temp_dir().join(format!("termiland-mtime-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut app = App::new(Config::default()).expect("app");
        #[cfg(target_os = "linux")]
        align_shell_cwd(&mut app, &base);
        app.dir = crate::dirpane::DirPane::load(base.clone());
        app.dir_mtime = None;

        app.last_dir_poll = Instant::now() - DIR_POLL_INTERVAL;
        app.poll_dir_pane();
        assert!(
            !app.dir.entries.iter().any(|e| e.name == "new_file.txt"),
            "初始目录为空"
        );

        std::fs::File::create(base.join("new_file.txt")).unwrap();
        app.last_dir_poll = Instant::now() - DIR_POLL_INTERVAL;
        app.poll_dir_pane();
        assert!(
            app.dir.entries.iter().any(|e| e.name == "new_file.txt"),
            "目录内容变化后应自动刷新"
        );

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dir_pane_follows_shell_cwd() {
        let mut app = App::new(Config::default()).expect("app");

        app.send_terminal_input(b"cd /tmp\r").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !app.dir.path.ends_with("tmp") && Instant::now() < deadline {
            let _ = app.pty.poll_output();
            app.last_dir_poll = Instant::now() - DIR_POLL_INTERVAL;
            app.poll_dir_pane();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            app.dir.path.ends_with("tmp"),
            "目录栏应跟随 shell 的 cwd: {:?}",
            app.dir.path
        );
    }

    #[test]
    fn dir_pane_drag_select_and_ctrl_c_copies() {
        let mut app = App::new(Config::default()).expect("app");
        app.dir_pane_rect = Rect::new(0, 0, 22, 20);
        app.dir.entries = vec![
            crate::dirpane::Entry {
                name: "alpha".to_owned(),
                is_dir: true,
                hidden: false,
            },
            crate::dirpane::Entry {
                name: "beta.txt".to_owned(),
                is_dir: false,
                hidden: false,
            },
        ];

        let down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(down).unwrap();
        assert!(app.dir_selection.is_some(), "按下应锚定目录栏选区");

        let drag = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 8,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(drag).unwrap();
        let Some(DirSelection::Rect(sel)) = &app.dir_selection else {
            panic!("拖选后应为矩形选区");
        };
        assert_eq!(sel.normalized(), ((2, 1), (2, 7)));

        let mut input = Vec::new();
        app.handle_key(
            press_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut input,
        )
        .unwrap();
        assert!(input.is_empty(), "有选区时 Ctrl+C 不应发 SIGINT");
    }

    fn click_at(app: &mut App, column: u16, row: u16) {
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.handle_mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
            .unwrap();
        }
    }

    #[test]
    fn click_entry_selects_and_copies_full_name() {
        let mut app = App::new(Config::default()).expect("app");
        app.dir_pane_rect = Rect::new(0, 0, 22, 20);
        let long_name = "very_long_file_name_that_overflows_the_pane_width.txt";
        app.dir.entries = vec![
            crate::dirpane::Entry {
                name: "src".to_owned(),
                is_dir: true,
                hidden: false,
            },
            crate::dirpane::Entry {
                name: long_name.to_owned(),
                is_dir: false,
                hidden: false,
            },
        ];

        click_at(&mut app, 2, 4);

        let Some(DirSelection::Entry { name, is_dir }) = &app.dir_selection else {
            panic!("点击条目后应为整行选区");
        };
        assert_eq!(name, long_name);
        assert!(!*is_dir);

        let text = app.dir_selection_text(app.dir_selection.as_ref().unwrap());
        assert_eq!(text, long_name, "复制应取完整文件名而非截断显示");

        let (notice, kind) = app.notice_text().expect("截断时应闪现完整名");
        assert_eq!(notice, long_name);
        assert_eq!(kind, NoticeKind::Highlight);
    }

    #[test]
    fn click_dir_entry_copies_name_with_slash() {
        let mut app = App::new(Config::default()).expect("app");
        app.dir_pane_rect = Rect::new(0, 0, 22, 20);
        app.dir.entries = vec![crate::dirpane::Entry {
            name: "src".to_owned(),
            is_dir: true,
            hidden: false,
        }];

        click_at(&mut app, 2, 3);

        let text = app.dir_selection_text(app.dir_selection.as_ref().expect("应选中条目"));
        assert_eq!(text, "src/");
    }

    #[test]
    fn click_path_row_selects_full_path() {
        let mut app = App::new(Config::default()).expect("app");
        app.dir_pane_rect = Rect::new(0, 0, 22, 20);
        let long_path = "C:\\very\\long\\directory\\path\\that\\cannot\\fit\\in\\pane";
        app.dir.path = PathBuf::from(long_path);
        app.dir.entries = Vec::new();

        click_at(&mut app, 2, 1);

        assert!(matches!(app.dir_selection, Some(DirSelection::Path)));
        let text = app.dir_selection_text(app.dir_selection.as_ref().unwrap());
        assert_eq!(text, long_path);

        let (notice, kind) = app.notice_text().expect("路径截断时应闪现完整路径");
        assert_eq!(notice, long_path);
        assert_eq!(kind, NoticeKind::Highlight);
    }

    #[test]
    fn click_blank_separator_and_empty_rows_clear_selection() {
        let mut app = App::new(Config::default()).expect("app");
        app.dir_pane_rect = Rect::new(0, 0, 22, 20);
        app.dir.entries = vec![crate::dirpane::Entry {
            name: "only".to_owned(),
            is_dir: false,
            hidden: false,
        }];

        click_at(&mut app, 2, 2);
        assert!(app.dir_selection.is_none(), "点击空白分隔行应无选区");

        click_at(&mut app, 2, 5);
        assert!(app.dir_selection.is_none(), "点击条目之外空行应无选区");
    }

    #[test]
    fn dir_scroll_keeps_entry_and_clears_rect_selection() {
        let mut app = App::new(Config::default()).expect("app");
        app.dir_pane_rect = Rect::new(0, 0, 22, 10);
        app.dir.entries = (0..50)
            .map(|i| crate::dirpane::Entry {
                name: format!("f{i}"),
                is_dir: false,
                hidden: false,
            })
            .collect();

        app.dir_selection = Some(DirSelection::Entry {
            name: "f0".to_owned(),
            is_dir: false,
        });
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(wheel).unwrap();
        assert!(app.dir.offset > 0, "滚轮向下应向列表后方滚动");
        assert!(
            matches!(app.dir_selection, Some(DirSelection::Entry { .. })),
            "滚动后条目选区应保留"
        );

        app.dir.offset = 0;
        app.dir_selection = Some(DirSelection::Rect(Selection {
            start: (2, 0),
            end: (2, 3),
        }));
        app.handle_mouse(wheel).unwrap();
        assert!(
            app.dir_selection.is_none(),
            "滚动后拖选矩形应清除，避免错位复制"
        );
    }

    #[test]
    fn dir_reload_prunes_stale_selections() {
        let base = std::env::temp_dir().join(format!("termiland-prune-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut app = App::new(Config::default()).expect("app");
        #[cfg(target_os = "linux")]
        align_shell_cwd(&mut app, &base);
        app.dir = crate::dirpane::DirPane::load(base.clone());
        app.dir_mtime = None;

        app.dir_selection = Some(DirSelection::Entry {
            name: "gone.txt".to_owned(),
            is_dir: false,
        });
        app.last_dir_poll = Instant::now() - DIR_POLL_INTERVAL;
        app.poll_dir_pane();
        assert!(app.dir_selection.is_none(), "条目不存在时刷新后选区应清除");

        std::fs::File::create(base.join("new_file.txt")).unwrap();
        app.dir_selection = Some(DirSelection::Entry {
            name: "new_file.txt".to_owned(),
            is_dir: false,
        });
        app.dir_mtime = None;
        app.last_dir_poll = Instant::now() - DIR_POLL_INTERVAL;
        app.poll_dir_pane();
        assert!(
            matches!(app.dir_selection, Some(DirSelection::Entry { .. })),
            "条目仍存在时选区应保留"
        );

        app.dir_selection = Some(DirSelection::Rect(Selection {
            start: (2, 0),
            end: (2, 3),
        }));
        std::fs::File::create(base.join("another.txt")).unwrap();
        app.dir_mtime = None;
        app.last_dir_poll = Instant::now() - DIR_POLL_INTERVAL;
        app.poll_dir_pane();
        assert!(app.dir_selection.is_none(), "刷新后拖选矩形应清除");

        app.dir_selection = Some(DirSelection::Path);
        std::fs::File::create(base.join("third.txt")).unwrap();
        app.dir_mtime = None;
        app.last_dir_poll = Instant::now() - DIR_POLL_INTERVAL;
        app.poll_dir_pane();
        assert!(
            matches!(app.dir_selection, Some(DirSelection::Path)),
            "目录未变时路径选区应保留"
        );

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn entry_selection_highlight_follows_scroll() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::style::Modifier;

        let mut app = App::new(Config::default()).expect("app");
        app.dir.entries = (0..100)
            .map(|i| crate::dirpane::Entry {
                name: format!("f{i}"),
                is_dir: false,
                hidden: false,
            })
            .collect();
        app.dir_selection = Some(DirSelection::Entry {
            name: "f0".to_owned(),
            is_dir: false,
        });

        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| ui::draw(f, &app)).unwrap();
        let buf = terminal.backend().buffer();
        let reversed = |buf: &ratatui::buffer::Buffer, x: u16, y: u16| {
            buf[(x, y)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed(buf, 1, 3), "条目 f0 的首列应反白");
        assert!(reversed(buf, 2, 3), "条目 f0 的次列应反白");
        assert!(!reversed(buf, 3, 3), "条目文字之外不应反白");

        app.dir.offset = 10;
        terminal.draw(|f| ui::draw(f, &app)).unwrap();
        let buf = terminal.backend().buffer();
        assert!(!reversed(buf, 1, 3), "f0 滚出视图后该行不应再反白");
    }

    #[test]
    fn click_record_executes_without_enter() {
        let mut app = App::new(Config::default()).expect("app");
        app.left_page = LeftPage::Jump;
        app.paged_panel = Rect::new(0, 0, 22, 12);
        let long = "/very/long/path/that/overflows/the/pane/width/entirely";
        app.config.jump.bookmarks = vec![long.to_owned()];
        app.focus = Focus::Island(0);

        assert_eq!(
            app.panel_record_payload(0).unwrap(),
            format!("cd {long}"),
            "跳转负载应为 cd 命令且不带回车"
        );

        click_at(&mut app, 5, 1);
        assert_eq!(app.focus, Focus::Terminal, "点击执行后焦点应回终端");
        assert!(app.armed_record.is_some(), "点击后应同时进入待删状态");
        assert!(
            app.notice_text().is_none(),
            "点击不再闪现完整内容，输入终端即可见"
        );

        let mut app = App::new(Config::default()).expect("app");
        app.left_page = LeftPage::Commands;
        app.config.commands.items = vec![CommandItem {
            name: None,
            command: "df -h".to_owned(),
        }];
        assert_eq!(app.panel_record_payload(0).unwrap(), "df -h");
    }

    #[test]
    fn wheel_over_paged_panel_scrolls_records() {
        let mut app = App::new(Config::default()).expect("app");
        app.left_page = LeftPage::Jump;
        app.dir_pane_rect = Rect::new(60, 0, 22, 5);
        app.paged_panel = Rect::new(0, 0, 22, 12);
        app.config.jump.bookmarks = (0..50).map(|i| format!("/p{i}")).collect();

        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 5,
            row: 8,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(wheel).unwrap();
        assert_eq!(app.panel_offset, 0, "已在顶部时滚轮向上不应下移列表");

        let wheel_down = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 5,
            row: 8,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(wheel_down).unwrap();
        assert!(app.panel_offset > 0, "滚轮向下应向列表后方滚动");
        assert_eq!(app.dir.offset, 0, "目录栏不应被影响");
        assert_eq!(app.scroll(), 0, "主终端不应被影响");

        app.panel_offset = 10;
        app.handle_key(
            press_key(KeyCode::F(1), KeyModifiers::NONE),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(app.panel_offset, 0, "切页应重置滚动偏移");
    }

    #[test]
    fn adding_record_scrolls_to_show_it() {
        let mut app = App::new(Config::default()).expect("app");
        app.left_page = LeftPage::Jump;
        app.paged_panel = Rect::new(0, 0, 22, 12);
        app.config.jump.bookmarks = (0..10).map(|i| format!("/p{i}")).collect();
        app.dir.path = PathBuf::from("/tmp/newone");

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(4), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.config.jump.bookmarks.len(), 11);
        assert_eq!(app.panel_offset, 1, "添加后应滚动到能看到新记录");
    }

    #[test]
    fn panel_input_bar_renders_at_panel_bottom() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = App::new(Config::default()).expect("app");
        app.panel_input = Some("hi".to_owned());

        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| ui::draw(f, &app)).unwrap();
        let buf = terminal.backend().buffer();
        assert_eq!(buf[(1, 28)].symbol(), ">");
        assert_eq!(buf[(3, 28)].symbol(), "h");
        assert_eq!(buf[(4, 28)].symbol(), "i");
        assert_eq!(buf[(5, 28)].symbol(), "▎");
    }

    #[test]
    fn clear_line_bytes_backspace_left_and_delete_right() {
        let mut term = vt100::Parser::new(5, 40, 100);
        term.process(b"C:\\x>dir hello");
        let bytes = terminal_clear_line_bytes(&term);
        assert_eq!(bytes, vec![0x7f; 14], "光标在行尾时应全部用退格");

        term.process(b"\x1b[1;10H");
        let bytes = terminal_clear_line_bytes(&term);
        assert_eq!(&bytes[..9], &[0x7f; 9], "光标前 9 个字符用退格");
        assert_eq!(bytes[9..].len(), 5 * 4, "光标后 5 个字符各用一次 ESC[3~");
        assert_eq!(&bytes[9..13], b"\x1b[3~");

        let mut term = vt100::Parser::new(5, 10, 100);
        term.process(b"C:\\x>dir hello");
        assert_eq!(
            terminal_line_before_cursor(&term),
            "C:\\x>dir hello",
            "跨行内容应拼接为完整逻辑行"
        );
        assert_eq!(
            terminal_clear_line_bytes(&term),
            vec![0x7f; 14],
            "换行输入行清空应退格全部字符"
        );
    }

    #[test]
    fn sane_area_replaces_degenerate_sizes() {
        assert_eq!(sane_area(1, 1), Rect::new(0, 0, 80, 24));
        assert_eq!(sane_area(0, 30), Rect::new(0, 0, 80, 24));
        assert_eq!(sane_area(120, 30), Rect::new(0, 0, 120, 30));
        assert_eq!(sane_area(2, 2), Rect::new(0, 0, 2, 2));
    }

    #[test]
    fn strips_prompt_markers() {
        assert_eq!(strip_prompt("user@host:~$ cargo build"), "cargo build");
        assert_eq!(strip_prompt("PS C:\\termiland> cargo test"), "cargo test");
        assert_eq!(strip_prompt("zsh% ls -la"), "ls -la");
        assert_eq!(strip_prompt("root# id"), "id");
        assert_eq!(strip_prompt("no prompt here"), "no prompt here");
        assert_eq!(strip_prompt("user@host:~$ "), "");
        assert_eq!(strip_prompt("user@host:~$  spaced"), "spaced");
        assert_eq!(
            strip_prompt("C:\\Users\\z00958688>dir"),
            "dir",
            "cmd 提示符 > 后无空格也应截断"
        );
        assert_eq!(
            strip_prompt("C:\\Users\\x>echo a > b"),
            "echo a > b",
            "命令内的重定向不应被误截"
        );
        assert_eq!(strip_prompt("user@h:~$ echo $HOME"), "echo $HOME");
    }

    #[test]
    fn f4_adds_current_path_to_jump_page() {
        let mut app = App::new(Config::default()).expect("app");
        app.left_page = LeftPage::Jump;
        app.dir.path = PathBuf::from("/tmp/demo");

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(4), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert!(input.is_empty(), "F4 不应发给 shell");
        assert_eq!(app.config.jump.bookmarks, vec!["/tmp/demo".to_owned()]);

        app.handle_key(press_key(KeyCode::F(4), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.config.jump.bookmarks.len(), 1, "重复添加应跳过");
        let (notice, kind) = app.notice_text().expect("重复时应有提示");
        assert_eq!(kind, NoticeKind::Warn);
        assert!(notice.contains("已存在"), "提示内容: {notice}");

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::Tab, KeyModifiers::CONTROL), &mut input)
            .unwrap();
        assert_eq!(
            input,
            vec![b'\t'],
            "Ctrl+I 不再拦截，应作为 Tab 字节发给 shell"
        );
        assert_eq!(app.config.jump.bookmarks.len(), 1, "Ctrl+I 不应触发收藏");
    }

    #[test]
    fn f4_adds_current_command_to_commands_page() {
        let mut app = App::new(Config::default()).expect("app");
        app.left_page = LeftPage::Commands;
        app.term.process(b"user@host:~$ cargo build --release");

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(4), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert!(input.is_empty(), "F4 不应发给 shell");
        assert_eq!(
            app.config.commands.items,
            vec![CommandItem {
                name: None,
                command: "cargo build --release".to_owned(),
            }]
        );
    }

    #[test]
    fn f4_skips_when_command_empty() {
        let mut app = App::new(Config::default()).expect("app");
        app.left_page = LeftPage::Commands;
        app.term.process(b"user@host:~$ ");

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(4), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert!(app.config.commands.items.is_empty());
        let (_, kind) = app.notice_text().expect("空命令应有提示");
        assert_eq!(kind, NoticeKind::Warn);
    }

    #[test]
    fn adding_record_persists_to_config_file() {
        let path = std::env::temp_dir().join(format!("termiland-add-{}.toml", std::process::id()));
        let config = Config {
            save_path: Some(path.clone()),
            ..Config::default()
        };
        let mut app = App::new(config).expect("app");
        app.dir.path = PathBuf::from("/tmp/persist");
        app.left_page = LeftPage::Jump;

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(4), KeyModifiers::NONE), &mut input)
            .unwrap();

        let loaded = Config::load(Some(&path)).unwrap();
        assert_eq!(loaded.jump.bookmarks, ["/tmp/persist"]);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn record_click_arms_and_arrow_click_deletes() {
        let mut app = App::new(Config::default()).expect("app");
        app.left_page = LeftPage::Jump;
        app.paged_panel = Rect::new(0, 0, 22, 12);
        app.config.jump.bookmarks = vec!["/a".to_owned(), "/b".to_owned()];

        click_at(&mut app, 2, 1);
        assert_eq!(app.armed_record, Some(0), "点击条目应进入待删状态");

        click_at(&mut app, 2, 1);
        assert!(app.armed_record.is_none(), "删除后应退出待删状态");
        assert_eq!(app.config.jump.bookmarks, vec!["/b".to_owned()]);

        click_at(&mut app, 5, 1);
        assert_eq!(app.armed_record, Some(0), "点击文字区域应选中该条");

        click_at(&mut app, 2, 6);
        assert!(app.armed_record.is_none(), "点击空白区域应退出待删状态");

        click_at(&mut app, 2, 1);
        app.handle_key(
            press_key(KeyCode::F(1), KeyModifiers::NONE),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(app.armed_record.is_none(), "切页应清除待删状态");
    }

    #[test]
    fn add_button_opens_input_bar() {
        let mut app = App::new(Config::default()).expect("app");
        app.left_page = LeftPage::Jump;
        app.paged_panel = Rect::new(0, 0, 22, 12);

        click_at(&mut app, 19, 0);
        assert_eq!(app.panel_input, Some(String::new()), "[+] 应打开输入栏");

        for c in "C:\\custom\\path".chars() {
            app.handle_key(
                press_key(KeyCode::Char(c), KeyModifiers::NONE),
                &mut Vec::new(),
            )
            .unwrap();
        }
        assert_eq!(app.panel_input.as_deref(), Some("C:\\custom\\path"));

        app.handle_key(
            press_key(KeyCode::Enter, KeyModifiers::NONE),
            &mut Vec::new(),
        )
        .unwrap();
        assert!(app.panel_input.is_none(), "回车后输入栏应关闭");
        assert_eq!(
            app.config.jump.bookmarks,
            vec!["C:\\custom\\path".to_owned()]
        );
    }

    #[test]
    fn panel_input_esc_cancels_and_backspace_edits() {
        let mut app = App::new(Config::default()).expect("app");
        app.left_page = LeftPage::Commands;
        app.paged_panel = Rect::new(0, 0, 22, 12);
        app.panel_input = Some(String::new());

        for c in "df -hX".chars() {
            app.handle_key(
                press_key(KeyCode::Char(c), KeyModifiers::NONE),
                &mut Vec::new(),
            )
            .unwrap();
        }
        app.handle_key(
            press_key(KeyCode::Backspace, KeyModifiers::NONE),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(app.panel_input.as_deref(), Some("df -h"));

        app.handle_key(press_key(KeyCode::Esc, KeyModifiers::NONE), &mut Vec::new())
            .unwrap();
        assert!(app.panel_input.is_none());
        assert!(app.config.commands.items.is_empty(), "取消不应添加记录");
    }

    #[test]
    fn panel_input_swallows_keys_and_mouse() {
        let mut app = App::new(Config::default()).expect("app");
        app.paged_panel = Rect::new(0, 0, 22, 12);
        app.panel_input = Some("abc".to_owned());

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(1), KeyModifiers::NONE), &mut input)
            .unwrap();
        app.handle_key(press_key(KeyCode::F(4), KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.left_page, LeftPage::Jump, "输入栏打开时 F1/F4 应被吞掉");
        assert!(input.is_empty(), "输入栏打开时不应有字节发给 shell");

        click_at(&mut app, 5, 3);
        assert!(app.panel_input.is_none(), "点击输入栏外应取消输入");
        assert!(
            app.dir_selection.is_none(),
            "取消输入的点击不应触发其他动作"
        );

        app.panel_input = Some("keep".to_owned());
        click_at(&mut app, 5, 10);
        assert_eq!(
            app.panel_input.as_deref(),
            Some("keep"),
            "点击输入栏所在行应保持输入"
        );
    }

    #[test]
    fn lines_selection_extraction_handles_wide_chars() {
        let lines = vec![
            "/some/path".to_owned(),
            "alpha/".to_owned(),
            "测试文件.txt".to_owned(),
        ];
        let sel = Selection {
            start: (1, 0),
            end: (1, 4),
        };
        assert_eq!(lines_selection_text(&lines, &sel), "alpha");

        let sel = Selection {
            start: (1, 0),
            end: (1, 3),
        };
        assert_eq!(lines_selection_text(&lines, &sel), "alph");

        let sel = Selection {
            start: (2, 4),
            end: (2, 4),
        };
        assert_eq!(lines_selection_text(&lines, &sel), "文", "单半宽字符选取");

        let sel = Selection {
            start: (2, 4),
            end: (2, 6),
        };
        assert_eq!(lines_selection_text(&lines, &sel), "文件", "整宽字符选取");

        let sel = Selection {
            start: (0, 1),
            end: (1, 5),
        };
        assert_eq!(
            lines_selection_text(&lines, &sel),
            format!("some/path{LINE_SEP}alpha/")
        );
    }

    #[test]
    fn end_key_jumps_to_bottom_when_scrolled() {
        let mut app = App::new(Config::default()).expect("app");
        for i in 0..100 {
            app.term.process(format!("line{i}\r\n").as_bytes());
        }
        app.scroll_by(10);
        assert_eq!(app.scroll(), 10);

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::End, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.scroll(), 0);
        assert!(input.is_empty(), "回看时 End 不应发给 shell");
    }

    #[test]
    fn end_key_goes_to_shell_when_live() {
        let mut app = App::new(Config::default()).expect("app");
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::End, KeyModifiers::NONE), &mut input)
            .unwrap();
        assert_eq!(app.scroll(), 0);
        assert_eq!(input, b"\x1b[F".to_vec());
    }

    #[test]
    fn jump_button_click_returns_to_bottom() {
        let mut app = App::new(Config::default()).expect("app");
        for i in 0..100 {
            app.term.process(format!("line{i}\r\n").as_bytes());
        }
        app.scroll_by(10);
        assert_eq!(app.scroll(), 10);

        app.pane_inner = Rect::new(10, 1, 40, 20);
        app.island_areas.clear();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 48,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(click).unwrap();
        assert_eq!(app.scroll(), 0);

        let no_hit = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 30,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        app.scroll_by(10);
        app.handle_mouse(no_hit).unwrap();
        assert_eq!(app.scroll(), 10, "按钮区域外不应触发回底");
    }

    #[test]
    fn scrolling_clears_selection() {
        let mut app = App::new(Config::default()).expect("app");
        for i in 0..100 {
            app.term.process(format!("line{i}\r\n").as_bytes());
        }

        app.selection = Some(Selection {
            start: (0, 0),
            end: (0, 5),
        });
        app.scroll_by(-3);
        assert_eq!(app.scroll(), 0);
        assert!(app.selection.is_some(), "视图未移动时选区应保留");

        app.scroll_by(3);
        assert_eq!(app.scroll(), 3);
        assert!(app.selection.is_none(), "视图移动后选区应清除");
    }

    #[test]
    #[ignore]
    fn bench_render_vs_history() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        use crate::ui;

        for history in [0usize, 500, 2000, 5000] {
            let mut app = app_with_islands();
            app.term.screen_mut().set_size(28, 60);
            for i in 0..history {
                app.term.process(format!("line {i}\r\n").as_bytes());
            }
            let depth = {
                let screen = app.term.screen_mut();
                screen.set_scrollback(usize::MAX);
                let d = screen.scrollback();
                screen.set_scrollback(0);
                d
            };
            let cell_start = std::time::Instant::now();
            for _ in 0..1000 {
                let _ = app.term.screen().cell(0, 0);
            }
            let cell_us = cell_start.elapsed().as_micros() as f64 / 1000.0;
            let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
            let start = std::time::Instant::now();
            for _ in 0..50 {
                terminal.draw(|f| ui::draw(f, &app)).unwrap();
            }
            let per_frame = start.elapsed() / 50;
            println!(
                "history={history:5}  depth={depth:5}  cell(0,0)={cell_us:.3}ms  {per_frame:?}/帧"
            );
        }
    }
}
