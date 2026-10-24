//! Die Brücke zwischen zwei Konsolen: Socket, Partner, Senden und Empfangen.
//!
//! Regeln: Es wird nie ein Enter eingefügt. Text wird auf beiden Seiten bereinigt und
//! auf `proto::MAX_TEXT` begrenzt. Mehrzeiliger Text geht nur per Bracketed Paste ins
//! Programm, sonst wird er zurückgehalten (`Ctrl-Space Ctrl-v` holt ihn als eine Zeile).

use std::io::{BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::picker::{self, Action, Picker};
use crate::profile::{Last, Profile};
use crate::proto::{self, Control, Msg, What};
use crate::text::{self, Injection};
use crate::title::{self as title_filter, Piece};
use crate::{desk, help, name, runtime, terminal};

/// So viel Ausgabe seit dem letzten Enter wird für „letzte Ausgabe“ aufgehoben.
const LOG_CAP: usize = 512 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
/// So lange steht eine Meldung im Fenstertitel, danach wieder der Titel des Programms.
const NOTE_TIME: Duration = Duration::from_secs(4);

pub const CTRL_V: u8 = 0x16;
const TAB: u8 = b'\t';

struct Partner {
    name: String,
    link_id: u64,
    out: UnixStream,
}

pub struct Bridge {
    pub name: String,
    program: String,
    dir: PathBuf,
    profile: Profile,
    pub screen: Arc<Mutex<text::Parser>>,
    pty_in: Mutex<Box<dyn Write + Send>>,
    partner: Mutex<Option<Partner>>,
    log: Mutex<Vec<u8>>,
    rows_at_enter: Mutex<Vec<String>>,
    /// `text::scrolled` beim letzten Enter.
    scrolled_at_enter: Mutex<usize>,
    pending_one_line: Mutex<Option<String>>,
    choosing: Mutex<Option<Vec<String>>>,
    overlay: Mutex<Option<Overlay>>,
    /// Ausgabe an den echten Bildschirm. `Some(puffer)`, solange bridget den Bildschirm
    /// selbst benutzt (Auswahl oder Hilfe): Dann wird die Programmausgabe dort gesammelt.
    out_gate: Mutex<Option<Vec<u8>>>,
    next_id: AtomicU64,
    titles: Mutex<title_filter::Filter>,
    title_state: Mutex<TitleState>,
}

#[derive(Default)]
struct TitleState {
    /// Letzter Titel des Programms (OSC-Nummer, Text).
    child: Option<(u8, String)>,
    /// Solange steht eine Meldung von bridget im Titel.
    note_until: Option<Instant>,
}

struct OpenPicker {
    picker: Picker,
    /// Zielnummer (`Ctrl-Space 3 A`), sonst der verbundene Partner.
    target: Option<u32>,
    /// War das Programm beim Öffnen im Alternate Screen?
    child_alt: bool,
}

enum Overlay {
    Picker(OpenPicker),
    Help { child_alt: bool },
}

enum OverlayOutcome {
    Stay,
    Picker { child_alt: bool, action: Action, target: Option<u32> },
    Help { child_alt: bool },
}

impl Bridge {
    pub fn new(
        name: String,
        program: String,
        dir: PathBuf,
        profile: Profile,
        screen: Arc<Mutex<text::Parser>>,
        pty_in: Box<dyn Write + Send>,
    ) -> Arc<Self> {
        Arc::new(Bridge {
            name,
            program,
            dir,
            profile,
            screen,
            pty_in: Mutex::new(pty_in),
            partner: Mutex::new(None),
            log: Mutex::new(Vec::new()),
            rows_at_enter: Mutex::new(Vec::new()),
            scrolled_at_enter: Mutex::new(0),
            pending_one_line: Mutex::new(None),
            choosing: Mutex::new(None),
            overlay: Mutex::new(None),
            out_gate: Mutex::new(None),
            next_id: AtomicU64::new(1),
            titles: Mutex::new(title_filter::Filter::new()),
            title_state: Mutex::new(TitleState::default()),
        })
    }

    pub fn start_listener(self: &Arc<Self>, listener: UnixListener) {
        // Abgelaufene Meldungen im Titel durch den Programmtitel ersetzen.
        let clock = Arc::clone(self);
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(250));
                let expired = {
                    let mut st = clock.title_state.lock().unwrap();
                    match st.note_until {
                        Some(t) if t <= Instant::now() => {
                            st.note_until = None;
                            true
                        }
                        _ => false,
                    }
                };
                if expired {
                    clock.refresh_title();
                }
            }
        });
        let me = Arc::clone(self);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let me = Arc::clone(&me);
                thread::spawn(move || {
                    if let Err(e) = me.handle_incoming(stream) {
                        me.title(&format!("⚠ {e:#}"));
                    }
                });
            }
        });
    }

    /// Hinweis zum Start: Profil, Versionsprüfung.
    pub fn greet(&self) {
        let mut msg = format!("{} {}", tr!("Profile", "Profil"), self.profile.name);
        if self.profile.version_mismatch {
            let warning = if crate::locale::german() {
                format!(
                    " · ⚠ Version {} nicht geprüft, Ctrl-Space Enter schickt den Bildschirm",
                    self.profile.version.as_deref().unwrap_or("unbekannt")
                )
            } else {
                format!(
                    " · ⚠ Version {} not verified, Ctrl-Space Enter sends the screen",
                    self.profile.version.as_deref().unwrap_or("unknown")
                )
            };
            msg.push_str(&warning);
        }
        msg.push_str(tr!(" · Ctrl-Space H: Help", " · Ctrl-Space H: Hilfe"));
        self.title(&msg);
    }

    // ── Ereignisse aus dem Wrapper ──────────────────────────────────────────────

    /// Programmausgabe auf den echten Bildschirm (oder zurückhalten, siehe `out_gate`).
    pub fn output_to_terminal(&self, bytes: &[u8]) -> bool {
        let mut gate = self.out_gate.lock().unwrap();
        let pieces = self.titles.lock().unwrap().feed(bytes);
        let mut out = Vec::with_capacity(bytes.len());
        for piece in pieces {
            match piece {
                Piece::Bytes(b) => out.extend_from_slice(&b),
                Piece::Title(kind, t) => {
                    let label = self.label();
                    let mut st = self.title_state.lock().unwrap();
                    st.child = Some((kind, t));
                    if st.note_until.is_none() {
                        out.extend_from_slice(&osc_title(kind, &compose(&label, &st)));
                    }
                }
            }
        }
        if let Some(held) = gate.as_mut() {
            held.extend_from_slice(&out);
            return true;
        }
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&out).and_then(|_| stdout.flush()).is_ok()
    }

    pub fn on_output(&self, bytes: &[u8]) {
        let mut log = self.log.lock().unwrap();
        log.extend_from_slice(bytes);
        if log.len() > LOG_CAP {
            let cut = log.len() - LOG_CAP;
            log.drain(..cut);
        }
    }

    /// Tastatureingabe ans Programm. `false`, wenn das PTY weg ist.
    pub fn write_to_program(&self, bytes: &[u8]) -> bool {
        let mut w = self.pty_in.lock().unwrap();
        w.write_all(bytes).and_then(|_| w.flush()).is_ok()
    }

    /// Der Mensch hat Enter gedrückt: Merkpunkt für „letzte Ausgabe“.
    pub fn on_enter(&self) {
        self.log.lock().unwrap().clear();
        let mut parser = self.screen.lock().unwrap();
        *self.rows_at_enter.lock().unwrap() = text::screen_rows(parser.screen());
        *self.scrolled_at_enter.lock().unwrap() = text::scrolled(&mut parser);
    }

    /// Läuft gerade die Auswahl nach "Verbinden"? Dann gehört die nächste Taste ihr.
    pub fn take_choice(self: &Arc<Self>, key: u8) -> bool {
        let Some(names) = self.choosing.lock().unwrap().take() else {
            return false;
        };
        let idx = (key as char).to_digit(10).map(|d| d as usize);
        match idx.and_then(|i| i.checked_sub(1)).and_then(|i| names.get(i)) {
            Some(target) => self.connect_and_report(target),
            None => self.title(tr!("Connection canceled", "Verbinden abgebrochen")),
        }
        true
    }

    /// Taste nach dem Präfix; `target` ist die Nummer davor (`Ctrl-Space 3 Enter`).
    pub fn command(self: &Arc<Self>, key: u8, target: Option<u32>) {
        match key.to_ascii_lowercase() {
            b'v' | b'c' => match target {
                Some(n) => self.connect_and_report(&self.target_name(n)),
                None => self.choose_partner(),
            },
            b'\r' | b'\n' | b'l' => self.report(self.send(What::Last, target)),
            b'b' | b's' => self.report(self.send(What::Screen, target)),
            b'm' => self.report(self.send_marked(target)),
            b't' | b'd' => self.disconnect(true),
            b'a' | b'p' => self.open_picker(target),
            TAB => self.report(match target {
                Some(n) => desk::show(n),
                None => desk::next(),
            }),
            b'n' => self.report(desk::new_tab(&self.name)),
            b'h' | b'?' => self.open_help(),
            CTRL_V => self.insert_pending_one_line(),
            _ => {
                let _ = terminal::bell(&mut std::io::stdout().lock());
            }
        }
    }

    pub fn shutdown(&self) {
        let child_alt = self.overlay.lock().unwrap().take().map(|overlay| match overlay {
            Overlay::Picker(open) => open.child_alt,
            Overlay::Help { child_alt } => child_alt,
        });
        if let Some(child_alt) = child_alt {
            self.close_overlay(child_alt);
        }
        self.disconnect(false);
        let _ = std::fs::remove_file(runtime::socket_path(&self.dir, &self.name));
    }

    // ── Verbinden ───────────────────────────────────────────────────────────────

    fn choose_partner(self: &Arc<Self>) {
        let others: Vec<String> = match runtime::list(&self.dir) {
            Ok(names) => names
                .into_iter()
                .filter(|n| *n != self.name)
                .filter(|n| runtime::connect(&self.dir, n).is_ok())
                .collect(),
            Err(e) => return self.title(&format!("⚠ {e:#}")),
        };
        match others.len() {
            0 => self.title(tr!(
                "No other bridget console found",
                "Keine andere bridget-Konsole gefunden"
            )),
            1 => self.connect_and_report(&others[0]),
            _ => {
                let shown: Vec<String> = others
                    .iter()
                    .take(9)
                    .enumerate()
                    .map(|(i, n)| format!("{}={n}", i + 1))
                    .collect();
                let prompt = if crate::locale::german() {
                    format!("Verbinden mit: {} (andere Taste = Abbruch)", shown.join(" "))
                } else {
                    format!("Connect to: {} (any other key cancels)", shown.join(" "))
                };
                self.title(&prompt);
                *self.choosing.lock().unwrap() = Some(others.into_iter().take(9).collect());
            }
        }
    }

    /// `--connect NAME`: Beim Start verbinden. NAME darf noch hochfahren (bis zu 10 s).
    pub fn auto_connect(self: &Arc<Self>, target: String) {
        let me = Arc::clone(self);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match me.connect_to(&target) {
                    Ok(()) => return me.title(&connected(&target)),
                    Err(e) if Instant::now() >= deadline => return me.title(&format!("⚠ {e:#}")),
                    Err(_) => thread::sleep(Duration::from_millis(200)),
                }
            }
        });
    }

    fn connect_and_report(self: &Arc<Self>, target: &str) {
        match self.connect_to(target) {
            Ok(()) => self.title(&connected(target)),
            Err(e) => self.title(&format!("⚠ {e:#}")),
        }
    }

    fn connect_to(self: &Arc<Self>, target: &str) -> Result<()> {
        name::validate(target)?;
        if target == self.name {
            bail!(
                "{}",
                tr!(
                    "A console cannot connect to itself",
                    "Eine Konsole kann sich nicht mit sich selbst verbinden"
                )
            );
        }
        if let Some(p) = self.partner.lock().unwrap().as_ref() {
            if p.name == target {
                return Ok(());
            }
            bail!(
                "{}",
                if crate::locale::german() {
                    format!("Schon verbunden mit {} (erst Ctrl-Space T)", p.name)
                } else {
                    format!("Already connected to {} (use Ctrl-Space T first)", p.name)
                }
            );
        }
        let stream = runtime::connect(&self.dir, target)?;
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        proto::write(&mut &stream, &Msg::Hello { v: proto::VERSION, from: self.name.clone() })?;
        let mut reader = BufReader::new(stream.try_clone()?);
        match proto::read(&mut reader)? {
            Some(Msg::Hello { v, from }) if from == target => proto::check_version(v)?,
            Some(Msg::Error { message }) => bail!("{target}: {message}"),
            other => bail!(
                "{}",
                if crate::locale::german() {
                    format!("{target} antwortet unerwartet: {other:?}")
                } else {
                    format!("unexpected response from {target}: {other:?}")
                }
            ),
        }
        stream.set_read_timeout(None)?;
        let link_id = self.set_partner(target, stream.try_clone()?)?;
        self.spawn_link_reader(reader, link_id, target.to_string());
        Ok(())
    }

    /// Partner eintragen, falls noch keiner (oder derselbe) eingetragen ist.
    fn set_partner(&self, name: &str, out: UnixStream) -> Result<u64> {
        let mut guard = self.partner.lock().unwrap();
        if let Some(p) = guard.as_ref().filter(|p| p.name != name) {
            bail!(
                "{}",
                if crate::locale::german() {
                    format!("schon verbunden mit {}", p.name)
                } else {
                    format!("already connected to {}", p.name)
                }
            );
        }
        let link_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        *guard = Some(Partner { name: name.to_string(), link_id, out });
        Ok(link_id)
    }

    fn handle_incoming(self: &Arc<Self>, stream: UnixStream) -> Result<()> {
        runtime::check_peer(&stream)?;
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut out = stream.try_clone()?;
        match proto::read(&mut reader)? {
            Some(Msg::Hello { v, from }) => {
                name::validate(&from)?;
                if let Err(e) = proto::check_version(v) {
                    proto::write(&mut out, &Msg::Error { message: format!("{e:#}") })?;
                    return Ok(());
                }
                let link_id = match self.set_partner(&from, stream.try_clone()?) {
                    Ok(id) => id,
                    Err(e) => {
                        proto::write(&mut out, &Msg::Error { message: format!("{e:#}") })?;
                        return Ok(());
                    }
                };
                proto::write(&mut out, &Msg::Hello { v: proto::VERSION, from: self.name.clone() })?;
                stream.set_read_timeout(None)?;
                self.title(&connected(&from));
                self.spawn_link_reader(reader, link_id, from);
            }
            Some(Msg::Control { v, cmd }) => {
                let reply = match proto::check_version(v) {
                    Ok(()) => self.control(cmd),
                    Err(e) => Msg::Error { message: format!("{e:#}") },
                };
                proto::write(&mut out, &reply)?;
            }
            // Nur geprüft, ob hier jemand zuhört (bind, Auswahl): kein Fehler.
            None => {}
            Some(other) => bail!(
                "{}",
                if crate::locale::german() {
                    format!("unerwartete erste Nachricht: {other:?}")
                } else {
                    format!("unexpected first message: {other:?}")
                }
            ),
        }
        Ok(())
    }

    fn control(self: &Arc<Self>, cmd: Control) -> Msg {
        let result = match cmd {
            // Einmal einfügen ohne Verbindung; `receive` meldet selbst im Titel.
            Control::Paste { from, text } => {
                return match name::validate(&from) {
                    Ok(()) => Msg::Ack { id: 0, note: self.receive(&from, &text) },
                    Err(e) => Msg::Error { message: format!("{e:#}") },
                };
            }
            Control::Status => {
                return Msg::Status {
                    name: self.name.clone(),
                    partner: self.partner_name(),
                    program: self.program.clone(),
                };
            }
            Control::Link { to } => self.connect_to(&to).map(|()| connected(&to)),
            Control::Unlink => match self.partner_name() {
                Some(p) => {
                    self.disconnect(false);
                    Ok(disconnected(&p))
                }
                None => Ok(tr!("was not connected", "war nicht verbunden").into()),
            },
            Control::Send { what } => self.send(what, None),
        };
        self.report(result.as_ref().map(String::clone).map_err(|e| anyhow::anyhow!("{e:#}")));
        match result {
            Ok(note) => Msg::Ack { id: 0, note },
            Err(e) => Msg::Error { message: format!("{e:#}") },
        }
    }

    /// `partner` ist der Name aus dem Handshake. Ein `from` in späteren Nachrichten zählt
    /// nicht: Eingefügt wird nur, was über genau diese Verbindung kommt.
    fn spawn_link_reader(
        self: &Arc<Self>,
        mut reader: BufReader<UnixStream>,
        link_id: u64,
        partner: String,
    ) {
        let me = Arc::clone(self);
        thread::spawn(move || {
            let reason = loop {
                match proto::read(&mut reader) {
                    Ok(Some(Msg::Paste { v, .. })) if v != proto::VERSION => {
                        break if crate::locale::german() {
                            format!("Protokollversion {v} statt {}", proto::VERSION)
                        } else {
                            format!("protocol version {v} instead of {}", proto::VERSION)
                        };
                    }
                    Ok(Some(Msg::Paste { id, text, .. })) => {
                        let note = me.receive(&partner, &text);
                        me.send_to_partner(link_id, &Msg::Ack { id, note });
                    }
                    Ok(Some(Msg::Ack { note, .. })) => {
                        let who = me.partner_name().unwrap_or_default();
                        me.title(&format!("✓ {who}: {note}"));
                    }
                    Ok(Some(Msg::Error { message })) => me.title(&format!("⚠ {message}")),
                    Ok(Some(Msg::Bye)) | Ok(None) => {
                        break tr!("disconnected", "getrennt").to_string();
                    }
                    Ok(Some(other)) => {
                        break if crate::locale::german() {
                            format!("unerwartete Nachricht {other:?}")
                        } else {
                            format!("unexpected message {other:?}")
                        };
                    }
                    Err(e) => break format!("{e:#}"),
                }
            };
            let mut guard = me.partner.lock().unwrap();
            if guard.as_ref().is_some_and(|p| p.link_id == link_id) {
                let name = guard.take().unwrap().name;
                drop(guard);
                let note = if crate::locale::german() {
                    format!("Verbindung zu {name}: {reason}")
                } else {
                    format!("Connection to {name}: {reason}")
                };
                me.title(&note);
            }
        });
    }

    fn send_to_partner(&self, link_id: u64, msg: &Msg) {
        let guard = self.partner.lock().unwrap();
        if let Some(p) = guard.as_ref().filter(|p| p.link_id == link_id) {
            let _ = proto::write(&mut &p.out, msg);
        }
    }

    fn disconnect(&self, report: bool) {
        let old = self.partner.lock().unwrap().take();
        match old {
            Some(p) => {
                let _ = proto::write(&mut &p.out, &Msg::Bye);
                let _ = p.out.shutdown(std::net::Shutdown::Both);
                if report {
                    self.title(&disconnected(&p.name));
                }
            }
            None if report => self.title(tr!("not connected", "nicht verbunden")),
            None => {}
        }
    }

    fn partner_name(&self) -> Option<String> {
        self.partner.lock().unwrap().as_ref().map(|p| p.name.clone())
    }

    // ── Senden ──────────────────────────────────────────────────────────────────

    /// Der Text und ob sein Anfang vermutlich fehlt.
    fn extract(&self, what: What) -> (String, bool) {
        let mut parser = self.screen.lock().unwrap();
        let mut cut = false;
        let raw = match (what, self.profile.effective_last()) {
            (What::Screen, _) | (What::Last, Last::Screen) => text::screen_text(parser.screen()),
            (What::Last, Last::Log) => {
                let (_, cols) = parser.screen().size();
                text::replay_since_enter(&self.log.lock().unwrap(), cols.get())
            }
            // Auch was seit dem Enter schon aus dem Bild gescrollt ist.
            (What::Last, Last::ScreenDiff) => {
                let before = self.rows_at_enter.lock().unwrap();
                cut = text::top_row_is_new(&before, parser.screen());
                text::screen_diff(
                    &before,
                    &text::rows_since(&mut parser, *self.scrolled_at_enter.lock().unwrap()),
                    &self.profile.frame,
                )
            }
        };
        (text::sanitize(&raw), cut)
    }

    /// Gibt eine kurze Meldung zurück, was passiert ist.
    fn send(&self, what: What, target: Option<u32>) -> Result<String> {
        if target.is_none() && self.partner_name().is_none() {
            bail!("{}", tr!("not connected (Ctrl-Space V)", "nicht verbunden (Ctrl-Space V)"));
        }
        let (text, cut) = self.extract(what);
        let note = self.deliver(text, target)?;
        Ok(if cut {
            format!(
                "{note}{}",
                tr!(
                    " · ⚠ start may be missing (full-screen program)",
                    " · ⚠ Anfang fehlt evtl. (Programm im Vollbild)"
                )
            )
        } else {
            note
        })
    }

    /// An Konsole `target` (Nummer in der eigenen Gruppe) oder an den Partner.
    fn deliver(&self, text: String, target: Option<u32>) -> Result<String> {
        let Some(n) = target else { return self.send_text(text) };
        let name = self.target_name(n);
        if self.partner_name().as_deref() == Some(name.as_str()) {
            return self.send_text(text);
        }
        self.send_once(&name, text)
    }

    /// Name der Konsole mit Nummer `n` in derselben Gruppe (`3` bzw. `2-3`).
    fn target_name(&self, n: u32) -> String {
        format!("{}{n}", desk::group(&self.name))
    }

    /// Ohne feste Verbindung: einmal verbinden, einfügen lassen, Antwort abwarten.
    fn send_once(&self, target: &str, text: String) -> Result<String> {
        if target == self.name {
            bail!(
                "{}",
                tr!("cannot send to this console itself", "kann nicht an sich selbst senden")
            );
        }
        let text = checked(text)?;
        let lines = text::line_count(&text);
        name::validate(target)?;
        let stream = runtime::connect(&self.dir, target)?;
        stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let cmd = Control::Paste { from: self.name.clone(), text };
        proto::write(&mut &stream, &Msg::Control { v: proto::VERSION, cmd })?;
        match proto::read(&mut BufReader::new(stream))? {
            Some(Msg::Ack { note, .. }) => Ok(if crate::locale::german() {
                format!("→ {lines} Zeile(n) an {target}: {note}")
            } else {
                format!("→ {lines} line(s) to {target}: {note}")
            }),
            Some(Msg::Error { message }) => bail!("{target}: {message}"),
            other => bail!(
                "{}",
                if crate::locale::german() {
                    format!("{target} antwortet unerwartet: {other:?}")
                } else {
                    format!("{target} sent an unexpected reply: {other:?}")
                }
            ),
        }
    }

    /// Mausmarkierung (tmux-Puffer) senden und danach verwerfen.
    fn send_marked(&self, target: Option<u32>) -> Result<String> {
        if target.is_none() && self.partner_name().is_none() {
            bail!("{}", tr!("not connected (Ctrl-Space V)", "nicht verbunden (Ctrl-Space V)"));
        }
        let note = self.deliver(text::dedent(&desk::marked()?), target)?;
        desk::clear_marked();
        Ok(note)
    }

    fn send_text(&self, text: String) -> Result<String> {
        let text = checked(text)?;
        let lines = text::line_count(&text);
        let guard = self.partner.lock().unwrap();
        let Some(p) = guard.as_ref() else {
            bail!("{}", tr!("not connected (Ctrl-Space V)", "nicht verbunden (Ctrl-Space V)"));
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        proto::write(
            &mut &p.out,
            &Msg::Paste { v: proto::VERSION, id, from: self.name.clone(), text },
        )
        .with_context(|| {
            if crate::locale::german() {
                format!("an {} senden", p.name)
            } else {
                format!("send to {}", p.name)
            }
        })?;
        Ok(if crate::locale::german() {
            format!("→ {lines} Zeile(n) an {}", p.name)
        } else {
            format!("→ {lines} line(s) to {}", p.name)
        })
    }

    fn report(&self, result: Result<String>) {
        match result {
            Ok(msg) => self.title(&msg),
            Err(e) => self.title(&format!("⚠ {e:#}")),
        }
    }

    // ── Auswahlmodus ────────────────────────────────────────────────────────────

    pub fn overlay_active(&self) -> bool {
        self.overlay.lock().unwrap().is_some()
    }

    fn open_picker(&self, target: Option<u32>) {
        if target.is_none() && self.partner_name().is_none() {
            return self
                .title(tr!("⚠ not connected (Ctrl-Space V)", "⚠ nicht verbunden (Ctrl-Space V)"));
        }
        let (lines, child_alt, rows, cols) = {
            let mut parser = self.screen.lock().unwrap();
            let child_alt = parser.screen().alternate_screen();
            let (rows, cols) = parser.screen().size();
            (text::all_lines(&mut parser), child_alt, rows.get(), cols.get())
        };
        let picker = Picker::new(lines, rows);
        // Ab jetzt gehört der Bildschirm uns; Programmausgabe wird gesammelt.
        *self.out_gate.lock().unwrap() = Some(Vec::new());
        let mut out = String::new();
        if !child_alt {
            out.push_str("\x1b[?1049h"); // eigener Alternate Screen, Hauptbildschirm bleibt
        }
        out.push_str(&picker.render(cols));
        write_stdout(out.as_bytes());
        *self.overlay.lock().unwrap() =
            Some(Overlay::Picker(OpenPicker { picker, target, child_alt }));
    }

    fn open_help(&self) {
        let (child_alt, rows, cols) = {
            let parser = self.screen.lock().unwrap();
            let (rows, cols) = parser.screen().size();
            (parser.screen().alternate_screen(), rows.get(), cols.get())
        };
        let partner = self.partner_name();
        *self.out_gate.lock().unwrap() = Some(Vec::new());
        let mut out = String::new();
        if !child_alt {
            out.push_str("\x1b[?1049h");
        }
        out.push_str(&help::render(&self.name, partner.as_deref(), rows, cols));
        write_stdout(out.as_bytes());
        *self.overlay.lock().unwrap() = Some(Overlay::Help { child_alt });
    }

    /// Tastatureingabe, solange Auswahl oder Hilfe offen ist.
    pub fn overlay_input(&self, input: &[u8]) {
        let mut guard = self.overlay.lock().unwrap();
        let Some(overlay) = guard.as_mut() else { return };
        let outcome = match overlay {
            Overlay::Picker(open) => {
                let mut action = Action::Continue;
                for key in picker::parse_keys(input) {
                    action = open.picker.handle(key);
                    if action != Action::Continue {
                        break;
                    }
                }
                if action == Action::Continue {
                    let cols = self.screen.lock().unwrap().screen().size().1.get();
                    write_stdout(open.picker.render(cols).as_bytes());
                    OverlayOutcome::Stay
                } else {
                    OverlayOutcome::Picker {
                        child_alt: open.child_alt,
                        action,
                        target: open.target,
                    }
                }
            }
            Overlay::Help { child_alt } => {
                let close = input.iter().any(|b| {
                    matches!(b.to_ascii_lowercase(), 0x1b | b'\r' | b'\n' | b'h' | b'?' | b'q')
                });
                if close {
                    OverlayOutcome::Help { child_alt: *child_alt }
                } else {
                    let _ = terminal::bell(&mut std::io::stdout().lock());
                    OverlayOutcome::Stay
                }
            }
        };
        if matches!(outcome, OverlayOutcome::Stay) {
            return;
        }
        guard.take();
        drop(guard);
        match outcome {
            OverlayOutcome::Picker { child_alt, action, target } => {
                self.close_overlay(child_alt);
                match action {
                    Action::Send(text) => self.report(self.deliver(text::dedent(&text), target)),
                    _ => self.title(tr!("Selection canceled", "Auswahl abgebrochen")),
                }
            }
            OverlayOutcome::Help { child_alt } => self.close_overlay(child_alt),
            OverlayOutcome::Stay => {}
        }
    }

    /// Bildschirm des Programms wiederherstellen und zurückgehaltene Ausgabe nachreichen.
    fn close_overlay(&self, child_alt: bool) {
        let mut gate = self.out_gate.lock().unwrap();
        let held = gate.take().unwrap_or_default();
        let parser = self.screen.lock().unwrap();
        let screen = parser.screen();
        let mut out: Vec<u8> = Vec::new();
        if child_alt && screen.alternate_screen() {
            // Wir haben über den Bildschirm des Programms gezeichnet: aus dem Abbild neu
            // aufbauen. Die zurückgehaltene Ausgabe steckt dort schon drin.
            out.extend_from_slice(b"\x1b[0m\x1b[H\x1b[2J");
            out.extend_from_slice(screen.contents_formatted().as_bytes());
            out.extend_from_slice(screen.cursor_state_formatted().as_bytes());
            out.extend_from_slice(screen.attributes_formatted().as_bytes());
        } else {
            if !child_alt {
                out.extend_from_slice(b"\x1b[?1049l");
            }
            out.extend_from_slice(b"\x1b[0m");
            if !screen.hide_cursor() {
                out.extend_from_slice(b"\x1b[?25h");
            }
            out.extend_from_slice(&held);
        }
        write_stdout(&out);
        drop(gate);
    }

    // ── Empfangen ───────────────────────────────────────────────────────────────

    /// Fügt empfangenen Text ein (nie mit Enter) und sagt, was passiert ist.
    fn receive(&self, from: &str, raw: &str) -> String {
        if raw.len() > proto::MAX_TEXT {
            let note = if crate::locale::german() {
                format!("⚠ {from} schickt zu viel ({} KiB), abgelehnt", raw.len() / 1024)
            } else {
                format!("⚠ {from} sent too much ({} KiB), rejected", raw.len() / 1024)
            };
            self.title(&note);
            return tr!("rejected: too large", "abgelehnt: zu groß").into();
        }
        let text = text::sanitize(raw);
        let lines = text::line_count(&text);
        let bracketed = self.screen.lock().unwrap().screen().bracketed_paste();
        match text::encode(&text, bracketed) {
            Injection::Bytes(bytes) => {
                let mut w = self.pty_in.lock().unwrap();
                if w.write_all(&bytes).and_then(|_| w.flush()).is_err() {
                    return tr!("insertion failed", "Fehler beim Einfügen").into();
                }
                self.title(&if crate::locale::german() {
                    format!("← {lines} Zeile(n) von {from} eingefügt")
                } else {
                    format!("← {lines} line(s) from {from} inserted")
                });
                tr!("inserted", "eingefügt").into()
            }
            Injection::NeedsOneLine => {
                *self.pending_one_line.lock().unwrap() = Some(text);
                let note = if crate::locale::german() {
                    format!(
                        "← {lines} Zeilen von {from} NICHT eingefügt: Programm kann kein Bracketed Paste · Ctrl-Space Ctrl-v = als eine Zeile"
                    )
                } else {
                    format!(
                        "← {lines} lines from {from} NOT inserted: program has no Bracketed Paste · Ctrl-Space Ctrl-v = insert as one line"
                    )
                };
                self.title(&note);
                tr!(
                    "multiline, waiting for Ctrl-Space Ctrl-v",
                    "mehrzeilig, wartet auf Ctrl-Space Ctrl-v"
                )
                .into()
            }
        }
    }

    fn insert_pending_one_line(&self) {
        let Some(text) = self.pending_one_line.lock().unwrap().take() else {
            return self.title(tr!("No text held", "Nichts zurückgehalten"));
        };
        let line = text::one_line(&text);
        let mut w = self.pty_in.lock().unwrap();
        let _ = w.write_all(line.as_bytes()).and_then(|_| w.flush());
        drop(w);
        self.title(tr!("inserted as one line", "als eine Zeile eingefügt"));
    }

    /// Meldung für `NOTE_TIME` in den Fenstertitel, danach wieder der Programmtitel.
    fn title(&self, msg: &str) {
        let label = self.label();
        self.title_state.lock().unwrap().note_until = Some(Instant::now() + NOTE_TIME);
        let _ =
            terminal::set_title(&mut std::io::stdout().lock(), &format!("bridget {label} · {msg}"));
    }

    /// Kennzeichen dieser Konsole: `A` bzw. verbunden `A⇄B`.
    fn label(&self) -> String {
        match self.partner_name() {
            Some(p) => format!("{}⇄{p}", self.name),
            None => self.name.clone(),
        }
    }

    /// Titel neu setzen, z.B. nach einer Meldung oder wenn sich der Partner ändert.
    fn refresh_title(&self) {
        let label = self.label();
        let st = self.title_state.lock().unwrap();
        if st.note_until.is_some() {
            return;
        }
        let kind = st.child.as_ref().map_or(2, |(k, _)| *k);
        let text = compose(&label, &st);
        drop(st);
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&osc_title(kind, &text)).and_then(|_| stdout.flush());
    }
}

