use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ui: UiConfig,
    pub islands: IslandsConfig,
    pub jump: JumpConfig,
    pub commands: CommandsConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub left_width: u16,
    pub right_width: u16,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            left_width: 22,
            right_width: 36,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IslandsConfig {
    pub max: usize,
}

impl Default for IslandsConfig {
    fn default() -> Self {
        Self { max: 3 }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JumpConfig {
    pub bookmarks: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandsConfig {
    pub items: Vec<CommandItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandItem {
    pub name: Option<String>,
    pub command: String,
}

impl Config {
    pub fn load(explicit: Option<&Path>) -> anyhow::Result<Self> {
        match explicit {
            Some(path) => Self::from_file(path),
            None => {
                let path = default_path();
                if path.exists() {
                    Self::from_file(&path)
                } else {
                    Ok(Self::default())
                }
            }
        }
    }

    fn from_file(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读取配置失败 {path:?}: {e}"))?;
        toml::from_str(&raw).map_err(|e| anyhow::anyhow!("解析配置失败 {path:?}: {e}"))
    }
}

fn default_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(dir).join("termiland").join("config.toml");
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .unwrap_or_default();
    PathBuf::from(home)
        .join(".config")
        .join("termiland")
        .join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.ui.left_width, 22);
        assert_eq!(cfg.ui.right_width, 36);
        assert_eq!(cfg.islands.max, 3);
        assert!(cfg.jump.bookmarks.is_empty());
        assert!(cfg.commands.items.is_empty());
    }

    #[test]
    fn parses_full_config() {
        let raw = r#"
[ui]
left_width = 30
right_width = 50

[islands]
max = 2

[jump]
bookmarks = ["/var/log", "/data"]

[[commands.items]]
name = "服务状态"
command = "systemctl status nginx"

[[commands.items]]
command = "df -h"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.ui.left_width, 30);
        assert_eq!(cfg.islands.max, 2);
        assert_eq!(cfg.jump.bookmarks, ["/var/log", "/data"]);
        assert_eq!(cfg.commands.items.len(), 2);
        assert!(cfg.commands.items[0].name.is_some());
        assert!(cfg.commands.items[1].name.is_none());
    }
}
