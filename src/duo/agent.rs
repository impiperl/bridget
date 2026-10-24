//! Adapter für die KI-CLIs: Befehle bauen, headless starten, Ereignisstrom auswerten.
//!
//! Rechte (Ergebnis Phase 0):
//! - claude: `--restricted --tools Read Grep Glob --strict-mcp-config`. `--allowedTools`
//!   reicht nicht, weil die eigenen Freigaben aus den Settings weiter gelten.
//! - codex: `--ignore-user-config --ignore-rules`, Sandbox read-only; bei `exec resume` gibt
//!   es kein `-s`, dort `-c sandbox_mode="read-only"`. Modell explizit per `-m`.
//! - Der Prompt geht immer über stdin (Listen-Optionen würden ihn sonst verschlucken).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Höchstgröße einer Zeile im Ereignisstrom (Werkzeug-Ergebnisse können groß sein).
const MAX_EVENT_LINE: usize = 16 * 1024 * 1024;
/// Höchstgröße einer Antwort, die weitergegeben wird.
pub const MAX_ANSWER: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Claude,
    Codex,
}

impl Kind {
    pub fn parse(s: &str) -> Result<Kind> {
        match s {
            "claude" => Ok(Kind::Claude),
            "codex" => Ok(Kind::Codex),
            other => bail!(
                "{}",
                if crate::locale::german() {
                    format!("unbekannte KI-CLI {other:?} (möglich: claude, codex)")
                } else {
                    format!("unknown AI CLI {other:?} (available: claude, codex)")
                }
            ),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Codex => "codex",
        }
    }
}

/// Was während eines Aufrufs passiert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentEvent {
    /// Sitzungs-ID bekannt (codex: erst nach dem ersten Ereignis).
    Session(String),
    /// Text-Stück (claude: Wort für Wort).
    Delta(String),
    /// Eine ganze Nachricht (codex: pro Schritt).
    Message(String),
    /// Werkzeugaufruf, kurz beschrieben.
    Tool(String),
    /// Abgelehnter Werkzeugaufruf (z.B. Schreiben).
    Denied(String),
    Done(String),
    Failed(String),
}

/// Eine Seite des Gesprächs mit ihrer Sitzung.
#[derive(Debug, Clone)]
pub struct Agent {
    pub kind: Kind,
    pub cwd: PathBuf,
    /// claude: von duo vergeben; codex: kommt mit dem ersten Aufruf.
    pub session: Option<String>,
    /// Ob schon ein Aufruf mit dieser Sitzung lief (claude: dann `--resume`).
    pub started: bool,
    /// codex: Modell (ohne User-Config sonst unbestimmt).
    pub model: Option<String>,
}

impl Agent {
    pub fn new(kind: Kind, cwd: PathBuf, model: Option<String>) -> Agent {
        let session = match kind {
            Kind::Claude => Some(uuid_v4()),
            Kind::Codex => None,
        };
        Agent { kind, cwd, session, started: false, model }
    }

    /// Argumente für den headless Aufruf (ohne Programmname).
    pub fn headless_args(&self, out_file: &Path) -> Vec<String> {
        let s = |v: &str| v.to_string();
        match self.kind {
            Kind::Claude => {
                let mut a = vec![s("-p")];
                let id = self.session.clone().unwrap_or_default();
                if self.started {
                    a.extend([s("--resume"), id]);
                } else {
                    a.extend([s("--session-id"), id]);
                }
                a.extend(
                    [
                        "--output-format",
                        "stream-json",
                        "--verbose",
                        "--include-partial-messages",
                        "--restricted",
                        "--strict-mcp-config",
                        "--tools",
                        "Read",
                        "Grep",
                        "Glob",
                    ]
                    .map(s),
                );
                a
            }
            Kind::Codex => {
                let mut a = vec![s("exec")];
                match (&self.session, self.started) {
                    (Some(id), true) => {
                        a.extend([s("resume"), s("--json")]);
                        a.extend([s("-c"), s("sandbox_mode=\"read-only\"")]);
                        a.extend(common_codex(&self.model, out_file));
                        a.extend([id.clone(), s("-")]);
                    }
                    _ => {
                        a.extend([s("--json"), s("-s"), s("read-only")]);
                        a.extend([s("-C"), self.cwd.display().to_string()]);
                        a.extend(common_codex(&self.model, out_file));
                        a.push(s("-"));
                    }
                }
                a
            }
        }
    }