/// Bereinigt und prüft einen Text vor dem Senden (nicht leer, nicht zu groß).
fn checked(text: String) -> Result<String> {
    let text = text::sanitize(&text);
    if text.is_empty() {
        bail!("{}", tr!("nothing to send", "nichts zu senden"));
    }
    if text.len() > proto::MAX_TEXT {
        bail!(
            "{}",
            if crate::locale::german() {
                format!(
                    "zu groß ({} KiB, höchstens {} KiB), nicht gesendet",
                    text.len() / 1024,
                    proto::MAX_TEXT / 1024
                )
            } else {
                format!(
                    "too large ({} KiB, maximum {} KiB), not sent",
                    text.len() / 1024,
                    proto::MAX_TEXT / 1024
                )
            }
        );
    }
    Ok(text)
}

fn compose(label: &str, st: &TitleState) -> String {
    match &st.child {
        Some((_, t)) if !t.trim().is_empty() => format!("[{label}] {t}"),
        _ => format!("bridget {label}"),
    }
}

fn osc_title(kind: u8, text: &str) -> Vec<u8> {
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    format!("\x1b]{kind};{clean}\x07").into_bytes()
}

fn write_stdout(bytes: &[u8]) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(bytes).and_then(|_| stdout.flush());
}

fn connected(target: &str) -> String {
    if crate::locale::german() {
        format!("verbunden mit {target}")
    } else {
        format!("connected to {target}")
    }
}

fn disconnected(target: &str) -> String {
    if crate::locale::german() {
        format!("getrennt von {target}")
    } else {
        format!("disconnected from {target}")
    }
}
