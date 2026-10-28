//! Integrationstests für die Markierung: Hintergrundfarbe, Titel-Kennzeichen, `--connect`.

mod common;

use std::time::Duration;

use common::*;

fn colored(dir: &std::path::Path, args: &[&str], color: &str) -> Session {
    Session::start_with_env(dir, args, &[("BRIDGET_COLOR", color.to_string())])
}

#[test]
fn tints_background_and_restores_it() {
    let dir = runtime_dir();
    let mut s = colored(&dir, &["T", "--", "sh", "-c", "read x; echo got:$x"], "blau");
    s.expect("\x1b]11;?\x1b\\\x1b[c");
    // So antwortet ein Terminal mit schwarzem Hintergrund.
    s.send(b"\x1b]11;rgb:0000/0000/0000\x1b\\\x1b[?62;22c");
    s.expect("\x1b]11;#0c1724\x1b\\");
    s.send(b"eins\r");
    s.expect("got:eins");
    assert_eq!(s.wait().exit_code(), 0);
    assert!(contains(&s.seen, b"\x1b]11;#000000\x1b\\"), "Hintergrund nicht zurückgesetzt");
    assert!(!contains(&s.seen, b"62;22c"), "Antwort des Terminals darf nicht beim Programm landen");
}

#[test]
fn no_tint_without_terminal_answer() {
    let dir = runtime_dir();
    let mut s = colored(&dir, &["T", "--", "sh", "-c", "read x; echo got:$x"], "grün");
    s.expect("\x1b]11;?");
    std::thread::sleep(Duration::from_millis(700)); // Abfrage läuft ab
    s.send(b"zwei\r");
    s.expect("got:zwei");
    assert_eq!(s.wait().exit_code(), 0);
    assert!(!contains(&s.seen, b"\x1b]11;#"), "ohne Antwort keine Farbe");
}

#[test]
fn color_off_sends_no_query() {
    let dir = runtime_dir();
    let mut s = colored(&dir, &["T", "--", "sh", "-c", "echo fertig"], "aus");
    s.expect("fertig");
    assert_eq!(s.wait().exit_code(), 0);
    assert!(!contains(&s.seen, b"\x1b]11;"));
}

#[test]
fn program_title_gets_console_label() {
    let mut s = Session::start(&["T", "--", "sh", "-c", "printf '\\033]2;Hallo\\007'; sleep 6"]);
    // Erst steht die Startmeldung im Titel, danach (4 s) der Programmtitel mit Kennzeichen.
    s.expect("\x1b]2;bridget T · Profile");
    s.expect("\x1b]2;[T] Hallo\x07");
    assert!(
        !contains(&s.seen, b"\x1b]2;Hallo\x07"),
        "Programmtitel ohne Kennzeichen durchgelassen"
    );
}

#[test]
fn connect_option_links_on_start() {
    let dir = runtime_dir();
    // B startet zuerst und wartet auf A.
    let mut b = Session::start_in(&dir, &["B", "--connect", "A", "--", "sleep", "10"]);
    wait_for_socket(&dir, "B");
    let _a = Session::start_in(&dir, &["A", "--", "sleep", "10"]);
    b.expect("connected to A");
    let out = ctl(&dir, &["ls"]);
    let ls = String::from_utf8_lossy(&out.stdout);
    assert!(ls.contains("⇄ A"), "{ls}");
}

/// `pair --here` in einem Test-PTY: tmux auf eigenem Test-Server, zwei verbundene Panes,
/// Panes nach der Antwort des „Terminals“ eingefärbt, Sitzung weg nach dem Schließen.
#[test]
fn pair_here_opens_two_linked_tinted_panes() {
    if std::process::Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux fehlt, Test übersprungen");
        return;
    }
    let dir = runtime_dir();
    let server = format!("bridget-test-{}", std::process::id());
    let tmux = |args: &[&str]| {
        let out =
            std::process::Command::new("tmux").arg("-L").arg(&server).args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let mut s = Session::start_with_env(
        &dir,
        &["pair", "--here", "--names", "L,R", "sh -c 'exec cat'", "sh -c 'exec cat'"],
        &[("BRIDGET_TMUX_SERVER", server.clone())],
    );
    s.expect("\x1b]11;?");
    s.send(b"\x1b]11;rgb:0000/0000/0000\x1b\\\x1b[?62;22c");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let ls = loop {
        let ls = String::from_utf8_lossy(&ctl(&dir, &["ls"]).stdout).into_owned();
        if ls.contains("⇄ L") || std::time::Instant::now() > deadline {
            break ls;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(ls.contains("L            ⇄ R") && ls.contains("R            ⇄ L"), "{ls}");
    assert_eq!(tmux(&["list-panes", "-t", "bridget-L-R"]).lines().count(), 2);
    assert!(tmux(&["show-options", "-g", "status"]).contains("status on"));
    let status = tmux(&["show-options", "-t", "bridget-L-R", "status-right"]);
    assert!(status.contains("Actions") && status.contains("Ctrl-Space Enter"), "{status}");
    // Die Farben landen im Bild, das tmux an das „Terminal“ schickt.
    s.expect("48;2;12;23;36"); // blau auf Schwarz
    s.expect("48;2;12;28;13"); // grün auf Schwarz
    assert_eq!(
        tmux(&["show-options", "-t", "bridget-L-R", "destroy-unattached"]).trim(),
        "destroy-unattached on"
    );

    // A real send animates only the reserved status area, and the frame disappears.
    s.send(b"animation-payload\r");
    s.expect("animation-payload");
    assert!(ctl(&dir, &["send", "L", "--screen"]).status.success());
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let options = tmux(&["show-options", "-g"]);
        if options.lines().any(|line| line.starts_with("@bridget_feedback_") && line.contains('●'))
        {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "No transfer animation: {options}");
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(!tmux(&["capture-pane", "-p", "-t", "bridget-L-R"]).contains('●'));
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while tmux(&["show-options", "-g"])
        .lines()
        .any(|line| line.starts_with("@bridget_feedback_") && line.contains('●'))
    {
        assert!(std::time::Instant::now() < deadline, "Animation did not stop");
        std::thread::sleep(Duration::from_millis(50));
    }

    // Fenster zu (Client weg) → Sitzung und beide Konsolen enden.
    s.child.kill().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !tmux(&["ls"]).is_empty() {
        assert!(std::time::Instant::now() < deadline, "Sitzung läuft nach dem Schließen weiter");
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::process::Command::new("tmux").args(["-L", &server, "kill-server"]).output();
}
