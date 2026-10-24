//! `bridget pair`: neues Fenster mit zwei verbundenen Konsolen nebeneinander.
//!
//! Läuft über tmux auf einem eigenen Server (`tmux -L bridget`), damit eigene tmux-Sitzungen
//! und -Einstellungen unberührt bleiben. Das Fenster startet `bridget pair-attach`: Das fragt
//! dort (nicht in der aufrufenden Konsole) den Hintergrund ab, färbt die Panes ein und hängt
//! sich an die Sitzung. Schließt man das Fenster, endet die Sitzung (`destroy-unattached`).

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::color;
use crate::terminal::RawGuard;
use crate::{name, runtime};

/// Eigener tmux-Server; `BRIDGET_TMUX_SERVER` nur für Tests.
pub(crate) fn server() -> String {
    std::env::var("BRIDGET_TMUX_SERVER").unwrap_or_else(|_| "bridget".into())
}
/// Scheitert der Start gleich (z.B. Programm nicht gefunden), bleibt der Pane mit der
/// Meldung offen, statt sofort zu verschwinden.
const KEEP_ERROR_EN: &str = r#"t=$(date +%s); "$@"; rc=$?
if [ "$rc" -ne 0 ] && [ $(( $(date +%s) - t )) -lt 3 ]; then
  printf '\n[bridget: startup failed (%s), Enter closes]' "$rc"; read _
fi
exit "$rc""#;
const KEEP_ERROR_DE: &str = r#"t=$(date +%s); "$@"; rc=$?
if [ "$rc" -ne 0 ] && [ $(( $(date +%s) - t )) -lt 3 ]; then
  printf '\n[bridget: Start fehlgeschlagen (%s), Enter schließt]' "$rc"; read _
fi
exit "$rc""#;
pub(crate) const COLORS: [&str; 2] = ["blue", "green"];
pub(crate) const WINDOW: (u16, u16) = (50, 200); // Zeilen, Spalten des neuen Fensters

pub struct Options {
    pub left: Option<String>,
    pub right: Option<String>,
    pub names: Option<Vec<String>>,
    pub here: bool,
}

pub fn run(opts: Options) -> Result<i32> {
    tmux(&["-V"]).context(tr!("bridget pair requires tmux", "bridget pair braucht tmux"))?;
    let dir = runtime::dir()?;
    let [a, b] = match opts.names {
        Some(n) => {
            let [a, b]: [String; 2] = n.try_into().map_err(|_| {
                anyhow::anyhow!(tr!("--names requires two names", "--names braucht zwei Namen"))
            })?;
            name::validate(&a)?;
            name::validate(&b)?;
            anyhow::ensure!(
                a != b,
                "{}",
                tr!("The two names must be different", "Die beiden Namen müssen verschieden sein")
            );
            for n in [&a, &b] {
                anyhow::ensure!(
                    !runtime::is_live(&dir, n),
                    "{}",
                    if crate::locale::german() {
                        format!("Der Name {n:?} ist schon belegt")
                    } else {
                        format!("Name {n:?} is already in use")
                    }
                );
            }
            [a, b]
        }
        None => free_names(&dir),
    };
    let session = format!("bridget-{a}-{b}");
    anyhow::ensure!(
        tmux(&["has-session", "-t", &session]).is_err(),
        "{}",
        if crate::locale::german() {
            format!("tmux-Sitzung {session} gibt es schon")
        } else {
            format!("tmux session {session} already exists")
        }
    );

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let left = split(opts.left.as_deref().unwrap_or(&shell))?;
    let right = split(opts.right.as_deref().unwrap_or(&shell))?;
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let runtime_base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();

    // Die Panes färben nicht selbst ein: Das macht `pair-attach` im echten Fenster.
    let pane_cmd = |name: &str, connect: Option<&str>, program: &[String]| {
        pane_command(&exe, &runtime_base, name, connect, program)
    };

    let (rows, cols) = if opts.here { crate::terminal::size() } else { WINDOW };
    let pane_a = tmux(&[
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
        &pane_cmd(&a, None, &left),
    ])?;
    let setup = (|| -> Result<String> {
        base_options()?;
        for opt in [
            ["status-left", "#[fg=black,bg=cyan,bold] bridget pair #[default]"],
            [
                "status-right",
                tr!(
                    "#[fg=black,bg=green,bold] Ctrl-Space Enter  SEND #[default]  [A Select] [B Screen] [M Marked] [V Connect] [T Disconnect] [H Help] ",
                    "#[fg=black,bg=green,bold] Ctrl-Space Enter  SENDEN #[default]  [A Auswahl] [B Bildschirm] [M Markierung] [V Verbinden] [T Trennen] [H Hilfe] "
                ),
            ],
        ] {
            tmux(&["set-option", "-g", opt[0], opt[1]])?;
        }
        let pane_b = tmux(&[
            "split-window",
            "-h",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            &pane_a,
            "-c",
            &cwd,
            &pane_cmd(&b, Some(&a), &right),
        ])?;
        tmux(&["select-pane", "-t", &pane_a])?;
        Ok(pane_b)
    })();
    let pane_b = match setup {
        Ok(p) => p,
        Err(e) => {
            let _ = tmux(&["kill-session", "-t", &session]);
            return Err(e);
        }
    };
    let panes = vec![format!("{pane_a}={}", COLORS[0]), format!("{pane_b}={}", COLORS[1])];

    let (term, version) = match launch(&exe, &session, panes, opts.here)? {
        Launched::Here(code) => return Ok(code),
        Launched::Window { term, version } => (term, version),
    };
    println!(
        "{}",
        if crate::locale::german() {
            format!("Fenster offen ({term}, {version}): links {a}, rechts {b}, verbunden.")
        } else {
            format!(
                "Window open ({term}, {version}): {a} on the left, {b} on the right, connected."
            )
        }
    );
    Ok(0)
}

