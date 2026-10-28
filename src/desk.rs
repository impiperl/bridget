//! `bridget desk`: Arbeitsplatz mit zwei verbundenen Konsolen oben (1 links, 2 rechts) und
//! einem Terminal-Bereich unten über die ganze Breite, mit nummerierten Tabs 3, 4, …
//!
//! Läuft wie `pair` auf dem eigenen tmux-Server. Jeder Tab ist eine eigene bridget-Konsole.
//! Sichtbar ist immer nur einer im unteren Bereich, die anderen liegen jeweils in einem
//! versteckten tmux-Fenster und laufen dort weiter. Gewechselt wird mit `swap-pane`.
//!
//! Der Zustand steht in der Umgebung der tmux-Sitzung: `BRIDGET_DESK_MAIN` (Fenster-ID),
//! `BRIDGET_DESK_TOP` (Panes 1 und 2) und `BRIDGET_DESK_TAB_<n>` (Pane-ID je Nummer). Endet
//! ein Programm, räumt ein `pane-exited`-Hook auf (`bridget desk-fix`) und holt bei Bedarf
//! den nächsten Tab nach unten.
//!
//! Aus einer Konsole heraus (`Ctrl-Space 4 Tab`, `Ctrl-Space N`) läuft tmux über `$TMUX`,
//! also immer auf dem Server, in dem die Konsole steckt. Im Desk schalten zusätzlich
//! `Alt+1` … `Alt+9` direkt um; `F1` öffnet die Hilfe.

use std::collections::BTreeMap;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::pair::{self, Launched};
use crate::runtime;

const MAIN: &str = "BRIDGET_DESK_MAIN";
const TOP: &str = "BRIDGET_DESK_TOP";
const TAB: &str = "BRIDGET_DESK_TAB_";
/// Sitzungsoption für tmux-Bindings: Nur im Desk Sondertasten abfangen.
const DESK_OPTION: &str = "@bridget_desk";
/// Höhe des Terminal-Bereichs in Prozent.
const AREA_PERCENT: &str = "35";
/// Erste Nummer im Terminal-Bereich.
const FIRST_TAB: u32 = 3;

pub struct Options {
    pub left: Option<String>,
    pub right: Option<String>,
    /// Anzahl Terminal-Tabs zum Start (mindestens 1).
    pub tabs: u32,
    pub here: bool,
}

