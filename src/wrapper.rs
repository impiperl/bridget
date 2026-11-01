//! Der PTY-Wrapper: startet das Programm in einem Pseudo-Terminal und reicht alles durch.
//!
//! Datenpfade:
//! - Tastatur → Hotkey-Filter → PTY (Hotkeys gehen an die Brücke)
//! - PTY → Bildschirm, parallel ins vt100-Abbild und in den Mitschnitt seit dem letzten Enter
//!
//! Signale: SIGWINCH passt PTY und Abbild an, SIGTERM/SIGHUP beenden das Programm.
//! Vorher wird die letzte Ausgabe für den Abschluss-Dump (`crate::dump`) gesichert: Nach dem
//! Signal räumt das Programm den Bildschirm womöglich noch um.
//!
//! Beenden geht ordentlich, wenn das Programm gerade an seiner Eingabe steht: bridget leert
//! die Zeile und tippt den Befehl des Profils (`/exit`, bei einer interaktiven Shell `exit`).
//! Erst wenn das Programm danach nicht in `EXIT_WAIT` endet, bekommt es das Signal; ein
//! zweites Signal geht sofort durch. Steht es nicht an der Eingabe (Dialog offen, versteckter
//! Cursor, in der Shell läuft ein Vordergrundprogramm), geht das Signal gleich weiter, damit
//! der Text nirgends hineingetippt wird, wo er etwas auslöst.

use std::io::Read;
use std::mem::ManuallyDrop;
use std::num::NonZeroU16;
use std::os::fd::FromRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use signal_hook::consts::{SIGHUP, SIGTERM, SIGWINCH};
use signal_hook::iterator::Signals;

use crate::bridge::Bridge;
use crate::color;
use crate::hotkey::{self, Event};
use crate::profile::Profile;
use crate::runtime;
use crate::terminal::{self, RawGuard};
use crate::text;

pub const SCROLLBACK: usize = 10_000;
/// So lange darf ein Programm nach dem getippten Beenden-Befehl brauchen.
const EXIT_WAIT: Duration = Duration::from_secs(10);
/// Shells, die beim Schließen `exit` bekommen, sofern sie interaktiv laufen.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh"];

pub struct Options {
    pub name: String,
    pub program: Vec<String>,
    pub profile: Profile,
    pub color: Color,
    pub connect: Option<String>,
}

pub enum Color {
    /// Nach Reihenfolge: erste laufende Konsole blau, zweite grün …
    Auto,
    /// Feste Farbe, `None` = nicht einfärben.
    Fixed(Option<color::Rgb>),
}

