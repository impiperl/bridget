//! Integrationstests für `bridget duo` mit nachgebauten CLIs (tests/fake/).

mod common;

use common::*;

struct Duo {
    dir: std::path::PathBuf,
    log: std::path::PathBuf,
}

impl Duo {
    fn new() -> Duo {
        let dir = runtime_dir();
        let log = dir.join("fake.log");
        Duo { dir, log }
    }

    fn env(&self, sleep: &str) -> Vec<(&'static str, String)> {
        let fake = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake");
        vec![
            ("PATH", format!("{fake}:{}", std::env::var("PATH").unwrap())),
            ("FAKE_LOG", self.log.display().to_string()),
            ("FAKE_SLEEP", sleep.to_string()),
            ("HOME", self.dir.display().to_string()), // keine echte codex-Config
        ]
    }

    fn start(&self, args: &[&str], sleep: &str) -> Session {
        let mut all = vec!["duo"];
        all.extend_from_slice(args);
        Session::start_with_env(&self.dir, &all, &self.env(sleep))
    }

    fn start_with_memory(&self, args: &[&str], mem: &std::path::Path) -> Session {
        let mut all = vec!["duo"];
        all.extend_from_slice(args);
        let mut env = self.env("0");
        env.push(("BRIDGET_MEMORY", mem.display().to_string()));
        Session::start_with_env(&self.dir, &all, &env)
    }

    fn memory(&self) -> std::path::PathBuf {
        let mem = self.dir.join("mem");
        std::fs::create_dir_all(mem.join("regeln")).unwrap();
        std::fs::write(mem.join("INDEX.md"), "# Index\n").unwrap();
        mem.canonicalize().unwrap()
    }

