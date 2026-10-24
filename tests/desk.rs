//! Integrationstest für `bridget desk`: tmux auf eigenem Test-Server, Konsolen 1 und 2 oben,
//! Terminal-Tabs unten, Wechseln, Mausmarkierung senden, neuer Tab, Aufräumen beim Ende.

mod common;

use std::time::{Duration, Instant};

use common::*;

struct Tmux(String);

impl Tmux {
    fn run(&self, args: &[&str]) -> String {
        let out =
            std::process::Command::new("tmux").arg("-L").arg(&self.0).args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    fn env(&self, key: &str) -> String {
        let line = self.run(&["show-environment", "-t", "bridget-desk", key]);
        line.split_once('=').map(|(_, v)| v.to_string()).unwrap_or_default()
    }

    /// Pane im unteren Bereich (im Hauptfenster, aber nicht 1 oder 2).
    fn bottom(&self) -> String {
        let top = self.env("BRIDGET_DESK_TOP");
        self.run(&["list-panes", "-t", &self.env("BRIDGET_DESK_MAIN"), "-F", "#{pane_id}"])
            .lines()
            .find(|p| !top.split_whitespace().any(|t| t == *p))
            .unwrap_or_default()
            .to_string()
    }

    fn keys(&self, pane: &str, keys: &[&str]) {
        let mut args = vec!["send-keys", "-t", pane];
        args.extend(keys);
        self.run(&args);
    }
}

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn desk_tabs_marking_and_cleanup() {
    if std::process::Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux fehlt, Test übersprungen");
        return;
    }
    let dir = runtime_dir();
    let t = Tmux(format!("bridget-desk-test-{}", std::process::id()));
    let mut s = Session::start_with_env(
        &dir,
        &["desk", "--here", "--tabs", "2", "sh -c 'exec cat'", "sh -c 'exec cat'"],
        &[("BRIDGET_TMUX_SERVER", t.0.clone()), ("SHELL", "/bin/sh".into())],
    );
    s.expect("\x1b]11;?");
    s.send(b"\x1b]11;rgb:0000/0000/0000\x1b\\\x1b[?62;22c");

    // 1 und 2 verbunden, Tabs 3 und 4 laufen.
    wait_until("Konsolen 1–4 und Verbindung 1⇄2", || {
        let ls = String::from_utf8_lossy(&ctl(&dir, &["ls"]).stdout).into_owned();
        ls.contains("⇄ 1") && ["3 ", "4 "].iter().all(|n| ls.lines().any(|l| l.starts_with(n)))
    });
    let p1 = t.env("BRIDGET_DESK_TAB_1");
    let p3 = t.env("BRIDGET_DESK_TAB_3");
    let p4 = t.env("BRIDGET_DESK_TAB_4");
    assert_eq!(t.run(&["list-panes", "-t", &t.env("BRIDGET_DESK_MAIN")]).lines().count(), 3);
    assert_eq!(t.bottom(), p3, "Tab 3 ist zuerst unten");
    let status = t.run(&["show-options", "-t", "bridget-desk", "status-right"]);
    assert!(status.contains("Ctrl-Space [n] Enter"), "{status}");

    // Ctrl-] 4 Tab in Konsole 1: Tab 4 kommt nach unten.
    t.keys(&p1, &["C-]", "4", "Tab"]);
    wait_until("Tab 4 unten", || t.bottom() == p4);

    // Mausmarkierung (tmux-Puffer) aus Konsole 1 an den versteckten Tab 3.
    t.run(&["set-buffer", "hallo markiert"]);
    t.keys(&p1, &["C-]", "3", "m"]);
    wait_until("Markierung in Tab 3", || {
        t.run(&["capture-pane", "-p", "-t", &p3]).contains("hallo markiert")
    });
    assert!(t.run(&["list-buffers"]).is_empty(), "Markierung nach dem Senden verworfen");

    // Ctrl-] n: neuer Tab 5, gleich unten.
    t.keys(&p1, &["C-]", "n"]);
    wait_until("Tab 5 angelegt", || !t.env("BRIDGET_DESK_TAB_5").is_empty());
    let p5 = t.env("BRIDGET_DESK_TAB_5");
    wait_until("Tab 5 unten", || t.bottom() == p5);

    // Tab 5 endet (cat bekommt EOF): Der Hook holt Tab 3 nach unten.
    t.keys(&p5, &["C-d"]);
    wait_until("Tab 3 nach Ende von 5 wieder unten", || t.bottom() == p3);
    assert!(t.env("BRIDGET_DESK_TAB_5").is_empty(), "Eintrag für Tab 5 entfernt");
    assert_eq!(t.run(&["list-panes", "-t", &t.env("BRIDGET_DESK_MAIN")]).lines().count(), 3);

    // Fenster zu (Client weg) → Sitzung endet.
    s.child.kill().unwrap();
    wait_until("Sitzung nach dem Schließen beendet", || t.run(&["ls"]).is_empty());
    let _ = t.run(&["kill-server"]);
}
