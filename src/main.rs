mod app;
mod cli;
mod config;
mod ui;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    let config = config::Config::load(cli.config.as_deref())?;

    let mut terminal = ratatui::init();
    let result = app::App::new(config).run(&mut terminal);
    ratatui::restore();
    result
}