    /// Befehl für die interaktive Übernahme.
    pub fn interactive_command(&self) -> Option<Vec<String>> {
        let id = self.session.clone()?;
        Some(match self.kind {
            Kind::Claude => vec!["claude".into(), "--resume".into(), id],
            // Inline statt Alternate Screen: Die Sitzung bleibt im Scrollback des Terminals,
            // lange Antworten lassen sich danach noch lesen und kopieren.
            Kind::Codex => vec!["codex".into(), "--no-alt-screen".into(), "resume".into(), id],
        })
    }
}

fn common_codex(model: &Option<String>, out_file: &Path) -> Vec<String> {
    let mut a = vec![
        "--ignore-user-config".to_string(),
        "--ignore-rules".to_string(),
        "-o".to_string(),
        out_file.display().to_string(),
    ];
    if let Some(m) = model {
        a.extend(["-m".to_string(), m.clone()]);
    }
    a
}

/// Modell aus `~/.codex/config.toml` lesen (nur dieser eine Schlüssel), weil die
/// User-Config für die headless Aufrufe bewusst nicht geladen wird.
pub fn codex_model_from_user_config() -> Option<String> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))?;
    let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let value: toml::Table = toml::from_str(&text).ok()?;
    value.get("model")?.as_str().map(String::from)
}

fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    let random = std::fs::File::open("/dev/urandom").and_then(|mut f| {
        use std::io::Read;
        f.read_exact(&mut b)
    });
    if random.is_err() {
        fallback_random(&mut b);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Ohne `/dev/urandom`: Zeit, PID und Zähler durch zufällig geschlüsselte Hasher.
/// Nicht kryptografisch, aber eindeutig genug für Sitzungs- und Dateinamen.
fn fallback_random(b: &mut [u8; 16]) {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    for half in b.chunks_mut(8) {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(nanos);
        h.write_u32(std::process::id());
        h.write_u64(count);
        half.copy_from_slice(&h.finish().to_le_bytes());
    }
}

pub fn new_id() -> String {
    uuid_v4()
}

// ── Ereignisstrom auswerten ─────────────────────────────────────────────────────

/// Eine JSON-Zeile von `claude -p --output-format stream-json` auswerten.
pub fn parse_claude(line: &Value) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    match line["type"].as_str() {
        Some("system") if line["subtype"] == "init" => {
            if let Some(id) = line["session_id"].as_str() {
                out.push(AgentEvent::Session(id.into()));
            }
        }
        Some("stream_event") => {
            let ev = &line["event"];
            if ev["type"] == "content_block_delta"
                && ev["delta"]["type"] == "text_delta"
                && let Some(t) = ev["delta"]["text"].as_str()
            {
                out.push(AgentEvent::Delta(t.into()));
            }
        }
        Some("assistant") => {
            for block in line["message"]["content"].as_array().into_iter().flatten() {
                if block["type"] == "tool_use" {
                    out.push(AgentEvent::Tool(describe_tool(block)));
                }
            }
        }
        Some("result") => {
            for d in line["permission_denials"].as_array().into_iter().flatten() {
                out.push(AgentEvent::Denied(describe_tool(&serde_json::json!({
                    "name": d["tool_name"], "input": d["tool_input"]
                }))));
            }
            let text = line["result"].as_str().unwrap_or_default().to_string();
            if line["is_error"] == true || line["subtype"] != "success" {
                let why = if text.is_empty() { line["subtype"].to_string() } else { text };
                out.push(AgentEvent::Failed(format!("claude: {why}")));
            } else {
                out.push(AgentEvent::Done(text));
            }
        }
        _ => {}
    }
    out
}

fn describe_tool(block: &Value) -> String {
    let name = block["name"].as_str().unwrap_or("?");
    let input = &block["input"];
    let detail = ["file_path", "path", "pattern", "command"]
        .iter()
        .find_map(|k| input[*k].as_str())
        .unwrap_or("");
    if detail.is_empty() { name.to_string() } else { format!("{name} {detail}") }
}

