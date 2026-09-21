use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;

use crate::config::Config;
use crate::island::IslandState;
use crate::keys;
use crate::pty;
use crate::ui;

const SCROLLBACK_LEN: usize = 10_000;
const SCROLL_STEP: i32 = 3;
const COPY_NOTICE: Duration = Duration::from_secs(3);
const TRIGGER_SETTLE: Duration = Duration::from_millis(300);

#[cfg(windows)]
const LINE_SEP: &str = "\r\n";
#[cfg(not(windows))]
const LINE_SEP: &str = "\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Terminal,
    Island(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IslandHit {
    Body,
    Toggle,
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
    pty_size: (u16, u16),
    pane_inner: Rect,
    layout_area: Rect,
    island_areas: Vec<Rect>,
    paged_panel: Rect,
    scroll: u16,
    selection: Option<Selection>,
    copy_notice: Option<(usize, Instant)>,
    focus: Focus,
    trigger_pending: bool,
    last_output_at: Instant,
    should_quit: bool,
}

impl App {
    pub fn new(config: Config) -> Result<Self> {
        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
        let area = Rect::new(0, 0, width, height);
        let pane_inner = ui::terminal_pane_inner(area, &config.ui);
        let (rows, cols) = (pane_inner.height.max(1), pane_inner.width.max(1));
        let heights = [None];
        let island_areas = ui::island_layout(ui::right_pane_rect(area, &config.ui), &heights);
        let islands = island_areas
            .iter()
            .map(|rect| {
                let output = ui::island_output_inner(*rect);
                IslandState::empty(output.height.max(1), output.width.max(1))
            })
            .collect();
        let paged_panel = ui::paged_panel_rect(area, &config.ui);
        let pty = pty::PtySession::spawn(rows, cols)?;
        let term = vt100::Parser::new(rows, cols, SCROLLBACK_LEN);
        Ok(Self {
            config,
            left_page: LeftPage::Jump,
            pty,
            term,
            islands,
            pty_size: (rows, cols),
            pane_inner,
            layout_area: area,
            island_areas,
            paged_panel,
            scroll: 0,
            selection: None,
            copy_notice: None,
            focus: Focus::Terminal,
            trigger_pending: false,
            last_output_at: Instant::now(),
            should_quit: false,
        })
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
        for island in &mut self.islands {
            if island.follow && island.armed {
                island.execute();
            }
        }
    }

    fn sync_layout(&mut self, terminal: &DefaultTerminal) -> Result<()> {
        let area: Rect = terminal.size()?.into();
        if area == self.layout_area {
            return Ok(());
        }
        self.layout_area = area;
        self.pane_inner = ui::terminal_pane_inner(area, &self.config.ui);
        let heights: Vec<Option<u16>> = self.islands.iter().map(|i| i.height).collect();
        self.island_areas = ui::island_layout(ui::right_pane_rect(area, &self.config.ui), &heights);
        for (island, &rect) in self.islands.iter_mut().zip(self.island_areas.iter()) {
            let output = ui::island_output_inner(rect);
            island.resize(output.height.max(1), output.width.max(1));
        }
        self.paged_panel = ui::paged_panel_rect(area, &self.config.ui);
        let size = (self.pane_inner.height.max(1), self.pane_inner.width.max(1));
        if self.pty_size != size {
            self.pty_size = size;
            self.pty.resize(size.0, size.1)?;
            self.term.screen_mut().set_size(size.0, size.1);
        }
        Ok(())
    }

    pub fn scroll(&self) -> u16 {
        self.scroll
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn selection_chars(&self) -> Option<usize> {
        self.selection
            .map(|sel| selection_text(&self.term, &sel).chars().count())
    }

    pub fn copy_notice_text(&self) -> Option<String> {
        self.copy_notice.and_then(|(chars, at)| {
            (at.elapsed() < COPY_NOTICE).then(|| format!("已复制 {chars} 字符"))
        })
    }

    fn handle_events(&mut self) -> Result<()> {
        if !event::poll(Duration::from_millis(50))? {
            return Ok(());
        }
        let mut input = Vec::new();
        loop {
            match event::read()? {
                Event::Key(key) => self.handle_key(key, &mut input),
                Event::Paste(text) => match self.focus {
                    Focus::Island(_) => {
                        let line: String =
                            text.chars().filter(|c| *c != '\r' && *c != '\n').collect();
                        self.edit_focused_command(Some(&line));
                    }
                    Focus::Terminal => input.extend_from_slice(&keys::paste_bytes(&text)),
                },
                Event::Mouse(mouse) => self.handle_mouse(mouse),
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

    fn handle_key(&mut self, key: KeyEvent, input: &mut Vec<u8>) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let is_press = key.kind == KeyEventKind::Press;
        if is_press
            && key.code == KeyCode::Char('q')
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.should_quit = true;
        } else if is_press && key.code == KeyCode::F(1) {
            self.left_page = self.left_page.next();
        } else if is_press && key.code == KeyCode::F(2) {
            self.cycle_focus();
        } else if is_press
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && self.has_focused_selection()
        {
            self.copy_focused_selection();
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
    }

    fn handle_terminal_key(&mut self, key: KeyEvent, is_press: bool, input: &mut Vec<u8>) {
        if is_press && key.code == KeyCode::End && key.modifiers.is_empty() && self.scroll > 0 {
            self.reset_scroll();
        } else if let Some(bytes) = keys::encode(&key) {
            input.extend_from_slice(&bytes);
        }
    }

    fn has_focused_selection(&self) -> bool {
        match self.focus {
            Focus::Terminal => self.selection.is_some(),
            Focus::Island(i) => self
                .islands
                .get(i)
                .is_some_and(|island| island.selection.is_some()),
        }
    }

    fn copy_focused_selection(&mut self) {
        let (term, sel) = match self.focus {
            Focus::Terminal => (&self.term, self.selection),
            Focus::Island(i) => match self.islands.get(i) {
                Some(island) => (&island.parser, island.selection),
                None => return,
            },
        };
        let Some(sel) = sel else {
            return;
        };
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let text = selection_text(term, &sel);
            let count = text.chars().count();
            if !text.is_empty() && clipboard.set_text(text).is_ok() {
                self.copy_notice = Some((count, Instant::now()));
            }
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
        if let Focus::Island(i) = self.focus
            && let Some(island) = self.islands.get_mut(i)
        {
            island.execute();
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_mouse(SCROLL_STEP, mouse.column, mouse.row),
            MouseEventKind::ScrollDown => self.scroll_mouse(-SCROLL_STEP, mouse.column, mouse.row),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((index, hit)) = self.island_hit(mouse.column, mouse.row) {
                    self.clear_all_selections();
                    self.focus = Focus::Island(index);
                    if hit == IslandHit::Toggle {
                        self.toggle_island_follow(index);
                    } else if let Some(pos) = self.island_cell_at(index, mouse.column, mouse.row) {
                        self.islands[index].selection = Some(Selection {
                            start: pos,
                            end: pos,
                        });
                    }
                } else if self.jump_button_hit(mouse.column, mouse.row) {
                    self.reset_scroll();
                } else if let Some(page) =
                    ui::paged_tab_hit(self.paged_panel, mouse.column, mouse.row)
                {
                    self.left_page = page;
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
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(sel) = self.selection
                    && sel.start == sel.end
                {
                    self.selection = None;
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
    }

    fn scroll_mouse(&mut self, delta: i32, column: u16, row: u16) {
        if let Some((index, _)) = self.island_hit(column, row) {
            self.islands[index].scroll_by(delta);
        } else {
            self.scroll_by(delta);
        }
    }

    fn clear_all_selections(&mut self) {
        self.selection = None;
        for island in &mut self.islands {
            island.selection = None;
        }
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

    fn island_hit(&self, column: u16, row: u16) -> Option<(usize, IslandHit)> {
        for (index, area) in self.island_areas.iter().enumerate() {
            if column >= area.x
                && column < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
            {
                let hit = if row == area.y && column + 10 >= area.x + area.width {
                    IslandHit::Toggle
                } else {
                    IslandHit::Body
                };
                return Some((index, hit));
            }
        }
        None
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
    fn f2_cycles_focus_through_islands() {
        let mut app = app_with_islands();
        let mut input = Vec::new();

        assert_eq!(app.focus(), Focus::Terminal);
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Island(0));
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Island(1));
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Terminal);
        assert!(input.is_empty());

        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input);
        app.handle_key(press_key(KeyCode::Tab, KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Terminal);
    }

    #[test]
    fn f2_with_single_island_round_trips() {
        let mut app = App::new(Config::default()).expect("app");
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Island(0));
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Terminal);
    }

    #[test]
    fn island_input_executes_command() {
        let mut app = app_with_islands();
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Island(0));

        for ch in "echo island-run-ok".chars() {
            app.handle_key(press_key(KeyCode::Char(ch), KeyModifiers::NONE), &mut input);
        }
        assert_eq!(app.islands[0].command, "echo island-run-ok");
        assert!(input.is_empty(), "岛内输入不应进入 shell");

        app.handle_key(
            press_key(KeyCode::Backspace, KeyModifiers::NONE),
            &mut input,
        );
        assert_eq!(app.islands[0].command, "echo island-run-o");
        app.handle_key(
            press_key(KeyCode::Char('k'), KeyModifiers::NONE),
            &mut input,
        );

        app.handle_key(press_key(KeyCode::Enter, KeyModifiers::NONE), &mut input);
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
        app.handle_key(press_key(KeyCode::Tab, KeyModifiers::NONE), &mut input);
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
        app.handle_mouse(body_click);
        assert_eq!(app.focus(), Focus::Island(0));
        assert!(!app.islands[0].follow, "点击岛体不应拨动开关");

        let toggle_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 84,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(toggle_click);
        assert_eq!(app.focus(), Focus::Island(0));
        assert!(app.islands[0].follow);
        assert!(!app.islands[1].follow, "只拨动被点击的岛");

        app.handle_mouse(toggle_click);
        assert!(!app.islands[0].follow, "再次点击应拨回");
    }

    #[test]
    fn clicking_terminal_returns_focus() {
        let mut app = app_with_islands();
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Island(0));

        app.island_areas = Vec::new();
        app.pane_inner = Rect::new(10, 1, 40, 20);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 30,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(click);
        assert_eq!(app.focus(), Focus::Terminal);
        assert!(app.selection.is_some());
    }

    #[test]
    fn island_focus_navigates_with_arrows() {
        let mut app = app_with_islands();
        let mut input = Vec::new();

        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Island(0));

        app.handle_key(press_key(KeyCode::Up, KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Island(0), "顶部应钳位");

        app.handle_key(press_key(KeyCode::Down, KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Island(1));
        app.handle_key(press_key(KeyCode::Down, KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Island(1), "底部应钳位");

        app.handle_key(press_key(KeyCode::Up, KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Island(0));

        app.handle_key(
            press_key(KeyCode::Char('a'), KeyModifiers::NONE),
            &mut input,
        );
        assert!(input.is_empty(), "岛聚焦时按键不应进入 shell");

        app.handle_key(press_key(KeyCode::Esc, KeyModifiers::NONE), &mut input);
        assert_eq!(app.focus(), Focus::Terminal);
    }

    #[test]
    fn clearing_command_resets_island() {
        let mut app = app_with_islands();
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input);
        for ch in "echo island-reset-marker".chars() {
            app.handle_key(press_key(KeyCode::Char(ch), KeyModifiers::NONE), &mut input);
        }
        app.handle_key(press_key(KeyCode::Enter, KeyModifiers::NONE), &mut input);
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
            );
        }
        assert!(app.islands[0].command.is_empty());
        assert!(app.islands[0].session.is_none());

        app.handle_key(
            press_key(KeyCode::Char('a'), KeyModifiers::NONE),
            &mut input,
        );
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
        app.handle_mouse(wheel);
        assert_eq!(app.islands[0].scroll, 3);
        assert_eq!(app.scroll(), 0, "主终端不应被岛内滚动影响");

        let outside = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(outside);
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
        app.handle_mouse(down);
        assert_eq!(app.focus(), Focus::Island(0));
        assert!(app.islands[0].selection.is_some());

        let drag = MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 55,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(drag);
        let sel = app.islands[0].selection.expect("拖动后选区应存在");
        assert_eq!(sel.normalized(), ((1, 1), (1, 4)));
    }

    #[test]
    fn follow_island_runs_command() {
        let mut app = app_with_islands();
        app.islands[0].follow = true;
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input);
        for ch in "echo live-island-ok".chars() {
            app.handle_key(press_key(KeyCode::Char(ch), KeyModifiers::NONE), &mut input);
        }
        app.handle_key(press_key(KeyCode::Enter, KeyModifiers::NONE), &mut input);
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
        app.handle_key(press_key(KeyCode::F(2), KeyModifiers::NONE), &mut input);
        for ch in "ping -t 127.0.0.1".chars() {
            app.handle_key(press_key(KeyCode::Char(ch), KeyModifiers::NONE), &mut input);
        }
        app.handle_key(press_key(KeyCode::Enter, KeyModifiers::NONE), &mut input);
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
    fn end_key_jumps_to_bottom_when_scrolled() {
        let mut app = App::new(Config::default()).expect("app");
        for i in 0..100 {
            app.term.process(format!("line{i}\r\n").as_bytes());
        }
        app.scroll_by(10);
        assert_eq!(app.scroll(), 10);

        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::End, KeyModifiers::NONE), &mut input);
        assert_eq!(app.scroll(), 0);
        assert!(input.is_empty(), "回看时 End 不应发给 shell");
    }

    #[test]
    fn end_key_goes_to_shell_when_live() {
        let mut app = App::new(Config::default()).expect("app");
        let mut input = Vec::new();
        app.handle_key(press_key(KeyCode::End, KeyModifiers::NONE), &mut input);
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
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 48,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(click);
        assert_eq!(app.scroll(), 0);

        let no_hit = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 30,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        app.scroll_by(10);
        app.handle_mouse(no_hit);
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
