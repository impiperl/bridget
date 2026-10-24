//! `bridget hook claude|codex`: Wird von den Hooks der CLIs aufgerufen (eingetragen von
//! `install.sh`) und schreibt bei `/compact` und `/clear` einen Dump (`crate::dump`).
//!
//! Was die CLIs melden (geprüft 24.10.2026, claude 2.1.310, codex 0.164.0):
//! - claude `/compact` (und automatisch): `PreCompact` (`trigger`), danach `SessionStart`
//!   mit `source: compact`. Grenze (`compact_boundary`) und Zusammenfassung
//!   (`isCompactSummary`) stehen aber erst danach im Verlauf, auch nach dem `SessionStart`
//!   (gemessen: 10 ms später). Deshalb startet `PreCompact` einen Wartenden im Hintergrund,
//!   der den Dump schreibt, sobald die neue Grenze da ist.
//! - claude `/clear`: `SessionEnd` mit `reason: clear` für die alte Sitzung.
//! - codex `/compact`: `PreCompact`, `PostCompact` (`trigger`). Grenze ist die Zeile
//!   `compacted`; die Zusammenfassung darin ist verschlüsselt, lesbar sind die Nachrichten.
//! - codex `/clear`, `/new`: nur `SessionStart` der neuen Sitzung (`source: clear`), kein
//!   `SessionEnd`. Die alte Sitzung merkt sich bridget deshalb je codex-Prozess im
//!   Runtime-Ordner.
//!
//! - Jeder `SessionStart` in einer bridget-Konsole (`BRIDGET_NAME`) wird notiert, damit der
//!   Abschluss-Dump der Konsole die genaue Sitzung nennt (`console_session`).
//!
//! Schreibt nie auf stdout (bei `SessionStart` käme das in den Kontext der KI) und endet
//! immer mit 0: Ein Fehler hier darf die CLI nicht stören.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

/// So lange wartet der Hintergrundprozess höchstens auf das Ende des Kompaktierens.
const COMPACT_WAIT: Duration = Duration::from_secs(15 * 60);

/// `after_compact`: Aufruf als Wartender, mit der Zahl der Grenzen beim `PreCompact`.
pub fn run(cli: &str, after_compact: Option<usize>) -> i32 {
    let mut input = String::new();
    let _ = std::io::stdin().take(1 << 20).read_to_string(&mut input);
    if let Ok(v) = serde_json::from_str::<Value>(&input) {
        match after_compact {
            Some(n) => wait_for_compaction(cli, &v, n),
            None => handle(cli, &v, &input),
        };
    }
    0
}

fn transcript_of(cli: &str, path: &Path) -> Transcript {
    if cli == "codex" { codex_transcript(path) } else { claude_transcript(path) }
}

