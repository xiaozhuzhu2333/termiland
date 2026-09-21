use portable_pty::CommandBuilder;

use crate::app::Selection;
use crate::pty::PtySession;

const ISLAND_SCROLLBACK: usize = 1000;

pub struct IslandState {
    pub command: String,
    pub live: bool,
    pub height: Option<u16>,
    pub parser: vt100::Parser,
    pub session: Option<PtySession>,
    pub exited: bool,
    pub scroll: u16,
    pub selection: Option<Selection>,
}

impl IslandState {
    pub fn empty(rows: u16, cols: u16) -> Self {
        Self {
            command: String::new(),
            live: false,
            height: None,
            parser: vt100::Parser::new(rows, cols, ISLAND_SCROLLBACK),
            session: None,
            exited: false,
            scroll: 0,
            selection: None,
        }
    }

    pub fn execute(&mut self) {
        if self.command.is_empty() {
            return;
        }
        self.session.take();
        let (rows, cols) = self.parser.screen().size();
        self.parser = vt100::Parser::new(rows, cols, ISLAND_SCROLLBACK);
        self.exited = false;
        self.scroll = 0;
        self.selection = None;
        match PtySession::spawn_command(shell_command(&self.command), rows, cols) {
            Ok(session) => self.session = Some(session),
            Err(err) => {
                self.exited = true;
                self.parser
                    .process(format!("启动失败: {err}\r\n").as_bytes());
            }
        }
    }

    pub fn clear(&mut self) {
        self.session.take();
        let (rows, cols) = self.parser.screen().size();
        self.parser = vt100::Parser::new(rows, cols, ISLAND_SCROLLBACK);
        self.exited = false;
        self.scroll = 0;
        self.selection = None;
    }

    pub fn poll_output(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        let output = session.poll_output();
        if !output.is_empty() {
            self.parser.process(&output);
            let (row, col) = self.parser.screen().cursor_position();
            session.set_cursor_position(row, col);
        }
        if !session.is_alive() {
            self.exited = true;
        }
        if session.is_finished() {
            self.session = None;
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
        if let Some(session) = &mut self.session {
            let _ = session.resize(rows, cols);
        }
    }

    pub fn scroll_by(&mut self, delta: i32) {
        let screen = self.parser.screen_mut();
        let before = screen.scrollback();
        let target = (before as i32 + delta).max(0) as usize;
        screen.set_scrollback(target);
        let after = screen.scrollback();
        self.scroll = after as u16;
        if after != before {
            self.selection = None;
        }
    }

    pub fn reset_scroll(&mut self) {
        if self.scroll > 0 {
            self.selection = None;
        }
        self.parser.screen_mut().set_scrollback(0);
        self.scroll = 0;
    }

    pub fn toggle_live(&mut self) {
        self.live = !self.live;
    }
}

fn shell_command(command: &str) -> CommandBuilder {
    let mut cmd = if cfg!(windows) {
        let mut c = CommandBuilder::new("cmd");
        c.arg("/C");
        c
    } else {
        let mut c = CommandBuilder::new("sh");
        c.arg("-c");
        c
    };
    cmd.arg(command);
    cmd
}
