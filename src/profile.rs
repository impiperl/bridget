//! Profile je Programm: Wie wird „letzte Ausgabe“ ermittelt, welche Zeilen sind Rahmen.
//!
//! Eingebaute Profile stehen in `DEFAULTS`. `~/.config/bridget/config.toml` kann sie
//! überschreiben oder neue hinzufügen (gleiches Format). Ein Profil mit `versions` gilt nur
//! für diese Versionen; bei anderer Version fällt `Ctrl-] l` auf den sichtbaren Bildschirm
//! zurück, weil sich die Oberfläche geändert haben kann. `args` werden beim Start direkt
//! hinter den Programmnamen gesetzt, sofern sie noch fehlen.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;

use crate::text::Frame;

const DEFAULTS: &str = r#"
[profile.default]
last = "log"

[profile.claude]
match = ["claude"]
versions = ["2.1.*"]
last = "screen-diff"
input_box = true
prompt = '^❯ '
strip = ['^\s*✻ ', '/effort\s*$', '^\s*⏵⏵', '^\s*[✢✳✶✻✽·*] \S.*…']
trim_prefix = '^(\s*)● '

[profile.codex]
match = ["codex"]
# Alternate screen mode has no scrollback, only the end of a long response.
args = ["--no-alt-screen"]
last = "screen-diff"
prompt = '^›( |$)'
strip = ['^\s*Worked for \d', '^\s*• (Working|Starting MCP servers) \(', '\? for shortcuts\s*$']
trim_prefix = '^• '
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Last {
    /// Mitgeschnittene Ausgabe seit dem letzten Enter (Shells, zeilenorientierte Programme).
    Log,
    /// Neue Zeilen gegenüber dem Bildschirm beim letzten Enter (Vollbild-Oberflächen).
    ScreenDiff,
    /// Einfach der sichtbare Bildschirm.
    Screen,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    #[serde(default, rename = "match")]
    matches: Vec<String>,
    #[serde(default)]
    versions: Vec<String>,
    #[serde(default)]
    args: Vec<String>,
    last: Option<Last>,
    #[serde(default)]
    input_box: bool,
    prompt: Option<String>,
    #[serde(default)]
    strip: Vec<String>,
    trim_prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    profile: BTreeMap<String, RawProfile>,
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub last: Last,
    pub frame: Frame,
    /// Kommt beim Start hinter den Programmnamen.
    pub args: Vec<String>,
    /// Gefundene Programmversion, falls das Profil Versionen vorschreibt.
    pub version: Option<String>,
    /// Version passt nicht zum Profil: Rückfall auf `Screen`.
    pub version_mismatch: bool,
}

impl Profile {
    /// Was `Ctrl-] l` wirklich tut (nach Versionsprüfung).
    pub fn effective_last(&self) -> Last {
        if self.version_mismatch { Last::Screen } else { self.last }
    }

    /// `args` direkt hinter den Programmnamen setzen, soweit sie noch fehlen.
    pub fn apply_args(&self, mut program: Vec<String>) -> Vec<String> {
        let missing: Vec<String> =
            self.args.iter().filter(|a| !program[1..].contains(a)).cloned().collect();
        program.splice(1..1, missing);
        program
    }
}

pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("bridget").join("config.toml"))
}

fn load_profiles(user_file: Option<&Path>) -> Result<BTreeMap<String, RawProfile>> {
    let mut profiles =
        toml::from_str::<RawConfig>(DEFAULTS).expect("built-in profiles are valid").profile;
    if let Some(path) = user_file.filter(|p| p.exists()) {
        let text = std::fs::read_to_string(path).with_context(|| {
            if crate::locale::german() {
                format!("{path:?} lesen")
            } else {
                format!("read {path:?}")
            }
        })?;
        let user: RawConfig = toml::from_str(&text).with_context(|| {
            if crate::locale::german() {
                format!("{path:?} ist kein gültiges TOML")
            } else {
                format!("{path:?} is not valid TOML")
            }
        })?;
        profiles.extend(user.profile);
    }
    Ok(profiles)
}

/// Profil für `program` wählen: `forced` (per `--profile`), sonst über den Programmnamen.
pub fn select(program: &[String], forced: Option<&str>) -> Result<Profile> {
    select_from(program, forced, config_path().as_deref(), program_version)
}

