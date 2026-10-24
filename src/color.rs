//! Farbliche Markierung: Der Hintergrund der Konsole wird leicht eingefärbt.
//!
//! Ohne tmux: Hintergrund mit `OSC 11 ?` abfragen, Mischfarbe mit `OSC 11` setzen und beim
//! Ende den alten Wert zurückschreiben. Damit eine verspätete Antwort nicht beim Programm
//! landet, wird danach `DA1` (`ESC [ c`) abgefragt: Darauf antwortet jedes Terminal, und die
//! Antworten kommen in Reihenfolge. In tmux (2.7 reicht `OSC 11` nicht weiter) wird nur der
//! eigene Pane per `select-pane -P` eingefärbt; den Grundton liefert `BRIDGET_BG`.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub const PALETTE: [(&str, Rgb); 6] = [
    ("blue", Rgb(0x4a, 0x90, 0xe2)),
    ("green", Rgb(0x4c, 0xaf, 0x50)),
    ("purple", Rgb(0x9c, 0x6a, 0xde)),
    ("orange", Rgb(0xf5, 0xa6, 0x23)),
    ("red", Rgb(0xe0, 0x50, 0x50)),
    ("cyan", Rgb(0x26, 0xb5, 0xb5)),
];

const QUERY_TIMEOUT: Duration = Duration::from_millis(500);

impl Rgb {
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    /// `#rrggbb` oder die Antwortform `rgb:R/G/B` mit 1–4 Hex-Ziffern je Kanal.
    pub fn parse(s: &str) -> Option<Rgb> {
        if let Some(hex) = s.strip_prefix('#') {
            if hex.len() != 6 {
                return None;
            }
            let c = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
            return Some(Rgb(c(0)?, c(2)?, c(4)?));
        }
        let parts: Vec<&str> = s.strip_prefix("rgb:")?.split('/').collect();
        if parts.len() != 3 {
            return None;
        }
        let chan = |p: &str| -> Option<u8> {
            if p.is_empty() || p.len() > 4 {
                return None;
            }
            let v = u32::from_str_radix(p, 16).ok()?;
            let max = (1u32 << (4 * p.len())) - 1;
            Some(((v * 255 + max / 2) / max) as u8)
        };
        Some(Rgb(chan(parts[0])?, chan(parts[1])?, chan(parts[2])?))
    }

    fn is_dark(self) -> bool {
        let l = 0.2126 * self.0 as f32 + 0.7152 * self.1 as f32 + 0.0722 * self.2 as f32;
        l < 128.0
    }
}

/// Mischt `hue` in den Hintergrund: kräftig genug zum Erkennen, schwach genug für Text.
pub fn tint(base: Rgb, hue: Rgb) -> Rgb {
    let s = if base.is_dark() { 0.16 } else { 0.12 };
    let mix = |b: u8, h: u8| (b as f32 + (h as f32 - b as f32) * s).round() as u8;
    Rgb(mix(base.0, hue.0), mix(base.1, hue.1), mix(base.2, hue.2))
}

/// `--color` bzw. `BRIDGET_COLOR`: Farbname, `#rrggbb` oder `aus`. `None` = nicht einfärben.
pub fn parse_choice(s: &str) -> Result<Option<Rgb>> {
    let key = s.trim().to_lowercase();
    if matches!(key.as_str(), "aus" | "off" | "none" | "nein" | "no") {
        return Ok(None);
    }
    let index = match key.as_str() {
        "blue" | "blau" => Some(0),
        "green" | "grün" | "gruen" => Some(1),
        "purple" | "lila" => Some(2),
        "orange" => Some(3),
        "red" | "rot" => Some(4),
        "cyan" | "turquoise" | "türkis" | "tuerkis" => Some(5),
        _ => None,
    };
    if let Some(index) = index {
        return Ok(Some(PALETTE[index].1));
    }
    if let Some(rgb) = Rgb::parse(&key) {
        return Ok(Some(rgb));
    }
    let names: Vec<&str> = PALETTE.iter().map(|(n, _)| *n).collect();
    bail!(
        "{}",
        if crate::locale::german() {
            format!(
                "Unbekannte Farbe {s:?}: blau, grün, lila, orange, rot, türkis, #rrggbb oder aus"
            )
        } else {
            format!("Unknown color {s:?}: {}, #rrggbb or off", names.join(", "))
        }
    )
}

pub fn by_index(i: usize) -> Rgb {
    PALETTE[i % PALETTE.len()].1
}

/// Farbe einer duo-Konsole: Seite A blau, Seite B grün; `BRIDGET_COLOR` hat Vorrang.
pub fn for_duo(side: usize) -> Result<Option<Rgb>> {
    match std::env::var("BRIDGET_COLOR") {
        Ok(c) => parse_choice(&c),
        Err(_) => Ok(Some(by_index(side))),
    }
}

/// Sucht in einer Terminal-Antwort `ESC ] 11 ; <farbe> (BEL | ESC \)`.
pub fn parse_osc11_reply(bytes: &[u8]) -> Option<Rgb> {
    let start = find(bytes, b"\x1b]11;")? + 5;
    let rest = &bytes[start..];
    let end = rest.iter().position(|&b| b == 0x07 || b == 0x1b)?;
    Rgb::parse(std::str::from_utf8(&rest[..end]).ok()?)
}

