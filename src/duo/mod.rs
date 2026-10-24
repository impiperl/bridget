//! `bridget duo`: zwei KI-CLIs reden headless abwechselnd miteinander.
//!
//! Der Koordinator (`bridget duo …`) hält die Zustandsmaschine und startet die Aufrufe.
//! Eine Ansicht (`bridget duo join ID`) zeigt Seite B in einer zweiten Konsole.

pub mod agent;
pub mod machine;
mod run;
pub mod ui;
mod view;

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use run::{Options, run};
pub use view::join;

use machine::AfterTakeover;
use ui::Show;

/// Koordinator → Ansicht.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ToView {
    Welcome { conv: String, side_name: String },
    Show { item: Show },
    TakeoverGranted { command: Vec<String>, cwd: PathBuf },
    Bye { reason: String },
}

/// Ansicht → Koordinator.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum FromView {
    Join { conv: String },
    Key { key: u8 },
    Inject { text: String },
    Takeover,
    TakeoverDone { choice: Choice },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Choice {
    Continue,
    WithoutHandover,
    Quit,
}

impl From<Choice> for AfterTakeover {
    fn from(c: Choice) -> Self {
        match c {
            Choice::Continue => AfterTakeover::Continue,
            Choice::WithoutHandover => AfterTakeover::ContinueWithoutHandover,
            Choice::Quit => AfterTakeover::Quit,
        }
    }
}

/// Ordner für duo-Sockets und Arbeitsdateien: `<runtime>/bridget/duo` (0700).
fn duo_dir() -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = crate::runtime::dir()?.join("duo");
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            return Err(e).with_context(|| {
                if crate::locale::german() {
                    format!("{dir:?} anlegen")
                } else {
                    format!("create {dir:?}")
                }
            });
        }
    }
    Ok(dir)
}

/// Interaktive Übernahme: Programm mit dem echten Terminal laufen lassen.
/// Ctrl-C/Ctrl-Z sollen nur das Programm treffen, nicht bridget.
fn run_interactive(command: &[String], cwd: &std::path::Path) -> Result<std::process::ExitStatus> {
    use std::os::unix::process::CommandExt;
    let _ = crossterm::terminal::disable_raw_mode();
    ui::write("\x1b[0m\r\n");
    let ignored = [libc::SIGINT, libc::SIGQUIT, libc::SIGTSTP];
    // SAFETY: Signale nur für die Dauer des Kindprozesses ignorieren und danach
    // zurücksetzen; im Kind werden sie vor exec auf Standard gesetzt.
    let previous: Vec<libc::sighandler_t> =
        ignored.iter().map(|&s| unsafe { libc::signal(s, libc::SIG_IGN) }).collect();
    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..]).current_dir(cwd).env_remove("CLAUDECODE");
    unsafe {
        cmd.pre_exec(move || {
            for s in ignored {
                libc::signal(s, libc::SIG_DFL);
            }
            Ok(())
        });
    }
    let status = cmd.status().with_context(|| {
        if crate::locale::german() {
            format!("{} starten", command[0])
        } else {
            format!("start {}", command[0])
        }
    });
    for (s, prev) in ignored.iter().zip(previous) {
        unsafe { libc::signal(*s, prev) };
    }
    let _ = crossterm::terminal::enable_raw_mode();
    status
}

fn after_takeover_help() -> &'static str {
    tr!(
        "Back in the conversation. w = continue with handoff · o = continue without handoff (paused) · q = end conversation",
        "Zurück im Gespräch. w = weiter mit Übergabe · o = weiter ohne Übergabe (Pause) · q = Gespräch beenden"
    )
}

fn choice_from_key(key: u8) -> Option<Choice> {
    match key {
        b'w' | b'\r' => Some(Choice::Continue),
        b'o' => Some(Choice::WithoutHandover),
        b'q' => Some(Choice::Quit),
        _ => None,
    }
}