pub fn run(opts: Options) -> Result<i32> {
    pair::tmux(&["-V"]).context(tr!("bridget desk requires tmux", "bridget desk braucht tmux"))?;
    anyhow::ensure!(
        (1..=9).contains(&opts.tabs),
        "{}",
        tr!("--tabs must be between 1 and 9", "--tabs muss zwischen 1 und 9 liegen")
    );
    let dir = runtime::dir()?;
    let last = FIRST_TAB + opts.tabs - 1;
    let (group, session) = free_group(&dir, last);
    let name = |n: u32| format!("{group}{n}");

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let left = pair::split(opts.left.as_deref().unwrap_or(&shell))?;
    let right = pair::split(opts.right.as_deref().unwrap_or(&shell))?;
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let runtime_base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let cmd = |n: u32, connect: Option<&str>, program: &[String]| {
        pair::pane_command(&exe, &runtime_base, &name(n), connect, program)
    };
    let shell = vec![shell];

    let (rows, cols) = if opts.here { crate::terminal::size() } else { pair::WINDOW };
    let p1 = pair::tmux(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        &session,
        "-x",
        &cols.to_string(),
        "-y",
        &rows.to_string(),
        "-c",
        &cwd,
        &cmd(1, None, &left),
    ])?;
    let setup = (|| -> Result<String> {
        pair::base_options()?;
        let t = Tmux::Server(pair::server());
        t.run(&["set-option", "-t", &session, DESK_OPTION, "1"])?;
        bind_direct_keys(&t)?;
        t.run(&[
            "set-option",
            "-t",
            &session,
            "status-left",
            "#[fg=black,bg=cyan,bold] bridget desk #[default] ",
        ])?;
        let p3 = t.run(&[
            "split-window",
            "-v",
            "-f",
            "-p",
            AREA_PERCENT,
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &p1,
            "-c",
            &cwd,
            &cmd(FIRST_TAB, None, &shell),
        ])?;
        let p2 = t.run(&[
            "split-window",
            "-h",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &p1,
            "-c",
            &cwd,
            &cmd(2, Some(&name(1)), &right),
        ])?;
        let main = t.run(&["display-message", "-p", "-t", &p1, "#{window_id}"])?;
        t.run(&["set-option", "-w", "-t", &main, "pane-border-format", desk_border_format()])?;
        t.run(&["set-option", "-t", &session, "status-right-length", "240"])?;
        t.set_env(&session, MAIN, &main)?;
        t.set_env(&session, TOP, &format!("{p1} {p2}"))?;
        for (n, pane) in [(1, &p1), (2, &p2), (FIRST_TAB, &p3)] {
            t.set_env(&session, &format!("{TAB}{n}"), pane)?;
        }
        for n in FIRST_TAB + 1..=last {
            let pane = t.run(&[
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                &format!("{session}:"),
                "-c",
                &cwd,
                &cmd(n, None, &shell),
            ])?;
            t.set_env(&session, &format!("{TAB}{n}"), &pane)?;
        }
        let fix = shell_words::join([
            exe.as_str(),
            "--lang",
            crate::locale::code(),
            "desk-fix",
            &pair::server(),
            &session,
        ]);
        t.run(&[
            "set-hook",
            "-t",
            &session,
            "pane-exited",
            &format!("run-shell {}", shell_words::quote(&fix)),
        ])?;
        t.run(&["select-pane", "-t", &p1])?;
        refresh_status(&t, &session)?;
        Ok(p2)
    })();
    let p2 = match setup {
        Ok(p) => p,
        Err(e) => {
            let _ = pair::tmux(&["kill-session", "-t", &session]);
            return Err(e);
        }
    };

    let panes = vec![format!("{p1}={}", pair::COLORS[0]), format!("{p2}={}", pair::COLORS[1])];
    let (term, version) = match pair::launch(&exe, &session, panes, opts.here)? {
        Launched::Here(code) => return Ok(code),
        Launched::Window { term, version } => (term, version),
    };
    println!(
        "{}",
        if crate::locale::german() {
            format!(
                "Fenster offen ({term}, {version}): oben {} und {} (verbunden), unten Terminal {}–{}.",
                name(1),
                name(2),
                name(FIRST_TAB),
                name(last)
            )
        } else {
            format!(
                "Window open ({term}, {version}): {} and {} on top (connected), terminals {}–{} below.",
                name(1),
                name(2),
                name(FIRST_TAB),
                name(last)
            )
        }
    );
    Ok(0)
}

/// Namen 1, 2, 3 …; ist das belegt, `2-1`, `2-2` … (Gruppe `2-`), usw.
fn free_group(dir: &std::path::Path, last: u32) -> (String, String) {
    (1..)
        .map(|k: u32| {
            if k == 1 {
                (String::new(), "bridget-desk".to_string())
            } else {
                (format!("{k}-"), format!("bridget-desk-{k}"))
            }
        })
        .find(|(group, session)| {
            (1..=last).all(|n| !runtime::is_live(dir, &format!("{group}{n}")))
                && pair::tmux(&["has-session", "-t", session]).is_err()
        })
        .expect("unendliche Folge")
}

// ── Aus einer Konsole heraus ─────────────────────────────────────────────────────

/// Terminal `n` zeigen (1 und 2: nur auswählen) und hineinwechseln.
pub fn show(n: u32) -> Result<String> {
    let (t, session) = here()?;
    show_in(&t, &session, n, true)?;
    Ok(if crate::locale::german() { format!("Terminal {n}") } else { format!("terminal {n}") })
}

/// Nächsten Tab im Terminal-Bereich zeigen.
pub fn next() -> Result<String> {
    let (t, session) = here()?;
    let tabs: Vec<u32> = area_tabs(&t, &session)?.into_keys().collect();
    if tabs.is_empty() {
        bail!("{}", tr!("no terminal tabs (Ctrl-Space N)", "keine Terminal-Tabs (Ctrl-Space N)"));
    }
    let current = current_tab(&t, &session)?;
    let n = current.and_then(|c| tabs.iter().copied().find(|&n| n > c)).unwrap_or(tabs[0]);
    show_in(&t, &session, n, true)?;
    Ok(if crate::locale::german() { format!("Terminal {n}") } else { format!("terminal {n}") })
}

