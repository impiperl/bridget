//! Abschluss-Dump: Beim Ende einer Konsole oder eines `duo`-Gesprächs (auch wenn das Fenster
//! geschlossen wird) schreibt bridget eine kurze Notiz nach `<gedächtnis>/sitzungen/`.
//!
//! Darin: was lief, wo, wie es endete, wie man die Sitzung fortsetzt, und die letzte
//! Ausgabe. Nur wenn es das Gedächtnis gibt (`crate::memory::dir`) und `BRIDGET_NO_DUMP`
//! nicht gesetzt ist. Dateien `0600`, nach 14 Tagen beim nächsten Dump gelöscht.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Höchstgröße der mitgeschriebenen Ausgabe je Konsole bzw. Seite (das Ende bleibt).
const MAX_TEXT: usize = 16 * 1024;
const KEEP: Duration = Duration::from_secs(14 * 24 * 3600);

/// Ordner für Dumps, `None` = keine Dumps.
pub fn dir() -> Option<PathBuf> {
    if std::env::var_os("BRIDGET_NO_DUMP").is_some_and(|v| !v.is_empty() && v != "0") {
        return None;
    }
    Some(crate::memory::dir()?.join("sitzungen"))
}

pub struct Console<'a> {
    pub name: &'a str,
    pub program: &'a str,
    /// Profilname (`claude`, `codex`, `default`).
    pub kind: &'a str,
    pub cwd: &'a Path,
    pub started: SystemTime,
    pub reason: String,
    pub partner: Option<String>,
    pub last: String,
}

pub fn console(c: &Console) -> Option<PathBuf> {
    let dir = dir()?;
    let what = match c.kind {
        "default" => c.program.split_whitespace().next().unwrap_or("shell"),
        kind => kind,
    };
    let mut body = head(&format!("bridget-Konsole {} ({what}) beendet", c.name));
    body += &format!(
        "- Konsole: `{}` · Programm: `{}` · Ordner: `{}`\n- Zeit: {} – {} · Ende: {}\n",
        c.name,
        c.program,
        c.cwd.display(),
        stamp(c.started).1,
        stamp(SystemTime::now()).1,
        c.reason
    );
    if let Some(p) = &c.partner {
        body += &format!("- Verbunden mit: `{p}`\n");
    }
    // Genau, wenn der Hook die Sitzung dieser Konsole notiert hat; sonst geschätzt.
    let exact = crate::hook::console_session(c.name, c.started).map(|(cli, id)| {
        let cmd = if cli == "codex" { "codex resume" } else { "claude --resume" };
        format!("`cd {} && {cmd} {id}`", c.cwd.display())
    });
    if let Some(hint) = exact.or_else(|| resume_hint(c.kind, c.cwd, c.started)) {
        body += &format!("- Fortsetzen: {hint}\n");
    }
    body += &section("Letzte Ausgabe", &c.last);
    write(&dir, c.name, &body)
}

pub struct DuoSide {
    pub label: String,
    pub resume: Option<String>,
    pub last: Option<String>,
}

pub fn duo(
    short: &str,
    cwd: &Path,
    started: SystemTime,
    reason: &str,
    prompt: &str,
    sides: &[DuoSide],
) -> Option<PathBuf> {
    let dir = dir()?;
    let mut body = head(&format!("bridget duo {short} beendet"));
    body += &format!(
        "- Gespräch: `{short}` · Ordner: `{}`\n- Zeit: {} – {} · Ende: {reason}\n",
        cwd.display(),
        stamp(started).1,
        stamp(SystemTime::now()).1,
    );
    for s in sides {
        if let Some(r) = &s.resume {
            body += &format!("- Fortsetzen {}: `cd {} && {r}`\n", s.label, cwd.display());
        }
    }
    body += &section("Startfrage", prompt);
    for s in sides {
        if let Some(last) = &s.last {
            body += &section(&format!("Letzte Antwort {}", s.label), last);
        }
    }
    write(&dir, &format!("duo-{short}"), &body)
}