/// Ist die DA1-Antwort (`ESC [ ? … c`) vollständig da?
fn has_da1_reply(bytes: &[u8]) -> bool {
    let Some(start) = find(bytes, b"\x1b[?") else {
        return false;
    };
    bytes[start + 3..]
        .iter()
        .find(|b| !(b.is_ascii_digit() || **b == b';'))
        .is_some_and(|&b| b == b'c')
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Fragt die echte Konsole nach ihrem Hintergrund. Nur im Raw-Mode und bevor jemand
/// anderes von stdin liest. Tasten, die in diesem Moment kommen, gehen verloren.
pub fn query_background() -> Option<Rgb> {
    {
        let mut out = std::io::stdout().lock();
        out.write_all(b"\x1b]11;?\x1b\\\x1b[c").ok()?;
        out.flush().ok()?;
    }
    let deadline = Instant::now() + QUERY_TIMEOUT;
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    while !has_da1_reply(&got) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || got.len() > 4096 {
            break;
        }
        let mut pfd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        // SAFETY: pollfd zeigt auf gültigen Speicher, fd 0 ist offen.
        let ready = unsafe { libc::poll(&mut pfd, 1, left.as_millis() as i32) };
        if ready <= 0 {
            break;
        }
        // SAFETY: buf ist gültig und groß genug.
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
        got.extend_from_slice(&buf[..n as usize]);
    }
    parse_osc11_reply(&got)
}

enum Restore {
    Osc(Rgb),
    Tmux(String),
}

/// Färbt beim Anlegen ein und setzt beim Drop zurück.
pub struct Marker {
    restore: Option<Restore>,
}

impl Marker {
    pub fn none() -> Marker {
        Marker { restore: None }
    }

    /// `hue = None`: nichts tun. Muss im Raw-Mode aufgerufen werden (Abfrage).
    pub fn apply(hue: Option<Rgb>) -> Marker {
        let Some(hue) = hue else {
            return Marker::none();
        };
        let env_bg = std::env::var("BRIDGET_BG").ok().and_then(|s| Rgb::parse(&s));
        if let Ok(pane) = std::env::var("TMUX_PANE")
            && std::env::var_os("TMUX").is_some()
        {
            let Some(base) = env_bg else {
                return Marker::none();
            };
            if tmux_pane_bg(&pane, &tint(base, hue).hex()) {
                return Marker { restore: Some(Restore::Tmux(pane)) };
            }
            return Marker::none();
        }
        let Some(base) = query_background() else {
            return Marker::none();
        };
        set_osc11(tint(base, hue));
        Marker { restore: Some(Restore::Osc(base)) }
    }

    pub fn reset(&mut self) {
        match self.restore.take() {
            Some(Restore::Osc(base)) => set_osc11(base),
            Some(Restore::Tmux(pane)) => {
                tmux_pane_bg(&pane, "default");
            }
            None => {}
        }
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        self.reset();
    }
}

fn set_osc11(rgb: Rgb) {
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "\x1b]11;{}\x1b\\", rgb.hex()).and_then(|_| out.flush());
}

fn tmux_pane_bg(pane: &str, bg: &str) -> bool {
    std::process::Command::new("tmux")
        .args(["select-pane", "-t", pane, "-P", &format!("bg={bg}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors() {
        assert_eq!(Rgb::parse("#1e1e2e"), Some(Rgb(0x1e, 0x1e, 0x2e)));
        assert_eq!(Rgb::parse("rgb:ffff/8080/0000"), Some(Rgb(255, 128, 0)));
        assert_eq!(Rgb::parse("rgb:f/8/0"), Some(Rgb(255, 136, 0)));
        assert_eq!(Rgb::parse("rgb:ff/80"), None);
        assert_eq!(Rgb::parse("#12345"), None);
        assert_eq!(Rgb::parse("blau"), None);
    }

    #[test]
    fn parses_osc11_reply_with_bel_or_st_and_noise() {
        let st = b"x\x1b]11;rgb:3030/0a0a/2424\x1b\\\x1b[?62;22c";
        assert_eq!(parse_osc11_reply(st), Some(Rgb(0x30, 0x0a, 0x24)));
        assert_eq!(parse_osc11_reply(b"\x1b]11;rgb:ffff/ffff/ffff\x07"), Some(Rgb(255, 255, 255)));
        assert_eq!(parse_osc11_reply(b"\x1b[?62;22c"), None);
        assert!(has_da1_reply(st));
        assert!(!has_da1_reply(b"\x1b[?62;2"));
    }

    #[test]
    fn tint_stays_close_to_base() {
        let dark = tint(Rgb(0, 0, 0), Rgb(0x4a, 0x90, 0xe2));
        assert_eq!(dark, Rgb(12, 23, 36));
        let light = tint(Rgb(255, 255, 255), Rgb(0x4a, 0x90, 0xe2));
        assert_eq!(light, Rgb(233, 242, 252));
    }

    #[test]
    fn choices() {
        assert_eq!(parse_choice("Blau").unwrap(), Some(PALETTE[0].1));
        assert_eq!(parse_choice("gruen").unwrap(), Some(PALETTE[1].1));
        assert_eq!(parse_choice("tuerkis").unwrap(), Some(PALETTE[5].1));
        assert_eq!(parse_choice("aus").unwrap(), None);
        assert_eq!(parse_choice("#102030").unwrap(), Some(Rgb(16, 32, 48)));
        assert!(parse_choice("pink").is_err());
        assert_eq!(by_index(7), PALETTE[1].1);
    }
}