/// Kommando für einen Pane: bridget mit Namen, ohne eigene Farbe, ggf. mit `--connect`.
pub(crate) fn pane_command(
    exe: &str,
    runtime_base: &str,
    name: &str,
    connect: Option<&str>,
    program: &[String],
) -> String {
    let mut argv: Vec<String> = vec![
        "env".into(),
        format!("XDG_RUNTIME_DIR={runtime_base}"),
        exe.into(),
        "--lang".into(),
        crate::locale::code().into(),
        name.into(),
    ];
    argv.extend(["--color".into(), "off".into()]);
    if let Some(c) = connect {
        argv.extend(["--connect".into(), c.into()]);
    }
    argv.push("--".into());
    argv.extend(program.iter().cloned());
    let mut wrapped: Vec<String> = vec![
        "sh".into(),
        "-c".into(),
        tr!(KEEP_ERROR_EN, KEEP_ERROR_DE).into(),
        "bridget-pane".into(),
    ];
    wrapped.extend(argv);
    shell_words::join(wrapped)
}

/// Einstellungen des eigenen tmux-Servers, die `pair` und `desk` teilen.
pub(crate) fn base_options() -> Result<()> {
    for opt in [
        ["pane-border-status", "top"],
        ["pane-border-format", " #{pane_title} "],
        ["status", "on"],
        ["status-position", "bottom"],
        ["status-left-length", "18"],
        ["status-right-length", "180"],
        ["window-status-format", ""],
        ["window-status-current-format", ""],
        ["mouse", "on"],
        ["set-titles", "on"],
        ["set-titles-string", "#{pane_title}"],
    ] {
        tmux(&["set-option", "-g", opt[0], opt[1]])?;
    }
    tmux(&["set-option", "-ga", "terminal-overrides", ",*:Tc"])?;
    Ok(())
}

pub(crate) enum Launched {
    /// In dieser Konsole angehängt und wieder zurück (Exit-Code von tmux).
    Here(i32),
    /// Neues Fenster ist offen.
    Window { term: String, version: String },
}

