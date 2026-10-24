//! Text aus dem Bildschirm-Abbild holen und sicher einfügen.

use std::num::NonZeroU16;

use regex::Regex;

pub const PASTE_START: &[u8] = b"\x1b[200~";
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// Nur `\n` und `\t` bleiben; `\r\n` und einzelnes `\r` werden zu `\n`. Alle anderen
/// Steuerzeichen (auch ESC, also auch ein eingeschleustes `ESC[201~`) fallen weg.
/// Leerzeichen am Zeilenende und Leerzeilen am Anfang/Ende werden entfernt.
pub fn sanitize(text: &str) -> String {
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    let clean: String =
        unified.chars().filter(|&c| c == '\n' || c == '\t' || !c.is_control()).collect();
    trim_lines(clean.lines())
}

/// Zeilen rechts trimmen, Leerzeilen am Anfang und Ende entfernen.
pub fn trim_lines<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    let lines: Vec<&str> = lines.map(str::trim_end).collect();
    let start = lines.iter().position(|l| !l.is_empty()).unwrap_or(lines.len());
    let end = lines.iter().rposition(|l| !l.is_empty()).map_or(start, |i| i + 1);
    lines[start..end].join("\n")
}

pub fn line_count(text: &str) -> usize {
    if text.is_empty() { 0 } else { text.lines().count() }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Injection {
    /// Diese Bytes ins PTY schreiben (nie mit Enter).
    Bytes(Vec<u8>),
    /// Mehrzeilig, aber das Programm kann kein Bracketed Paste: nicht einfügen, sonst
    /// würde jeder Zeilenumbruch als Enter wirken.
    NeedsOneLine,
}

/// `text` muss bereits durch `sanitize` gelaufen sein.
pub fn encode(text: &str, bracketed_paste: bool) -> Injection {
    if bracketed_paste {
        let mut b = Vec::with_capacity(text.len() + 12);
        b.extend_from_slice(PASTE_START);
        b.extend_from_slice(text.as_bytes());
        b.extend_from_slice(PASTE_END);
        Injection::Bytes(b)
    } else if text.contains('\n') {
        Injection::NeedsOneLine
    } else {
        // Ohne Bracketed Paste würde ein Tab in der Shell die Vervollständigung auslösen.
        Injection::Bytes(text.replace('\t', "    ").into_bytes())
    }
}

/// Mehrzeiligen Text auf eine Zeile bringen: Zeilen getrimmt, mit Leerzeichen verbunden.
pub fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .replace('\t', " ")
}

/// Zählt Zeilen, die aus dem vollen Verlauf wieder herausfallen. Zusammen mit der Länge
/// des Verlaufs ergibt das, wie viele Zeilen insgesamt hochgescrollt sind.
#[derive(Debug, Default)]
pub struct Dropped(usize);

impl vt100::Callbacks for Dropped {
    fn on_scroll(&mut self, _: vt100::capture::RowContents<'_>, alternate_screen: bool) {
        if !alternate_screen {
            self.0 += 1;
        }
    }
}

/// Bildschirm-Abbild eines Programms.
pub type Parser = vt100::Parser<Dropped>;

pub fn parser(rows: NonZeroU16, cols: NonZeroU16, scrollback: usize) -> Parser {
    vt100::Parser::new_with_callbacks(rows, cols, scrollback, Dropped::default())
}

/// Wie viele Zeilen seit dem Start in den Verlauf gewandert sind, auch die schon
/// wieder verlorenen. Steigt nur, dient als Merkpunkt.
pub fn scrolled(parser: &mut Parser) -> usize {
    parser.callbacks().0 + history_len(parser)
}

fn history_len(parser: &mut Parser) -> usize {
    let screen = parser.screen_mut();
    screen.set_scrollback(usize::MAX);
    let len = screen.scrollback();
    screen.set_scrollback(0);
    len
}

/// Die letzten `n` Zeilen des Verlaufs und danach der sichtbare Bildschirm.
fn tail_and_screen(parser: &mut Parser, n: usize) -> Vec<String> {
    let screen = parser.screen_mut();
    let (rows, cols) = screen.size();
    let (rows, cols) = (usize::from(rows.get()), cols.get());
    let mut lines: Vec<String> = Vec::with_capacity(n + rows);
    let mut offset = n;
    loop {
        screen.set_scrollback(offset);
        let base = n - offset;
        for (r, line) in screen.rows(0, cols).enumerate() {
            if base + r == lines.len() {
                lines.push(line.trim_end().to_string());
            }
        }
        if offset == 0 {
            break;
        }
        offset = offset.saturating_sub(rows);
    }
    screen.set_scrollback(0);
    lines
}