/// Läuft, bis das Programm endet, und gibt dessen Exit-Code zurück.
pub fn run(opts: Options) -> Result<i32> {
    // Ohne Terminal endet bridget ohnehin; dann soll das Programm gar nicht erst laufen.
    terminal::require_tty()?;
    // Erst den Socket: Ein belegter Name oder ein unsicherer Runtime-Ordner soll
    // auffallen, bevor das Programm startet.
    let dir = runtime::dir()?;
    let listener = runtime::bind(&dir, &opts.name)?;
    let hue = match opts.color {
        Color::Fixed(hue) => hue,
        Color::Auto => Some(color::by_index(runtime::live_others(&dir, &opts.name))),
    };

    // Signale vor dem Programmstart anmelden, sonst geht eine frühe Größenänderung verloren.
    let mut signals = Signals::new([SIGWINCH, SIGTERM, SIGHUP])?;

    let (rows, cols) = terminal::size();
    let pty = native_pty_system();
    let pair = pty
        .openpty(pty_size(rows, cols))
        .context(tr!("open pseudo-terminal", "Pseudo-Terminal öffnen"))?;

    let mut cmd = CommandBuilder::new(&opts.program[0]);
    cmd.args(&opts.program[1..]);
    let cwd = std::env::current_dir().ok();
    if let Some(cwd) = &cwd {
        cmd.cwd(cwd);
    }
    let started = SystemTime::now();
    let kind = opts.profile.name.clone();
    let exit_cmd = exit_command(opts.profile.exit.as_deref(), &opts.program);
    let prompt = opts.profile.frame.prompt.clone();
    cmd.env("BRIDGET_NAME", &opts.name);

    let mut child = pair.slave.spawn_command(cmd).with_context(|| {
        if crate::locale::german() {
            format!("{} starten", opts.program[0])
        } else {
            format!("start {}", opts.program[0])
        }
    })?;
    // Ohne diesen Drop bekäme der Leser nie EOF, wenn das Programm endet.
    drop(pair.slave);
    let child_pid = child.process_id();

    let master: Arc<Mutex<Box<dyn MasterPty + Send>>> = Arc::new(Mutex::new(pair.master));
    let screen = Arc::new(Mutex::new(text::parser(nz(rows), nz(cols), SCROLLBACK)));

    let mut pty_reader = master.lock().unwrap().try_clone_reader()?;
    let pty_writer = master.lock().unwrap().take_writer()?;

    let bridge = Bridge::new(
        opts.name.clone(),
        opts.program.join(" "),
        dir,
        opts.profile,
        Arc::clone(&screen),
        pty_writer,
    );
    bridge.start_listener(listener);

    let guard = RawGuard::enable()?;
    // Vor dem Tastatur-Thread: Die Abfrage liest die Antwort des Terminals von stdin.
    let marker = color::Marker::apply(hue);
    let _ = terminal::push_title(&mut std::io::stdout().lock());
    bridge.greet();
    if let Some(target) = opts.connect {
        bridge.auto_connect(target);
    }

    // PTY → Bildschirm + Abbild
    let (out_done_tx, out_done_rx) = mpsc::channel::<()>();
    {
        let bridge = Arc::clone(&bridge);
        thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            // Ist das Terminal weg (Fenster zu), weiterlesen: Sonst bleibt das Programm beim
            // Schreiben hängen und kann sich nicht mehr selbst beenden.
            let mut to_terminal = true;
            loop {
                match pty_reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if to_terminal && !bridge.output_to_terminal(&buf[..n]) {
                            to_terminal = false;
                        }
                        bridge.screen.lock().unwrap().process(&buf[..n]);
                        bridge.on_output(&buf[..n]);
                    }
                }
            }
            let _ = out_done_tx.send(());
        });
    }

    // Tastatur → Hotkey-Filter → PTY
    {
        let bridge = Arc::clone(&bridge);
        thread::spawn(move || {
            // Direkt von fd 0 lesen, ohne Rusts Puffer, damit jede Taste sofort durchgeht.
            // ManuallyDrop: fd 0 gehört nicht uns und darf nicht geschlossen werden.
            // SAFETY: fd 0 ist während der ganzen Laufzeit offen.
            let mut stdin = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) });
            let mut filter = hotkey::Filter::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let mut input = &buf[..n];
                if bridge.overlay_active() {
                    let consumed = bridge.overlay_input(input);
                    input = &input[consumed..];
                    if input.is_empty() {
                        continue;
                    }
                }
                bridge.hide_action_menu();
                // Nach "Verbinden" mit mehreren Konsolen gehört die nächste Taste der Auswahl.
                if bridge.take_choice(input[0]) {
                    input = &input[1..];
                }
                for ev in filter.feed(input) {
                    match ev {
                        // Vor dem Weitergeben, damit der Merkpunkt vor der Antwort liegt.
                        Event::Enter => bridge.on_enter(),
                        Event::Pass(bytes) => {
                            if !bridge.write_to_program(&bytes) {
                                return;
                            }
                        }
                        Event::Command(key) => bridge.command(key, None),
                        Event::CommandTo(n, key) => bridge.command(key, Some(n)),
                    }
                }
                bridge.action_menu(filter.pending_action());
            }
        });
    }

    // Signale
    let at_signal: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    // Wie es nach dem Signal ausging, für den Dump.
    let ending: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let exited = Arc::new(AtomicBool::new(false));
    {
        let ending = Arc::clone(&ending);
        let exited = Arc::clone(&exited);
        let master = Arc::clone(&master);
        let screen = Arc::clone(&screen);
        let bridge = Arc::clone(&bridge);
        let at_signal = Arc::clone(&at_signal);
        thread::spawn(move || {
            for sig in signals.forever() {
                match sig {
                    SIGWINCH => {
                        let (rows, cols) = terminal::size();
                        let _ = master.lock().unwrap().resize(pty_size(rows, cols));
                        screen.lock().unwrap().set_size(nz(rows), nz(cols));
                    }
                    _ => {
                        let mut saved = at_signal.lock().unwrap();
                        let first = saved.is_none();
                        if first {
                            let why = if sig == SIGHUP {
                                "Fenster geschlossen (SIGHUP)"
                            } else {
                                "beendet von außen (SIGTERM)"
                            };
                            *saved = Some((why.to_string(), bridge.dump_text()));
                        }
                        drop(saved);
                        let Some(pid) = child_pid else { continue };
                        let ready = || {
                            let fg = master.lock().unwrap().process_group_leader();
                            fg == Some(pid as libc::pid_t)
                                && at_prompt(
                                    bridge.screen.lock().unwrap().screen(),
                                    prompt.as_ref(),
                                )
                        };
                        match exit_cmd.clone() {
                            Some(cmd) if first && ready() => {
                                *ending.lock().unwrap() = Some(format!("mit `{cmd}` beendet"));
                                let bridge = Arc::clone(&bridge);
                                let ending = Arc::clone(&ending);
                                let exited = Arc::clone(&exited);
                                thread::spawn(move || {
                                    type_exit(&bridge, &cmd);
                                    let wait = exit_wait();
                                    thread::sleep(wait);
                                    if !exited.load(Ordering::SeqCst) {
                                        *ending.lock().unwrap() = Some(format!(
                                            "`{cmd}` ohne Wirkung, nach {} s Signal",
                                            wait.as_secs()
                                        ));
                                        // SAFETY: kill sendet nur ein Signal an den Kindprozess.
                                        unsafe { libc::kill(pid as libc::pid_t, sig) };
                                    }
                                });
                            }
                            // SAFETY: kill sendet nur ein Signal an den Kindprozess.
                            _ => unsafe {
                                libc::kill(pid as libc::pid_t, sig);
                            },
                        }
                    }
                }
            }
        });
    }

    let status = child.wait().context(tr!("wait for program", "auf das Programm warten"));
    exited.store(true, Ordering::SeqCst);
    let partner = bridge.partner_name();
    let saved = at_signal.lock().unwrap().take();
    let last = match &saved {
        Some((_, text)) => text.clone(),
        None => bridge.dump_text(),
    };
    bridge.shutdown();
    let status = status?;
    // Restliche Ausgabe noch abholen. Hintergrundprozesse, die das PTY offen halten,
    // dürfen das Beenden nicht blockieren.
    let _ = out_done_rx.recv_timeout(Duration::from_millis(300));
    drop(marker);
    let _ = terminal::pop_title(&mut std::io::stdout().lock());
    drop(guard);

    let code = exit_code(&status);
    if let Some(cwd) = &cwd {
        let program = opts.program.join(" ");
        crate::dump::console(&crate::dump::Console {
            name: &opts.name,
            program: &program,
            kind: &kind,
            cwd,
            started,
            reason: match (saved, ending.lock().unwrap().take()) {
                (Some((why, _)), Some(how)) => format!("{why}, {how}"),
                (Some((why, _)), None) => why,
                (None, _) => format!("Programm beendet (Exit {code})"),
            },
            partner,
            last,
        });
    }
    Ok(code)
}