    fn cwd_log(&self) -> String {
        std::fs::read_to_string(self.log.with_extension("log.cwd")).unwrap_or_default()
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn duo_id(s: &Session) -> String {
    let out = String::from_utf8_lossy(&s.seen);
    let pos = out.find("duo join ").expect("ID angezeigt") + "duo join ".len();
    out[pos..pos + 8].to_string()
}

#[test]
fn alternates_with_restricted_flags_and_resumes_sessions() {
    let d = Duo::new();
    let mut s = d.start(&["--rounds", "4", "Wie", "heißt", "das", "Projekt?"], "0");
    s.expect("Round limit reached (4/4)");
    s.expect("codex-antwort zu: claude-antwort zu:");
    s.send(b"q");
    s.expect("Resume sessions: claude --resume");
    assert_eq!(s.wait().exit_code(), 0);

    let log = d.log();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 4, "{log}");
    assert!(lines[0].starts_with("claude -p --session-id "), "{log}");
    for l in [lines[0], lines[2]] {
        assert!(l.contains("--restricted") && l.contains("--strict-mcp-config"), "{l}");
        assert!(l.ends_with("--tools Read Grep Glob"), "{l}");
        assert!(!l.contains("allowedTools") && !l.contains("dangerously") && !l.contains("bypass"));
    }
    assert!(lines[2].starts_with("claude -p --resume "), "{log}");
    assert!(lines[1].starts_with("codex exec --json -s read-only"), "{log}");
    assert!(lines[1].contains("--ignore-user-config --ignore-rules"), "{log}");
    assert!(
        lines[3].starts_with("codex exec resume --json -c sandbox_mode=\"read-only\""),
        "{log}"
    );
    assert!(lines[3].contains("fake-thread-1 -"), "{log}");
}

#[test]
fn pause_inject_and_continue() {
    let d = Duo::new();
    let mut s = d.start(&["--rounds", "1", "Start"], "0");
    s.expect("Round limit reached (1/1)");
    s.send(b"i");
    s.expect("Message to both");
    s.send(b"Bitte kurz\r");
    s.expect("speaks next): Bitte kurz");
    s.send(b" ");
    // B bekommt beide Nachrichten; die letzte Zeile ist die des Menschen.
    s.expect("codex-antwort zu: Bitte kurz");
    s.expect("Round limit reached (3/3)");
    s.send(b"q");
    assert_eq!(s.wait().exit_code(), 0);
}

#[test]
fn second_console_shows_side_b_and_can_quit() {
    let d = Duo::new();
    let mut a = d.start(&["--rounds", "2", "Start"], "1");
    a.expect("duo join ");
    let id = duo_id(&a);
    let mut v = Session::start_with_env(&d.dir, &["duo", "join", &id], &d.env("1"));
    v.expect("this console shows codex (B)");
    a.expect("Second console connected");
    v.expect("codex-antwort zu:");
    a.expect("Round limit reached (2/2)");
    v.send(b"q");
    a.expect("Conversation ended");
    v.expect("Conversation ended");
    assert_eq!(a.wait().exit_code(), 0);
    assert_eq!(v.wait().exit_code(), 0);
    // Seite B wurde nicht in der ersten Konsole gezeigt.
    assert!(!contains(&a.seen, b"codex-antwort"));
}

#[test]
fn timeout_turns_into_error_and_retry_is_possible() {
    let d = Duo::new();
    let mut s = d.start(&["--rounds", "2", "--timeout", "1", "Start"], "30");
    s.expect("Timed out after 1 s");
    s.expect("Error: Timed out");
    s.send(b"q");
    assert_eq!(s.wait().exit_code(), 0);
}

#[test]
fn wrong_join_id_is_rejected() {
    let d = Duo::new();
    let mut v = Session::start_with_env(&d.dir, &["duo", "join", "00000000"], &d.env("0"));
    v.expect("No running duo conversation");
    assert_eq!(v.wait().exit_code(), 1);
}

#[test]
fn memory_is_writable_by_default_and_nothing_else() {
    let d = Duo::new();
    let mem = d.memory();
    let m = mem.display().to_string();
    let mut s = d.start_with_memory(&["--rounds", "4", "Start"], &mem);
    s.expect("Round limit reached (4/4)");
    s.send(b"q");
    assert_eq!(s.wait().exit_code(), 0);

    let log = d.log();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 4, "{log}");
    for l in [lines[0], lines[2]] {
        assert!(l.contains("--restricted") && l.contains("--permission-mode dontAsk"), "{l}");
        let tail = format!("--allowedTools Edit(/{m}/**) --tools Read Grep Glob Edit Write");
        assert!(l.ends_with(&tail), "{l}");
        assert!(!l.contains("bypass") && !l.contains("dangerously"), "{l}");
    }
    assert!(
        lines[1].starts_with(&format!("codex exec --json -s workspace-write -C {m} ")),
        "{log}"
    );
    assert!(
        lines[3].starts_with("codex exec resume --json -c sandbox_mode=\"workspace-write\""),
        "{log}"
    );
    for l in [lines[1], lines[3]] {
        assert!(l.contains("exclude_slash_tmp=true") && l.contains("network_access=false"), "{l}");
        assert!(l.contains("--skip-git-repo-check"), "{l}");
    }
    // codex startet bei beiden Aufrufen im Gedächtnis: Bei `resume` ist das Startverzeichnis
    // der Schreibbereich.
    let cwds: Vec<String> = d.cwd_log().lines().map(String::from).collect();
    assert_eq!(cwds, vec![m.clone(), m.clone()]);
}

#[test]
fn no_memory_write_keeps_everything_read_only() {
    let d = Duo::new();
    let mem = d.memory();
    let m = mem.display().to_string();
    let mut s = d.start_with_memory(&["--rounds", "2", "--no-memory-write", "Start"], &mem);
    s.expect("Round limit reached (2/2)");
    s.send(b"q");
    assert_eq!(s.wait().exit_code(), 0);

    let log = d.log();
    let lines: Vec<&str> = log.lines().collect();
    assert!(lines[0].ends_with(&format!("--add-dir {m} --tools Read Grep Glob")), "{log}");
    assert!(!log.contains("dontAsk") && !log.contains("allowedTools"), "{log}");
    assert!(lines[1].starts_with("codex exec --json -s read-only"), "{log}");
    assert!(!log.contains("workspace-write") && !log.contains("skip-git-repo-check"), "{log}");
    assert!(!d.cwd_log().lines().any(|c| c == m), "{}", d.cwd_log());
}
