//! Content-Length framed JSON over a child process's stdio.
//!
//! LSP and DAP share this framing (`Content-Length: N\r\n\r\n<json>`), so both
//! clients sit on top of [`Transport`]. Reading happens on background threads that
//! forward [`Event`]s to the GLib main loop through an async channel.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::Value;

pub enum Event {
    Message(Value),
    Stderr(String),
    Exit(i32),
}

pub struct Transport {
    stdin: RefCell<Option<ChildStdin>>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Transport {
    pub fn spawn(argv: &[&str], cwd: &Path) -> std::io::Result<(Transport, async_channel::Receiver<Event>)> {
        let mut child = Command::new(argv[0])
            .args(&argv[1..])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let child = Arc::new(Mutex::new(Some(child)));
        let (tx, rx) = async_channel::unbounded();

        let tx_out = tx.clone();
        let child_out = child.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(msg) = read_frame(&mut reader) {
                if tx_out.send_blocking(Event::Message(msg)).is_err() {
                    break;
                }
            }
            let code = child_out
                .lock()
                .ok()
                .and_then(|mut c| c.as_mut().and_then(|c| c.wait().ok()))
                .and_then(|s| s.code())
                .unwrap_or(-1);
            let _ = tx_out.send_blocking(Event::Exit(code));
        });

        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                match line {
                    Ok(l) => {
                        if tx.send_blocking(Event::Stderr(l)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok((Transport { stdin: RefCell::new(stdin), child }, rx))
    }

    pub fn send(&self, msg: &Value) {
        let body = msg.to_string();
        if let Some(stdin) = self.stdin.borrow_mut().as_mut() {
            let _ = write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body);
            let _ = stdin.flush();
        }
    }

    pub fn kill(&self) {
        self.stdin.borrow_mut().take();
        if let Ok(mut c) = self.child.lock() {
            if let Some(c) = c.as_mut() {
                let _ = c.kill();
            }
        }
    }
}

fn read_frame(reader: &mut impl BufRead) -> Option<Value> {
    loop {
        let mut length = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                if k.trim().eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse::<usize>().ok();
                }
            }
        }
        let Some(n) = length else { continue };
        let mut body = vec![0; n];
        reader.read_exact(&mut body).ok()?;
        match serde_json::from_slice(&body) {
            Ok(v) => return Some(v),
            Err(e) => eprintln!("plars: bad message: {e}"),
        }
    }
}
