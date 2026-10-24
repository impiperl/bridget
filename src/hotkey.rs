//! Hotkey-Filter für die Tastatureingabe.
//!
//! Alles geht unverändert ans Programm durch, außer einem der Präfixe (`Ctrl-Space` oder
//! das alte `Ctrl-]`) und der Taste direkt danach. Innerhalb eines Bracketed-Paste-Blocks
//! der eigenen Tastatur
//! (`ESC[200~ … ESC[201~`) wird nichts ausgewertet, auch kein Präfix. Ein Enter außerhalb
//! davon wird zusätzlich gemeldet, direkt vor dem Abschnitt, der es enthält.
//!
//! Zwischen Präfix und Taste darf eine Zielnummer stehen (`Ctrl-Space 3 Enter`): höchstens
//! drei Ziffern, die Taste danach kommt dann als `CommandTo`.

pub const PREFIX: u8 = 0x1d; // Ctrl-]
pub const FRIENDLY_PREFIX: u8 = 0x00; // Ctrl-Space

const MAX_TARGET_DIGITS: u8 = 3;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Bytes, die unverändert ans Programm gehen.
    Pass(Vec<u8>),
    /// Taste nach dem Präfix (ein doppeltes Präfix wird zu `Pass`).
    Command(u8),
    /// Taste nach Präfix und Zielnummer, z.B. `Ctrl-Space 3 Enter` → `CommandTo(3, b'\r')`.
    CommandTo(u32, u8),
    /// Der Mensch hat Enter gedrückt (nicht eingefügt). Das `\r` folgt im nächsten `Pass`.
    Enter,
}

#[derive(Debug, Default)]
pub struct Filter {
    /// Das gerade gedrückte Präfix. Wird es wiederholt, geht es wörtlich ans Programm.
    after_prefix: Option<u8>,
    /// Zielnummer zwischen Präfix und Taste, mit Anzahl der Ziffern.
    target: Option<(u32, u8)>,
    in_paste: bool,
    /// Wie viele Bytes des gerade erwarteten Paste-Markers schon gesehen wurden.
    /// Die Bytes selbst wurden bereits durchgereicht, Marker gehen ans Programm.
    marker_pos: usize,
}

impl Filter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, input: &[u8]) -> Vec<Event> {
        let mut out = Vec::new();
        let mut pass = Vec::with_capacity(input.len());

        for &b in input {
            if let Some(prefix) = self.after_prefix {
                let digits = self.target.map_or(0, |(_, d)| d);
                if b.is_ascii_digit() && digits < MAX_TARGET_DIGITS {
                    let n = self.target.map_or(0, |(n, _)| n);
                    self.target = Some((n * 10 + u32::from(b - b'0'), digits + 1));
                    continue;
                }
                self.after_prefix = None;
                match self.target.take() {
                    None if b == prefix => pass.push(prefix),
                    None => {
                        flush(&mut pass, &mut out);
                        out.push(Event::Command(b));
                    }
                    Some((n, _)) => {
                        flush(&mut pass, &mut out);
                        out.push(Event::CommandTo(n, b));
                    }
                }
                continue;
            }

            if !self.in_paste && is_prefix(b) {
                self.marker_pos = 0;
                self.after_prefix = Some(b);
                continue;
            }

            if b == b'\r' && !self.in_paste {
                flush(&mut pass, &mut out);
                out.push(Event::Enter);
            }
            pass.push(b);
            self.track_marker(b);
        }

        flush(&mut pass, &mut out);
        out
    }

    fn track_marker(&mut self, b: u8) {
        let marker = if self.in_paste { PASTE_END } else { PASTE_START };
        if b == marker[self.marker_pos] {
            self.marker_pos += 1;
            if self.marker_pos == marker.len() {
                self.in_paste = !self.in_paste;
                self.marker_pos = 0;
            }
        } else {
            // Beide Marker beginnen nur mit ESC, ein Neustart ist also nur bei ESC möglich.
            self.marker_pos = usize::from(b == marker[0]);
        }
    }
}

fn is_prefix(b: u8) -> bool {
    b == FRIENDLY_PREFIX || b == PREFIX
}

