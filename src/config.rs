use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ui: UiConfig,
    pub islands: IslandsConfig,
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
            left_width: 28,
            right_width: 44,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IslandsConfig {
    pub max: usize,
    pub items: Vec<Island>,
}

impl Default for IslandsConfig {
    fn default() -> Self {
        Self {
            max: 3,
            items: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Island {
    pub name: Option<String>,
    pub command: String,
    pub height: Option<u16>,
    #[serde(default)]
    pub live: bool,
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
        assert_eq!(cfg.ui.left_width, 28);
        assert_eq!(cfg.ui.right_width, 44);
        assert_eq!(cfg.islands.max, 3);
        assert!(cfg.islands.items.is_empty());
    }

    #[test]
    fn parses_islands() {
        let raw = r#"
[ui]
left_width = 30
right_width = 50

[islands]
max = 2

[[islands.items]]
name = "history"
command = "tail -n 30 $HISTFILE"
height = 12

[[islands.items]]
name = "top"
command = "top"
height = 20
live = true
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.ui.left_width, 30);
        assert_eq!(cfg.islands.max, 2);
        assert_eq!(cfg.islands.items.len(), 2);
        assert!(!cfg.islands.items[0].live);
        assert!(cfg.islands.items[1].live);
        assert_eq!(cfg.islands.items[1].height, Some(20));
        assert!(cfg.islands.items[0].name.is_some());
    }
}
