use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use crate::config::Config;
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
    should_quit: bool,
}

impl App {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            left_page: LeftPage::Jump,
            should_quit: false,
        }
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|f| ui::draw(f, self))?;
            self.handle_events()?;
        }
        Ok(())
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
