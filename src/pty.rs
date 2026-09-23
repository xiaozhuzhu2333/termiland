use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const CURSOR_QUERY: &[u8] = b"\x1b[6n";
const OSC_START: &[u8] = b"\x1b]";
const OSC_BUFFER_CAP: usize = 4096;

pub struct PtySession {
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    cursor: Arc<Mutex<(u16, u16)>>,
    #[cfg_attr(not(windows), allow(dead_code))]
    cwd: Arc<Mutex<Option<String>>>,
    child: Box<dyn Child + Send + Sync>,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    child_pid: Option<u32>,
    reader_thread: Option<JoinHandle<()>>,
    output: Receiver<Vec<u8>>,
    alive: bool,
    finished: bool,
}

impl PtySession {
    pub fn spawn(rows: u16, cols: u16) -> Result<Self> {
        Self::spawn_command(default_shell_command(), rows, cols)
    }

    pub fn spawn_command(cmd: CommandBuilder, rows: u16, cols: u16) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("打开 PTY 失败")?;
        let child = pair.slave.spawn_command(cmd).context("启动子进程失败")?;
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .context("克隆 PTY 读端失败")?;
        let writer = pair.master.take_writer().context("获取 PTY 写端失败")?;
        let writer = Arc::new(Mutex::new(writer));
        let thread_writer = Arc::clone(&writer);
        let cursor = Arc::new(Mutex::new((0, 0)));
        let thread_cursor = Arc::clone(&cursor);
        let cwd = Arc::new(Mutex::new(None));
        let thread_cwd = Arc::clone(&cwd);
        let (tx, rx) = mpsc::channel();
        let reader_thread = std::thread::Builder::new()
            .name("pty-reader".to_owned())
            .spawn(move || {
                let mut carry = Vec::new();
                let mut osc_buffer = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            respond_to_cursor_queries(
                                &mut carry,
                                &buf[..n],
                                &thread_writer,
                                &thread_cursor,
                            );
                            osc_buffer.extend_from_slice(&buf[..n]);
                            if let Some(path) = extract_cwd_report(&mut osc_buffer)
                                && let Ok(mut slot) = thread_cwd.lock()
                            {
                                *slot = Some(path);
                            }
                            if tx.send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .context("启动 PTY 读线程失败")?;
        Ok(Self {
            master: Some(pair.master),
            writer: Some(writer),
            cursor,
            cwd,
            child_pid: child.process_id(),
            child,
            reader_thread: Some(reader_thread),
            output: rx,
            alive: true,
            finished: false,
        })
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn child_pid(&self) -> Option<u32> {
        self.child_pid
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn reported_cwd(&self) -> Option<String> {
        self.cwd.lock().ok().and_then(|slot| slot.clone())
    }

    pub fn poll_output(&mut self) -> Vec<u8> {
        let mut drained = Vec::new();
        loop {
            match self.output.try_recv() {
                Ok(chunk) => drained.extend_from_slice(&chunk),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.finished = true;
                    break;
                }
            }
        }
        if self.alive
            && let Ok(Some(_)) = self.child.try_wait()
        {
            self.alive = false;
            self.writer.take();
            self.master.take();
        }
        drained
    }

    pub fn is_alive(&self) -> bool {
        self.alive
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        if let Some(writer) = &self.writer {
            let mut w = writer.lock().unwrap();
            w.write_all(bytes).context("写入 PTY 失败")?;
            w.flush().context("刷新 PTY 失败")?;
        }
        Ok(())
    }

    pub fn set_cursor_position(&self, row: u16, col: u16) {
        *self.cursor.lock().unwrap() = (row, col);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        if let Some(master) = &self.master {
            master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .context("调整 PTY 尺寸失败")?;
        }
        Ok(())
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.writer.take();
        self.master.take();
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}

fn respond_to_cursor_queries(
    carry: &mut Vec<u8>,
    chunk: &[u8],
    writer: &Mutex<Box<dyn Write + Send>>,
    cursor: &Mutex<(u16, u16)>,
) {
    let mut window = Vec::with_capacity(carry.len() + chunk.len());
    window.extend_from_slice(carry);
    window.extend_from_slice(chunk);

    let mut rest: &[u8] = &window;
    while let Some(pos) = rest
        .windows(CURSOR_QUERY.len())
        .position(|w| w == CURSOR_QUERY)
    {
        let report = cursor_report(cursor);
        if let Ok(mut w) = writer.lock() {
            let _ = w.write_all(report.as_bytes());
            let _ = w.flush();
        }
        rest = &rest[pos + CURSOR_QUERY.len()..];
    }

    let keep = CURSOR_QUERY.len() - 1;
    let start = rest.len().saturating_sub(keep);
    *carry = rest[start..].to_vec();
}

fn cursor_report(cursor: &Mutex<(u16, u16)>) -> String {
    let (row, col) = *cursor.lock().unwrap();
    format!("\x1b[{};{}R", row + 1, col + 1)
}

fn default_shell_command() -> CommandBuilder {
    let mut cmd = CommandBuilder::new_default_prog();
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    #[cfg(unix)]
    if let Some(prompt) = history_flush_prompt(std::env::var("PROMPT_COMMAND").ok().as_deref()) {
        cmd.env("PROMPT_COMMAND", prompt);
    }
    #[cfg(windows)]
    if let Some(prompt) = osc7_prompt(std::env::var("PROMPT").ok().as_deref()) {
        cmd.env("PROMPT", prompt);
    }
    cmd
}

#[cfg(windows)]
fn osc7_prompt(existing: Option<&str>) -> Option<String> {
    let tail = match existing {
        Some(value) if !value.is_empty() => value,
        _ => "$P$G",
    };
    if tail.contains("]7;") || tail.contains("]9;9;") {
        return None;
    }
    Some(format!("$E]7;file:///$P$E\\$E]9;9;$P$E\\{tail}"))
}

fn extract_cwd_report(buffer: &mut Vec<u8>) -> Option<String> {
    let mut last = None;
    loop {
        let Some(start) = find_subsequence(buffer, OSC_START) else {
            if buffer.len() > OSC_START.len() {
                buffer.drain(..buffer.len() - OSC_START.len());
            }
            return last;
        };
        let rest = &buffer[start + OSC_START.len()..];
        let mut end = None;
        let mut term_len = 0;
        for (i, &b) in rest.iter().enumerate() {
            if b == 0x07 {
                end = Some(i);
                term_len = 1;
                break;
            }
            if b == 0x1b && rest.get(i + 1) == Some(&b'\\') {
                end = Some(i);
                term_len = 2;
                break;
            }
        }
        let Some(end) = end else {
            buffer.drain(..start);
            if buffer.len() > OSC_BUFFER_CAP {
                buffer.clear();
            }
            return last;
        };
        if let Some(path) = parse_cwd_payload(&rest[..end]) {
            last = Some(path);
        }
        buffer.drain(..start + OSC_START.len() + end + term_len);
    }
}

fn parse_cwd_payload(payload: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(payload).ok()?;
    if let Some(rest) = text.strip_prefix("7;") {
        let rest = rest.strip_prefix("file://")?;
        let path = match rest.find('/') {
            Some(index) if index > 0 => &rest[index..],
            _ => rest,
        };
        let path = match path.strip_prefix('/') {
            Some(stripped) if !stripped.is_empty() && stripped.as_bytes().get(1) == Some(&b':') => {
                stripped
            }
            _ => path,
        };
        return (!path.is_empty() && path != "/").then(|| path.to_owned());
    }
    if let Some(rest) = text.strip_prefix("9;9;") {
        let path = rest.trim_matches('"');
        return (!path.is_empty()).then(|| path.to_owned());
    }
    None
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(unix)]
fn history_flush_prompt(existing: Option<&str>) -> Option<String> {
    match existing {
        Some(value) if value.contains("history -a") => None,
        Some("") => Some("history -a".to_owned()),
        Some(value) => Some(format!("history -a; {value}")),
        None => Some("history -a".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_report_parses_osc7_and_windows_99() {
        assert_eq!(
            parse_cwd_payload(b"7;file:///C:\\Users\\z"),
            Some("C:\\Users\\z".to_owned()),
            "OSC 7 的 file:/// 前缀应剥除并保留盘符路径"
        );
        assert_eq!(
            parse_cwd_payload(b"7;file://host/home/u"),
            Some("/home/u".to_owned()),
            "带主机的 OSC 7 应取路径部分"
        );
        assert_eq!(
            parse_cwd_payload(b"9;9;\"C:\\Program Files\""),
            Some("C:\\Program Files".to_owned()),
            "OSC 9;9 应剥除引号"
        );
        assert_eq!(parse_cwd_payload(b"0;title"), None, "标题序列应忽略");
        assert_eq!(parse_cwd_payload(b"7;file:///"), None);
    }

    #[test]
    fn cwd_report_extracts_across_split_chunks() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"junk\x1b]7;file:///");
        assert_eq!(extract_cwd_report(&mut buffer), None, "未终结的序列应等待");
        buffer.extend_from_slice(b"C:\\x\x1b\\rest");
        assert_eq!(
            extract_cwd_report(&mut buffer),
            Some("C:\\x".to_owned()),
            "跨块序列应拼接提取"
        );

        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"a\x1b]9;9;C:\\y\x07b\x1b]9;9;C:\\z\x07");
        assert_eq!(
            extract_cwd_report(&mut buffer),
            Some("C:\\z".to_owned()),
            "多条取最后一条"
        );

        let mut buffer = Vec::new();
        buffer.extend_from_slice(&vec![b'x'; 10_000]);
        extract_cwd_report(&mut buffer);
        assert!(
            buffer.len() <= OSC_BUFFER_CAP.max(OSC_START.len()),
            "无序列时缓冲区应有界"
        );
    }

    #[test]
    #[cfg(windows)]
    fn prompt_injection_preserves_existing_prompt() {
        assert_eq!(
            osc7_prompt(None).as_deref(),
            Some("$E]7;file:///$P$E\\$E]9;9;$P$E\\$P$G"),
            "无自定义 PROMPT 时保持默认外观"
        );
        assert_eq!(
            osc7_prompt(Some("A$G")).as_deref(),
            Some("$E]7;file:///$P$E\\$E]9;9;$P$E\\A$G"),
            "已有 PROMPT 应拼接在后"
        );
        assert_eq!(
            osc7_prompt(Some("$E]7;file:///$P$E\\$G")),
            None,
            "已注入过的不重复注入"
        );
    }

    #[test]
    #[cfg(windows)]
    fn cmd_reports_cwd_via_osc() {
        let mut session = PtySession::spawn(20, 80).expect("spawn");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut sink = Vec::new();
        while session.reported_cwd().is_none() && std::time::Instant::now() < deadline {
            sink.extend(session.poll_output());
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let reported = session
            .reported_cwd()
            .expect("cmd 首个提示符应上报 cwd（若失败说明 ConPTY 未透传 OSC，需换 PEB 方案）");
        assert_eq!(
            std::path::PathBuf::from(reported),
            std::env::current_dir().unwrap(),
            "终端启动目录应与 termiland 进程目录一致"
        );
        drop(sink);
    }

    #[test]
    #[cfg(unix)]
    fn prompt_command_injection_combines() {
        assert_eq!(
            history_flush_prompt(None).as_deref(),
            Some("history -a"),
            "无既有值时注入基础落盘命令"
        );
        assert_eq!(
            history_flush_prompt(Some("")).as_deref(),
            Some("history -a")
        );
        assert_eq!(
            history_flush_prompt(Some("foo")).as_deref(),
            Some("history -a; foo"),
            "既有值应追加在后"
        );
        assert_eq!(
            history_flush_prompt(Some("history -a; foo")),
            None,
            "已含 history -a 时不重复注入"
        );
    }

    struct Sink(std::sync::mpsc::Sender<Vec<u8>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.send(buf.to_vec());
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn strip_ansi(bytes: &[u8]) -> String {
        let text = String::from_utf8_lossy(bytes);
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('[') => {
                    for c2 in chars.by_ref() {
                        if c2.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c2) = chars.next() {
                        if c2 == '\x07' {
                            break;
                        }
                        if c2 == '\x1b' && chars.next() == Some('\\') {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn responds_to_cursor_query_split_across_chunks() {
        let (tx, rx) = mpsc::channel();
        let writer = Mutex::new(Box::new(Sink(tx)) as Box<dyn Write + Send>);
        let cursor = Mutex::new((0, 0));
        let mut carry = Vec::new();

        respond_to_cursor_queries(&mut carry, b"abc\x1b[", &writer, &cursor);
        assert!(rx.try_recv().is_err());

        respond_to_cursor_queries(&mut carry, b"6n tail", &writer, &cursor);
        assert_eq!(rx.try_recv().unwrap(), b"\x1b[1;1R".to_vec());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn responds_with_current_cursor_position() {
        let (tx, rx) = mpsc::channel();
        let writer = Mutex::new(Box::new(Sink(tx)) as Box<dyn Write + Send>);
        let cursor = Mutex::new((5, 7));
        let mut carry = Vec::new();

        respond_to_cursor_queries(&mut carry, b"\x1b[6n", &writer, &cursor);
        assert_eq!(rx.try_recv().unwrap(), b"\x1b[6;8R".to_vec());
    }

    #[test]
    fn responds_to_each_cursor_query_in_chunk() {
        let (tx, rx) = mpsc::channel();
        let writer = Mutex::new(Box::new(Sink(tx)) as Box<dyn Write + Send>);
        let cursor = Mutex::new((2, 3));
        let mut carry = Vec::new();

        respond_to_cursor_queries(&mut carry, b"\x1b[6nX\x1b[6n", &writer, &cursor);
        assert_eq!(rx.try_recv().unwrap(), b"\x1b[3;4R".to_vec());
        assert_eq!(rx.try_recv().unwrap(), b"\x1b[3;4R".to_vec());
        assert!(rx.try_recv().is_err());
    }

    #[cfg(windows)]
    #[test]
    fn lifecycle_with_short_lived_command() {
        let mut cmd = CommandBuilder::new("cmd");
        cmd.arg("/C");
        cmd.arg("echo termiland-pty-ok");

        let mut session = PtySession::spawn_command(cmd, 24, 80).expect("spawn");
        assert!(
            session.child_pid().is_some_and(|pid| pid > 0),
            "应暴露子进程 pid"
        );
        let mut all = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !session.is_finished() && std::time::Instant::now() < deadline {
            all.extend_from_slice(&session.poll_output());
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let text = strip_ansi(&all);
        assert!(session.is_finished(), "reader did not reach EOF");
        assert!(text.contains("termiland-pty-ok"), "output was: {text:?}");
        drop(session);
    }

    #[cfg(unix)]
    #[test]
    fn lifecycle_with_short_lived_command() {
        let mut cmd = CommandBuilder::new("echo");
        cmd.arg("termiland-pty-ok");

        let mut session = PtySession::spawn_command(cmd, 24, 80).expect("spawn");
        let mut all = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !session.is_finished() && std::time::Instant::now() < deadline {
            all.extend_from_slice(&session.poll_output());
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let text = strip_ansi(&all);
        assert!(session.is_finished(), "reader did not reach EOF");
        assert!(text.contains("termiland-pty-ok"), "output was: {text:?}");
        drop(session);
    }
}