/// Befehl zum ordentlichen Beenden: aus dem Profil, sonst `exit` für eine interaktive Shell
/// (keine Skriptdatei, kein `-c`).
fn exit_command(from_profile: Option<&str>, program: &[String]) -> Option<String> {
    if let Some(cmd) = from_profile {
        return Some(cmd.to_string());
    }
    let base = Path::new(&program[0]).file_name()?.to_str()?;
    let interactive = program[1..]
        .iter()
        .all(|a| a.starts_with("--") || (a.starts_with('-') && !a.contains('c')));
    (SHELLS.contains(&base) && interactive).then(|| "exit".to_string())
}

/// Das Programm wartet auf Eingabe: Cursor sichtbar und, wenn das Profil die Eingabezeile
/// kennt, in einer solchen Zeile. Ein Dialog (z.B. das Update-Angebot von codex) versteckt
/// den Cursor; dort würde Enter eine Auswahl bestätigen. claude setzt hinter `❯` ein
/// geschütztes Leerzeichen, das hier als normales zählt.
fn at_prompt(screen: &vt100::Screen, prompt: Option<&regex::Regex>) -> bool {
    if screen.hide_cursor() {
        return false;
    }
    let Some(re) = prompt else { return true };
    let row = usize::from(screen.cursor_position().0);
    text::screen_rows(screen).get(row).is_some_and(|line| re.is_match(&line.replace('\u{a0}', " ")))
}

