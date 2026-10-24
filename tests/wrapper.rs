//! Integrationstests: bridget läuft in einem Test-PTY, so wie in einer echten Konsole.

mod common;

use std::thread;
use std::time::Duration;

use common::*;
use portable_pty::PtySize;

#[test]
fn passes_input_and_output_through() {
    let mut s = Session::start(&["T", "--", "cat"]);
    s.send(b"hallo welt\r");
    s.expect("hallo welt\r\nhallo welt\r\n");
    s.send(b"\x04"); // Ctrl-D beendet cat
    assert_eq!(s.wait().exit_code(), 0);
}

#[test]
fn propagates_exit_code() {
    let mut s = Session::start(&["T", "--", "sh", "-c", "exit 3"]);
    assert_eq!(s.wait().exit_code(), 3);
}

#[test]
fn double_prefix_reaches_program_single_prefix_does_not() {
    let mut s = Session::start(&[
        "T",
        "--",
        "sh",
        "-c",
        "stty raw -echo; dd bs=1 count=3 2>/dev/null | od -An -tx1",
    ]);
    thread::sleep(Duration::from_millis(300)); // stty muss gelaufen sein
    // Ctrl-] ? (Hotkey, kommt nicht an), Ctrl-] Ctrl-] (→ ein 0x1d), dann "ab"
    s.send(b"\x1d?\x1d\x1dab");
    s.expect("1d 61 62");
    assert_eq!(s.wait().exit_code(), 0);
    assert!(contains(&s.seen, b"\x1b]2;bridget T"), "Hilfe sollte den Titel setzen");
}

#[test]
fn forwards_window_size_changes() {
    let mut s = Session::start(&["T", "--", "sh", "-c", "stty size; read x; stty size"]);
    s.expect("24 80");
    s.master.resize(PtySize { rows: 30, cols: 100, pixel_width: 0, pixel_height: 0 }).unwrap();
    thread::sleep(Duration::from_millis(200));
    s.send(b"\r");
    s.expect("30 100");
    assert_eq!(s.wait().exit_code(), 0);
}

#[test]
fn forwards_sigterm_to_program() {
    let mut s = Session::start(&["T", "--", "sleep", "30"]);
    thread::sleep(Duration::from_millis(300));
    let pid = s.child.process_id().unwrap() as libc::pid_t;
    unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(s.wait().exit_code(), 128 + libc::SIGTERM as u32);
}

#[test]
fn restores_terminal_after_program_is_killed() {
    // Das Programm wird hart beendet; danach muss das Test-PTY wieder im Normalmodus sein.
    let mut s = Session::start(&["T", "--", "sh", "-c", "kill -9 $$"]);
    s.wait();
    let termios = s.master.get_termios().expect("termios lesbar");
    assert!(
        termios.local_flags.contains(nix::sys::termios::LocalFlags::ICANON),
        "Terminal ist noch im Raw-Mode"
    );
}

#[test]
fn rejects_invalid_name() {
    let mut s = Session::start(&["../x", "--", "true"]);
    s.expect("Name may only");
    assert_eq!(s.wait().exit_code(), 1);
}

#[test]
fn needs_a_terminal() {
    let out = std::process::Command::new(BIN)
        .args(["T", "--", "true"])
        .env("XDG_RUNTIME_DIR", runtime_dir())
        .env_remove("BRIDGET_LANG")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("requires a terminal"));
}

#[test]
fn without_terminal_the_program_never_starts() {
    let dir = runtime_dir();
    let marker = dir.join("gestartet");
    let out = std::process::Command::new(BIN)
        .args(["T", "--", "touch"])
        .arg(&marker)
        .env("XDG_RUNTIME_DIR", &dir)
        .env_remove("BRIDGET_LANG")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("terminal"));
    assert!(!marker.exists(), "Programm darf ohne Terminal nicht laufen");
    assert!(!dir.join("bridget").join("T.sock").exists());
}