/// Was seit dem Merkpunkt `mark` (aus `scrolled`) hochgescrollt ist, plus der sichtbare
/// Bildschirm. Zeilen, die der Verlauf schon wieder verloren hat, fehlen.
pub fn rows_since(parser: &mut Parser, mark: usize) -> Vec<String> {
    let len = history_len(parser);
    let n = scrolled(parser).saturating_sub(mark).min(len);
    tail_and_screen(parser, n)
}

/// Alle Zeilen: Verlauf und sichtbarer Bildschirm, Leerzeilen am Ende weg.
pub fn all_lines(parser: &mut Parser) -> Vec<String> {
    let len = history_len(parser);
    let mut lines = tail_and_screen(parser, len);
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// Sichtbare Zeilen des Abbilds.
pub fn screen_rows(screen: &vt100::Screen) -> Vec<String> {
    let (_, cols) = screen.size();
    screen.rows(0, cols.get()).collect()
}

pub fn screen_text(screen: &vt100::Screen) -> String {
    let rows = screen_rows(screen);
    trim_lines(rows.iter().map(String::as_str))
}

/// Ausgabe einer Shell seit dem letzten Enter: Die mitgeschnittenen Bytes werden in ein
/// frisches Abbild gespielt; die Zeile mit dem Cursor (der neue Prompt) fällt weg.
pub fn replay_since_enter(log: &[u8], cols: u16) -> String {
    let newlines = log.iter().filter(|&&b| b == b'\n').count();
    let rows = (newlines + 2).clamp(2, 5000) as u16;
    let mut p = vt100::Parser::new(
        NonZeroU16::new(rows).unwrap(),
        NonZeroU16::new(cols.max(1)).unwrap(),
        0,
    );
    p.process(log);
    let screen = p.screen();
    let cursor_row = usize::from(screen.cursor_position().0);
    let rows = screen_rows(screen);
    trim_lines(rows.iter().take(cursor_row).map(String::as_str))
}

/// Regeln, mit denen eine KI-Oberfläche auf den Antworttext reduziert wird.
#[derive(Debug, Clone, Default)]
pub struct Frame {
    /// Ab der vorletzten Linie aus `─` bis zum Ende ist alles Eingabefeld und Fußzeile.
    pub input_box: bool,
    /// Eine passende Zeile ist eine Eingabe des Menschen (z.B. `❯ Frage`). Sie fällt weg,
    /// samt umgebrochener Folgezeilen (eingerückt, bis zur nächsten nicht eingerückten).
    pub prompt: Option<Regex>,
    /// Zeilen, die auf eine dieser Regeln passen, gehören zum Rahmen.
    pub strip: Vec<Regex>,
    /// Wird am Zeilenanfang entfernt (z.B. das `● ` vor Antworten).
    pub trim_prefix: Option<Regex>,
}

impl Frame {
    pub fn apply(&self, rows: &[String]) -> Vec<String> {
        let mut rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        if self.input_box {
            let rules: Vec<usize> =
                rows.iter().enumerate().filter(|(_, r)| is_rule(r)).map(|(i, _)| i).collect();
            if rules.len() >= 2 {
                rows.truncate(rules[rules.len() - 2]);
            }
        }
        if let Some(re) = &self.prompt {
            let mut in_prompt = false;
            rows.retain(|r| {
                if re.is_match(r) {
                    in_prompt = true;
                } else if !r.starts_with(char::is_whitespace) || r.trim().is_empty() {
                    in_prompt = false;
                }
                !in_prompt
            });
        }
        rows.into_iter()
            .filter(|r| !self.strip.iter().any(|re| re.is_match(r)))
            // Durch Leerzeichen gleicher Breite ersetzen, damit Folgezeilen bündig bleiben;
            // die gemeinsame Einrückung entfernt danach `dedent`.
            .map(|r| match &self.trim_prefix {
                Some(re) => re
                    .replace(r, |c: &regex::Captures| " ".repeat(c[0].chars().count()))
                    .into_owned(),
                None => r.to_string(),
            })
            .collect()
    }
}

fn is_rule(row: &str) -> bool {
    let t = row.trim();
    t.chars().count() >= 10 && t.chars().all(|c| c == '─')
}

/// Neue Zeilen einer KI-Oberfläche: was jetzt (nach Rahmen-Regeln) zu sehen ist und beim
/// letzten Enter noch nicht da war.
pub fn screen_diff(before: &[String], now: &[String], frame: &Frame) -> String {
    let before = frame.apply(before);
    let now = frame.apply(now);
    let mut remaining: Vec<&str> = before.iter().map(|s| s.trim_end()).collect();
    let fresh: Vec<&str> = now
        .iter()
        .map(|s| s.trim_end())
        .filter(|line| {
            if line.is_empty() {
                return true;
            }
            match remaining.iter().position(|b| b == line) {
                Some(i) => {
                    remaining.remove(i);
                    false
                }
                None => true,
            }
        })
        .collect();
    dedent(&trim_lines(fresh.into_iter()))
}

/// Im Alternate Screen gibt es keinen Verlauf. Ist die oberste Zeile dort neu seit dem
/// Enter, reicht die Antwort vermutlich über den Bildschirm hinaus und ihr Anfang fehlt.
pub fn top_row_is_new(before: &[String], screen: &vt100::Screen) -> bool {
    if !screen.alternate_screen() {
        return false;
    }
    let top = screen_rows(screen).into_iter().next().unwrap_or_default();
    let top = top.trim_end();
    !top.is_empty() && !before.iter().any(|b| b.trim_end() == top)
}

/// Gemeinsame Einrückung entfernen.
pub fn dedent(text: &str) -> String {
    let indent = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    text.lines().map(|l| l.get(indent..).unwrap_or("").trim_end()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_removes_controls_and_paste_end() {
        let evil = "a\x1b[201~\x07b\r\nc\rd\t\u{9b}e  \n\n";
        assert_eq!(sanitize(evil), "a[201~b\nc\nd\te");
    }

    #[test]
    fn encode_with_and_without_bracketed_paste() {
        assert_eq!(encode("a\nb", true), Injection::Bytes(b"\x1b[200~a\nb\x1b[201~".to_vec()));
        assert_eq!(encode("a\nb", false), Injection::NeedsOneLine);
        assert_eq!(encode("ls\t-la", false), Injection::Bytes(b"ls    -la".to_vec()));
        assert_eq!(one_line("  a\n\n b \n"), "a b");
    }

    #[test]
    fn replay_drops_prompt_and_command_echo() {
        // Nach dem Enter: Echo "\r\n", Ausgabe, neuer Prompt mit Cursor.
        let log = b"\r\nProjekt Sonnenblume\r\nZiel: drei Testpunkte.\r\n$ ";
        assert_eq!(replay_since_enter(log, 80), "Projekt Sonnenblume\nZiel: drei Testpunkte.");
    }

    #[test]
    fn replay_handles_carriage_return_progress() {
        let log = b"\r\nLade 10%\rLade 50%\rLade 100%\r\nfertig\r\n$ ";
        assert_eq!(replay_since_enter(log, 80), "Lade 100%\nfertig");
    }

    #[test]
    fn replay_from_bash_fixture() {
        let raw = include_bytes!("../tests/fixtures/tui/bash.raw");
        // Der Teil nach dem Enter von "cat notiz.txt; false" (ohne die script-Kopfzeile).
        let start = find(raw, b"false").unwrap() + 5;
        let end = find(raw, b"exit\r\n").unwrap();
        assert_eq!(
            replay_since_enter(&raw[start..end], 100),
            "Projekt Sonnenblume\nZiel: drei Testpunkte."
        );
    }

    fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    fn rows(s: &str) -> Vec<String> {
        s.lines().map(String::from).collect()
    }

    fn claude_frame() -> Frame {
        Frame {
            input_box: true,
            prompt: Some(Regex::new(r"^❯ ").unwrap()),
            strip: [r"^\s*✻ ", r"/effort\s*$"].iter().map(|r| Regex::new(r).unwrap()).collect(),
            trim_prefix: Some(Regex::new(r"^(\s*)● ").unwrap()),
        }
    }

    #[test]
    fn screen_diff_extracts_claude_answer() {
        let rule = "─".repeat(40);
        let before = rows(&format!(
            "  Opus 5.5\n\n{rule}\n❯ Antworte mit drei Zeilen\n{rule}\n  ⏵⏵ auto mode on\n"
        ));
        let now = rows(&format!(
            "  Opus 5.5\n\n❯ Antworte mit drei Zeilen, die Frage ist\n  so lang, dass sie umbricht\n● eins\n  zwei\n  drei\n✻ Worked for 1s · done 4:11 AM\n           ◐ medium · /effort\n{rule}\n❯ Wie lautet das Codewort?\n{rule}\n  ⏵⏵ auto mode on\n"
        ));
        assert_eq!(screen_diff(&before, &now, &claude_frame()), "eins\nzwei\ndrei");
    }

    #[test]
    fn dedent_keeps_relative_indent() {
        assert_eq!(dedent("  def f():\n      return 1\n\n  x"), "def f():\n    return 1\n\nx");
    }

    fn lines(p: &mut Parser, from: u32, to: u32) {
        for i in from..=to {
            p.process(format!("z{i}\r\n").as_bytes());
        }
    }

    fn z(from: u32, to: u32) -> Vec<String> {
        (from..=to).map(|i| format!("z{i}")).collect()
    }

    #[test]
    fn all_lines_includes_scrollback_in_order() {
        let mut p = parser(NonZeroU16::new(5).unwrap(), NonZeroU16::new(20).unwrap(), 100);
        lines(&mut p, 1, 23);
        assert_eq!(all_lines(&mut p), z(1, 23));
        assert_eq!(p.screen().scrollback(), 0, "Ansicht muss zurück auf live");
    }

    #[test]
    fn rows_since_includes_lines_scrolled_out_of_view() {
        let mut p = parser(NonZeroU16::new(5).unwrap(), NonZeroU16::new(20).unwrap(), 100);
        lines(&mut p, 1, 10);
        let mark = scrolled(&mut p);
        lines(&mut p, 11, 30);
        let mut got = rows_since(&mut p, mark);
        while got.last().is_some_and(|l| l.is_empty()) {
            got.pop();
        }
        // Bildschirm beim Merkpunkt: z7..z10 und die Cursorzeile.
        assert_eq!(got, z(7, 30));
        assert_eq!(p.screen().scrollback(), 0);
    }

    #[test]
    fn rows_since_survives_full_scrollback() {
        // Verlauf hat nur 8 Zeilen Platz: Der Merkpunkt darf nicht wandern.
        let mut p = parser(NonZeroU16::new(3).unwrap(), NonZeroU16::new(20).unwrap(), 8);
        lines(&mut p, 1, 50);
        let mark = scrolled(&mut p);
        lines(&mut p, 51, 55);
        let mut got = rows_since(&mut p, mark);
        while got.last().is_some_and(|l| l.is_empty()) {
            got.pop();
        }
        assert_eq!(got, z(49, 55));
        // Mehr Neues, als der Verlauf fasst: der Rest ist weg, kein Absturz.
        lines(&mut p, 56, 80);
        assert_eq!(rows_since(&mut p, mark).first().map(String::as_str), Some("z71"));
    }

    #[test]
    fn long_answer_in_alternate_screen_is_flagged() {
        let mut p = parser(NonZeroU16::new(4).unwrap(), NonZeroU16::new(20).unwrap(), 100);
        p.process(b"\x1b[?1049h\x1b[Hkopf\r\n\r\n> frage");
        let before = screen_rows(p.screen());
        assert!(!top_row_is_new(&before, p.screen()), "kurze Antwort, Kopf noch da");
        p.process(b"\x1b[2J\x1b[Hzeile 7\r\nzeile 8\r\nzeile 9\r\n> ");
        assert!(top_row_is_new(&before, p.screen()));
        // Ohne Alternate Screen gibt es den Verlauf, dann fehlt nichts.
        let mut q = parser(NonZeroU16::new(4).unwrap(), NonZeroU16::new(20).unwrap(), 100);
        q.process(b"zeile 7\r\nzeile 8");
        assert!(!top_row_is_new(&[], q.screen()));
    }

    #[test]
    fn screen_text_trims() {
        let mut p = parser(NonZeroU16::new(5).unwrap(), NonZeroU16::new(20).unwrap(), 0);
        p.process(b"\r\nhallo   \r\nwelt\r\n");
        assert_eq!(screen_text(p.screen()), "hallo\nwelt");
    }
}