/// Zeile leeren (Ctrl-E, Ctrl-U), Befehl tippen, Enter. Mit Pausen, damit eine Oberfläche
/// das nicht als Einfügen liest, bei dem Enter nur ein Zeilenumbruch wäre.
fn type_exit(bridge: &Bridge, cmd: &str) {
    for (bytes, pause) in [(&b"\x05\x15"[..], 150), (cmd.as_bytes(), 300), (b"\r", 0)] {
        if !bridge.write_to_program(bytes) {
            return;
        }
        thread::sleep(Duration::from_millis(pause));
    }
}

/// `EXIT_WAIT`, für Tests über `BRIDGET_EXIT_WAIT` (Sekunden) verkürzbar.
fn exit_wait() -> Duration {
    std::env::var("BRIDGET_EXIT_WAIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .map_or(EXIT_WAIT, Duration::from_secs)
}

/// Wie die Shell: Abbruch durch Signal N ergibt 128+N.
fn exit_code(status: &portable_pty::ExitStatus) -> i32 {
    match status.signal() {
        Some(name) => 128 + signal_number(name).unwrap_or(1),
        None => status.exit_code() as i32,
    }
}

/// portable-pty liefert nur den Namen aus strsignal(); die Nummer wird daraus zurückgesucht.
fn signal_number(name: &str) -> Option<i32> {
    (1..65).find(|&n| {
        // SAFETY: strsignal gibt einen Zeiger auf einen statischen oder Thread-lokalen
        // String zurück, der sofort kopiert wird.
        let ptr = unsafe { libc::strsignal(n) };
        !ptr.is_null() && unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy() == name
    })
}

fn pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v).unwrap_or(NonZeroU16::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prog(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn only_interactive_shells_get_exit() {
        assert_eq!(exit_command(None, &prog(&["/bin/bash"])).as_deref(), Some("exit"));
        assert_eq!(exit_command(None, &prog(&["zsh", "-l", "--login"])).as_deref(), Some("exit"));
        assert_eq!(exit_command(None, &prog(&["sh", "-c", "sleep 9"])), None);
        assert_eq!(exit_command(None, &prog(&["bash", "-lc", "x"])), None);
        assert_eq!(exit_command(None, &prog(&["bash", "skript.sh"])), None);
        assert_eq!(exit_command(None, &prog(&["vim"])), None);
        assert_eq!(exit_command(Some("/exit"), &prog(&["claude"])).as_deref(), Some("/exit"));
    }

    #[test]
    fn prompt_needs_visible_cursor_on_input_line() {
        let re = regex::Regex::new("^› ").unwrap();
        let mut p = text::parser(nz(5), nz(40), 0);
        p.process(b"Ausgabe\r\n\xe2\x80\xba Eingabe");
        assert!(at_prompt(p.screen(), Some(&re)));
        assert!(at_prompt(p.screen(), None));
        let claude = regex::Regex::new("^❯ ").unwrap();
        let mut c = text::parser(nz(5), nz(40), 0);
        c.process("❯\u{a0}angefangen".as_bytes());
        assert!(at_prompt(c.screen(), Some(&claude)));
        // Dialog: Cursor versteckt.
        p.process(b"\x1b[?25l");
        assert!(!at_prompt(p.screen(), Some(&re)));
        // Cursor sichtbar, aber nicht in der Eingabezeile.
        p.process(b"\x1b[?25h\x1b[1;1H");
        assert!(!at_prompt(p.screen(), Some(&re)));
    }
}