/// Neuen Terminal-Tab mit `$SHELL` anlegen und zeigen. `me` ist der Name der Konsole, die
/// gefragt hat: Ihre Gruppe bestimmt die Namen.
pub fn new_tab(me: &str) -> Result<String> {
    let (t, session) = here()?;
    let group = group(me);
    let dir = runtime::dir()?;
    let taken = tabs(&t, &session)?;
    let n = (FIRST_TAB..)
        .find(|n| !taken.contains_key(n) && !runtime::is_live(&dir, &format!("{group}{n}")))
        .expect("unendliche Folge");
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let runtime_base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let cmd = pair::pane_command(&exe, &runtime_base, &format!("{group}{n}"), None, &[shell]);
    let pane = t.run(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &format!("{session}:"),
        "-c",
        &cwd,
        &cmd,
    ])?;
    t.set_env(&session, &format!("{TAB}{n}"), &pane)?;
    show_in(&t, &session, n, true)?;
    Ok(if crate::locale::german() {
        format!("neues Terminal {n}")
    } else {
        format!("new terminal {n}")
    })
}

/// Text der letzten Mausmarkierung (tmux-Puffer). Leer heißt: nichts markiert.
pub fn marked() -> Result<String> {
    if std::env::var_os("TMUX").is_none() {
        bail!(
            "{}",
            tr!(
                "mouse selection only works inside tmux (bridget desk or pair)",
                "Mausmarkierung geht nur in tmux (bridget desk oder pair)"
            )
        );
    }
    match Tmux::Here.run_raw(&["show-buffer"]) {
        Ok(text) if !text.trim().is_empty() => Ok(text),
        _ => bail!(
            "{}",
            tr!(
                "nothing selected (drag with the mouse first)",
                "nichts markiert (zuerst mit der Maus ziehen)"
            )
        ),
    }
}

/// Markierung nach dem Senden verwerfen, damit sie nicht versehentlich noch einmal geht.
pub fn clear_marked() {
    let _ = Tmux::Here.run(&["delete-buffer"]);
}

/// `bridget desk-fix SERVER SESSION`: vom `pane-exited`-Hook. Fehler bleiben still, sonst
/// zeigt tmux sie mitten im Fenster an.
pub fn fix(server: &str, session: &str) -> i32 {
    let t = Tmux::Server(server.to_string());
    let _ = (|| -> Result<()> {
        let alive = alive_panes(&t, session)?;
        for (n, pane) in tabs_raw(&t, session)? {
            if !alive.contains(&pane) {
                t.run(&["set-environment", "-u", "-t", session, &format!("{TAB}{n}")])?;
            }
        }
        if bottom(&t, session)?.is_none()
            && let Some(&n) = area_tabs(&t, session)?.keys().next()
        {
            show_in(&t, session, n, false)?;
        }
        refresh_status(&t, session)
    })();
    0
}

// ── Gemeinsam ────────────────────────────────────────────────────────────────────

/// Gruppe eines Konsolennamens: `2-` bei `2-3`, sonst leer. `Ctrl-Space 3 Enter` aus `2-1`
/// geht so an `2-3`, aus `1` an `3`.
pub fn group(name: &str) -> &str {
    match name.rfind('-') {
        Some(i) if i + 1 < name.len() && name[i + 1..].bytes().all(|b| b.is_ascii_digit()) => {
            &name[..=i]
        }
        _ => "",
    }
}

enum Tmux {
    /// Aus einer Konsole: `$TMUX` bestimmt den Server.
    Here,
    /// Eigener Server (`-L`), z.B. vom Hook.
    Server(String),
}

impl Tmux {
    fn run(&self, args: &[&str]) -> Result<String> {
        Ok(self.run_raw(args)?.trim_end().to_string())
    }

