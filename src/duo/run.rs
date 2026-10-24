//! Der Koordinator: Ereignisschleife, Aufrufe, Anzeige, Übernahme.

use std::fs::{self, OpenOptions};
use std::io::{BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};

use super::agent::{self, Agent, AgentEvent, Kind, Turn};
use super::machine::{self, AfterTakeover, Effect, Machine, Phase, Side};
use super::ui::{Console, Keys, LineInput, LineResult, Show};
use super::{
    Choice, FromView, ToView, after_takeover_help, choice_from_key, duo_dir, run_interactive,
};
use crate::terminal::RawGuard;
use crate::{proto, runtime};

pub struct Options {
    pub a: Kind,
    pub b: Kind,
    pub rounds: u32,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub transcript: bool,
    /// Gemeinsames Gedächtnis, siehe `crate::memory`.
    pub memory: Option<PathBuf>,
    pub prompt: String,
}

enum Ev {
    LocalKeys(Vec<u8>),
    /// Nachrichten und Ende einer Ansicht tragen deren Nummer: Reste einer alten
    /// Verbindung dürfen eine neue nicht treffen.
    View(u64, FromView),
    ViewJoined(UnixStream),
    ViewGone(u64),
    Agent {
        side: Side,
        seq: u64,
        event: AgentEvent,
    },
    Tick,
    /// SIGHUP (Fenster geschlossen) oder SIGTERM: sauber beenden, damit der Dump entsteht.
    Hangup(i32),
}

enum Mode {
    Normal,
    Inject(LineInput),
    ChooseSide,
    AfterTakeover,
}

struct View {
    id: u64,
    out: UnixStream,
}

struct Running {
    side: Side,
    seq: u64,
    turn: Turn,
    since: Instant,
}

struct Coordinator {
    conv: String,
    short: String,
    machine: Machine,
    agents: [Agent; 2],
    /// Hat diese Seite ihre Einleitung schon bekommen?
    introduced: [bool; 2],
    running: Option<Running>,
    console: Console,
    view: Option<View>,
    next_view: u64,
    mode: Mode,
    keys: Keys,
    tx: Sender<Ev>,
    work: PathBuf,
    timeout: Duration,
    transcript: Option<fs::File>,
    /// Für den Abschluss-Dump (`crate::dump`), nur mit Gedächtnis.
    dump: bool,
    cwd: PathBuf,
    prompt: String,
    started: SystemTime,
    last: [Option<String>; 2],
    end_reason: Option<String>,
}

pub fn run(opts: Options) -> Result<i32> {
    let dir = duo_dir()?;
    let conv = agent::new_id();
    let short = conv[..8].to_string();
    let listener = runtime::bind(&dir, &format!("duo-{short}"))?;
    let work = dir.join(format!("{short}-work"));
    fs::DirBuilder::new().mode(0o700).create(&work)?;

    let codex_model = agent::codex_model_from_user_config();
    let make = |kind: Kind| {
        let model = if kind == Kind::Codex { codex_model.clone() } else { None };
        let mut agent = Agent::new(kind, opts.cwd.clone(), model);
        agent.memory = opts.memory.clone();
        agent
    };
    let agents = [make(opts.a), make(opts.b)];
    let transcript = if opts.transcript { Some(open_transcript(&short)?) } else { None };

    let (tx, rx) = mpsc::channel();
    let _guard = RawGuard::enable()?;
    let _marker = crate::color::Marker::apply(crate::color::for_duo(0)?);
    let keys = {
        let (ktx, krx) = mpsc::channel::<Vec<u8>>();
        let tx2 = tx.clone();
        thread::spawn(move || {
            for k in krx {
                if tx2.send(Ev::LocalKeys(k)).is_err() {
                    return;
                }
            }
        });
        Keys::start(ktx)
    };
    spawn_listener(listener, tx.clone());
    {
        use signal_hook::consts::{SIGHUP, SIGTERM};
        let mut signals = signal_hook::iterator::Signals::new([SIGHUP, SIGTERM])?;
        let tx = tx.clone();
        thread::spawn(move || {
            for sig in signals.forever() {
                if tx.send(Ev::Hangup(sig)).is_err() {
                    return;
                }
            }
        });
    }
    {
        let tx = tx.clone();
        thread::spawn(move || {
            while tx.send(Ev::Tick).is_ok() {
                thread::sleep(Duration::from_secs(1));
            }
        });
    }

    let (machine, effects) = Machine::new(opts.rounds, opts.prompt.clone());
    let mut c = Coordinator {
        conv,
        short,
        machine,
        agents,
        introduced: [false; 2],
        running: None,
        console: Console::new(),
        view: None,
        next_view: 0,
        mode: Mode::Normal,
        keys,
        tx,
        work,
        timeout: opts.timeout,
        transcript,
        dump: opts.memory.is_some(),
        cwd: opts.cwd.clone(),
        prompt: opts.prompt.clone(),
        started: SystemTime::now(),
        last: [None, None],
        end_reason: None,
    };
    c.intro(&opts);
    c.log(tr!("Human", "Mensch"), None, &opts.prompt);
    c.apply(effects);
    c.status();
    let result = c.event_loop(rx);
    c.finish();
    result.map(|()| 0)
}