fn handle(cli: &str, v: &Value, raw: &str) -> Option<()> {
    let event = v["hook_event_name"].as_str()?;
    let session = v["session_id"].as_str()?;
    let transcript = PathBuf::from(v["transcript_path"].as_str()?);
    let cwd = v["cwd"].as_str().map(PathBuf::from).or_else(|| std::env::current_dir().ok())?;
    let s = |k: &str| v[k].as_str().unwrap_or("");
    // Headless codex (z.B. duo) hat keinen Verlauf für Menschen.
    if cli == "codex" && codex_is_exec(&transcript) {
        return None;
    }
    if event == "SessionStart" {
        note_console(cli, session);
    }
    match (cli, event) {
        (_, "PreCompact") => spawn_waiter(cli, raw, transcript_of(cli, &transcript).compactions),
        ("claude", "SessionEnd") if s("reason") == "clear" => {
            cleared(cli, session, &cwd, claude_transcript(&transcript).last)
        }
        ("codex", "SessionStart") => {
            let previous = remember_codex(session, &transcript);
            let new_chat = !matches!(s("source"), "startup" | "resume" | "compact");
            match previous {
                Some((old, old_transcript)) if new_chat && old != session => {
                    cleared(cli, &old, &cwd, codex_transcript(&old_transcript).last)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Letzte Frage und Antwort.
#[derive(Debug, Default, Clone, PartialEq)]
struct Exchange {
    prompt: Option<String>,
    answer: Option<String>,
}

impl Exchange {
    fn ask(&mut self, text: String) {
        self.prompt = Some(text);
        self.answer = None;
    }
    fn reply(&mut self, text: &str) {
        match &mut self.answer {
            Some(a) => {
                a.push_str("\n\n");
                a.push_str(text);
            }
            None => self.answer = Some(text.to_string()),
        }
    }
}

#[derive(Debug, Default)]
struct Transcript {
    last: Exchange,
    /// Stand vor dem letzten Kompaktieren.
    before_compact: Exchange,
    /// Zahl der Grenzen (`compact_boundary` bzw. `compacted`).
    compactions: usize,
    summary: Option<String>,
}

/// Wartenden starten: eigener Prozess, eigene Sitzung, ohne stdout/stderr, damit die CLI
/// nicht auf ihn wartet und ein Schließen des Fensters ihn nicht trifft.
fn spawn_waiter(cli: &str, raw: &str, compactions: usize) -> Option<()> {
    let exe = std::env::current_exe().ok()?;
    let mut cmd = Command::new(exe);
    cmd.args(["hook", cli, "--after-compact", &compactions.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid ist async-signal-safe und betrifft nur das Kind.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().ok()?;
    child.stdin.take()?.write_all(raw.as_bytes()).ok()?;
    Some(()) // nicht warten; der Prozess wird von init übernommen
}

fn wait_for_compaction(cli: &str, v: &Value, before: usize) -> Option<()> {
    let session = v["session_id"].as_str()?;
    let path = PathBuf::from(v["transcript_path"].as_str()?);
    let cwd = v["cwd"].as_str().map(PathBuf::from)?;
    let deadline = Instant::now() + COMPACT_WAIT;
    let t = loop {
        let t = transcript_of(cli, &path);
        // claude schreibt die Zusammenfassung zur Grenze; codex hat keine lesbare.
        let done = t.compactions > before && (cli != "claude" || t.summary.is_some());
        if done || Instant::now() > deadline {
            break t;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    let (before, summary) = if t.compactions > before {
        (t.before_compact, t.summary)
    } else {
        (t.last, None) // abgebrochen oder zu lange: wenigstens der letzte Stand
    };
    compacted(cli, session, &cwd, before, v["trigger"].as_str(), summary)
}

// ── Konsole ↔ Sitzung ─────────────────────────────────────────────────────────────────

fn hooks_dir() -> Option<PathBuf> {
    let dir = crate::runtime::dir().ok()?.join("hooks");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn note_console(cli: &str, session: &str) {
    let Ok(name) = std::env::var("BRIDGET_NAME") else { return };
    if crate::name::validate(&name).is_err() {
        return;
    }
    if let Some(dir) = hooks_dir() {
        let v = serde_json::json!({ "cli": cli, "session": session });
        let _ = std::fs::write(dir.join(format!("console-{name}.json")), v.to_string());
    }
}

/// Für den Abschluss-Dump der Konsole `name`: (CLI, Sitzung), wenn seit `since` notiert.
/// Der Eintrag wird dabei entfernt.
pub fn console_session(name: &str, since: SystemTime) -> Option<(String, String)> {
    let file = hooks_dir()?.join(format!("console-{name}.json"));
    let fresh = std::fs::metadata(&file).and_then(|m| m.modified()).is_ok_and(|t| t >= since);
    let text = std::fs::read_to_string(&file).ok();
    let _ = std::fs::remove_file(&file);
    let v: Value = serde_json::from_str(&text?).ok()?;
    fresh.then(|| Some((v["cli"].as_str()?.to_string(), v["session"].as_str()?.to_string())))?
}

fn facts(cli: &str, cwd: &Path, what: String) -> Vec<String> {
    let mut f = Vec::new();
    if let Ok(name) = std::env::var("BRIDGET_NAME") {
        f.push(format!("Konsole: `{name}`"));
    }
    f.push(format!("Programm: `{cli}` · Ordner: `{}`", cwd.display()));
    f.push(what);
    f
}

fn resume(cli: &str, cwd: &Path, session: &str) -> String {
    let cmd = if cli == "codex" { "codex resume" } else { "claude --resume" };
    format!("Fortsetzen: `cd {} && {cmd} {session}`", cwd.display())
}

fn stem(cli: &str, what: &str) -> String {
    let who = std::env::var("BRIDGET_NAME").unwrap_or_else(|_| cli.to_string());
    format!("{who}-{what}")
}

fn compacted(
    cli: &str,
    session: &str,
    cwd: &Path,
    before: Exchange,
    trigger: Option<&str>,
    summary: Option<String>,
) -> Option<()> {
    let how = match trigger {
        Some("auto") => "automatisch (Kontext voll)",
        _ => "`/compact`",
    };
    let mut f = facts(cli, cwd, format!("Ereignis: kompaktiert, {how}; die Sitzung läuft weiter"));
    f.push(resume(cli, cwd, session));
    crate::dump::event(
        &stem(cli, "compact"),
        &format!("{cli}: Verlauf kompaktiert"),
        &f,
        &[
            ("Zusammenfassung", summary.as_deref().unwrap_or("")),
            ("Letzte Frage vor dem Kompaktieren", before.prompt.as_deref().unwrap_or("")),
            ("Letzte Antwort vor dem Kompaktieren", before.answer.as_deref().unwrap_or("")),
        ],
    )
    .map(|_| ())
}

fn cleared(cli: &str, old: &str, cwd: &Path, last: Exchange) -> Option<()> {
    let cmd = if cli == "codex" { "`/clear` oder `/new`" } else { "`/clear`" };
    let mut f = facts(cli, cwd, format!("Ereignis: {cmd}, neue Sitzung begonnen"));
    f.push(resume(cli, cwd, old));
    crate::dump::event(
        &stem(cli, "clear"),
        &format!("{cli}: Sitzung geleert"),
        &f,
        &[
            ("Letzte Frage", last.prompt.as_deref().unwrap_or("")),
            ("Letzte Antwort", last.answer.as_deref().unwrap_or("")),
        ],
    )
    .map(|_| ())
}

// ── Verläufe lesen ────────────────────────────────────────────────────────────────────

fn lines(path: &Path) -> impl Iterator<Item = Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect::<Vec<_>>()
        .into_iter()
}

/// Text aus `content`: ein String oder eine Liste von Blöcken mit `kind` (andere Blöcke wie
/// Werkzeugaufrufe fallen weg).
fn text_of(content: &Value, kinds: &[&str]) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter(|i| i["type"].as_str().is_some_and(|t| kinds.contains(&t)))
            .filter_map(|i| i["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Echte Eingaben des Menschen: keine eingeschleusten Blöcke (`<command-name>`,
/// `<system-reminder>`, `# AGENTS.md …`) und keine Slash-Befehle (`/compact`). Ein Pfad
/// wie `/home/…` am Anfang zählt als Eingabe.
fn is_human(text: &str) -> bool {
    let t = text.trim_start();
    let first = t.split_whitespace().next().unwrap_or("");
    let command = first.starts_with('/') && !first[1..].contains('/');
    !t.is_empty() && !t.starts_with('<') && !t.starts_with("# AGENTS.md") && !command
}

fn claude_transcript(path: &Path) -> Transcript {
    let mut t = Transcript::default();
    for v in lines(path) {
        match v["type"].as_str() {
            Some("system") if v["subtype"] == "compact_boundary" => {
                t.before_compact = t.last.clone();
                t.compactions += 1;
                t.summary = None;
            }
            Some("user") => {
                let text = text_of(&v["message"]["content"], &["text"]);
                if v["isCompactSummary"] == true {
                    t.summary = Some(text);
                } else if v["isMeta"] != true && is_human(&text) {
                    t.last.ask(text);
                }
            }
            Some("assistant") => {
                let text = text_of(&v["message"]["content"], &["text"]);
                if !text.trim().is_empty() {
                    t.last.reply(&text);
                }
            }
            _ => {}
        }
    }
    t
}

fn codex_transcript(path: &Path) -> Transcript {
    let mut t = Transcript::default();
    for v in lines(path) {
        let p = &v["payload"];
        match (v["type"].as_str(), p["type"].as_str(), p["role"].as_str()) {
            (Some("compacted"), ..) => {
                t.before_compact = t.last.clone();
                t.compactions += 1;
            }
            (Some("response_item"), Some("message"), Some("user")) => {
                let text = text_of(&p["content"], &["input_text"]);
                if is_human(&text) {
                    t.last.ask(text);
                }
            }
            (Some("response_item"), Some("message"), Some("assistant")) => {
                let text = text_of(&p["content"], &["output_text"]);
                if !text.trim().is_empty() {
                    t.last.reply(&text);
                }
            }
            _ => {}
        }
    }
    t
}

fn codex_is_exec(transcript: &Path) -> bool {
    lines(transcript).next().is_some_and(|v| v["payload"]["originator"] == "codex_exec")
}

// ── codex: alte Sitzung je Prozess merken ─────────────────────────────────────────────

/// Speichert `session` als aktuelle Sitzung dieses codex-Prozesses und gibt die vorige zurück.
fn remember_codex(session: &str, transcript: &Path) -> Option<(String, PathBuf)> {
    let dir = hooks_dir()?;
    // Einträge beendeter codex-Prozesse wegräumen.
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let pid = name.strip_prefix("codex-").and_then(|n| n.strip_suffix(".json"));
        if pid.is_some_and(|pid| !Path::new(&format!("/proc/{pid}")).exists()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let file = dir.join(format!("codex-{}.json", codex_pid()));
    let previous = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| {
            Some((v["session"].as_str()?.to_string(), PathBuf::from(v["transcript"].as_str()?)))
        });
    let now = serde_json::json!({ "session": session, "transcript": transcript });
    let _ = std::fs::write(&file, now.to_string());
    previous
}

/// Der codex-Prozess über uns (der Hook läuft evtl. in einer Shell), sonst der Elternprozess.
fn codex_pid() -> u32 {
    let parent = std::os::unix::process::parent_id();
    let mut pid = parent;
    for _ in 0..5 {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        if comm.trim() == "codex" {
            return pid;
        }
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        // Feld 4 nach dem eingeklammerten Namen, der selbst Leerzeichen enthalten kann.
        let Some(next) = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|p| p.parse().ok())
        else {
            break;
        };
        if next <= 1 {
            break;
        }
        pid = next;
    }
    parent
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, lines: &[Value]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("bridget-hook-{}-{name}", std::process::id()));
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        std::fs::write(&p, text.join("\n")).unwrap();
        p
    }

    #[test]
    fn claude_transcript_splits_at_compaction() {
        use serde_json::json;
        let p = file(
            "claude.jsonl",
            &[
                json!({"type":"user","message":{"content":"Frage 1"}}),
                json!({"type":"assistant","message":{"content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"Antwort 1"}]}}),
                json!({"type":"user","message":{"content":[{"type":"tool_result","content":"egal"}]}}),
                json!({"type":"assistant","message":{"content":[{"type":"text","text":"Nachtrag"}]}}),
                json!({"type":"user","message":{"content":"/compact"}}),
                json!({"type":"system","subtype":"compact_boundary","compactMetadata":{"trigger":"manual"}}),
                json!({"type":"user","isCompactSummary":true,"message":{"content":"Zusammenfassung X"}}),
                json!({"type":"user","message":{"content":"<command-name>/compact</command-name>"}}),
                json!({"type":"user","isMeta":true,"message":{"content":"Meta"}}),
                json!({"type":"user","message":{"content":"Frage 2"}}),
            ],
        );
        let t = claude_transcript(&p);
        let before = Exchange {
            prompt: Some("Frage 1".into()),
            answer: Some("Antwort 1\n\nNachtrag".into()),
        };
        assert_eq!(t.before_compact, before);
        assert_eq!(t.compactions, 1);
        assert_eq!(t.summary.as_deref(), Some("Zusammenfassung X"));
        assert_eq!(t.last, Exchange { prompt: Some("Frage 2".into()), answer: None });
        std::fs::remove_file(p).unwrap();
    }

    #[test]
    fn codex_transcript_skips_injected_context() {
        use serde_json::json;
        let msg = |role: &str, kind: &str, text: &str| json!({"type":"response_item","payload":{"type":"message","role":role,"content":[{"type":kind,"text":text}]}});
        let p = file(
            "codex.jsonl",
            &[
                json!({"type":"session_meta","payload":{"originator":"codex-tui"}}),
                msg("user", "input_text", "# AGENTS.md instructions …"),
                msg("user", "input_text", "Antworte nur: Walnuss"),
                msg("assistant", "output_text", "Walnuss"),
                json!({"type":"compacted","payload":{"message":""}}),
                msg("user", "input_text", "<environment_context>…"),
            ],
        );
        let t = codex_transcript(&p);
        let w = Exchange {
            prompt: Some("Antworte nur: Walnuss".into()),
            answer: Some("Walnuss".into()),
        };
        assert_eq!(t.before_compact, w);
        assert_eq!(t.last, w);
        assert!(!codex_is_exec(&p));
        assert!(is_human("/home/fant/x geht nicht") && !is_human("/compact fokus"));
        std::fs::remove_file(p).unwrap();
    }
}
