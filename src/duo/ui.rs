//! Anzeige und Tastatur für `duo` (Koordinator und Ansicht benutzen dasselbe).
//!
//! Zeilenorientiert statt Vollbild: Jede Seite schreibt fortlaufend in ihre Konsole, der
//! Zustand steht in eigenen Statuszeilen und im Fenstertitel.

use std::io::Write;
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::agent::AgentEvent;
use super::machine::Side;
use crate::terminal;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Show {
    /// Neuer Aufruf beginnt.
    TurnStart {
        side: Side,
        who: String,
        round: u32,
        max: u32,
    },
    Event {
        side: Side,
        event: AgentEvent,
    },
    /// Zustandszeile (auch Fenstertitel).
    Status(String),
    /// Hinweis (gelb).
    Note(String),
}

pub struct Console {
    at_line_start: bool,
}

impl Console {
    pub fn new() -> Self {
        Console { at_line_start: true }
    }

    pub fn show(&mut self, item: &Show) {
        let mut out = String::new();
        match item {
            Show::TurnStart { side, who, round, max } => {
                self.newline(&mut out);
                out.push_str(&if crate::locale::german() {
                    format!(
                        "\r\n\x1b[1;36m▶ {side:?} · {who}\x1b[0m \x1b[2m(Antwort {} von {max})\x1b[0m\r\n",
                        round + 1
                    )
                } else {
                    format!(
                        "\r\n\x1b[1;36m▶ {side:?} · {who}\x1b[0m \x1b[2m(response {} of {max})\x1b[0m\r\n",
                        round + 1
                    )
                });
            }
            Show::Event { event, .. } => match event {
                AgentEvent::Delta(t) => self.text(&mut out, t),
                AgentEvent::Message(t) => {
                    self.newline(&mut out);
                    self.text(&mut out, t);
                    self.newline(&mut out);
                }
                AgentEvent::Tool(t) => {
                    self.newline(&mut out);
                    out.push_str(&format!("\x1b[2m  🔧 {}\x1b[0m\r\n", clean(t)));
                }
                AgentEvent::Denied(t) => {
                    self.newline(&mut out);
                    out.push_str(&if crate::locale::german() {
                        format!("\x1b[33m  ⚠ abgelehnt: {} – zum Ändern pausieren und übernehmen (u)\x1b[0m\r\n", clean(t))
                    } else {
                        format!("\x1b[33m  ⚠ denied: {} – pause and take over to make changes (u)\x1b[0m\r\n", clean(t))
                    });
                }
                AgentEvent::Done(_) => self.newline(&mut out),
                AgentEvent::Failed(why) => {
                    self.newline(&mut out);
                    out.push_str(&format!("\x1b[31m  ✗ {}\x1b[0m\r\n", clean(why)));
                }
                AgentEvent::Session(_) => {}
            },
            Show::Status(line) => {
                self.newline(&mut out);
                out.push_str(&format!("\x1b[7m {} \x1b[0m\r\n", clean(line)));
                let _ = terminal::set_title(&mut std::io::stdout().lock(), line);
            }
            Show::Note(line) => {
                self.newline(&mut out);
                out.push_str(&format!("\x1b[33m{}\x1b[0m\r\n", clean(line)));
            }
        }
        write(&out);
    }

    fn newline(&mut self, out: &mut String) {
        if !self.at_line_start {
            out.push_str("\r\n");
            self.at_line_start = true;
        }
    }

    fn text(&mut self, out: &mut String, t: &str) {
        let t = clean(t);
        if t.is_empty() {
            return;
        }
        out.push_str(&t.replace('\n', "\r\n"));
        self.at_line_start = t.ends_with('\n');
    }
}

/// Steuerzeichen aus fremdem Text entfernen (nur `\n` und `\t` bleiben), damit eine
/// Antwort keine Escape-Sequenzen ins Terminal schreiben kann.
fn clean(t: &str) -> String {
    t.chars().filter(|&c| c == '\n' || c == '\t' || !c.is_control()).collect()
}

pub fn write(s: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(s.as_bytes()).and_then(|_| out.flush());
}

/// Liest Tasten von fd 0 in einem eigenen Thread. Während einer Übernahme wird nicht
/// gelesen (`suspend`), damit alle Tasten beim interaktiven Programm ankommen.
pub struct Keys {
    suspended: Arc<AtomicBool>,
    idle: Arc<AtomicBool>,
}

impl Keys {
    pub fn start(tx: Sender<Vec<u8>>) -> Keys {
        let suspended = Arc::new(AtomicBool::new(false));
        let idle = Arc::new(AtomicBool::new(false));
        let (s, i) = (Arc::clone(&suspended), Arc::clone(&idle));
        thread::spawn(move || {
            use std::io::Read;
            // SAFETY: fd 0 bleibt offen; ManuallyDrop verhindert das Schließen.
            let mut stdin = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) });
            let mut buf = [0u8; 1024];
            loop {
                if s.load(Ordering::SeqCst) {
                    i.store(true, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
                i.store(false, Ordering::SeqCst);
                let mut fds = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
                // SAFETY: ein gültiges pollfd.
                let ready = unsafe { libc::poll(&mut fds, 1, 100) };
                if ready <= 0 || s.load(Ordering::SeqCst) {
                    continue;
                }
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Keys { suspended, idle }
    }

    /// Lesen anhalten und warten, bis der Thread wirklich nicht mehr liest.
    pub fn suspend(&self) {
        self.suspended.store(true, Ordering::SeqCst);
        while !self.idle.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn resume(&self) {
        self.suspended.store(false, Ordering::SeqCst);
    }
}

/// Einfache Zeileneingabe im Raw-Mode (für `i`): Zeichen sammeln, Enter fertig,
/// Esc abbrechen, Backspace löscht.
pub struct LineInput {
    pub text: String,
    pending: Vec<u8>,
}

pub enum LineResult {
    More,
    Done(String),
    Cancel,
}

impl LineInput {
    pub fn new() -> Self {
        LineInput { text: String::new(), pending: Vec::new() }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> LineResult {
        let mut echo = String::new();
        for &b in bytes {
            match b {
                b'\r' | b'\n' => {
                    write("\r\n");
                    return LineResult::Done(std::mem::take(&mut self.text));
                }
                0x1b | 0x03 => {
                    write(tr!(" (canceled)\r\n", " (abgebrochen)\r\n"));
                    return LineResult::Cancel;
                }
                0x7f | 0x08 => {
                    if self.text.pop().is_some() {
                        echo.push_str("\x08 \x08");
                    }
                }
                b if b < 0x20 => {}
                b => {
                    self.pending.push(b);
                    if let Ok(s) = std::str::from_utf8(&self.pending) {
                        self.text.push_str(s);
                        echo.push_str(s);
                        self.pending.clear();
                    } else if self.pending.len() > 4 {
                        self.pending.clear();
                    }
                }
            }
        }
        write(&echo);
        LineResult::More
    }
}