    fn run_raw(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new("tmux");
        if let Tmux::Server(server) = self {
            cmd.args(["-L", server]).env_remove("TMUX");
        }
        let out = cmd
            .args(args)
            .stdin(Stdio::null())
            .output()
            .context(tr!("start tmux", "tmux starten"))?;
        if !out.status.success() {
            bail!(
                "tmux {}: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn set_env(&self, session: &str, key: &str, value: &str) -> Result<()> {
        self.run(&["set-environment", "-t", session, key, value]).map(drop)
    }

    fn env(&self, session: &str, key: &str) -> Option<String> {
        let line = self.run(&["show-environment", "-t", session, key]).ok()?;
        line.strip_prefix(&format!("{key}=")).map(str::to_string)
    }
}

/// Sitzung der Konsole, in der wir laufen; nur in einem `desk`.
fn here() -> Result<(Tmux, String)> {
    let not_desk = || {
        anyhow::anyhow!(
            "{}",
            tr!("only available in bridget desk", "nur in bridget desk verfügbar")
        )
    };
    let pane = std::env::var("TMUX_PANE").map_err(|_| not_desk())?;
    if std::env::var_os("TMUX").is_none() {
        return Err(not_desk());
    }
    let t = Tmux::Here;
    let session = t.run(&["display-message", "-p", "-t", &pane, "#{session_name}"])?;
    if t.env(&session, MAIN).is_none() {
        return Err(not_desk());
    }
    Ok((t, session))
}

/// Alle Einträge `BRIDGET_DESK_TAB_<n>`, auch die von beendeten Panes.
fn tabs_raw(t: &Tmux, session: &str) -> Result<BTreeMap<u32, String>> {
    Ok(t.run(&["show-environment", "-t", session])?
        .lines()
        .filter_map(|line| {
            let (key, pane) = line.strip_prefix(TAB)?.split_once('=')?;
            Some((key.parse().ok()?, pane.to_string()))
        })
        .collect())
}

fn alive_panes(t: &Tmux, session: &str) -> Result<Vec<String>> {
    Ok(t.run(&["list-panes", "-s", "-t", session, "-F", "#{pane_id}"])?
        .lines()
        .map(str::to_string)
        .collect())
}

/// Laufende Konsolen der Sitzung nach Nummer.
fn tabs(t: &Tmux, session: &str) -> Result<BTreeMap<u32, String>> {
    let alive = alive_panes(t, session)?;
    Ok(tabs_raw(t, session)?.into_iter().filter(|(_, pane)| alive.contains(pane)).collect())
}

/// Nur die Tabs des Terminal-Bereichs (ab 3).
fn area_tabs(t: &Tmux, session: &str) -> Result<BTreeMap<u32, String>> {
    Ok(tabs(t, session)?.into_iter().filter(|(n, _)| *n >= FIRST_TAB).collect())
}

fn main_and_top(t: &Tmux, session: &str) -> Result<(String, Vec<String>)> {
    let main = t.env(session, MAIN).context(MAIN)?;
    let top = t.env(session, TOP).unwrap_or_default();
    Ok((main, top.split_whitespace().map(str::to_string).collect()))
}

/// Der Pane im unteren Bereich, falls es einen gibt.
fn bottom(t: &Tmux, session: &str) -> Result<Option<String>> {
    let (main, top) = main_and_top(t, session)?;
    Ok(t.run(&["list-panes", "-t", &main, "-F", "#{pane_id}"])?
        .lines()
        .find(|pane| !top.iter().any(|p| p == pane))
        .map(str::to_string))
}

/// Direkte, leicht merkbare Desk-Tasten. Die Bindings liegen zwar auf dem eigenen
/// bridget-tmux-Server, prüfen aber die Sitzungsmarkierung: In `pair` und fremden
/// Sitzungen gehen dieselben Tasten unverändert an das Programm.
fn bind_direct_keys(t: &Tmux) -> Result<()> {
    for n in 1..=9 {
        let key = format!("M-{n}");
        let in_desk = format!("send-keys C-Space {n} Tab");
        let otherwise = format!("send-keys M-{n}");
        t.run(&[
            "bind-key",
            "-n",
            &key,
            "if-shell",
            "-F",
            &format!("#{{{DESK_OPTION}}}"),
            &in_desk,
            &otherwise,
        ])?;
    }
    t.run(&[
        "bind-key",
        "-n",
        "F1",
        "if-shell",
        "-F",
        &format!("#{{{DESK_OPTION}}}"),
        "send-keys C-Space H",
        "send-keys F1",
    ])?;
    Ok(())
}

/// Der Fokus ist zusätzlich zur Farbe ausgeschrieben; die Ortsangabe macht die drei
/// sichtbaren Bereiche ohne räumliches Rätsel eindeutig.
fn desk_border_format() -> &'static str {
    tr!(
        "#{?pane_active,#[fg=black,bg=yellow,bold] ● ACTIVE #[default],#[fg=colour244] ○ inactive #[default]} #{?#{==:#{pane_top},0},#{?#{==:#{pane_left},0},TOP LEFT,TOP RIGHT},BOTTOM TERMINAL} · #{pane_title} ",
        "#{?pane_active,#[fg=black,bg=yellow,bold] ● AKTIV #[default],#[fg=colour244] ○ inaktiv #[default]} #{?#{==:#{pane_top},0},#{?#{==:#{pane_left},0},OBEN LINKS,OBEN RECHTS},TERMINAL UNTEN} · #{pane_title} "
    )
}

fn current_tab(t: &Tmux, session: &str) -> Result<Option<u32>> {
    let Some(pane) = bottom(t, session)? else { return Ok(None) };
    Ok(tabs(t, session)?.into_iter().find(|(_, p)| *p == pane).map(|(n, _)| n))
}

fn show_in(t: &Tmux, session: &str, n: u32, focus: bool) -> Result<()> {
    let Some(pane) = tabs(t, session)?.remove(&n) else {
        bail!(
            "{}",
            if crate::locale::german() {
                format!("Terminal {n} gibt es nicht")
            } else {
                format!("terminal {n} does not exist")
            }
        );
    };
    let (main, top) = main_and_top(t, session)?;
    let window = t.run(&["display-message", "-p", "-t", &pane, "#{window_id}"])?;
    if window != main {
        match bottom(t, session)? {
            Some(slot) => {
                t.run(&["swap-pane", "-d", "-s", &pane, "-t", &slot])?;
            }
            // Kein unterer Bereich mehr (letzter Tab beendet): Platzhalter über die ganze
            // Breite anlegen und tauschen. `join-pane -f` gibt es in tmux 2.7 nicht.
            None => {
                let anchor = top.first().context(TOP)?;
                let slot = t.run(&[
                    "split-window",
                    "-d",
                    "-v",
                    "-f",
                    "-p",
                    AREA_PERCENT,
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-t",
                    anchor,
                    "sleep 60",
                ])?;
                t.run(&["swap-pane", "-d", "-s", &pane, "-t", &slot])?;
                t.run(&["kill-pane", "-t", &slot])?;
            }
        }
    }
    if focus {
        t.run(&["select-window", "-t", &main])?;
        t.run(&["select-pane", "-t", &pane])?;
    }
    refresh_status(t, session)
}

fn switch_key(n: u32) -> String {
    if n <= 9 { format!("Alt+{n}") } else { format!("Ctrl-Space {n} Tab") }
}

/// Statuszeile: beschriftete Ziele statt nackter Nummern. Der sichtbare untere Tab ist
/// nicht nur farblich, sondern auch mit Text markiert.
fn refresh_status(t: &Tmux, session: &str) -> Result<()> {
    let current = current_tab(t, session)?;
    let list: String = area_tabs(t, session)?
        .keys()
        .map(|&n| {
            if Some(n) == current {
                format!(
                    "#[fg=black,bg=yellow,bold] [{} · {}] #[default]",
                    switch_key(n),
                    tr!("VISIBLE", "SICHTBAR")
                )
            } else {
                format!(" [{}]", switch_key(n))
            }
        })
        .collect();
    let keys = format!(
        "{}{}{}",
        tr!(
            " #[fg=black,bg=cyan,bold] SWITCH: [Alt+1 LEFT] [Alt+2 RIGHT] #[default] BOTTOM:",
            " #[fg=black,bg=cyan,bold] WECHSELN: [Alt+1 LINKS] [Alt+2 RECHTS] #[default] UNTEN:"
        ),
        list,
        tr!("  [F1 HELP] ", "  [F1 HILFE] ")
    );
    t.run(&["set-option", "-t", session, "status-right", &keys]).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_comes_from_trailing_number_after_dash() {
        assert_eq!(group("1"), "");
        assert_eq!(group("12"), "");
        assert_eq!(group("2-3"), "2-");
        assert_eq!(group("my-desk-10"), "my-desk-");
        assert_eq!(group("A"), "");
        assert_eq!(group("A2"), "");
        assert_eq!(group("my-console"), "");
        assert_eq!(group("x-"), "");
    }

    #[test]
    fn direct_key_hint_has_a_fallback_for_two_digit_tabs() {
        assert_eq!(switch_key(4), "Alt+4");
        assert_eq!(switch_key(10), "Ctrl-Space 10 Tab");
    }

    #[test]
    fn pane_border_names_position_and_focus_in_text() {
        let format = desk_border_format();
        assert!(format.contains(tr!("ACTIVE", "AKTIV")));
        assert!(format.contains(tr!("TOP LEFT", "OBEN LINKS")));
        assert!(format.contains(tr!("BOTTOM TERMINAL", "TERMINAL UNTEN")));
    }
}