fn spawn_listener(listener: UnixListener, tx: Sender<Ev>) {
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if runtime::check_peer(&stream).is_err() {
                continue;
            }
            if tx.send(Ev::ViewJoined(stream)).is_err() {
                return;
            }
        }
    });
}

fn open_transcript(short: &str) -> Result<fs::File> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .context(tr!("HOME is not set", "HOME nicht gesetzt"))?;
    let dir = base.join("bridget");
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    cleanup_old_transcripts(&dir);
    let day = humantime_day();
    let path = dir.join(format!("duo-{day}-{short}.jsonl"));
    OpenOptions::new().create(true).append(true).mode(0o600).open(&path).with_context(|| {
        if crate::locale::german() { format!("{path:?} öffnen") } else { format!("open {path:?}") }
    })
}

/// Protokolle älter als 14 Tage löschen.
fn cleanup_old_transcripts(dir: &Path) {
    let limit = Duration::from_secs(14 * 24 * 3600);
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("duo-") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > limit);
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn humantime_day() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Tage seit 1970 → Datum (proleptischer gregorianischer Kalender).
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

impl Coordinator {
    fn name(&self, side: Side) -> String {
        format!("{} ({side:?})", self.agents[side as usize].kind.name())
    }

    fn intro(&mut self, opts: &Options) {
        self.console.show(&Show::Note(if crate::locale::german() {
            format!(
                "bridget duo {} · A: {} · B: {} · höchstens {} Antworten · Arbeitsverzeichnis {}",
                self.short,
                opts.a.name(),
                opts.b.name(),
                opts.rounds,
                opts.cwd.display()
            )
        } else {
            format!(
                "bridget duo {} · A: {} · B: {} · at most {} responses · working directory {}",
                self.short,
                opts.a.name(),
                opts.b.name(),
                opts.rounds,
                opts.cwd.display()
            )
        }));
        self.console.show(&Show::Note(if crate::locale::german() {
            format!("Seite B in einer zweiten Konsole zeigen:  bridget duo join {}", self.short)
        } else {
            format!("Show side B in a second console:  bridget duo join {}", self.short)
        }));
        self.console.show(&Show::Note(
            tr!(
                "Automatic turns are read-only. To make changes, take over a side (pause, then u).",
                "Automatisch wird nur gelesen. Ändern geht nur nach Übernahme (Pause, dann u)."
            )
            .into(),
        ));
        if opts.a == Kind::Codex || opts.b == Kind::Codex {
            self.console.show(&Show::Note(
                tr!(
                    "Note: headless codex may read anywhere on disk, not only in the working directory.",
                    "Hinweis: codex darf headless überall auf der Platte lesen (nicht nur im Arbeitsverzeichnis)."
                ).into(),
            ));
        }
        if let Some(dir) = &opts.memory {
            self.console.show(&Show::Note(if crate::locale::german() {
                format!(
                    "Gedächtnis: {} (Index und Regeln in der Einleitung, --no-memory schaltet ab)",
                    dir.display()
                )
            } else {
                format!(
                    "Memory: {} (index and rules in the preface, --no-memory turns it off)",
                    dir.display()
                )
            }));
        }
        if self.transcript.is_some() {
            self.console.show(&Show::Note(
                tr!(
                    "Transcript: ~/.local/state/bridget/ (0600, deleted after 14 days)",
                    "Protokoll: ~/.local/state/bridget/ (0600, nach 14 Tagen gelöscht)"
                )
                .into(),
            ));
        }
    }