fn select_from(
    program: &[String],
    forced: Option<&str>,
    user_file: Option<&Path>,
    version_of: impl Fn(&str) -> Option<String>,
) -> Result<Profile> {
    let profiles = load_profiles(user_file)?;
    let base = Path::new(&program[0]).file_name().and_then(|s| s.to_str()).unwrap_or(&program[0]);

    let name = match forced {
        Some(f) => {
            anyhow::ensure!(
                profiles.contains_key(f),
                "{}",
                if crate::locale::german() {
                    format!("Profil {f:?} gibt es nicht")
                } else {
                    format!("profile {f:?} does not exist")
                }
            );
            f.to_string()
        }
        None => profiles
            .iter()
            .find(|(_, p)| p.matches.iter().any(|m| m == base))
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| "default".into()),
    };
    let raw = &profiles[&name];

    let frame = Frame {
        input_box: raw.input_box,
        prompt: raw
            .prompt
            .as_deref()
            .map(Regex::new)
            .transpose()
            .with_context(|| format!("{} {name}: prompt", tr!("Profile", "Profil")))?,
        strip: raw
            .strip
            .iter()
            .map(|r| {
                Regex::new(r).with_context(|| {
                    format!("{} {name}: {} {r:?}", tr!("Profile", "Profil"), tr!("rule", "Regel"))
                })
            })
            .collect::<Result<_>>()?,
        trim_prefix: raw
            .trim_prefix
            .as_deref()
            .map(Regex::new)
            .transpose()
            .with_context(|| format!("{} {name}: trim_prefix", tr!("Profile", "Profil")))?,
    };

    let (version, version_mismatch) = if raw.versions.is_empty() {
        (None, false)
    } else {
        let v = version_of(&program[0]);
        let ok = v.as_deref().is_some_and(|v| raw.versions.iter().any(|g| glob(g, v)));
        (v, !ok)
    };

    Ok(Profile {
        name,
        last: raw.last.unwrap_or(Last::Screen),
        frame,
        args: raw.args.clone(),
        version,
        version_mismatch,
    })
}

/// `bridget config [PROG] [--init]`: Pfad, wirksames Profil, optional Vorlage anlegen.
pub fn show_config(program: Option<&str>, init: bool) -> Result<()> {
    let path = config_path().context(tr!("HOME is not set", "HOME nicht gesetzt"))?;
    if init {
        if path.exists() {
            println!(
                "{}",
                if crate::locale::german() {
                    format!("{} gibt es schon, nichts geändert", path.display())
                } else {
                    format!("{} already exists; nothing changed", path.display())
                }
            );
        } else {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let template = format!(
                "# bridget profiles. User profiles completely replace built-ins with the same name.\n# Built-in defaults:\n{DEFAULTS}"
            );
            std::fs::write(&path, template)?;
            println!("{}: {}", tr!("Template written", "Vorlage geschrieben"), path.display());
        }
        return Ok(());
    }
    let state = if path.exists() {
        tr!("present", "vorhanden")
    } else {
        tr!(
            "missing; built-in profiles apply (--init creates a template)",
            "fehlt, es gelten die eingebauten Profile (--init legt eine Vorlage an)"
        )
    };
    println!("{}: {} ({state})", tr!("Configuration", "Konfiguration"), path.display());
    let profiles = load_profiles(Some(&path))?;
    println!("Profile: {}", profiles.keys().cloned().collect::<Vec<_>>().join(", "));
    if let Some(prog) = program {
        let p = select(&[prog.to_string()], None)?;
        println!("\n{} {prog}: {} {}", tr!("For", "Für"), tr!("profile", "Profil"), p.name);
        if let Some(v) = &p.version {
            println!(
                "  Version: {v}{}",
                if p.version_mismatch {
                    tr!(" (not verified → fallback)", " (nicht geprüft → Rückfall)")
                } else {
                    tr!(" (verified)", " (geprüft)")
                }
            );
        } else if p.version_mismatch {
            println!("  Version: {}", tr!("unknown (→ fallback)", "unbekannt (→ Rückfall)"));
        }
        if !p.args.is_empty() {
            println!(
                "  {}: {}",
                tr!("Additional arguments", "Zusätzliche Argumente"),
                p.args.join(" ")
            );
        }
        println!("  Ctrl-] l: {:?}", p.effective_last());
        println!("  {}: {}", tr!("Remove input box", "Eingabefeld entfernen"), p.frame.input_box);
        if let Some(re) = &p.frame.prompt {
            println!("  {}: {re}", tr!("Input lines", "Eingabe-Zeilen"));
        }
        for re in &p.frame.strip {
            println!("  {}: {re}", tr!("Frame rule", "Rahmen-Regel"));
        }
    }
    Ok(())
}

