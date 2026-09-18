use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "termiland", version, about = "轻量级三栏终端增强工具")]
pub struct Cli {
    /// 配置文件路径，默认 ~/.config/termiland/config.toml
    #[arg(short, long, value_name = "PATH")]
    pub config: Option<PathBuf>,
}
