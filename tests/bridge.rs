//! Integrationstests für die Brücke: zwei Konsolen im selben Runtime-Ordner.

mod common;

use std::time::Duration;

use common::*;

/// Programm, das Bracketed Paste einschaltet und alles sichtbar ausgibt (`^[` = ESC).
const PASTE_AWARE: &str = r#"printf '\033[?2004h'; stty -echo; exec cat -v"#;
/// Dasselbe ohne Bracketed Paste, wie bash 4.4.
const PLAIN: &str = "stty -echo; exec cat -v";

fn shell(dir: &std::path::Path, name: &str) -> Session {
    let s = Session::start_in(dir, &[name, "--", "bash", "--norc", "--noprofile", "-i"]);
    wait_for_socket(dir, name);
    s
}

/// Befehl tippen, kurz warten (Echo), dann Enter, wie ein Mensch.
fn type_command(s: &mut Session, cmd: &str) {
    s.send(cmd.as_bytes());
    std::thread::sleep(Duration::from_millis(150));
    s.send(b"\r");
}

fn prog(dir: &std::path::Path, name: &str, script: &str) -> Session {
    let s = Session::start_in(dir, &[name, "--", "sh", "-c", script]);
    wait_for_socket(dir, name);
    s
}

#[test]
fn last_output_arrives_as_bracketed_paste_without_enter() {
    let dir = runtime_dir();
    let mut b = prog(&dir, "B", PASTE_AWARE);
    let mut a = shell(&dir, "A");

    type_command(&mut a, "printf 'eins\\nzwei\\n'");
    a.expect("zwei\r\n");
    a.send(b"\x1dc");
    a.expect("connected to B");
    // Neuer, leicht erreichbarer Standard: Ctrl-Space loslassen, dann Enter.
    a.send(b"\0\r");
    a.expect("2 line(s) to B");
    b.expect("^[[200~eins");
    a.expect("B: inserted");

    // Kein Enter: Die letzte Zeile bleibt im Eingabepuffer, bis B selbst Enter drückt.
    b.expect_absent("zwei^[[201~", Duration::from_millis(500));
    b.send(b"\r");
    b.expect("zwei^[[201~");
}

/// `Ctrl-Space 3 Enter`: einmal an Konsole 3, die feste Verbindung 1⇄2 bleibt.
#[test]
fn number_before_key_sends_once_to_that_console() {
    let dir = runtime_dir();
    let mut two = prog(&dir, "2", PASTE_AWARE);
    let mut three = prog(&dir, "3", PASTE_AWARE);
    let mut one = shell(&dir, "1");
    assert!(ctl(&dir, &["link", "1", "2"]).status.success());
    one.expect("connected to 2");

    type_command(&mut one, "printf 'nur\\nfuer drei\\n'");
    one.expect("fuer drei\r\n");
    one.send(b"\x003\r");
    one.expect("2 line(s) to 3: inserted");
    three.expect("^[[200~nur");
    two.expect_absent("nur", Duration::from_millis(300));
    let ls = String::from_utf8_lossy(&ctl(&dir, &["ls"]).stdout).into_owned();
    assert!(ls.contains("⇄ 2"), "Verbindung 1⇄2 muss bleiben: {ls}");

    // Ohne Nummer weiter an den Partner; eine unbekannte Nummer meldet einen Fehler.
    one.send(b"\x00\r");
    one.expect("2 line(s) to 2");
    two.expect("^[[200~nur");
    one.send(b"\x007\r");
    one.expect("\u{26a0}");
}

#[test]
fn multiline_without_bracketed_paste_is_held_back() {
    let dir = runtime_dir();
    let mut b = prog(&dir, "B", PLAIN);
    let mut a = shell(&dir, "A");

    type_command(&mut a, "printf 'eins\\nzwei\\n'");
    a.expect("zwei\r\n");
    a.send(b"\x1dc");
    a.expect("connected to B");
    a.send(b"\x1dl");
    b.expect("NOT inserted");
    a.expect("waiting for Ctrl-Space Ctrl-v");
    b.expect_absent("eins", Duration::from_millis(300));

    b.send(b"\x1d\x16"); // Ctrl-] Ctrl-v: als eine Zeile
    b.expect("inserted as one line");
    b.send(b"\r");
    b.expect("eins zwei\r\n");
}

