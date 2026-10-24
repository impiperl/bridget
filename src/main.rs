#[macro_use]
mod locale;

mod bridge;
mod color;
mod ctl;
mod desk;
mod dump;
mod duo;
mod help;
mod hook;
mod hotkey;
mod memory;
mod name;
mod pair;
mod picker;
mod profile;
mod proto;
mod runtime;
mod terminal;
mod text;
mod title;
mod wrapper;

use clap::{Command, CommandFactory, FromArgMatches, Parser, Subcommand};

/// Connect terminal programs and send text between them with a hotkey.
///
/// Hotkeys (press and release Ctrl-Space first): Enter sends the last response,
/// A selects lines, B sends the screen, V connects, T disconnects, H opens help.
#[derive(Parser)]
#[command(version, about, args_conflicts_with_subcommands = true)]
struct Cli {
    /// Interface language (default: en; also BRIDGET_LANG)
    #[arg(long, global = true, value_enum)]
    lang: Option<locale::Language>,

    #[command(subcommand)]
    command: Option<Cmd>,

    /// Name of this console ([A-Za-z0-9_-], at most 32 characters)
    name: Option<String>,

    /// Force a profile instead of detecting it from the program name
    #[arg(long)]
    profile: Option<String>,

    /// Background color: blue, green, purple, orange, red, cyan, #rrggbb or off
    /// (default: by order; also BRIDGET_COLOR)
    #[arg(long)]
    color: Option<String>,

    /// Connect to this console at startup (wait up to 10 seconds)
    #[arg(long)]
    connect: Option<String>,

    /// Program and arguments (default: $SHELL)
    #[arg(last = true)]
    program: Vec<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List running consoles and connections
    Ls,
    /// Open two connected consoles side by side (using tmux)
    Pair {
        /// Program on the left (default: $SHELL), e.g. claude or "claude --resume"
        left: Option<String>,
        /// Program on the right (default: $SHELL)
        right: Option<String>,
        /// Names of both consoles (default: A,B or A2,B2 … if occupied)
        #[arg(long, value_delimiter = ',')]
        names: Option<Vec<String>>,
        /// Open in this console instead of a new window
        #[arg(long)]
        here: bool,
    },
    /// Internal command used by the window opened by `pair`
    #[command(hide = true, name = "pair-attach")]
    PairAttach { session: String, panes: Vec<String> },
    /// Workspace: consoles 1 and 2 on top (connected), numbered terminal tabs below (tmux)
    Desk {
        /// Program top left, console 1 (default: $SHELL), e.g. claude
        left: Option<String>,
        /// Program top right, console 2 (default: $SHELL), e.g. codex
        right: Option<String>,
        /// Number of terminal tabs at startup (3, 4, …)
        #[arg(long, default_value_t = 1)]
        tabs: u32,
        /// Open in this console instead of a new window
        #[arg(long)]
        here: bool,
    },
    /// Internal command run by tmux when a pane of `desk` exits
    #[command(hide = true, name = "desk-fix")]
    DeskFix { server: String, session: String },
    /// Internal command run by claude/codex hooks (/compact, /clear → dump)
    #[command(hide = true)]
    Hook {
        cli: String,
        #[arg(long, hide = true)]
        after_compact: Option<usize>,
    },
    /// Show configuration and the effective profile for PROG
    Config {
        program: Option<String>,
        /// Create ~/.config/bridget/config.toml unless it exists
        #[arg(long)]
        init: bool,
    },
    /// Connect console A to console B
    Link { a: String, b: String },
    /// Disconnect console NAME
    Unlink { name: String },
    /// Let two AI CLIs talk headlessly (side B: `bridget duo join ID`)
    #[command(args_conflicts_with_subcommands = true)]
    Duo {
        #[command(subcommand)]
        join: Option<DuoCmd>,
        /// Side A (claude or codex)
        #[arg(long, default_value = "claude")]
        a: String,
        /// Side B (claude or codex)
        #[arg(long, default_value = "codex")]
        b: String,
        /// Maximum number of responses in the conversation
        #[arg(long, default_value_t = 6)]
        rounds: u32,
        /// Working directory for both sides (default: current directory)
        #[arg(long)]
        cwd: Option<std::path::PathBuf>,
        /// Timeout for one turn in seconds
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// Log the conversation in ~/.local/state/bridget/ (0600, 14 days)
        #[arg(long)]
        transcript: bool,
        /// Do not pass the shared memory (~/.local/share/bridget/memory) to either side
        #[arg(long)]
        no_memory: bool,
        /// Initial prompt for side A
        prompt: Vec<String>,
    },
    /// Make console NAME send (like Ctrl-Space Enter or Ctrl-Space B)
    Send {
        name: String,
        /// Last response (default)
        #[arg(long, conflicts_with = "screen")]
        last: bool,
        /// Visible screen
        #[arg(long)]
        screen: bool,
    },
}