/// Dump zu einem Ereignis innerhalb einer CLI-Sitzung (`/compact`, `/clear`, siehe
/// `crate::hook`): Stichpunkte, dann Textabschnitte (leere fallen weg).
pub fn event(
    stem: &str,
    title: &str,
    facts: &[String],
    sections: &[(&str, &str)],
) -> Option<PathBuf> {
    let dir = dir()?;
    let mut body = head(title);
    for f in facts {
        body += &format!("- {f}\n");
    }
    for (name, text) in sections {
        body += &section(name, text);
    }
    write(&dir, stem, &body)
}

fn head(title: &str) -> String {
    format!(
        "---\ntyp: sitzung\ntitel: {title}\nquelle: bridget\nstand: {}\n---\n\n\
         Automatisch von bridget beim Beenden geschrieben. Kann Bildschirminhalt enthalten.\n\n",
        stamp(SystemTime::now()).0
    )
}

fn section(title: &str, text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let text = tail(text, MAX_TEXT);
    // Zaun länger als jede Backtick-Folge im Text, damit der Block nicht vorzeitig endet.
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("\n## {title}\n\n{fence}text\n{text}\n{fence}\n")
}

/// Das Ende von `text`, höchstens `max` Bytes, an einer Zeichengrenze.
fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn write(dir: &Path, stem: &str, body: &str) -> Option<PathBuf> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).ok()?;
    cleanup(dir);
    let (day, time) = stamp(SystemTime::now());
    let base = format!("{day}-{}-{stem}", time.replace(':', ""));
    // Zwei Konsolen eines Fensters enden gleichzeitig: nie eine fremde Datei überschreiben.
    for n in 0..100 {
        let name = if n == 0 { format!("{base}.md") } else { format!("{base}-{n}.md") };
        let path = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(mut f) => return f.write_all(body.as_bytes()).ok().map(|()| path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

fn cleanup(dir: &Path) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > KEEP);
        if old && entry.path().extension().is_some_and(|x| x == "md") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Ortszeit als (`JJJJ-MM-TT`, `HH:MM:SS`).
fn stamp(t: SystemTime) -> (String, String) {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let secs = secs as libc::time_t;
    // SAFETY: localtime_r schreibt nur in das übergebene tm.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    (
        format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday),
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec),
    )
}

// ── Sitzung zum Fortsetzen finden ─────────────────────────────────────────────────────

/// Interaktive claude/codex-Sitzungen haben keine ID, die bridget kennt. Gesucht wird die
/// Sitzungsdatei der CLI, die seit dem Start in diesem Ordner geschrieben wurde. Bei mehreren
/// Treffern (z.B. zweimal claude im selben Ordner) werden alle genannt.
fn resume_hint(kind: &str, cwd: &Path, since: SystemTime) -> Option<String> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let root = |var: &str, dot: &str| {
        std::env::var_os(var).map(PathBuf::from).unwrap_or_else(|| home.join(dot))
    };
    let (ids, cmd, fallback) = match kind {
        "claude" => (
            claude_sessions(&root("CLAUDE_CONFIG_DIR", ".claude"), cwd, since),
            "claude --resume",
            "claude --continue",
        ),
        "codex" => (
            codex_sessions(&root("CODEX_HOME", ".codex"), cwd, since),
            "codex resume",
            "codex resume --last",
        ),
        _ => return None,
    };
    let at = cwd.display();
    Some(match ids.as_slice() {
        [] => format!("`cd {at} && {fallback}` (Sitzung nicht gefunden, neueste im Ordner)"),
        [id] => format!("`cd {at} && {cmd} {id}`"),
        many => format!(
            "eine von {} (mehrere Sitzungen in diesem Zeitraum): `cd {at} && {cmd} <ID>`",
            many.iter().map(|i| format!("`{i}`")).collect::<Vec<_>>().join(", ")
        ),
    })
}

