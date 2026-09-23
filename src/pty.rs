use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const CURSOR_QUERY: &[u8] = b"\x1b[6n";

pub struct PtySession {
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Arc<Mutex<Box<dyn Write + Send>>>>,
    cursor: Arc<Mutex<(u16, u16)>>,
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
        let (tx, rx) = mpsc::channel();
        let reader_thread = std::thread::Builder::new()
            .name("pty-reader".to_owned())
            .spawn(move || {
                let mut carry = Vec::new();
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

#[cfg(unix)]
fn default_shell_command() -> CommandBuilder {
    let mut cmd = CommandBuilder::new_default_prog();
    if let Some(prompt) = history_flush_prompt(std::env::var("PROMPT_COMMAND").ok().as_deref()) {
        cmd.env("PROMPT_COMMAND", prompt);
    }
    cmd
}

#[cfg(not(unix))]
fn default_shell_command() -> CommandBuilder {
    CommandBuilder::new_default_prog()
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
