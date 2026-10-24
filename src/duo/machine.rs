//! Halbduplex-Zustandsmaschine für `duo`. Reine Logik ohne Prozesse und Terminal.
//!
//! Regeln:
//! - Genau eine Seite hat das Rederecht, nie mehr als ein Aufruf gleichzeitig.
//! - Ein Rundenlimit für das ganze Gespräch (jede Antwort zählt, auch Übergaben).
//! - Pause lässt einen laufenden Aufruf zu Ende laufen, gibt aber erst nach „weiter“ weiter.
//! - Nachrichten werden nie ersetzt oder verworfen: Jede Seite hat einen Posteingang,
//!   beim nächsten Aufruf gehen alle offenen Nachrichten in Reihenfolge hinein.
//! - Antwortet ein Aufruf, der nicht läuft, ist das ein Fehler.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    A,
    B,
}

impl Side {
    pub fn other(self) -> Side {
        match self {
            Side::A => Side::B,
            Side::B => Side::A,
        }
    }
    fn idx(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum From {
    Human,
    Side(Side),
    /// Anweisung von duo selbst (z.B. nach einer Übernahme).
    Duo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub seq: u64,
    pub from: From,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Aufruf starten. `messages` sind alle offenen Nachrichten an diese Seite.
    Run { side: Side, seq: u64, messages: Vec<Message> },
    /// Laufenden Aufruf abbrechen.
    Kill { side: Side },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Ein Aufruf läuft (Pause kann vorgemerkt sein).
    Speaking {
        side: Side,
        seq: u64,
    },
    Paused,
    TakenOver {
        side: Side,
    },
    /// Rundenlimit erreicht: wie Pause, „weiter“ gibt zwei Runden dazu.
    Limit,
    Error {
        reason: String,
    },
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterTakeover {
    /// Übernommene Seite fasst zusammen und antwortet (Übergabe-Aufruf).
    Continue,
    /// Einfach weiter, ohne Übergabe-Aufruf (bleibt pausiert).
    ContinueWithoutHandover,
    Quit,
}

fn handover() -> &'static str {
    tr!(
        "The human spoke with you directly for a while. Briefly summarize for the other side what was learned or changed, then answer their last message if there is one.",
        "Der Mensch hat zwischendurch direkt mit dir gesprochen. Fasse für die andere Seite kurz zusammen, was sich dabei ergeben oder geändert hat, und antworte dann auf ihre letzte Nachricht, falls es eine gibt."
    )
}

#[derive(Debug)]
pub struct Machine {
    pub max_rounds: u32,
    pub rounds: u32,
    pub next: Side,
    pub phase: Phase,
    /// Pause vorgemerkt, während ein Aufruf läuft.
    pause_requested: bool,
    inbox: [Vec<Message>; 2],
    /// Nachrichten des laufenden Aufrufs, für einen Neuversuch nach einem Fehler.
    in_flight: Vec<Message>,
    next_seq: u64,
}

impl Machine {
    pub fn new(max_rounds: u32, first_prompt: String) -> (Machine, Vec<Effect>) {
        let mut m = Machine {
            max_rounds,
            rounds: 0,
            next: Side::A,
            phase: Phase::Paused,
            pause_requested: false,
            inbox: [Vec::new(), Vec::new()],
            in_flight: Vec::new(),
            next_seq: 1,
        };
        m.post(Side::A, From::Human, first_prompt);
        let effects = m.advance();
        (m, effects)
    }

    fn post(&mut self, to: Side, from: From, text: String) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.inbox[to.idx()].push(Message { seq, from, text });
    }

    #[cfg(test)]
    pub fn pending(&self, side: Side) -> &[Message] {
        &self.inbox[side.idx()]
    }

    /// Nächsten Aufruf starten, wenn nichts dagegen spricht.
    fn advance(&mut self) -> Vec<Effect> {
        if self.rounds >= self.max_rounds {
            self.phase = Phase::Limit;
            return vec![];
        }
        let side = self.next;
        if self.inbox[side.idx()].is_empty() {
            self.phase = Phase::Paused;
            return vec![];
        }
        let messages = std::mem::take(&mut self.inbox[side.idx()]);
        self.in_flight = messages.clone();
        let seq = self.next_seq;
        self.next_seq += 1;
        self.phase = Phase::Speaking { side, seq };
        vec![Effect::Run { side, seq, messages }]
    }

    /// Ein Aufruf ist fertig.
    pub fn turn_done(&mut self, side: Side, seq: u64, text: String) -> Vec<Effect> {
        if self.phase != (Phase::Speaking { side, seq }) {
            self.phase = Phase::Error {
                reason: if crate::locale::german() {
                    format!("Antwort von {side:?} (#{seq}) passt zu keinem laufenden Aufruf")
                } else {
                    format!("response from {side:?} (#{seq}) does not match a running turn")
                },
            };
            return vec![];
        }
        self.in_flight.clear();
        self.rounds += 1;
        self.post(side.other(), From::Side(side), text);
        self.next = side.other();
        if std::mem::take(&mut self.pause_requested) {
            self.phase = Phase::Paused;
            return vec![];
        }
        self.advance()
    }

    pub fn turn_failed(&mut self, side: Side, seq: u64, reason: String) -> Vec<Effect> {
        if self.phase != (Phase::Speaking { side, seq }) {
            return vec![];
        }
        // Die Nachrichten gehen zurück in den Posteingang: „weiter“ versucht es noch einmal.
        let mut back = std::mem::take(&mut self.in_flight);
        back.append(&mut self.inbox[side.idx()]);
        self.inbox[side.idx()] = back;
        self.pause_requested = false;
        self.phase = Phase::Error { reason };
        vec![]
    }

    /// Space: pausieren bzw. weiter.
    pub fn toggle_pause(&mut self) -> Vec<Effect> {
        match &self.phase {
            Phase::Speaking { .. } => {
                self.pause_requested = !self.pause_requested;
                vec![]
            }
            Phase::Paused | Phase::Error { .. } => self.advance(),
            Phase::Limit => {
                self.max_rounds = self.rounds + 2;
                self.advance()
            }
            Phase::TakenOver { .. } | Phase::Finished => vec![],
        }
    }

    pub fn pause_requested(&self) -> bool {
        self.pause_requested
    }

    /// Nachricht des Menschen an die Seite, die als nächste dran ist (nur in einer Pause).
    pub fn inject(&mut self, text: String) -> bool {
        if !self.can_interact() {
            return false;
        }
        let next = self.next;
        self.post(next, From::Human, text);
        true
    }

    fn can_interact(&self) -> bool {
        matches!(self.phase, Phase::Paused | Phase::Limit | Phase::Error { .. })
    }

    pub fn take_over(&mut self, side: Side) -> bool {
        if !self.can_interact() {
            return false;
        }
        self.phase = Phase::TakenOver { side };
        true
    }

    pub fn takeover_ended(&mut self, choice: AfterTakeover) -> Vec<Effect> {
        let Phase::TakenOver { side } = self.phase else {
            return vec![];
        };
        match choice {
            AfterTakeover::Continue => {
                // Übergabe-Aufruf: zuerst die Anweisung, dann was sonst an diese Seite offen ist.
                let seq = self.next_seq;
                self.next_seq += 1;
                self.inbox[side.idx()]
                    .insert(0, Message { seq, from: From::Duo, text: handover().into() });
                self.next = side;
                if self.rounds >= self.max_rounds {
                    self.max_rounds = self.rounds + 1;
                }
                self.advance()
            }
            AfterTakeover::ContinueWithoutHandover => {
                self.phase = Phase::Paused;
                vec![]
            }
            AfterTakeover::Quit => self.quit(),
        }
    }

    pub fn quit(&mut self) -> Vec<Effect> {
        let effects = match self.phase {
            Phase::Speaking { side, .. } => vec![Effect::Kill { side }],
            _ => vec![],
        };
        self.phase = Phase::Finished;
        effects
    }
}

/// Text für den Aufruf aus den offenen Nachrichten bauen.
pub fn compose(messages: &[Message], name_of: impl Fn(Side) -> String) -> String {
    messages
        .iter()
        .map(|m| match &m.from {
            From::Human => format!("[{}]:\n{}", tr!("Human", "Mensch"), m.text),
            From::Side(s) => {
                format!("[{} {}]:\n{}", name_of(*s), tr!("says", "sagt"), m.text)
            }
            From::Duo => format!("[duo]: {}", m.text),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_of(effects: &[Effect]) -> (Side, u64, Vec<Message>) {
        match effects {
            [Effect::Run { side, seq, messages }] => (*side, *seq, messages.clone()),
            other => panic!("erwartet genau einen Run, bekommen {other:?}"),
        }
    }

    #[test]
    fn alternates_and_stops_at_global_round_limit() {
        let (mut m, e) = Machine::new(3, "Start".into());
        let (side, seq, msgs) = run_of(&e);
        assert_eq!((side, msgs[0].text.as_str()), (Side::A, "Start"));

        let (side, seq, msgs) = run_of(&m.turn_done(side, seq, "a1".into()));
        assert_eq!(side, Side::B);
        assert_eq!(msgs[0].from, From::Side(Side::A));
        let (side, seq, _) = run_of(&m.turn_done(side, seq, "b1".into()));
        assert_eq!(side, Side::A);
        assert!(m.turn_done(side, seq, "a2".into()).is_empty());
        assert_eq!(m.phase, Phase::Limit);
        assert_eq!(m.rounds, 3);

        // „weiter“ gibt zwei Runden dazu; B bekommt a2.
        let (side, _, msgs) = run_of(&m.toggle_pause());
        assert_eq!((side, msgs[0].text.as_str()), (Side::B, "a2"));
        assert_eq!(m.max_rounds, 5);
    }

    #[test]
    fn pause_during_turn_holds_the_answer() {
        let (mut m, e) = Machine::new(6, "Start".into());
        let (side, seq, _) = run_of(&e);
        assert!(m.toggle_pause().is_empty());
        assert!(m.pause_requested());
        assert!(m.turn_done(side, seq, "a1".into()).is_empty());
        assert_eq!(m.phase, Phase::Paused);
        assert_eq!(m.pending(Side::B)[0].text, "a1");
        let (side, _, msgs) = run_of(&m.toggle_pause());
        assert_eq!((side, msgs.len()), (Side::B, 1));
    }

    #[test]
    fn injected_message_is_appended_not_replacing() {
        let (mut m, e) = Machine::new(6, "Start".into());
        let (side, seq, _) = run_of(&e);
        m.toggle_pause();
        m.turn_done(side, seq, "a1".into());
        assert!(m.inject("Bitte kürzer".into()));
        let (_, _, msgs) = run_of(&m.toggle_pause());
        let texts: Vec<_> = msgs.iter().map(|m| (m.from.clone(), m.text.as_str())).collect();
        assert_eq!(texts, vec![(From::Side(Side::A), "a1"), (From::Human, "Bitte kürzer")]);
    }

    #[test]
    fn inject_and_takeover_only_when_not_running() {
        let (mut m, _) = Machine::new(6, "Start".into());
        assert!(!m.inject("x".into()));
        assert!(!m.take_over(Side::A));
    }

    #[test]
    fn unexpected_answer_is_an_error() {
        let (mut m, e) = Machine::new(6, "Start".into());
        let (side, seq, _) = run_of(&e);
        assert!(m.turn_done(side.other(), seq, "falsch".into()).is_empty());
        assert!(matches!(m.phase, Phase::Error { .. }));
        let (mut m, e) = Machine::new(6, "Start".into());
        let (side, seq, _) = run_of(&e);
        m.turn_done(side, seq + 99, "alt".into());
        assert!(matches!(m.phase, Phase::Error { .. }));
    }

    #[test]
    fn failure_keeps_messages_for_retry() {
        let (mut m, e) = Machine::new(6, "Start".into());
        let (side, seq, _) = run_of(&e);
        m.turn_failed(side, seq, "Timeout".into());
        assert_eq!(m.phase, Phase::Error { reason: "Timeout".into() });
        assert_eq!(m.rounds, 0);
        let (side2, _, msgs) = run_of(&m.toggle_pause());
        assert_eq!((side2, msgs[0].text.as_str()), (Side::A, "Start"));
    }

    #[test]
    fn takeover_then_handover_turn() {
        let (mut m, e) = Machine::new(6, "Start".into());
        let (side, seq, _) = run_of(&e);
        m.toggle_pause();
        m.turn_done(side, seq, "a1".into()); // B ist dran, a1 liegt für B bereit
        assert!(m.take_over(Side::B));
        assert!(m.toggle_pause().is_empty(), "während Übernahme läuft nichts");
        assert!(!m.inject("x".into()));
        let (side, _, msgs) = run_of(&m.takeover_ended(AfterTakeover::Continue));
        assert_eq!(side, Side::B);
        assert_eq!(msgs[0].from, From::Duo);
        assert_eq!(msgs[1].text, "a1");
    }

    #[test]
    fn takeover_of_side_that_is_not_next() {
        let (mut m, e) = Machine::new(6, "Start".into());
        let (side, seq, _) = run_of(&e);
        m.toggle_pause();
        m.turn_done(side, seq, "a1".into()); // B ist dran
        m.take_over(Side::A);
        let (side, seq, msgs) = run_of(&m.takeover_ended(AfterTakeover::Continue));
        assert_eq!((side, msgs.len()), (Side::A, 1));
        // Danach hat B zwei Nachrichten von A: a1 und die Übergabe, beide in Reihenfolge.
        let (side, _, msgs) = run_of(&m.turn_done(side, seq, "übergabe".into()));
        let texts: Vec<_> = msgs.iter().map(|m| m.text.as_str()).collect();
        assert_eq!((side, texts), (Side::B, vec!["a1", "übergabe"]));
    }

    #[test]
    fn takeover_without_handover_stays_paused() {
        let (mut m, e) = Machine::new(6, "Start".into());
        let (side, seq, _) = run_of(&e);
        m.toggle_pause();
        m.turn_done(side, seq, "a1".into());
        m.take_over(Side::B);
        assert!(m.takeover_ended(AfterTakeover::ContinueWithoutHandover).is_empty());
        assert_eq!(m.phase, Phase::Paused);
    }

    #[test]
    fn quit_kills_running_turn() {
        let (mut m, e) = Machine::new(6, "Start".into());
        let (side, _, _) = run_of(&e);
        assert_eq!(m.quit(), vec![Effect::Kill { side }]);
        assert_eq!(m.phase, Phase::Finished);
        assert!(m.toggle_pause().is_empty());
    }

    #[test]
    fn compose_labels_senders() {
        let msgs = vec![
            Message { seq: 1, from: From::Side(Side::A), text: "hallo".into() },
            Message { seq: 2, from: From::Human, text: "kürzer".into() },
        ];
        let name = |s: Side| if s == Side::A { "claude".to_string() } else { "codex".into() };
        assert_eq!(compose(&msgs, name), "[claude says]:\nhallo\n\n[Human]:\nkürzer");
    }
}