    fn event_loop(&mut self, rx: Receiver<Ev>) -> Result<()> {
        for ev in rx {
            match ev {
                Ev::LocalKeys(bytes) => self.local_keys(&bytes),
                Ev::View(id, msg) if self.is_view(id) => self.view_msg(msg),
                Ev::View(..) => {}
                Ev::ViewJoined(stream) => self.view_joined(stream),
                Ev::ViewGone(id) => self.view_gone(id),
                Ev::Agent { side, seq, event } => self.agent_event(side, seq, event),
                Ev::Tick => self.check_timeout(),
                Ev::Hangup(sig) => {
                    self.end_reason = Some(
                        if sig == signal_hook::consts::SIGHUP {
                            "Fenster geschlossen (SIGHUP)"
                        } else {
                            "beendet von außen (SIGTERM)"
                        }
                        .into(),
                    );
                    let effects = self.machine.quit();
                    self.apply(effects);
                }
            }
            if self.machine.phase == Phase::Finished {
                break;
            }
        }
        Ok(())
    }

    // ── Anzeige ─────────────────────────────────────────────────────────────────

    fn show_side(&mut self, side: Side, item: Show) {
        if side == Side::B && self.view.is_some() {
            self.send_view(&ToView::Show { item });
        } else {
            self.console.show(&item);
        }
    }

    fn show_all(&mut self, item: Show) {
        if self.view.is_some() {
            self.send_view(&ToView::Show { item: item.clone() });
        }
        self.console.show(&item);
    }

    fn send_view(&mut self, msg: &ToView) {
        // Bei einem Fehler nur die Verbindung schließen: Das Aufräumen (Hinweis, Übernahme
        // beenden) macht `view_gone`, sobald der Leser das Ende sieht.
        if let Some(v) = self.view.as_mut()
            && proto::write_json(&mut v.out, msg).is_err()
        {
            let _ = v.out.shutdown(std::net::Shutdown::Both);
        }
    }

    fn status(&mut self) {
        let m = &self.machine;
        let progress = format!("{}/{}", m.rounds, m.max_rounds);
        let next = self.name(m.next);
        let line = if crate::locale::german() {
            match &m.phase {
                Phase::Speaking { side, .. } => format!(
                    "duo {} · {progress} · spricht: {} · {}q Ende",
                    self.short,
                    self.name(*side),
                    if m.pause_requested() {
                        "⏸ Pause vorgemerkt · Space nicht pausieren · "
                    } else {
                        "Space Pause · "
                    }
                ),
                Phase::Paused => format!(
                    "⏸ Pause · {progress} · als nächstes: {next} · Space weiter · i Nachricht · u übernehmen · q Ende"
                ),
                Phase::Limit => format!(
                    "⏸ Rundenlimit erreicht ({progress}) · Space +2 Antworten · i Nachricht · u übernehmen · q Ende"
                ),
                Phase::TakenOver { side } => {
                    format!("✋ {} übernommen · Gespräch wartet", self.name(*side))
                }
                Phase::Error { reason } => {
                    format!("✗ Fehler: {reason} · Space nochmal versuchen · i · u · q Ende")
                }
                Phase::Finished => format!("Gespräch beendet ({progress})"),
            }
        } else {
            match &m.phase {
                Phase::Speaking { side, .. } => format!(
                    "duo {} · {progress} · speaking: {} · {}q End",
                    self.short,
                    self.name(*side),
                    if m.pause_requested() {
                        "⏸ Pause queued · Space cancel pause · "
                    } else {
                        "Space Pause · "
                    }
                ),
                Phase::Paused => format!(
                    "⏸ Paused · {progress} · next: {next} · Space continue · i Message · u Take over · q End"
                ),
                Phase::Limit => format!(
                    "⏸ Round limit reached ({progress}) · Space +2 responses · i Message · u Take over · q End"
                ),
                Phase::TakenOver { side } => {
                    format!("✋ {} taken over · conversation waiting", self.name(*side))
                }
                Phase::Error { reason } => {
                    format!("✗ Error: {reason} · Space retry · i · u · q End")
                }
                Phase::Finished => format!("Conversation ended ({progress})"),
            }
        };
        self.show_all(Show::Status(line));
    }