/// Eine JSON-Zeile von `codex exec --json` auswerten. `Done` kommt erst vom Aufrufer
/// (aus der `-o`-Datei), wenn der Prozess sauber beendet ist.
pub fn parse_codex(line: &Value) -> Vec<AgentEvent> {
    let mut out = Vec::new();
    match line["type"].as_str() {
        Some("thread.started") => {
            if let Some(id) = line["thread_id"].as_str() {
                out.push(AgentEvent::Session(id.into()));
            }
        }
        Some("item.completed") => {
            let item = &line["item"];
            match item["type"].as_str() {
                Some("agent_message") => {
                    if let Some(t) = item["text"].as_str() {
                        out.push(AgentEvent::Message(t.into()));
                    }
                }
                Some("command_execution") => {
                    let cmd = item["command"].as_str().unwrap_or("?");
                    let code =
                        item["exit_code"].as_i64().map(|c| format!(" → {c}")).unwrap_or_default();
                    out.push(AgentEvent::Tool(format!("shell {cmd}{code}")));
                }
                Some("file_change") | Some("patch") => {
                    out.push(AgentEvent::Tool(format!(
                        "{} {}",
                        tr!("file change", "Dateiänderung"),
                        item["status"]
                    )));
                }
                _ => {}
            }
        }
        Some("turn.failed") | Some("error") => {
            let msg = line["error"]["message"]
                .as_str()
                .or(line["message"].as_str())
                .unwrap_or(tr!("Error", "Fehler"));
            out.push(AgentEvent::Failed(format!("codex: {msg}")));
        }
        _ => {}
    }
    out
}

// ── Aufruf ausführen ────────────────────────────────────────────────────────────

/// Laufender Aufruf: Prozessgruppe zum Abbrechen.
pub struct Turn {
    pgid: i32,
}

/// So lange darf ein Aufruf nach SIGTERM noch aufräumen, dann kommt SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(2);

impl Turn {
    /// Abbrechen, ohne zu warten. Wer SIGTERM ignoriert, bekommt im Hintergrund SIGKILL.
    pub fn kill(&self) {
        let pgid = self.pgid;
        thread::spawn(move || terminate_group(pgid));
    }

    /// Abbrechen und warten, bis die Gruppe weg ist (beim Beenden von bridget).
    pub fn kill_and_wait(&self) {
        terminate_group(self.pgid);
    }
}