#[derive(Subcommand)]
enum DuoCmd {
    /// Show side B of a running conversation in this console
    Join { id: String },
}

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    // `--lang` vor clap auswerten und herausnehmen, siehe `locale::take`.
    let (args, requested) = match locale::take(args) {
        Ok(taken) => taken,
        Err(e) => {
            eprintln!("bridget: {e:#}");
            std::process::exit(2);
        }
    };
    if let Err(e) = locale::configure(requested) {
        eprintln!("bridget: {e:#}");
        std::process::exit(2);
    }
    let command = if locale::german() { german_command(Cli::command()) } else { Cli::command() };
    let cli = Cli::from_arg_matches(&command.get_matches_from(args)).unwrap_or_else(|e| e.exit());
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("bridget: {e:#}");
            std::process::exit(1);
        }
    }
}

fn german_command(command: Command) -> Command {
    command
        .about("Aufsatz für Konsolenprogramme: Text per Hotkey zwischen Konsolen schicken")
        .long_about("Aufsatz für Konsolenprogramme: Text per Hotkey zwischen Konsolen schicken.\n\nShortcuts: zuerst Ctrl-Space drücken und loslassen; danach Enter zum Senden, A zur Auswahl, B für den Bildschirm, V zum Verbinden, T zum Trennen oder H für Hilfe.")
        .mut_arg("lang", |arg| arg.help("Sprache der Bedienoberfläche (Standard: en; auch BRIDGET_LANG)"))
        .mut_arg("name", |arg| arg.help("Name dieser Konsole ([A-Za-z0-9_-], höchstens 32 Zeichen)"))
        .mut_arg("profile", |arg| arg.help("Profil erzwingen (sonst nach Programmname)"))
        .mut_arg("color", |arg| arg.help("Hintergrundfarbe: blau, grün, lila, orange, rot, türkis, #rrggbb oder aus"))
        .mut_arg("connect", |arg| arg.help("Beim Start mit dieser Konsole verbinden (wartet bis zu 10 s)"))
        .mut_arg("program", |arg| arg.help("Programm mit Argumenten (Standard: $SHELL)"))
        .mut_subcommand("ls", |cmd| cmd.about("Laufende Konsolen und Verbindungen anzeigen"))
        .mut_subcommand("pair", |cmd| {
            cmd.about("Neues Fenster mit zwei verbundenen Konsolen nebeneinander")
                .mut_arg("left", |arg| arg.help("Programm links (Standard: $SHELL)"))
                .mut_arg("right", |arg| arg.help("Programm rechts (Standard: $SHELL)"))
                .mut_arg("names", |arg| arg.help("Namen der beiden Konsolen (Standard: A,B)"))
                .mut_arg("here", |arg| arg.help("In dieser Konsole öffnen statt in einem neuen Fenster"))
        })
        .mut_subcommand("desk", |cmd| {
            cmd.about("Arbeitsplatz: Konsolen 1 und 2 oben (verbunden), nummerierte Terminal-Tabs unten")
                .mut_arg("left", |arg| arg.help("Programm oben links, Konsole 1 (Standard: $SHELL)"))
                .mut_arg("right", |arg| arg.help("Programm oben rechts, Konsole 2 (Standard: $SHELL)"))
                .mut_arg("tabs", |arg| arg.help("Anzahl Terminal-Tabs zum Start (3, 4, …)"))
                .mut_arg("here", |arg| arg.help("In dieser Konsole öffnen statt in einem neuen Fenster"))
        })
        .mut_subcommand("config", |cmd| {
            cmd.about("Konfiguration und wirksames Profil für PROG anzeigen")
                .mut_arg("init", |arg| arg.help("Vorlage ~/.config/bridget/config.toml anlegen"))
        })
        .mut_subcommand("link", |cmd| cmd.about("Konsole A mit Konsole B verbinden"))
        .mut_subcommand("unlink", |cmd| cmd.about("Verbindung von Konsole NAME trennen"))
        .mut_subcommand("duo", |cmd| {
            cmd.about("Zwei KI-CLIs headless miteinander reden lassen")
                .mut_arg("a", |arg| arg.help("Seite A (claude oder codex)"))
                .mut_arg("b", |arg| arg.help("Seite B (claude oder codex)"))
                .mut_arg("rounds", |arg| arg.help("Höchstzahl Antworten im ganzen Gespräch"))
                .mut_arg("cwd", |arg| arg.help("Arbeitsverzeichnis beider Seiten"))
                .mut_arg("timeout", |arg| arg.help("Höchstdauer eines Aufrufs in Sekunden"))
                .mut_arg("transcript", |arg| arg.help("Gespräch protokollieren (0600, 14 Tage)"))
                .mut_arg("prompt", |arg| arg.help("Startfrage an Seite A"))
                .mut_subcommand("join", |join| join.about("Seite B eines laufenden Gesprächs hier zeigen"))
        })
        .mut_subcommand("send", |cmd| {
            cmd.about("In Konsole NAME senden lassen")
                .mut_arg("last", |arg| arg.help("Letzte Ausgabe (Standard)"))
                .mut_arg("screen", |arg| arg.help("Sichtbaren Bildschirm"))
        })
}

