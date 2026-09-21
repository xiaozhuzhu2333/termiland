use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub hidden: bool,
}

#[derive(Debug)]
pub struct DirPane {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    pub offset: u16,
    pub error: Option<String>,
}

impl DirPane {
    pub fn load(path: PathBuf) -> Self {
        let mut pane = Self {
            path,
            entries: Vec::new(),
            offset: 0,
            error: None,
        };
        pane.reload();
        pane
    }

    pub fn reload(&mut self) {
        self.entries.clear();
        self.offset = 0;
        match fs::read_dir(&self.path) {
            Ok(iter) => {
                self.error = None;
                for entry in iter.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let hidden = name.starts_with('.');
                    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    self.entries.push(Entry {
                        name,
                        is_dir,
                        hidden,
                    });
                }
                self.entries.sort_by(|a, b| {
                    b.is_dir
                        .cmp(&a.is_dir)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
            }
            Err(err) => {
                self.error = Some(err.to_string());
            }
        }
    }

    pub fn total(&self) -> u16 {
        self.entries.len() as u16
    }

    pub fn scroll_by(&mut self, delta: i32) {
        let total = self.total() as i32;
        self.offset = (self.offset as i32 + delta).clamp(0, total) as u16;
    }

    pub fn clamped_offset(&self, visible: u16) -> u16 {
        self.offset.min(self.total().saturating_sub(visible))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_sorts_dirs_first_case_insensitive() {
        let base = std::env::temp_dir().join(format!("termiland-dir-test-{}", std::process::id()));
        fs::create_dir_all(base.join("zed")).unwrap();
        fs::create_dir_all(base.join("alpha")).unwrap();
        fs::File::create(base.join("Beta.txt")).unwrap();
        fs::File::create(base.join(".hidden")).unwrap();
        fs::File::create(base.join("aaa.txt")).unwrap();

        let pane = DirPane::load(base.clone());
        let names: Vec<&str> = pane.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["alpha", "zed", ".hidden", "aaa.txt", "Beta.txt"]
        );
        assert!(pane.entries[0].is_dir);
        assert!(pane.entries[2].hidden);
        assert!(!pane.entries[3].is_dir);

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn load_error_is_recorded() {
        let pane = DirPane::load(PathBuf::from("/definitely/not/a/real/path"));
        assert!(pane.error.is_some());
        assert!(pane.entries.is_empty());
    }

    #[test]
    fn offset_clamps_to_total_and_viewport() {
        let mut pane = DirPane::load(std::env::temp_dir());
        pane.entries = (0..10)
            .map(|i| Entry {
                name: format!("f{i}"),
                is_dir: false,
                hidden: false,
            })
            .collect();
        pane.scroll_by(100);
        assert_eq!(pane.offset, 10);
        pane.scroll_by(-100);
        assert_eq!(pane.offset, 0);
        pane.scroll_by(7);
        assert_eq!(pane.clamped_offset(5), 5);
        pane.scroll_by(-7);
        pane.scroll_by(2);
        assert_eq!(pane.clamped_offset(5), 2, "不足一屏时偏移应归零");
    }
}
