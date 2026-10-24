//! Auswahlmodus (`Ctrl-] p`): Zeilen aus Bildschirm und Verlauf markieren und senden.
//!
//! Reine Logik ohne Terminal: Tasten rein, Aktion und Bildschirminhalt raus.

use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Mark,
    Enter,
    Cancel,
    Other,
}

/// Zerlegt einen Block Tastatureingabe in Tasten.
pub fn parse_keys(input: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        let (key, len) = match rest {
            [0x1b, b'[', b'A', ..] | [0x1b, b'O', b'A', ..] => (Key::Up, 3),
            [0x1b, b'[', b'B', ..] | [0x1b, b'O', b'B', ..] => (Key::Down, 3),
            [0x1b, b'[', b'5', b'~', ..] => (Key::PageUp, 4),
            [0x1b, b'[', b'6', b'~', ..] => (Key::PageDown, 4),
            [0x1b, b'[', b'H', ..] | [0x1b, b'O', b'H', ..] => (Key::Home, 3),
            [0x1b, b'[', b'F', ..] | [0x1b, b'O', b'F', ..] => (Key::End, 3),
            [0x1b, b'[', b'1', b'~', ..] => (Key::Home, 4),
            [0x1b, b'[', b'4', b'~', ..] => (Key::End, 4),
            // Andere Escape-Sequenzen überspringen (bis zum Endbyte).
            [0x1b, b'[', ..] => {
                let end = rest[2..]
                    .iter()
                    .position(|b| (0x40..=0x7e).contains(b))
                    .map_or(rest.len(), |p| p + 3);
                (Key::Other, end)
            }
            [0x1b, ..] => (Key::Cancel, 1),
            [b'k', ..] => (Key::Up, 1),
            [b'j', ..] => (Key::Down, 1),
            [b'g', ..] => (Key::Home, 1),
            [b'G', ..] => (Key::End, 1),
            [b' ', ..] | [b'v', ..] => (Key::Mark, 1),
            [b'\r', ..] | [b'\n', ..] => (Key::Enter, 1),
            [b'q', ..] | [0x03, ..] => (Key::Cancel, 1),
            _ => (Key::Other, 1),
        };
        keys.push(key);
        i += len;
    }
    keys
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Continue,
    Send(String),
    Cancel,
}

pub struct Picker {
    lines: Vec<String>,
    cursor: usize,
    anchor: Option<usize>,
    /// Erste sichtbare Zeile.
    top: usize,
    /// Zeilen für die Liste (Bildschirmhöhe minus Hilfezeile).
    height: usize,
}

impl Picker {
    /// Startet auf der letzten nicht leeren Zeile.
    pub fn new(lines: Vec<String>, screen_rows: u16) -> Self {
        let lines = if lines.is_empty() { vec![String::new()] } else { lines };
        let cursor = lines.iter().rposition(|l| !l.trim().is_empty()).unwrap_or(0);
        let mut p = Picker {
            lines,
            cursor,
            anchor: None,
            top: 0,
            height: usize::from(screen_rows.saturating_sub(1)).max(1),
        };
        p.scroll_into_view();
        p
    }

    pub fn handle(&mut self, key: Key) -> Action {
        let last = self.lines.len() - 1;
        match key {
            Key::Up => self.cursor = self.cursor.saturating_sub(1),
            Key::Down => self.cursor = (self.cursor + 1).min(last),
            Key::PageUp => self.cursor = self.cursor.saturating_sub(self.height),
            Key::PageDown => self.cursor = (self.cursor + self.height).min(last),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = last,
            Key::Mark => {
                self.anchor = match self.anchor {
                    Some(_) => None,
                    None => Some(self.cursor),
                }
            }
            Key::Enter => return Action::Send(self.selection()),
            Key::Cancel => return Action::Cancel,
            Key::Other => {}
        }
        self.scroll_into_view();
        Action::Continue
    }

    fn range(&self) -> (usize, usize) {
        let a = self.anchor.unwrap_or(self.cursor);
        (a.min(self.cursor), a.max(self.cursor))
    }

    pub fn selection(&self) -> String {
        let (from, to) = self.range();
        self.lines[from..=to].join("\n")
    }

    fn scroll_into_view(&mut self) {
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + self.height {
            self.top = self.cursor + 1 - self.height;
        }
    }