fn run(cli: Cli) -> anyhow::Result<i32> {
    match cli.command {
        Some(Cmd::Ls) => return ctl::ls().map(|()| 0),
        Some(Cmd::Pair { left, right, names, here }) => {
            return pair::run(pair::Options { left, right, names, here });
        }
        Some(Cmd::PairAttach { session, panes }) => return pair::attach(&session, &panes),
        Some(Cmd::Desk { left, right, tabs, here }) => {
            return desk::run(desk::Options { left, right, tabs, here });
        }
        Some(Cmd::DeskFix { server, session }) => return Ok(desk::fix(&server, &session)),
        Some(Cmd::Hook { cli, after_compact }) => return Ok(hook::run(&cli, after_compact)),
        Some(Cmd::Config { program, init }) => {
            return profile::show_config(program.as_deref(), init).map(|()| 0);
        }
        Some(Cmd::Link { a, b }) => return ctl::link(&a, &b).map(|()| 0),
        Some(Cmd::Unlink { name }) => return ctl::unlink(&name).map(|()| 0),
        Some(Cmd::Duo { join: Some(DuoCmd::Join { id }), .. }) => return duo::join(&id),
        Some(Cmd::Duo {
            join: None,
            a,
            b,
            rounds,
            cwd,
            timeout,
            transcript,
            no_memory,
            prompt,
        }) => {
            anyhow::ensure!(
                !prompt.is_empty(),
                "{}",
                tr!(
                    "initial prompt missing: bridget duo \"…\"",
                    "Startfrage fehlt: bridget duo \"…\""
                )
            );
            anyhow::ensure!(
                rounds > 0,
                "{}",
                tr!("--rounds must be at least 1", "--rounds muss mindestens 1 sein")
            );
            let cwd = match cwd {
                Some(c) => c.canonicalize()?,
                None => std::env::current_dir()?,
            };
            return duo::run(duo::Options {
                a: duo::agent::Kind::parse(&a)?,
                b: duo::agent::Kind::parse(&b)?,
                rounds,
                cwd,
                timeout: std::time::Duration::from_secs(timeout),
                transcript,
                memory: if no_memory { None } else { memory::dir() },
                prompt: prompt.join(" "),
            });
        }
        Some(Cmd::Send { name, screen, .. }) => {
            let what = if screen { proto::What::Screen } else { proto::What::Last };
            return ctl::send(&name, what).map(|()| 0);
        }
        None => {}
    }

    let name = match cli.name {
        Some(n) => n,
        None => format!("t{}", std::process::id()),
    };
    name::validate(&name)?;

    let program = if cli.program.is_empty() {
        vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())]
    } else {
        cli.program
    };
    let profile = profile::select(&program, cli.profile.as_deref())?;
    let program = profile.apply_args(program);
    let color = match cli.color.or_else(|| std::env::var("BRIDGET_COLOR").ok()) {
        Some(c) => wrapper::Color::Fixed(color::parse_choice(&c)?),
        None => wrapper::Color::Auto,
    };
    if let Some(c) = &cli.connect {
        name::validate(c)?;
    }

    wrapper::run(wrapper::Options { name, program, profile, color, connect: cli.connect })
}