/// SIGTERM an die Prozessgruppe, nach `KILL_GRACE` SIGKILL an alles, was noch lebt.
/// Solange die Gruppe Mitglieder hat, vergibt der Kernel ihre ID nicht neu; ist sie leer,
/// läuft das Signal ins Leere (ESRCH).
fn terminate_group(pgid: i32) {
    // SAFETY: Signale an die eigene, für diesen Aufruf angelegte Prozessgruppe;
    // Signal 0 prüft nur, ob es sie noch gibt.
    unsafe { libc::kill(-pgid, libc::SIGTERM) };
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        if unsafe { libc::kill(-pgid, 0) } != 0 {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    unsafe { libc::kill(-pgid, libc::SIGKILL) };
}

/// Startet einen headless Aufruf. Ereignisse kommen über `events`, das letzte ist immer
/// `Done` oder `Failed`.
pub fn start_turn(
    agent: &Agent,
    prompt: &str,
    work_dir: &Path,
    events: Sender<AgentEvent>,
) -> Result<Turn> {
    let out_file = work_dir.join(format!("last-{}.txt", new_id()));
    let args = agent.headless_args(&out_file);
    let mut cmd = Command::new(agent.kind.name());
    cmd.args(&args)
        .current_dir(&agent.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    // In einer Claude-Code-Sitzung gestartet: nicht als „verschachtelt“ gelten.
    cmd.env_remove("CLAUDECODE");
    let mut child = cmd.spawn().with_context(|| {
        if crate::locale::german() {
            format!("{} starten", agent.kind.name())
        } else {
            format!("start {}", agent.kind.name())
        }
    })?;
    let pgid = child.id() as i32;

    let mut stdin = child.stdin.take().unwrap();
    let prompt = prompt.to_string();
    thread::spawn(move || {
        let _ = stdin.write_all(prompt.as_bytes());
        // stdin schließen, sonst wartet die CLI auf mehr Eingabe.
    });

    let kind = agent.kind;
    thread::spawn(move || {
        let result = read_events(kind, &mut child, &out_file, &events);
        let _ = std::fs::remove_file(&out_file);
        if let Err(e) = result {
            let _ = events.send(AgentEvent::Failed(format!("{e:#}")));
        }
    });
    Ok(Turn { pgid })
}

fn read_events(
    kind: Kind,
    child: &mut Child,
    out_file: &Path,
    events: &Sender<AgentEvent>,
) -> Result<()> {
    let stderr = child.stderr.take().unwrap();
    let stderr_tail = thread::spawn(move || {
        let mut tail: Vec<String> = Vec::new();
        let mut denied = false;
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("blocked by read-only sandbox") || line.contains("patch rejected") {
                denied = true;
            }
            tail.push(line);
            if tail.len() > 20 {
                tail.remove(0);
            }
        }
        (tail, denied)
    });

    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut finished = false;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = read_bounded_line(&mut reader, &mut line)?;
        if n == 0 {
            break;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else { continue };
        let parsed = match kind {
            Kind::Claude => parse_claude(&value),
            Kind::Codex => parse_codex(&value),
        };
        for ev in parsed {
            if matches!(ev, AgentEvent::Done(_) | AgentEvent::Failed(_)) {
                if finished {
                    continue;
                }
                finished = true;
                if let AgentEvent::Done(t) = &ev
                    && t.len() > MAX_ANSWER
                {
                    events.send(AgentEvent::Failed(if crate::locale::german() {
                        format!(
                            "Antwort zu groß ({} KiB, höchstens {} KiB)",
                            t.len() / 1024,
                            MAX_ANSWER / 1024
                        )
                    } else {
                        format!(
                            "response too large ({} KiB, maximum {} KiB)",
                            t.len() / 1024,
                            MAX_ANSWER / 1024
                        )
                    }))?;
                    continue;
                }
            }
            events.send(ev)?;
        }
    }

    let status = child.wait()?;
    let (tail, denied) = stderr_tail.join().unwrap_or_default();
    if denied {
        events.send(AgentEvent::Denied(
            tr!("write access (read-only sandbox)", "Schreiben (Sandbox read-only)").into(),
        ))?;
    }
    if finished {
        return Ok(());
    }
    if !status.success() {
        bail!(
            "{} {} {status}: {}",
            kind.name(),
            tr!("exited with", "endete mit"),
            tail.last().map(String::as_str).unwrap_or(tr!("(no message)", "(keine Meldung)"))
        );
    }
    // codex: die Antwort steht in der -o-Datei.
    let text = std::fs::read_to_string(out_file).unwrap_or_default();
    let text = text.trim().to_string();
    if text.is_empty() {
        bail!("{} {}", kind.name(), tr!("returned no response", "hat keine Antwort geliefert"));
    }
    if text.len() > MAX_ANSWER {
        bail!(
            "{}",
            if crate::locale::german() {
                format!("Antwort zu groß ({} KiB)", text.len() / 1024)
            } else {
                format!("response too large ({} KiB)", text.len() / 1024)
            }
        );
    }
    events.send(AgentEvent::Done(text))?;
    Ok(())
}

fn read_bounded_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> Result<usize> {
    let mut total = 0;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(total);
        }
        let (chunk, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (&buf[..i], i + 1),
            None => (buf, buf.len()),
        };
        if line.len() + chunk.len() > MAX_EVENT_LINE {
            bail!(
                "{}",
                if crate::locale::german() {
                    format!("Ereigniszeile über {} MiB", MAX_EVENT_LINE / 1024 / 1024)
                } else {
                    format!("event line exceeds {} MiB", MAX_EVENT_LINE / 1024 / 1024)
                }
            );
        }
        line.extend_from_slice(chunk);
        let newline = done > chunk.len();
        reader.consume(done);
        total += done;
        if newline {
            return Ok(total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(fixture: &str, parse: fn(&Value) -> Vec<AgentEvent>) -> Vec<AgentEvent> {
        fixture
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .flat_map(|v| parse(&v))
            .collect()
    }

    #[test]
    fn claude_fixture_streams_text_tools_and_result() {
        let ev = events(include_str!("../../tests/fixtures/headless/claude1.jsonl"), parse_claude);
        assert!(matches!(&ev[0], AgentEvent::Session(id) if id.len() == 36));
        assert!(ev.contains(&AgentEvent::Tool("Read /work/notiz.txt".into())));
        let streamed: String = ev
            .iter()
            .filter_map(|e| if let AgentEvent::Delta(t) = e { Some(t.as_str()) } else { None })
            .collect();
        assert!(streamed.contains("Sonnenblume"), "{streamed}");
        assert_eq!(
            ev.last().unwrap(),
            &AgentEvent::Done("Laut `notiz.txt` heißt das Projekt „Sonnenblume“.".into())
        );
    }

    #[test]
    fn claude_fixture_reports_denials() {
        let ev = events(include_str!("../../tests/fixtures/headless/claude2.jsonl"), parse_claude);
        assert!(ev.contains(&AgentEvent::Denied("Write /work/neu.txt".into())), "{ev:?}");
        assert!(matches!(ev.last(), Some(AgentEvent::Done(_))));
    }

    #[test]
    fn codex_fixture_messages_and_commands() {
        let ev = events(include_str!("../../tests/fixtures/headless/codex1.jsonl"), parse_codex);
        assert_eq!(ev[0], AgentEvent::Session("01a16684-f9b1-7450-a66e-3d8c99df352f".into()));
        assert!(ev.iter().any(
            |e| matches!(e, AgentEvent::Tool(t) if t.contains("notiz.txt") && t.ends_with("→ 0"))
        ));
        assert!(ev.contains(&AgentEvent::Message("Das Projekt heißt „Sonnenblume“.".into())));
        assert!(!ev.iter().any(|e| matches!(e, AgentEvent::Done(_))), "Done kommt aus -o");
    }

    #[test]
    fn claude_args_restrict_tools_and_resume() {
        let mut a = Agent::new(Kind::Claude, "/tmp".into(), None);
        let id = a.session.clone().unwrap();
        let args = a.headless_args(Path::new("/x"));
        let joined = args.join(" ");
        assert!(joined.contains(&format!("--session-id {id}")));
        assert!(joined.contains("--restricted"));
        assert!(joined.contains("--strict-mcp-config"));
        assert!(joined.ends_with("--tools Read Grep Glob"));
        assert!(!joined.contains("allowedTools") && !joined.contains("dangerously"));
        a.started = true;
        assert!(a.headless_args(Path::new("/x")).join(" ").contains(&format!("--resume {id}")));
        assert_eq!(a.interactive_command().unwrap(), vec!["claude", "--resume", &id]);
    }

    #[test]
    fn codex_args_first_and_resume() {
        let mut a = Agent::new(Kind::Codex, "/w".into(), Some("m1".into()));
        let first = a.headless_args(Path::new("/o")).join(" ");
        assert_eq!(
            first,
            "exec --json -s read-only -C /w --ignore-user-config --ignore-rules -o /o -m m1 -"
        );
        a.session = Some("T1".into());
        a.started = true;
        let resume = a.headless_args(Path::new("/o")).join(" ");
        assert_eq!(
            resume,
            "exec resume --json -c sandbox_mode=\"read-only\" --ignore-user-config --ignore-rules -o /o -m m1 T1 -"
        );
        assert!(!resume.contains(" -s "));
        assert_eq!(
            a.interactive_command().unwrap(),
            vec!["codex", "--no-alt-screen", "resume", "T1"]
        );
    }

    #[test]
    fn uuid_format() {
        let u = uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert_ne!(u, uuid_v4());
        let (mut x, mut y) = ([0u8; 16], [0u8; 16]);
        fallback_random(&mut x);
        fallback_random(&mut y);
        assert_ne!(x, [0u8; 16]);
        assert_ne!(x, y);
    }
}
