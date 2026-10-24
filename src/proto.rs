//! Nachrichten zwischen Konsolen und vom Steuer-CLI: JSON-Zeilen mit Größengrenze.

use std::io::{self, BufRead, Write};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 2;

/// Gegenseite spricht eine andere Protokollversion (anderes bridget-Binary).
pub fn check_version(v: u32) -> Result<()> {
    if v != VERSION {
        bail!(
            "{}",
            if crate::locale::german() {
                format!("Protokollversion {v}, hier {VERSION}: verschiedene bridget-Versionen?")
            } else {
                format!(
                    "protocol version {v}, local version {VERSION}: different bridget versions?"
                )
            }
        );
    }
    Ok(())
}
/// Höchstgröße einer JSON-Zeile. Längere Zeilen beenden die Verbindung.
pub const MAX_LINE: usize = 1024 * 1024;
/// Höchstgröße eines übertragenen Textes.
pub const MAX_TEXT: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Msg {
    /// Erste Nachricht einer Konsolen-Verbindung (beide Richtungen).
    Hello {
        v: u32,
        from: String,
    },
    /// Text zum Einfügen. Ein Enter gibt es in der Brücke nicht.
    Paste {
        v: u32,
        id: u64,
        from: String,
        text: String,
    },
    /// Bestätigung für `Paste`; `note` sagt, was die Gegenseite damit gemacht hat.
    Ack {
        id: u64,
        note: String,
    },
    /// Ablehnung mit Grund (z.B. schon verbunden, zu groß).
    Error {
        message: String,
    },
    Bye,
    /// Steuer-CLI → Konsole. Antwort: `Status` oder `Error`/`Ack`.
    Control {
        v: u32,
        cmd: Control,
    },
    Status {
        name: String,
        partner: Option<String>,
        program: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Control {
    Status,
    Link {
        to: String,
    },
    Unlink,
    Send {
        what: What,
    },
    /// Text einmalig einfügen, ohne Verbindung (z.B. `Ctrl-Space 3 Enter`).
    Paste {
        from: String,
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum What {
    Last,
    Screen,
}

pub fn write(out: &mut impl Write, msg: &Msg) -> Result<()> {
    write_json(out, msg)
}

/// Liest eine Zeile, höchstens `MAX_LINE` Bytes. `Ok(None)` bei sauberem Ende.
pub fn read(input: &mut impl BufRead) -> Result<Option<Msg>> {
    read_json(input)
}

/// Beliebige Nachricht als JSON-Zeile (auch für `duo`).
pub fn write_json<T: Serialize>(out: &mut impl Write, msg: &T) -> Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    if line.len() > MAX_LINE {
        bail!(
            "{}",
            if crate::locale::german() {
                format!("Nachricht zu groß ({} Bytes)", line.len())
            } else {
                format!("message too large ({} bytes)", line.len())
            }
        );
    }
    line.push(b'\n');
    out.write_all(&line)?;
    out.flush()?;
    Ok(())
}

pub fn read_json<T: serde::de::DeserializeOwned>(input: &mut impl BufRead) -> Result<Option<T>> {
    let mut line = Vec::new();
    loop {
        let buf = match input.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        if buf.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            bail!(
                "{}",
                tr!(
                    "connection ended in the middle of a message",
                    "Verbindung mitten in einer Nachricht beendet"
                )
            );
        }
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..i], i + 1),
            None => (buf, buf.len()),
        };
        if line.len() + chunk.len() > MAX_LINE {
            bail!(
                "{}",
                if crate::locale::german() {
                    format!("Nachricht über {MAX_LINE} Bytes, Verbindung wird beendet")
                } else {
                    format!("message exceeds {MAX_LINE} bytes; closing connection")
                }
            );
        }
        line.extend_from_slice(chunk);
        let found_newline = done > chunk.len();
        input.consume(done);
        if found_newline {
            return Ok(Some(serde_json::from_slice(&line)?));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn roundtrip() {
        let msgs = [
            Msg::Hello { v: VERSION, from: "A".into() },
            Msg::Paste { v: VERSION, id: 7, from: "A".into(), text: "a\nb".into() },
            Msg::Control { v: VERSION, cmd: Control::Send { what: What::Last } },
            Msg::Control { v: VERSION, cmd: Control::Link { to: "B".into() } },
            Msg::Control {
                v: VERSION,
                cmd: Control::Paste { from: "1".into(), text: "x\ny".into() },
            },
            Msg::Bye,
        ];
        let mut buf = Vec::new();
        for m in &msgs {
            write(&mut buf, m).unwrap();
        }
        let mut r = Cursor::new(buf);
        for m in &msgs {
            assert_eq!(read(&mut r).unwrap().as_ref(), Some(m));
        }
        assert_eq!(read(&mut r).unwrap(), None);
    }

    #[test]
    fn wire_format_is_tagged_json() {
        let mut buf = Vec::new();
        write(&mut buf, &Msg::Ack { id: 3, note: "ok".into() }).unwrap();
        assert_eq!(buf, b"{\"type\":\"ack\",\"id\":3,\"note\":\"ok\"}\n");
    }

    #[test]
    fn rejects_overlong_line_without_reading_it_all() {
        let mut data = vec![b'x'; MAX_LINE + 10];
        data.push(b'\n');
        let mut r = io::BufReader::with_capacity(4096, Cursor::new(data));
        assert!(read(&mut r).is_err());
    }

    #[test]
    fn truncated_message_is_an_error() {
        let mut r = Cursor::new(b"{\"type\":\"bye\"".to_vec());
        assert!(read(&mut r).is_err());
    }
}
