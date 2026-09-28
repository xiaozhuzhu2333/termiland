use portable_pty::CommandBuilder;

use crate::app::Selection;
use crate::pty::PtySession;

const ISLAND_SCROLLBACK: usize = 1000;

pub fn sanitize_resize_boundary(parser: &mut vt100::Parser, new_cols: u16) {
    if new_cols == 0 {
        return;
    }
    let (rows, cols) = parser.screen().size();
    if new_cols >= cols {
        return;
    }
    let boundary = new_cols - 1;
    let bad_rows: Vec<u16> = (0..rows)
        .filter(|&row| {
            parser
                .screen()
                .cell(row, boundary)
                .is_some_and(|c| c.is_wide() || c.is_wide_continuation())
        })
        .collect();
    if bad_rows.is_empty() {
        return;
    }
    let mut seq = String::from("\x1b7");
    for row in bad_rows {
        seq.push_str(&format!("\x1b[{};{new_cols}H ", row + 1));
    }
    seq.push_str("\x1b8");
    parser.process(seq.as_bytes());
}

pub struct IslandState {
    pub command: String,
    pub follow: bool,
    pub custom_path: Option<String>,
    pub height: Option<u16>,
    pub parser: vt100::Parser,
    pub session: Option<PtySession>,
    pub armed: bool,
    pub exited: bool,
    pub scroll: u16,
    pub selection: Option<Selection>,
}

impl IslandState {
    pub fn empty(rows: u16, cols: u16) -> Self {
        Self {
            command: String::new(),
            follow: false,
            custom_path: None,
            height: None,
            parser: vt100::Parser::new(rows, cols, ISLAND_SCROLLBACK),
            session: None,
            armed: false,
            exited: false,
            scroll: 0,
            selection: None,
        }
    }

