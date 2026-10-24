//! Steuer-CLI: `bridget ls`, `bridget link A B`, `bridget unlink A`,
//! `bridget send A --last|--screen`.

use std::io::BufReader;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::proto::{self, Control, Msg, What};
use crate::runtime;

const TIMEOUT: Duration = Duration::from_secs(5);

fn request(name: &str, cmd: Control) -> Result<Msg> {
    let dir = runtime::dir()?;
    let stream = runtime::connect(&dir, name)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    proto::write(&mut &stream, &Msg::Control { v: proto::VERSION, cmd })?;
    match proto::read(&mut BufReader::new(stream))? {
        Some(msg) => Ok(msg),
        None => bail!(
            "{}",
            if crate::locale::german() {
                format!("{name} hat nicht geantwortet")
            } else {
                format!("{name} did not respond")
            }
        ),
    }
}

fn expect_ack(name: &str, cmd: Control) -> Result<()> {
    match request(name, cmd)? {
        Msg::Ack { note, .. } => {
            println!("{name}: {note}");
            Ok(())
        }
        Msg::Error { message } => bail!("{name}: {message}"),
        other => bail!(
            "{}",
            if crate::locale::german() {
                format!("{name}: unerwartete Antwort {other:?}")
            } else {
                format!("{name}: unexpected response {other:?}")
            }
        ),
    }
}

pub fn ls() -> Result<()> {
    let dir = runtime::dir()?;
    let names = runtime::list(&dir)?;
    if names.is_empty() {
        println!("{}", tr!("no bridget consoles", "keine bridget-Konsolen"));
    }
    for name in names {
        match request(&name, Control::Status) {
            Ok(Msg::Status { partner, program, .. }) => {
                let link = partner.map(|p| format!("⇄ {p}")).unwrap_or_default();
                println!("{name:<12} {link:<14} {program}");
            }
            _ => println!("{name:<12} ({})", tr!("not responding", "reagiert nicht")),
        }
    }
    Ok(())
}

pub fn link(a: &str, b: &str) -> Result<()> {
    expect_ack(a, Control::Link { to: b.to_string() })
}

pub fn unlink(name: &str) -> Result<()> {
    expect_ack(name, Control::Unlink)
}

pub fn send(name: &str, what: What) -> Result<()> {
    expect_ack(name, Control::Send { what })
}
