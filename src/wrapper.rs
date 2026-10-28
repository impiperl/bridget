//! Der PTY-Wrapper: startet das Programm in einem Pseudo-Terminal und reicht alles durch.
//!
//! Datenpfade:
//! - Tastatur → Hotkey-Filter → PTY (Hotkeys gehen an die Brücke)
//! - PTY → Bildschirm, parallel ins vt100-Abbild und in den Mitschnitt seit dem letzten Enter
//!
//! Signale: SIGWINCH passt PTY und Abbild an, SIGTERM/SIGHUP gehen ans Programm.
//! Vorher wird die letzte Ausgabe für den Abschluss-Dump (`crate::dump`) gesichert: Nach dem
//! Signal räumt das Programm den Bildschirm womöglich noch um.

use std::io::Read;
use std::mem::ManuallyDrop;
use std::num::NonZeroU16;
use std::os::fd::FromRawFd;
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
            loop {
                match pty_reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if !bridge.output_to_terminal(&buf[..n]) {
                            break;
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
    {
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
                        if saved.is_none() {
                            let why = if sig == SIGHUP {
                                "Fenster geschlossen (SIGHUP)"
                            } else {
                                "beendet von außen (SIGTERM)"
                            };
                            *saved = Some((why.to_string(), bridge.dump_text()));
                        }
                        drop(saved);
                        if let Some(pid) = child_pid {
                            // SAFETY: kill sendet nur ein Signal an den Kindprozess.
                            unsafe { libc::kill(pid as libc::pid_t, sig) };
                        }
                    }
                }
            }
        });
    }

    let status = child.wait().context(tr!("wait for program", "auf das Programm warten"));
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
            reason: saved
                .map(|(why, _)| why)
                .unwrap_or_else(|| format!("Programm beendet (Exit {code})")),
            partner,
            last,
        });
    }
    Ok(code)
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