    pub fn execute(&mut self, cwd: &std::path::Path) {
        if self.command.is_empty() {
            return;
        }
        self.session.take();
        let (rows, cols) = self.parser.screen().size();
        self.parser = vt100::Parser::new(rows, cols, ISLAND_SCROLLBACK);
        self.exited = false;
        self.armed = true;
        self.scroll = 0;
        self.selection = None;
        match PtySession::spawn_command(shell_command(&self.command, cwd), rows, cols) {
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
        self.armed = false;
        self.custom_path = None;
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
        sanitize_resize_boundary(&mut self.parser, cols);
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

    pub fn toggle_follow(&mut self) {
        self.follow = !self.follow;
    }
}

fn island_command_line(shell: &str, command: &str) -> String {
    let is_bash = std::path::Path::new(shell)
        .file_name()
        .is_some_and(|name| name == "bash");
    if is_bash {
        format!("history -r 2>/dev/null; {command}")
    } else {
        command.to_owned()
    }
}

fn island_shell_is_bash(shell: &str) -> bool {
    std::path::Path::new(shell)
        .file_name()
        .is_some_and(|name| name == "bash")
}

fn island_rc_path() -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join("termiland-island-rc");
    std::fs::write(&path, "history -r 2>/dev/null\n").ok()?;
    Some(path)
}

fn island_argv(shell: &str, command: &str, rcfile: Option<&str>) -> Vec<String> {
    let mut argv = vec![shell.to_owned()];
    if island_shell_is_bash(shell)
        && let Some(rc) = rcfile
    {
        argv.push("--rcfile".to_owned());
        argv.push(rc.to_owned());
    }
    argv.push("-i".to_owned());
    argv.push("-c".to_owned());
    argv.push(island_command_line(shell, command));
    argv
}

fn shell_command(command: &str, cwd: &std::path::Path) -> CommandBuilder {
    if cfg!(windows) {
        let mut cmd = CommandBuilder::new("cmd");
        cmd.arg("/C");
        cmd.arg(command);
        cmd.cwd(cwd);
        cmd
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        let rcfile = island_shell_is_bash(&shell)
            .then(island_rc_path)
            .flatten()
            .map(|path| path.to_string_lossy().into_owned());
        let argv: Vec<std::ffi::OsString> = island_argv(&shell, command, rcfile.as_deref())
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect();
        let mut cmd = CommandBuilder::from_argv(argv);
        cmd.cwd(cwd);
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_islands_use_fast_rcfile_path() {
        assert_eq!(
            island_argv("/bin/bash", "echo hi", Some("/tmp/termiland-island-rc")),
            vec![
                "/bin/bash".to_owned(),
                "--rcfile".to_owned(),
                "/tmp/termiland-island-rc".to_owned(),
                "-i".to_owned(),
                "-c".to_owned(),
                "history -r 2>/dev/null; echo hi".to_owned(),
            ],
            "bash 岛应跳过重量级 .bashrc，仅预载历史（--rcfile 须为独立参数且在 -i 前）"
        );
        assert_eq!(
            island_argv("/bin/bash", "echo hi", None),
            vec![
                "/bin/bash".to_owned(),
                "-i".to_owned(),
                "-c".to_owned(),
                "history -r 2>/dev/null; echo hi".to_owned(),
            ],
            "rcfile 不可用时退回普通交互模式"
        );
        assert_eq!(
            island_argv("/bin/zsh", "echo hi", Some("/tmp/rc")),
            vec![
                "/bin/zsh".to_owned(),
                "-i".to_owned(),
                "-c".to_owned(),
                "echo hi".to_owned(),
            ],
            "非 bash 不使用 --rcfile"
        );
    }

    #[test]
    #[cfg(unix)]
    fn island_rc_file_contains_history_preload() {
        let path = island_rc_path().expect("临时目录应可写");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "history -r 2>/dev/null\n");
    }

    #[test]
    fn sanitize_destroys_dangling_wide_at_boundary() {
        let mut parser = vt100::Parser::new(4, 10, 100);
        parser.process("\x1b[3;9H中文".as_bytes());
        assert!(
            parser.screen().cell(2, 8).is_some_and(|c| c.is_wide()),
            "前置：宽字符对应位于 8-9 列"
        );
        sanitize_resize_boundary(&mut parser, 9);
        assert!(
            !parser.screen().cell(2, 8).is_some_and(|c| c.is_wide()),
            "收窄前边界列上的宽字符首格应被空格覆盖"
        );
        parser.screen_mut().set_size(4, 9);
        assert!(
            !parser.screen().cell(2, 8).is_some_and(|c| c.is_wide()),
            "set_size 裁剪后不应残留悬挂宽字符"
        );
    }

    #[test]
    #[cfg(unix)]
    fn bash_islands_preload_history_file() {
        assert_eq!(
            island_command_line("/bin/bash", "echo hi"),
            "history -r 2>/dev/null; echo hi",
            "bash 的 -i -c 不自动加载历史文件，需显式读取"
        );
        assert_eq!(
            island_command_line("/usr/bin/bash", "echo hi"),
            "history -r 2>/dev/null; echo hi"
        );
        assert_eq!(
            island_command_line("/bin/sh", "echo hi"),
            "echo hi",
            "sh 无历史文件概念"
        );
        assert_eq!(
            island_command_line("/bin/zsh", "echo hi"),
            "echo hi",
            "zsh 交互模式自动加载历史，且其 history -r 语义不同，不可注入"
        );
    }

    #[test]
    #[cfg(unix)]
    fn island_shell_resolves_user_shell_with_interactive_flag() {
        let cwd = std::env::temp_dir();
        let builder = shell_command("echo hi", &cwd);
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        let argv: Vec<String> = builder
            .get_argv()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(argv[0], shell, "岛应与终端同源解析 shell");
        let i_pos = argv.iter().position(|a| a == "-i");
        let c_pos = argv.iter().position(|a| a == "-c");
        assert!(
            i_pos.is_some_and(|i| i > 0 && c_pos.is_some_and(|c| c > i)),
            "岛应以交互模式运行命令: {argv:?}"
        );
        assert!(
            argv.last().is_some_and(|a| a.ends_with("echo hi")),
            "命令应为最后一个参数（可能带历史预载前缀）: {argv:?}"
        );
        assert_eq!(
            builder.get_cwd().map(|c| c.to_string_lossy().into_owned()),
            Some(cwd.to_string_lossy().into_owned()),
            "岛应以指定 cwd 启动"
        );
    }
}
