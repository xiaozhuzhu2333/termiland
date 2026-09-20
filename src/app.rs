use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use crate::config::Config;
use crate::pty;
use crate::ui;

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
    pty_bytes: u64,
    should_quit: bool,
}

impl App {
    pub fn new(config: Config) -> Result<Self> {
        let pty = pty::PtySession::spawn(24, 80)?;
        Ok(Self {
            config,
            left_page: LeftPage::Jump,
            pty,
            pty_bytes: 0,
            should_quit: false,
        })
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.should_quit {
            let output = self.pty.poll_output();
            self.pty_bytes += output.len() as u64;
            terminal.draw(|f| ui::draw(f, self))?;
            self.handle_events()?;
        }
        Ok(())
    }

    pub fn pty_bytes(&self) -> u64 {
        self.pty_bytes
    }

    fn handle_events(&mut self) -> Result<()> {
        if !event::poll(Duration::from_millis(250))? {
            return Ok(());
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                return Ok(());
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.should_quit = true;
                }
                KeyCode::Tab => self.left_page = self.left_page.next(),
                _ => {}
            }
        }
        Ok(())
    }
}