    fn log(&mut self, who: &str, side: Option<Side>, text: &str) {
        if let Some(f) = self.transcript.as_mut() {
            let line = serde_json::json!({
                "conv": self.conv,
                "time": SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                "who": who,
                "side": side.map(|s| format!("{s:?}")),
                "text": text,
            });
            if let Err(e) = writeln!(f, "{line}") {
                // Einmal melden und aufhören, statt still Lücken zu schreiben.
                self.transcript = None;
                self.show_all(Show::Note(if crate::locale::german() {
                    format!("⚠ Protokoll abgebrochen: {e}")
                } else {
                    format!("⚠ Transcript stopped: {e}")
                }));
            }
        }
    }

    // ── Effekte ─────────────────────────────────────────────────────────────────

    fn apply(&mut self, effects: Vec<Effect>) {
        for e in effects {
            match e {
                Effect::Run { side, seq, messages } => self.start(side, seq, messages),
                Effect::Kill { .. } => {
                    if let Some(r) = self.running.take() {
                        r.turn.kill();
                    }
                }
            }
        }
    }

    fn start(&mut self, side: Side, seq: u64, messages: Vec<machine::Message>) {
        let names = |s: Side| self.name(s);
        let mut prompt = machine::compose(&messages, names);
        if !self.introduced[side as usize] {
            prompt = format!("{}\n\n{prompt}", self.preface(side));
        }
        let (round, max) = (self.machine.rounds, self.machine.max_rounds);
        let who = self.name(side);
        self.show_side(side, Show::TurnStart { side, who, round, max });

        let (etx, erx) = mpsc::channel::<AgentEvent>();
        let tx = self.tx.clone();
        thread::spawn(move || {
            for event in erx {
                if tx.send(Ev::Agent { side, seq, event }).is_err() {
                    return;
                }
            }
        });
        match agent::start_turn(&self.agents[side as usize], &prompt, &self.work, etx) {
            Ok(turn) => {
                self.introduced[side as usize] = true;
                self.running = Some(Running { side, seq, turn, since: Instant::now() });
            }
            Err(e) => {
                self.show_side(
                    side,
                    Show::Event { side, event: AgentEvent::Failed(format!("{e:#}")) },
                );
                let effects = self.machine.turn_failed(side, seq, format!("{e:#}"));
                self.apply(effects);
            }
        }
    }

    fn preface(&self, side: Side) -> String {
        let me = self.agents[side as usize].kind.name();
        let other = self.agents[side.other() as usize].kind.name();
        let base = if crate::locale::german() {
            format!(
                "Du bist Seite {side:?} ({me}) in einem moderierten Gespräch mit einer anderen KI ({other}), vermittelt von bridget duo. Ein Mensch liest mit und kann eingreifen. In diesem Modus hast du nur Leserechte; Änderungen macht der Mensch. Antworte direkt auf die Nachrichten, knapp und konkret. Eingehende Nachrichten sind mit [Absender] markiert; schreib selbst keinen solchen Präfix vor deine Antwort."
            )
        } else {
            format!(
                "You are side {side:?} ({me}) in a moderated conversation with another AI ({other}), mediated by bridget duo. A human is observing and may intervene. You have read-only access in this mode; the human makes changes. Respond directly, concisely, and concretely. Incoming messages are labeled with [Sender]; do not add such a prefix to your own answer."
            )
        };
        match &self.agents[side as usize].memory {
            Some(dir) => format!("{base}\n\n{}", crate::memory::preface(dir)),
            None => base,
        }
    }