fn modified_since(path: &Path, since: SystemTime) -> bool {
    fs::metadata(path).and_then(|m| m.modified()).is_ok_and(|t| t >= since)
}

/// `~/.claude/projects/<ordner, jedes Nicht-Alphanumerische als ->/<uuid>.jsonl`
fn claude_sessions(root: &Path, cwd: &Path, since: SystemTime) -> Vec<String> {
    let key: String = cwd
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = root.join("projects").join(key);
    let mut ids: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl") && modified_since(p, since))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    ids.sort();
    ids
}

/// `~/.codex/sessions/JJJJ/MM/TT/rollout-….jsonl`, erste Zeile `session_meta` mit `cwd`.
/// Headless-Aufrufe (`codex exec`, z.B. aus `duo`) zählen nicht.
fn codex_sessions(root: &Path, cwd: &Path, since: SystemTime) -> Vec<String> {
    let root = root.join("sessions");
    let mut days: Vec<String> =
        [since, SystemTime::now()].iter().map(|t| stamp(*t).0.replace('-', "/")).collect();
    days.dedup();
    let mut ids = Vec::new();
    for day in days {
        for entry in fs::read_dir(root.join(day)).into_iter().flatten().flatten() {
            let path = entry.path();
            if !modified_since(&path, since) {
                continue;
            }
            let Some(meta) = first_line(&path) else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&meta) else { continue };
            let p = &v["payload"];
            let interactive = p["originator"].as_str().is_some_and(|o| o != "codex_exec");
            if p["cwd"].as_str() == Some(&*cwd.to_string_lossy())
                && interactive
                && let Some(id) = p["id"].as_str()
            {
                ids.push(id.to_string());
            }
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

fn first_line(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader, Read};
    let f = fs::File::open(path).ok()?;
    let mut line = String::new();
    // Die erste Zeile enthält die Basis-Anweisungen und kann groß sein.
    BufReader::new(f.take(1024 * 1024)).read_line(&mut line).ok()?;
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_fence_outlasts_backticks_in_text() {
        let s = section("X", "a ```rust b");
        assert!(s.contains("````text\n"), "{s}");
        assert!(section("X", "  \n").is_empty());
    }

    #[test]
    fn tail_keeps_end_on_char_boundary() {
        assert_eq!(tail("abc", 10), "abc");
        assert_eq!(tail("äbc", 3), "bc");
    }

    #[test]
    fn finds_sessions_written_since_start() {
        let home = std::env::temp_dir().join(format!("bridget-dump-{}", std::process::id()));
        let cwd = Path::new("/w/pro.j");
        let since = SystemTime::now() - Duration::from_secs(5);
        let cdir = home.join(".claude/projects/-w-pro-j");
        fs::create_dir_all(&cdir).unwrap();
        fs::write(cdir.join("u1.jsonl"), "{}").unwrap();
        let day = stamp(SystemTime::now()).0.replace('-', "/");
        let xdir = home.join(".codex/sessions").join(day);
        fs::create_dir_all(&xdir).unwrap();
        let meta = |id: &str, orig: &str| {
            format!(
                r#"{{"type":"session_meta","payload":{{"id":"{id}","cwd":"/w/pro.j","originator":"{orig}"}}}}"#
            )
        };
        fs::write(xdir.join("rollout-a.jsonl"), meta("T1", "codex_cli_rs") + "\n").unwrap();
        fs::write(xdir.join("rollout-b.jsonl"), meta("T2", "codex_exec") + "\n").unwrap();
        let (claude, codex) = (home.join(".claude"), home.join(".codex"));
        assert_eq!(claude_sessions(&claude, cwd, since), vec!["u1"]);
        assert_eq!(codex_sessions(&codex, cwd, since), vec!["T1"]);
        let later = SystemTime::now() + Duration::from_secs(60);
        assert!(claude_sessions(&claude, cwd, later).is_empty());
        fs::remove_dir_all(&home).unwrap();
    }
}
