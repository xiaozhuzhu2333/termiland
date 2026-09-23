mod app;
mod cli;
mod clipboard;
mod config;
mod dirpane;
mod island;
mod keys;
mod pty;
mod ui;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;

struct TerminalModesGuard;

impl TerminalModesGuard {
    fn enable() -> Result<Self> {
        execute!(std::io::stdout(), EnableBracketedPaste, EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for TerminalModesGuard {
    fn drop(&mut self) {
        let _ = execute!(
            std::io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture
        );
    }
}

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    let config = config::Config::load(cli.config.as_deref())?;
    let mut app = app::App::new(config)?;

    let mut terminal = ratatui::init();
    let _modes = TerminalModesGuard::enable()?;
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}