    /// Kompletter Bildschirm als Escape-Sequenzen (löschen, Liste, Hilfezeile).
    pub fn render(&self, cols: u16) -> String {
        let cols = usize::from(cols.max(10));
        let (from, to) = self.range();
        let mut out = String::from("\x1b[?25l\x1b[0m\x1b[H\x1b[2J");
        for (row, idx) in (self.top..self.lines.len()).take(self.height).enumerate() {
            out.push_str(&format!("\x1b[{};1H", row + 1));
            let selected = idx >= from && idx <= to;
            if idx == self.cursor {
                out.push_str("\x1b[7m"); // invertiert
            } else if selected {
                out.push_str("\x1b[44m"); // markiert
            }
            out.push_str(&fit(&self.lines[idx], cols));
            out.push_str("\x1b[0m");
        }
        let count = to - from + 1;
        let help = if crate::locale::german() {
            format!(
                " bridget Auswahl: {count} Zeile{} · Space markieren · Enter senden · Esc abbrechen · ↑↓ PgUp PgDn g G",
                if count == 1 { "" } else { "n" }
            )
        } else {
            format!(
                " bridget Selection: {count} line{} · Space mark · Enter send · Esc cancel · ↑↓ PgUp PgDn g G",
                if count == 1 { "" } else { "s" }
            )
        };
        out.push_str(&format!("\x1b[{};1H\x1b[30;47m{}\x1b[0m", self.height + 1, fit(&help, cols)));
        out
    }
}

/// Auf `cols` Spalten kürzen bzw. mit Leerzeichen auffüllen (breite Zeichen beachtet).
fn fit(line: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    for c in line.chars() {
        let w = c.width().unwrap_or(0);
        if width + w > cols {
            break;
        }
        out.push(c);
        width += w;
    }
    out.push_str(&" ".repeat(cols - width));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("Zeile {i}")).collect()
    }

    #[test]
    fn starts_at_last_non_empty_line() {
        let mut l = lines(3);
        l.push(String::new());
        let p = Picker::new(l, 10);
        assert_eq!(p.selection(), "Zeile 3");
    }

    #[test]
    fn select_range_with_mark_and_send() {
        let mut p = Picker::new(lines(200), 24);
        for k in [Key::Up, Key::Up, Key::Mark, Key::Up, Key::Up] {
            assert_eq!(p.handle(k), Action::Continue);
        }
        assert_eq!(p.handle(Key::Enter), Action::Send("Zeile 196\nZeile 197\nZeile 198".into()));
    }

    #[test]
    fn mark_toggles_and_cancel() {
        let mut p = Picker::new(lines(5), 24);
        p.handle(Key::Mark);
        p.handle(Key::Up);
        p.handle(Key::Mark); // Markierung wieder weg
        assert_eq!(p.selection(), "Zeile 4");
        assert_eq!(p.handle(Key::Cancel), Action::Cancel);
    }

    #[test]
    fn scrolls_through_history() {
        let mut p = Picker::new(lines(200), 11); // 10 Zeilen Liste
        p.handle(Key::Home);
        assert_eq!(p.selection(), "Zeile 1");
        assert_eq!(p.top, 0);
        p.handle(Key::PageDown);
        assert_eq!(p.selection(), "Zeile 11");
        assert_eq!(p.top, 1);
        p.handle(Key::End);
        assert_eq!(p.top, 190);
        let screen = p.render(40);
        assert!(screen.contains("Zeile 200"));
        assert!(!screen.contains("Zeile 189 "));
    }

    #[test]
    fn parses_keys() {
        assert_eq!(
            parse_keys(b"\x1b[A\x1bOB\x1b[5~\x1b[6~ \rq\x1bjkx\x1b[1;5C"),
            vec![
                Key::Up,
                Key::Down,
                Key::PageUp,
                Key::PageDown,
                Key::Mark,
                Key::Enter,
                Key::Cancel,
                Key::Cancel,
                Key::Down,
                Key::Up,
                Key::Other,
                Key::Other,
            ]
        );
    }

    #[test]
    fn fit_handles_wide_chars() {
        assert_eq!(fit("äöü", 5), "äöü  ");
        assert_eq!(fit("日本語", 5), "日本 ");
        assert_eq!(fit("abcdef", 3), "abc");
    }
}
