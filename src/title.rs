//! Fenstertitel des Programms abfangen (`OSC 0` / `OSC 2`), damit bridget sein
//! Kennzeichen davorsetzen kann. Alles andere geht unverändert durch, auch über
//! Lesepuffer-Grenzen hinweg.

/// Längere Titel gibt der Filter unverändert weiter, statt sie zu puffern.
const MAX_TITLE: usize = 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Piece {
    Bytes(Vec<u8>),
    /// (OSC-Nummer 0 oder 2, Titel)
    Title(u8, String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum St {
    Normal,
    Esc,
    Num,
    Payload,
    PayloadEsc,
}

pub struct Filter {
    st: St,
    /// Bisher gepufferter Anfang der Sequenz (`ESC ] N ;`).
    head: Vec<u8>,
    payload: Vec<u8>,
}

impl Default for Filter {
    fn default() -> Self {
        Self::new()
    }
}

impl Filter {
    pub fn new() -> Self {
        Filter { st: St::Normal, head: Vec::new(), payload: Vec::new() }
    }

    pub fn feed(&mut self, input: &[u8]) -> Vec<Piece> {
        let mut pieces = Vec::new();
        let mut out = Vec::with_capacity(input.len());
        let mut i = 0;
        while i < input.len() {
            let b = input[i];
            match self.st {
                St::Normal => {
                    match input[i..].iter().position(|&c| c == 0x1b) {
                        Some(p) => {
                            out.extend_from_slice(&input[i..i + p]);
                            self.head = vec![0x1b];
                            self.st = St::Esc;
                            i += p + 1;
                        }
                        None => {
                            out.extend_from_slice(&input[i..]);
                            i = input.len();
                        }
                    }
                    continue;
                }
                St::Esc => match b {
                    b']' => {
                        self.head.push(b);
                        self.st = St::Num;
                    }
                    0x1b => out.push(0x1b), // ESC ESC: das erste geht durch
                    _ => {
                        out.append(&mut self.head);
                        out.push(b);
                        self.st = St::Normal;
                    }
                },
                St::Num => match b {
                    b'0'..=b'9' if self.head.len() < 5 => self.head.push(b),
                    b';' if matches!(&self.head[2..], b"0" | b"2") => {
                        self.head.push(b);
                        self.payload.clear();
                        self.st = St::Payload;
                    }
                    _ => {
                        // Fremde OSC-Nummer: unverändert weiter, das Byte neu ansehen.
                        out.append(&mut self.head);
                        self.st = St::Normal;
                        continue;
                    }
                },
                St::Payload => match b {
                    0x07 => self.finish(&mut out, &mut pieces),
                    0x1b => self.st = St::PayloadEsc,
                    _ if self.payload.len() >= MAX_TITLE => {
                        out.append(&mut self.head);
                        out.append(&mut self.payload);
                        self.st = St::Normal;
                        continue;
                    }
                    _ => self.payload.push(b),
                },
                St::PayloadEsc => {
                    if b == b'\\' {
                        self.finish(&mut out, &mut pieces);
                    } else {
                        // Abgebrochene Sequenz: roh weiter, das ESC beginnt etwas Neues.
                        out.append(&mut self.head);
                        out.append(&mut self.payload);
                        self.head = vec![0x1b];
                        self.st = St::Esc;
                        continue;
                    }
                }
            }
            i += 1;
        }
        if !out.is_empty() {
            pieces.push(Piece::Bytes(out));
        }
        pieces
    }

    fn finish(&mut self, out: &mut Vec<u8>, pieces: &mut Vec<Piece>) {
        if !out.is_empty() {
            pieces.push(Piece::Bytes(std::mem::take(out)));
        }
        let kind = self.head[2] - b'0';
        let title = String::from_utf8_lossy(&self.payload).into_owned();
        pieces.push(Piece::Title(kind, title));
        self.head.clear();
        self.payload.clear();
        self.st = St::Normal;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(s: &[u8]) -> Piece {
        Piece::Bytes(s.to_vec())
    }

    #[test]
    fn extracts_titles_with_bel_and_st() {
        let mut f = Filter::new();
        let got = f.feed(b"a\x1b]0;eins\x07b\x1b]2;zwei\x1b\\c");
        assert_eq!(
            got,
            vec![
                bytes(b"a"),
                Piece::Title(0, "eins".into()),
                bytes(b"b"),
                Piece::Title(2, "zwei".into()),
                bytes(b"c"),
            ]
        );
    }

    #[test]
    fn title_split_over_chunks() {
        let mut f = Filter::new();
        assert_eq!(f.feed(b"x\x1b"), vec![bytes(b"x")]);
        assert_eq!(f.feed(b"]2;Hal"), vec![]);
        assert_eq!(f.feed(b"lo\x1b"), vec![]);
        assert_eq!(f.feed(b"\\y"), vec![Piece::Title(2, "Hallo".into()), bytes(b"y")]);
    }

    #[test]
    fn other_sequences_pass_unchanged() {
        let mut f = Filter::new();
        let input: &[u8] =
            b"\x1b[31mrot\x1b]11;?\x1b\\\x1b]8;;http://x\x07L\x1b]8;;\x07\x1b]1;icon\x07\x1b\x1bM";
        let got: Vec<u8> = f
            .feed(input)
            .into_iter()
            .flat_map(|p| match p {
                Piece::Bytes(b) => b,
                Piece::Title(..) => panic!("kein Titel erwartet"),
            })
            .collect();
        assert_eq!(got, input);
    }

    #[test]
    fn overlong_title_passes_raw() {
        let mut f = Filter::new();
        let mut input = b"\x1b]2;".to_vec();
        input.extend(std::iter::repeat_n(b'a', MAX_TITLE + 10));
        input.push(0x07);
        let got: Vec<u8> = f
            .feed(&input)
            .into_iter()
            .flat_map(|p| match p {
                Piece::Bytes(b) => b,
                Piece::Title(..) => panic!("kein Titel erwartet"),
            })
            .collect();
        assert_eq!(got, input);
    }

    #[test]
    fn aborted_title_passes_raw() {
        let mut f = Filter::new();
        let got = f.feed(b"\x1b]2;ab\x1b[0m");
        assert_eq!(got, vec![bytes(b"\x1b]2;ab\x1b[0m")]);
    }
}
