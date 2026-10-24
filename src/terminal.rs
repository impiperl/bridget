//! Die echte Konsole: Raw-Mode, Größe, Fenstertitel.

use std::io::Write;

use anyhow::{Context, Result, bail};

/// Ohne Terminal als Eingabe geht nichts; vor allem anderen prüfen.
pub fn require_tty() -> Result<()> {
    // SAFETY: isatty liest nur den Zustand des Deskriptors.
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
        bail!(
            "{}",
            tr!(
                "bridget requires a terminal on standard input",
                "bridget braucht ein Terminal als Eingabe"
            )
        );
    }
    Ok(())
}

/// Schaltet den Raw-Mode ein und beim Drop wieder aus, auch bei Fehlern.
/// Ein Panic-Hook setzt das Terminal zusätzlich zurück, bevor die Meldung erscheint.
pub struct RawGuard;

impl RawGuard {
    pub fn enable() -> Result<Self> {
        require_tty()?;
        crossterm::terminal::enable_raw_mode()
            .context(tr!("enable raw mode", "Raw-Mode einschalten"))?;

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = crossterm::terminal::disable_raw_mode();
            previous(info);
        }));
        Ok(RawGuard)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// (Zeilen, Spalten); Rückfall 24×80, wenn die Größe nicht lesbar ist.
pub fn size() -> (u16, u16) {
    match crossterm::terminal::size() {
        Ok((cols, rows)) if cols > 0 && rows > 0 => (rows, cols),
        _ => (24, 80),
    }
}

/// Setzt den Fenstertitel (OSC 2). Steuerzeichen werden entfernt.
pub fn set_title(out: &mut impl Write, title: &str) -> std::io::Result<()> {
    let clean: String = title.chars().filter(|c| !c.is_control()).collect();
    write!(out, "\x1b]2;{clean}\x07")?;
    out.flush()
}

/// Fenstertitel auf den Titel-Stapel legen bzw. zurückholen (xterm, VTE). Terminals ohne
/// Stapel ignorieren das.
pub fn push_title(out: &mut impl Write) -> std::io::Result<()> {
    out.write_all(b"\x1b[22;0t")?;
    out.flush()
}

pub fn pop_title(out: &mut impl Write) -> std::io::Result<()> {
    out.write_all(b"\x1b[23;0t")?;
    out.flush()
}

pub fn bell(out: &mut impl Write) -> std::io::Result<()> {
    out.write_all(b"\x07")?;
    out.flush()
}
