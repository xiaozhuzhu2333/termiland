use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ui: UiConfig,
    pub islands: IslandsConfig,
    pub jump: JumpConfig,
    pub commands: CommandsConfig,
    #[serde(skip)]
    pub save_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct IslandsConfig {
    pub max: usize,
}

impl Default for IslandsConfig {
    fn default() -> Self {
        Self { max: 3 }
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct JumpConfig {
    pub bookmarks: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandsConfig {
    pub items: Vec<CommandItem>,
}

impl Default for CommandsConfig {
    fn default() -> Self {
        Self {
            items: default_commands(),
        }
    }
}

fn default_commands() -> Vec<CommandItem> {
    if cfg!(target_os = "linux") {
        vec![
            CommandItem {
                name: None,
                command: "free -h".to_owned(),
            },
            CommandItem {
                name: None,
                command: "df -h".to_owned(),
            },
        ]
    } else {
        Vec::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandItem {
    #[serde(skip_serializing_if = "Option::is_none")]
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
                    Ok(Config {
                        save_path: Some(path),
                        ..Config::default()
                    })
                }
            }
        }
    }

    fn from_file(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("读取配置失败 {path:?}: {e}"))?;
        let mut config: Config =
            toml::from_str(&raw).map_err(|e| anyhow::anyhow!("解析配置失败 {path:?}: {e}"))?;
        config.save_path = Some(path.to_path_buf());
        Ok(config)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let Some(path) = &self.save_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("创建配置目录失败 {path:?}: {e}"))?;
        }
        let raw = toml::to_string_pretty(self)?;
        std::fs::write(path, raw).map_err(|e| anyhow::anyhow!("写入配置失败 {path:?}: {e}"))?;
        Ok(())
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
        assert_eq!(
            cfg.commands.items,
            default_commands(),
            "缺节时应回退默认命令"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_defaults_are_memory_and_disk() {
        let defaults = default_commands();
        assert_eq!(defaults.len(), 2);
        assert_eq!(defaults[0].name, None, "默认命令不带名称，与手动添加一致");
        assert_eq!(defaults[0].command, "free -h");
        assert_eq!(defaults[1].name, None);
        assert_eq!(defaults[1].command, "df -h");
    }

    #[test]
    fn explicit_empty_items_opts_out_of_defaults() {
        let cfg: Config = toml::from_str("[commands]\nitems = []").unwrap();
        assert!(cfg.commands.items.is_empty(), "显式空列表应退出默认命令");
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
        assert_eq!(cfg.ui.right_width, 50);
        assert_eq!(cfg.islands.max, 2);
        assert_eq!(cfg.jump.bookmarks, ["/var/log", "/data"]);
        assert_eq!(cfg.commands.items.len(), 2);
        assert!(cfg.commands.items[0].name.is_some());
        assert!(cfg.commands.items[1].name.is_none());
    }

    #[test]
    fn save_round_trips_through_file() {
        let path = std::env::temp_dir().join(format!("termiland-cfg-{}.toml", std::process::id()));
        let config = Config {
            save_path: Some(path.clone()),
            jump: JumpConfig {
                bookmarks: vec!["/var/log".to_owned(), "/data".to_owned()],
            },
            commands: CommandsConfig {
                items: vec![
                    CommandItem {
                        name: Some("服务状态".to_owned()),
                        command: "systemctl status nginx".to_owned(),
                    },
                    CommandItem {
                        name: None,
                        command: "df -h".to_owned(),
                    },
                ],
            },
            ..Config::default()
        };
        config.save().unwrap();

        let reloaded = Config::load(Some(&path)).unwrap();
        assert_eq!(reloaded.jump.bookmarks, config.jump.bookmarks);
        assert_eq!(reloaded.commands.items, config.commands.items);
        assert_eq!(reloaded.save_path, Some(path.clone()));

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("name = \"df"),
            "name 为 None 时不应序列化 name 字段"
        );
        assert!(!raw.contains("save_path"), "save_path 不应写入文件");

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn save_without_path_is_noop() {
        let config = Config::default();
        assert!(config.save().is_ok());
    }
}