/// Sitzung zeigen: hier anhängen oder in einem neuen Fenster, sobald das Fenster da ist.
pub(crate) fn launch(exe: &str, session: &str, panes: Vec<String>, here: bool) -> Result<Launched> {
    let has_display =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if here || !has_display {
        return attach(session, &panes).map(Launched::Here);
    }
    let version = tmux(&["-V"])?;
    let mut argv = vec![
        exe.to_string(),
        "--lang".into(),
        crate::locale::code().into(),
        "pair-attach".into(),
        session.to_string(),
    ];
    argv.extend(panes);
    let term = match open_window(&argv) {
        Ok(t) => t,
        Err(e) => {
            let _ = tmux(&["kill-session", "-t", session]);
            return Err(e);
        }
    };
    // Kommt kein Fenster an, darf die Sitzung nicht unsichtbar weiterlaufen.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if tmux(&["list-clients", "-t", session]).is_ok_and(|c| !c.is_empty()) {
            break;
        }
        if Instant::now() >= deadline {
            let _ = tmux(&["kill-session", "-t", session]);
            bail!(
                "{}",
                if crate::locale::german() {
                    format!("{term} hat kein Fenster geöffnet (Abbruch). Alternativ: --here")
                } else {
                    format!("{term} did not open a window (aborted). Alternatively: --here")
                }
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(Launched::Window { term, version })
}

/// `bridget pair-attach SESSION %ID=FARBE …`: läuft im neuen Fenster.
pub fn attach(session: &str, panes: &[String]) -> Result<i32> {
    let base = {
        let _guard = RawGuard::enable()?;
        color::query_background()
    };
    if let Some(base) = base {
        for spec in panes {
            let (pane, hue) = spec
                .split_once('=')
                .context(tr!("pane specification %ID=COLOR", "Pane-Angabe %ID=FARBE"))?;
            if let Some(hue) = color::parse_choice(hue)? {
                let _ = tmux(&[
                    "select-pane",
                    "-t",
                    pane,
                    "-P",
                    &format!("bg={}", color::tint(base, hue).hex()),
                ]);
            }
        }
    }
    let status = Command::new("tmux")
        // destroy-unattached erst nach dem Anhängen: tmux 2.7 räumt die Sitzung sonst sofort ab.
        .args(["-L", &server(), "attach-session", "-t", session, ";"])
        .args(["set-option", "-t", session, "destroy-unattached", "on"])
        .env_remove("TMUX")
        .status()
        .context("tmux attach")?;
    Ok(status.code().unwrap_or(1))
}

/// A/B, sonst A2/B2, A3/B3 …
fn free_names(dir: &std::path::Path) -> [String; 2] {
    (1..)
        .map(|i| {
            if i == 1 {
                ["A".to_string(), "B".to_string()]
            } else {
                [format!("A{i}"), format!("B{i}")]
            }
        })
        .find(|[a, b]| {
            !runtime::is_live(dir, a)
                && !runtime::is_live(dir, b)
                && tmux(&["has-session", "-t", &format!("bridget-{a}-{b}")]).is_err()
        })
        .expect("unendliche Folge")
}

pub(crate) fn split(cmd: &str) -> Result<Vec<String>> {
    let words = shell_words::split(cmd).with_context(|| {
        if crate::locale::german() {
            format!("Programm {cmd:?} lesen")
        } else {
            format!("parse program {cmd:?}")
        }
    })?;
    anyhow::ensure!(!words.is_empty(), "{}", tr!("empty program", "leeres Programm"));
    Ok(words)
}

/// Öffnet ein Terminalfenster mit `argv`. Reihenfolge: `$BRIDGET_TERMINAL` (Präfix, an den
/// `argv` angehängt wird), gnome-terminal, konsole, xfce4-terminal, xterm.
fn open_window(argv: &[String]) -> Result<String> {
    let geometry = format!("{}x{}", WINDOW.1, WINDOW.0);
    let mut candidates: Vec<Vec<String>> = Vec::new();
    if let Ok(t) = std::env::var("BRIDGET_TERMINAL") {
        candidates.push(split(&t)?);
    }
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    candidates.push(s(&["gnome-terminal", "--window", &format!("--geometry={geometry}"), "--"]));
    candidates.push(s(&["konsole", "-e"]));
    candidates.push(s(&["xfce4-terminal", "-x"]));
    candidates.push(s(&["xterm", "-geometry", &geometry, "-e"]));

    for prefix in candidates {
        if !in_path(&prefix[0]) {
            continue;
        }
        let spawned = Command::new(&prefix[0])
            .args(&prefix[1..])
            .args(argv)
            .env_remove("TMUX")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if spawned.is_ok() {
            return Ok(prefix[0].clone());
        }
    }
    bail!(
        "{}",
        tr!(
            "No terminal program found (set BRIDGET_TERMINAL or use bridget pair --here)",
            "Kein Terminalprogramm gefunden (BRIDGET_TERMINAL setzen oder bridget pair --here)"
        )
    )
}

fn in_path(prog: &str) -> bool {
    if prog.contains('/') {
        return std::path::Path::new(prog).exists();
    }
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
}

/// tmux auf dem eigenen Server; gibt stdout ohne Zeilenende zurück.
pub(crate) fn tmux(args: &[&str]) -> Result<String> {
    let out = Command::new("tmux")
        .args(["-L", &server()])
        .args(args)
        .env_remove("TMUX")
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
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}