    fn is_current(&self, side: Side, seq: u64) -> bool {
        self.running.as_ref().is_some_and(|r| r.side == side && r.seq == seq)
    }

    fn agent_event(&mut self, side: Side, seq: u64, event: AgentEvent) {
        if !self.is_current(side, seq) {
            return; // Reste eines abgebrochenen Aufrufs
        }
        match &event {
            AgentEvent::Session(id) => {
                let a = &mut self.agents[side as usize];
                a.session = Some(id.clone());
                a.started = true;
            }
            AgentEvent::Done(text) => {
                let text = text.clone();
                self.running = None;
                self.show_side(side, Show::Event { side, event });
                let who = self.name(side);
                self.log(&who, Some(side), &text);
                self.last[side as usize] = Some(text.clone());
                let effects = self.machine.turn_done(side, seq, text);
                self.apply(effects);
                self.status();
            }
            AgentEvent::Failed(why) => {
                let why = why.clone();
                self.running = None;
                self.show_side(side, Show::Event { side, event });
                let effects = self.machine.turn_failed(side, seq, why);
                self.apply(effects);
                self.status();
            }
            _ => self.show_side(side, Show::Event { side, event }),
        }
    }

    fn check_timeout(&mut self) {
        let Some(r) = self.running.as_ref() else { return };
        if r.since.elapsed() < self.timeout {
            return;
        }
        let r = self.running.take().unwrap();
        r.turn.kill();
        let why = if crate::locale::german() {
            format!("Timeout nach {} s", self.timeout.as_secs())
        } else {
            format!("Timed out after {} s", self.timeout.as_secs())
        };
        self.show_side(
            r.side,
            Show::Event { side: r.side, event: AgentEvent::Failed(why.clone()) },
        );
        let effects = self.machine.turn_failed(r.side, r.seq, why);
        self.apply(effects);
        self.status();
    }

    // ── Tasten ──────────────────────────────────────────────────────────────────