/// Nur `*` am Ende wird unterstützt („2.1.*“), sonst exakter Vergleich.
fn glob(pattern: &str, value: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => value.starts_with(prefix),
        None => pattern == value,
    }
}

/// Erste Zahl-mit-Punkten aus `<prog> --version` (höchstens 3 s).
/// Wird nur für Programme aufgerufen, deren Profil Versionen vorschreibt.
fn program_version(program: &str) -> Option<String> {
    let mut child = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    parse_version(&String::from_utf8_lossy(&out.stdout))
}

fn parse_version(s: &str) -> Option<String> {
    let re = Regex::new(r"\d+(\.\d+)+").unwrap();
    re.find(s).map(|m| m.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prog(p: &str) -> Vec<String> {
        vec![p.to_string()]
    }

    #[test]
    fn picks_profile_by_program_name() {
        let p = select_from(&prog("/usr/bin/bash"), None, None, |_| None).unwrap();
        assert_eq!((p.name.as_str(), p.effective_last()), ("default", Last::Log));

        let p = select_from(&prog("claude"), None, None, |_| Some("2.1.309".into())).unwrap();
        assert_eq!((p.name.as_str(), p.effective_last()), ("claude", Last::ScreenDiff));
    }

    #[test]
    fn unknown_version_falls_back_to_screen() {
        let p = select_from(&prog("claude"), None, None, |_| Some("3.0.1".into())).unwrap();
        assert!(p.version_mismatch);
        assert_eq!(p.effective_last(), Last::Screen);
        let p = select_from(&prog("claude"), None, None, |_| None).unwrap();
        assert!(p.version_mismatch);
    }

    #[test]
    fn user_file_overrides_and_forced_profile() {
        let dir = std::env::temp_dir().join(format!("bridget-prof-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.toml");
        std::fs::write(&file, "[profile.claude]\nmatch=[\"claude\"]\nversions=[\"3.*\"]\nlast=\"screen-diff\"\n\n[profile.repl]\nlast=\"screen\"\n").unwrap();
        let p = select_from(&prog("claude"), None, Some(&file), |_| Some("3.0.1".into())).unwrap();
        assert!(!p.version_mismatch);
        let p = select_from(&prog("python3"), Some("repl"), Some(&file), |_| None).unwrap();
        assert_eq!(p.effective_last(), Last::Screen);
        assert!(select_from(&prog("x"), Some("gibtsnicht"), Some(&file), |_| None).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codex_gets_inline_mode_once() {
        let p = select_from(&prog("codex"), None, None, |_| None).unwrap();
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            p.apply_args(args(&["codex", "resume", "x"])),
            args(&["codex", "--no-alt-screen", "resume", "x"])
        );
        assert_eq!(
            p.apply_args(args(&["codex", "--no-alt-screen"])),
            args(&["codex", "--no-alt-screen"])
        );
        let bash = select_from(&prog("bash"), None, None, |_| None).unwrap();
        assert_eq!(bash.apply_args(args(&["bash"])), args(&["bash"]));
    }

    #[test]
    fn codex_answer_longer_than_screen() {
        // Echte Sitzung (codex 0.164, inline, 20×90): 40 Zeilen Antwort auf 20 Zeilen Bildschirm.
        let raw = include_bytes!("../tests/fixtures/tui/codex.raw");
        let enter = raw.windows(12).position(|w| w == "Starting MCP".as_bytes()).unwrap();
        let p = select_from(&prog("codex"), None, None, |_| None).unwrap();
        let mut parser = crate::text::parser(
            std::num::NonZeroU16::new(20).unwrap(),
            std::num::NonZeroU16::new(90).unwrap(),
            1000,
        );
        parser.process(&raw[..enter]);
        let before = crate::text::screen_rows(parser.screen());
        let mark = crate::text::scrolled(&mut parser);
        parser.process(&raw[enter..]);
        let now = crate::text::rows_since(&mut parser, mark);
        let expected: Vec<String> = (1..=40).map(|i| format!("zeile {i}")).collect();
        assert_eq!(crate::text::screen_diff(&before, &now, &p.frame), expected.join("\n"));
    }

    #[test]
    fn version_parsing_and_glob() {
        assert_eq!(parse_version("2.1.309 (Claude Code)").as_deref(), Some("2.1.309"));
        assert_eq!(parse_version("codex-cli 0.153.3").as_deref(), Some("0.153.3"));
        assert!(glob("2.1.*", "2.1.309"));
        assert!(!glob("2.1.*", "2.10.0"));
        assert!(glob("0.153.3", "0.153.3"));
    }
}