fn flush(pass: &mut Vec<u8>, out: &mut Vec<Event>) {
    if !pass.is_empty() {
        out.push(Event::Pass(std::mem::take(pass)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(b: &[u8]) -> Event {
        Event::Pass(b.to_vec())
    }

    #[test]
    fn plain_input_passes_through() {
        let mut f = Filter::new();
        assert_eq!(f.feed(b"ls -la\r"), vec![pass(b"ls -la"), Event::Enter, pass(b"\r")]);
    }

    #[test]
    fn enter_inside_paste_is_not_reported() {
        let mut f = Filter::new();
        // Ganzer Paste in einem Stück, danach ein echtes Enter im selben Stück.
        assert_eq!(
            f.feed(b"\x1b[200~a\rb\x1b[201~\r"),
            vec![pass(b"\x1b[200~a\rb\x1b[201~"), Event::Enter, pass(b"\r")]
        );
        // Enter direkt vor einem Paste zählt.
        assert_eq!(f.feed(b"\r\x1b[200~x\ry"), vec![Event::Enter, pass(b"\r\x1b[200~x\ry")]);
        assert_eq!(f.feed(b"\r\x1b[201~"), vec![pass(b"\r\x1b[201~")]);
    }

    #[test]
    fn prefix_in_middle_of_chunk() {
        let mut f = Filter::new();
        assert_eq!(f.feed(b"ab\x1dlcd"), vec![pass(b"ab"), Event::Command(b'l'), pass(b"cd")]);
    }

    #[test]
    fn prefix_at_end_of_chunk() {
        let mut f = Filter::new();
        assert_eq!(f.feed(b"ab\x1d"), vec![pass(b"ab")]);
        assert_eq!(f.feed(b"s"), vec![Event::Command(b's')]);
    }

    #[test]
    fn double_prefix_sends_literal() {
        let mut f = Filter::new();
        assert_eq!(f.feed(b"\x1d\x1d"), vec![pass(b"\x1d")]);
        assert_eq!(f.feed(b"\x1d"), vec![]);
        assert_eq!(f.feed(b"\x1dx"), vec![pass(b"\x1dx")]);
    }

    #[test]
    fn ctrl_space_is_a_friendlier_prefix() {
        let mut f = Filter::new();
        assert_eq!(f.feed(b"\0\r"), vec![Event::Command(b'\r')]);
        assert_eq!(f.feed(b"\0\0"), vec![pass(b"\0")]);
        // Die beiden Präfixe lassen sich nicht versehentlich gegenseitig maskieren.
        assert_eq!(f.feed(b"\0\x1d"), vec![Event::Command(PREFIX)]);
    }

    #[test]
    fn number_between_prefix_and_key_is_a_target() {
        let mut f = Filter::new();
        assert_eq!(f.feed(b"\x003\r"), vec![Event::CommandTo(3, b'\r')]);
        // Über Lesestücke verteilt, mehrstellig, auch mit dem alten Präfix.
        assert_eq!(f.feed(b"x\x1d1"), vec![pass(b"x")]);
        assert_eq!(f.feed(b"2m"), vec![Event::CommandTo(12, b'm')]);
        // Höchstens drei Ziffern: die vierte ist die Taste.
        assert_eq!(f.feed(b"\x0012345"), vec![Event::CommandTo(123, b'4'), pass(b"5")]);
        // Präfix nach einer Nummer wird nicht durchgereicht.
        assert_eq!(f.feed(b"\x003\x00"), vec![Event::CommandTo(3, 0)]);
        // Danach gilt wieder alles normal.
        assert_eq!(f.feed(b"\x00\r"), vec![Event::Command(b'\r')]);
    }

    #[test]
    fn prefix_inside_paste_is_ignored() {
        let mut f = Filter::new();
        assert_eq!(
            f.feed(b"\x1b[200~a\x1dl\0b\x1b[201~"),
            vec![pass(b"\x1b[200~a\x1dl\0b\x1b[201~")]
        );
        // Nach dem Paste gilt das Präfix wieder.
        assert_eq!(f.feed(b"\x1dl"), vec![Event::Command(b'l')]);
    }

    #[test]
    fn paste_markers_split_across_chunks() {
        let mut f = Filter::new();
        assert_eq!(f.feed(b"\x1b[20"), vec![pass(b"\x1b[20")]);
        assert_eq!(f.feed(b"0~x\x1d"), vec![pass(b"0~x\x1d")]);
        assert_eq!(f.feed(b"\x1b[2"), vec![pass(b"\x1b[2")]);
        assert_eq!(f.feed(b"01~"), vec![pass(b"01~")]);
        assert_eq!(f.feed(b"\x1d?"), vec![Event::Command(b'?')]);
    }

    #[test]
    fn other_escape_sequences_do_not_start_paste() {
        let mut f = Filter::new();
        // Pfeiltaste, Mausereignis, ein ähnlicher, aber anderer CSI.
        assert_eq!(
            f.feed(b"\x1b[A\x1b[<0;3;4M\x1b[2001~"),
            vec![pass(b"\x1b[A\x1b[<0;3;4M\x1b[2001~")]
        );
        assert_eq!(f.feed(b"\x1dl"), vec![Event::Command(b'l')]);
    }

    #[test]
    fn lone_escape_before_paste_start() {
        // ESC (z.B. Alt oder Escape-Taste) direkt vor einem echten Paste.
        let mut f = Filter::new();
        assert_eq!(f.feed(b"\x1b\x1b[200~"), vec![pass(b"\x1b\x1b[200~")]);
        assert_eq!(f.feed(b"\x1dl"), vec![pass(b"\x1dl")]);
    }
}