    fn local_keys(&mut self, bytes: &[u8]) {
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Inject(mut input) => match input.feed(bytes) {
                LineResult::More => self.mode = Mode::Inject(input),
                LineResult::Done(text) => self.inject(text),
                LineResult::Cancel => {}
            },
            Mode::ChooseSide => match bytes.first() {
                Some(b'a') | Some(b'A') => self.local_takeover(Side::A),
                Some(b'b') | Some(b'B') => self.local_takeover(Side::B),
                _ => self
                    .console
                    .show(&Show::Note(tr!("Takeover canceled", "Übernahme abgebrochen").into())),
            },
            Mode::AfterTakeover => match bytes.first().copied().and_then(choice_from_key) {
                Some(choice) => self.takeover_done(choice),
                None => {
                    self.mode = Mode::AfterTakeover;
                    self.console.show(&Show::Note(after_takeover_help().into()));
                }
            },
            Mode::Normal => {
                for &b in bytes {
                    if !matches!(self.mode, Mode::Normal) {
                        break;
                    }
                    self.key(b, false);
                }
            }
        }
    }

    /// Gemeinsame Tasten für beide Konsolen. `from_view`: Taste kam aus der Ansicht.
    fn key(&mut self, key: u8, from_view: bool) {
        match key {
            b' ' => {
                let effects = self.machine.toggle_pause();
                self.apply(effects);
                self.status();
            }
            b'q' | 0x03 => {
                let effects = self.machine.quit();
                self.apply(effects);
            }
            b'i' if !from_view => {
                if self.interactive_phase() {
                    self.console.show(&Show::Note(if crate::locale::german() {
                        format!(
                            "Nachricht an {} (Enter senden, Esc abbrechen):",
                            self.name(self.machine.next)
                        )
                    } else {
                        format!(
                            "Message to {} (Enter sends, Esc cancels):",
                            self.name(self.machine.next)
                        )
                    }));
                    self.mode = Mode::Inject(LineInput::new());
                } else {
                    self.console.show(&Show::Note(
                        tr!("Pause first (Space), then i", "Erst pausieren (Space), dann i").into(),
                    ));
                }
            }
            b'u' if !from_view => {
                if !self.interactive_phase() {
                    self.console.show(&Show::Note(
                        tr!("Pause first (Space), then u", "Erst pausieren (Space), dann u").into(),
                    ));
                } else if self.view.is_some() {
                    self.local_takeover(Side::A);
                } else {
                    self.console.show(&Show::Note(
                        tr!("Take over which side? a / b", "Welche Seite übernehmen? a / b").into(),
                    ));
                    self.mode = Mode::ChooseSide;
                }
            }
            _ => {}
        }
    }

    fn interactive_phase(&self) -> bool {
        matches!(self.machine.phase, Phase::Paused | Phase::Limit | Phase::Error { .. })
    }

    fn inject(&mut self, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if self.machine.inject(text.clone()) {
            self.log(tr!("Human", "Mensch"), None, &text);
            let next = self.name(self.machine.next);
            self.show_all(Show::Note(if crate::locale::german() {
                format!("✉ Nachricht an {next} vorgemerkt: {text}")
            } else {
                format!("✉ Message to {next} queued: {text}")
            }));
        }
    }

    // ── Übernahme ───────────────────────────────────────────────────────────────

    fn local_takeover(&mut self, side: Side) {
        let agent = &self.agents[side as usize];
        let Some(command) = agent.interactive_command().filter(|_| agent.started) else {
            self.console.show(&Show::Note(if crate::locale::german() {
                format!(
                    "{} hat noch keine Sitzung, erst nach der ersten Antwort möglich",
                    self.name(side)
                )
            } else {
                format!(
                    "{} has no session yet; available after the first response",
                    self.name(side)
                )
            }));
            return;
        };
        if !self.machine.take_over(side) {
            return;
        }
        self.status();
        let cwd = self.agents[side as usize].cwd.clone();
        self.keys.suspend();
        let result = run_interactive(&command, &cwd);
        self.keys.resume();
        if let Err(e) = result {
            self.console.show(&Show::Note(format!("⚠ {e:#}")));
        }
        self.mode = Mode::AfterTakeover;
        self.console.show(&Show::Note(after_takeover_help().into()));
    }

    fn takeover_done(&mut self, choice: Choice) {
        let effects = self.machine.takeover_ended(AfterTakeover::from(choice));
        self.apply(effects);
        self.status();
    }

    // ── Ansicht ─────────────────────────────────────────────────────────────────

    fn view_joined(&mut self, stream: UnixStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let mut reader = match stream.try_clone() {
            Ok(s) => BufReader::new(s),
            Err(_) => return,
        };
        let joined = matches!(
            proto::read_json::<FromView>(&mut reader),
            Ok(Some(FromView::Join { conv })) if conv == self.short
        );
        let mut writer = stream;
        if !joined {
            return;
        }
        if self.view.is_some() {
            let _ = proto::write_json(
                &mut writer,
                &ToView::Bye {
                    reason: tr!(
                        "A view is already connected",
                        "Es ist schon eine Ansicht verbunden"
                    )
                    .into(),
                },
            );
            return;
        }
        let _ = reader.get_ref().set_read_timeout(None);
        let welcome = ToView::Welcome { conv: self.short.clone(), side_name: self.name(Side::B) };
        if proto::write_json(&mut writer, &welcome).is_err() {
            return;
        }
        self.next_view += 1;
        let id = self.next_view;
        self.view = Some(View { id, out: writer });
        let tx = self.tx.clone();
        thread::spawn(move || {
            while let Ok(Some(msg)) = proto::read_json::<FromView>(&mut reader) {
                if tx.send(Ev::View(id, msg)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Ev::ViewGone(id));
        });
        self.console.show(&Show::Note(if crate::locale::german() {
            format!("Zweite Konsole verbunden: {} wird jetzt dort angezeigt", self.name(Side::B))
        } else {
            format!("Second console connected: {} is now shown there", self.name(Side::B))
        }));
        self.status();
    }

    fn is_view(&self, id: u64) -> bool {
        self.view.as_ref().is_some_and(|v| v.id == id)
    }

    fn view_gone(&mut self, id: u64) {
        if !self.is_view(id) {
            return;
        }
        self.view = None;
        self.console.show(&Show::Note(if crate::locale::german() {
            format!("Zweite Konsole getrennt: {} wird wieder hier angezeigt", self.name(Side::B))
        } else {
            format!("Second console disconnected: {} is shown here again", self.name(Side::B))
        }));
        if self.machine.phase == (Phase::TakenOver { side: Side::B }) {
            let effects = self.machine.takeover_ended(AfterTakeover::ContinueWithoutHandover);
            self.apply(effects);
            self.status();
        }
    }

    fn view_msg(&mut self, msg: FromView) {
        match msg {
            FromView::Key { key } => self.key(key, true),
            FromView::Inject { text } => self.inject(text),
            FromView::Takeover => {
                let agent = &self.agents[Side::B as usize];
                let command = agent.interactive_command().filter(|_| agent.started);
                match command {
                    Some(command) if self.machine.take_over(Side::B) => {
                        let cwd = self.agents[Side::B as usize].cwd.clone();
                        self.status();
                        self.send_view(&ToView::TakeoverGranted { command, cwd });
                    }
                    Some(_) => self.show_side(
                        Side::B,
                        Show::Note(
                            tr!("Pause first (Space), then u", "Erst pausieren (Space), dann u")
                                .into(),
                        ),
                    ),
                    None => self.show_side(
                        Side::B,
                        Show::Note(
                            tr!(
                                "No session yet; available after the first response",
                                "Noch keine Sitzung, erst nach der ersten Antwort möglich"
                            )
                            .into(),
                        ),
                    ),
                }
            }
            FromView::TakeoverDone { choice } => {
                if self.machine.phase == (Phase::TakenOver { side: Side::B }) {
                    self.takeover_done(choice);
                }
            }
            FromView::Join { .. } => {}
        }
    }

    // ── Ende ────────────────────────────────────────────────────────────────────

    fn finish(&mut self) {
        if let Some(r) = self.running.take() {
            r.turn.kill_and_wait();
        }
        self.status();
        let mut hints = Vec::new();
        for a in &self.agents {
            if let (true, Some(cmd)) = (a.started, a.interactive_command()) {
                hints.push(cmd.join(" "));
            }
        }
        if !hints.is_empty() {
            self.console.show(&Show::Note(format!(
                "{}: {}",
                tr!("Resume sessions", "Sitzungen fortsetzen"),
                hints.join("  ·  ")
            )));
        }
        if self.dump {
            let sides: Vec<crate::dump::DuoSide> = [Side::A, Side::B]
                .into_iter()
                .map(|s| {
                    let a = &self.agents[s as usize];
                    crate::dump::DuoSide {
                        label: self.name(s),
                        resume: a.interactive_command().filter(|_| a.started).map(|c| c.join(" ")),
                        last: self.last[s as usize].clone(),
                    }
                })
                .collect();
            let reason = self.end_reason.as_deref().unwrap_or("Gespräch beendet");
            crate::dump::duo(&self.short, &self.cwd, self.started, reason, &self.prompt, &sides);
        }
        self.send_view(&ToView::Bye {
            reason: tr!("Conversation ended", "Gespräch beendet").into(),
        });
        let _ = fs::remove_dir_all(&self.work);
        if let Ok(dir) = duo_dir() {
            let _ = fs::remove_file(runtime::socket_path(&dir, &format!("duo-{}", self.short)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_format() {
        let d = humantime_day();
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("20"));
    }
}