#[test]
fn control_cli_lists_links_and_sends() {
    let dir = runtime_dir();
    let mut b = prog(&dir, "B", PASTE_AWARE);
    let mut a = shell(&dir, "A");
    type_command(&mut a, "echo hallo");
    a.expect("hallo\r\n");

    let out = ctl(&dir, &["link", "A", "B"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let ls = String::from_utf8(ctl(&dir, &["ls"]).stdout).unwrap();
    assert!(ls.contains("A") && ls.contains("⇄ B") && ls.contains("bash"), "{ls}");
    assert!(ls.contains("⇄ A"), "{ls}");

    let out = ctl(&dir, &["send", "A", "--last"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("to B"));
    b.expect("1 line(s) from A inserted");
    b.send(b"\r");
    b.expect("^[[200~hallo^[[201~");

    // Dritte Konsole kommt nicht an B heran, solange B mit A verbunden ist.
    let _c = prog(&dir, "C", PLAIN);
    let out = ctl(&dir, &["link", "C", "B"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already connected to A"));

    // Nach unlink geht es.
    let out = ctl(&dir, &["unlink", "B"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("disconnected from A"));
    a.expect("Connection to B: disconnected");
    let out = ctl(&dir, &["link", "C", "B"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn disconnect_is_seen_on_both_sides() {
    let dir = runtime_dir();
    let mut b = prog(&dir, "B", PASTE_AWARE);
    let mut a = shell(&dir, "A");
    a.send(b"\x1dc");
    a.expect("connected to B");
    b.expect("connected to A");
    a.send(b"\x1dd");
    a.expect("disconnected from B");
    b.expect("Connection to A: disconnected");
    a.send(b"\x1dl");
    a.expect("not connected");
}

#[test]
fn nothing_to_connect_to() {
    let dir = runtime_dir();
    let mut a = shell(&dir, "A");
    a.send(b"\x1dc");
    a.expect("No other bridget console");
}

#[test]
fn busy_name_is_rejected() {
    let dir = runtime_dir();
    let _a = prog(&dir, "A", PLAIN);
    let mut a2 = Session::start_in(&dir, &["A", "--", "true"]);
    a2.expect("already used by a running console");
    assert_eq!(a2.wait().exit_code(), 1);
}

#[test]
fn open_runtime_dir_is_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let dir = runtime_dir();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut a = Session::start_in(&dir, &["A", "--", "true"]);
    a.expect("overly broad permissions");
    assert_eq!(a.wait().exit_code(), 1);
}

#[test]
fn picker_sends_marked_lines_from_history() {
    let dir = runtime_dir();
    let mut b = prog(&dir, "B", PASTE_AWARE);
    let mut a = shell(&dir, "A");
    type_command(&mut a, "seq 1 200");
    // Erst den neuen Prompt abwarten, sonst startet die Auswahl auf „200" statt auf dem Prompt.
    a.expect_after("\r\n199\r\n200\r\n", "$ ");
    a.send(b"\x1dc");
    a.expect("connected to B");

    a.send(b"\x1dp");
    a.expect("bridget Selection");
    // Start auf dem Prompt; 50 Zeilen hoch (in den Verlauf), markieren, 2 weiter hoch.
    a.send(&b"\x1b[A".repeat(50));
    a.send(b" ");
    a.send(b"\x1b[A\x1b[A");
    a.expect("Selection: 3 lines");
    a.send(b"\r");
    a.expect("\x1b[?1049l"); // eigener Alternate Screen wieder verlassen
    a.expect("3 line(s) to B");
    b.expect("3 line(s) from A inserted");
    b.send(b"\r");
    b.expect("^[[200~149\r\n150\r\n151^[[201~");

    // Die Shell funktioniert danach normal weiter.
    type_command(&mut a, "echo weiter");
    a.expect("weiter\r\n");
}

#[test]
fn picker_redraws_alternate_screen_program() {
    let dir = runtime_dir();
    let _b = prog(&dir, "B", PASTE_AWARE);
    let mut a =
        prog(&dir, "A", r#"printf '\033[?1049h\033[HVollbild-Inhalt'; stty -echo; exec cat"#);
    a.expect("Vollbild-Inhalt");
    a.send(b"\x1dc");
    a.expect("connected to B");
    let before = a.seen.len();
    a.send(b"\x1dp");
    a.expect("bridget Selection");
    a.send(b"\x1b"); // Abbrechen
    a.expect("Selection canceled");
    let after = String::from_utf8_lossy(&a.seen[before..]).to_string();
    let redraw = after.rfind("Vollbild-Inhalt").expect("Bildschirm neu gezeichnet");
    assert!(redraw > after.find("bridget Selection").unwrap());
    assert!(!after.contains("\x1b[?1049l"), "Programm ist im Alternate Screen geblieben");
}

#[test]
fn picker_needs_partner() {
    let dir = runtime_dir();
    let mut a = shell(&dir, "A");
    a.send(b"\x1dp");
    a.expect("not connected");
    type_command(&mut a, "echo ok");
    a.expect("ok\r\n");
}

#[test]
fn help_is_a_fullscreen_overlay_and_returns_to_program() {
    let dir = runtime_dir();
    let mut a = shell(&dir, "A");

    a.send(b"\0h");
    a.expect("bridget Help");
    a.expect("[Enter]  Send last response");
    a.expect("[V]      Connect to a console");
    a.send(b"h");
    a.expect("\x1b[?1049l");

    type_command(&mut a, "echo wieder-da");
    a.expect("wieder-da\r\n");
}

#[test]
fn german_can_be_selected_by_flag_or_environment() {
    let dir = runtime_dir();
    let mut a = Session::start_with_env(
        &dir,
        &["--lang", "de", "A", "--", "bash", "--norc", "--noprofile", "-i"],
        &[],
    );
    wait_for_socket(&dir, "A");
    a.send(b"\0h");
    a.expect("bridget Hilfe");
    a.expect("[Enter]  Letzte Antwort senden");

    let help =
        std::process::Command::new(BIN).arg("--help").env("BRIDGET_LANG", "de").output().unwrap();
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("Laufende Konsolen und Verbindungen anzeigen"), "{help}");
}

#[test]
fn other_protocol_version_is_refused() {
    use std::io::{BufRead, BufReader, Write};
    let dir = runtime_dir();
    let mut b = prog(&dir, "B", PASTE_AWARE);
    let mut stream =
        std::os::unix::net::UnixStream::connect(dir.join("bridget").join("B.sock")).unwrap();
    stream.write_all(b"{\"type\":\"hello\",\"v\":99,\"from\":\"Z\"}\n").unwrap();
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).unwrap();
    assert!(reply.contains("\"error\"") && reply.contains("protocol version 99"), "{reply}");
    // B bleibt frei.
    b.expect_absent("connected to Z", Duration::from_millis(300));
}
