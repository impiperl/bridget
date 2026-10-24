//! Gemeinsame Hilfen der Integrationstests: bridget in einem Test-PTY starten.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

pub const BIN: &str = env!("CARGO_BIN_EXE_bridget");

pub struct Session {
    pub master: Box<dyn MasterPty + Send>,
    pub writer: Box<dyn Write + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub output: Receiver<Vec<u8>>,
    pub seen: Vec<u8>,
}

/// Eigener Runtime-Ordner (0700) je Test, damit parallele Tests sich nicht stören.
pub fn runtime_dir() -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!(
        "bridget-it-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir(&d).unwrap();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    d
}

impl Session {
    pub fn start(args: &[&str]) -> Session {
        Self::start_in(&runtime_dir(), args)
    }

    pub fn start_in(dir: &std::path::Path, args: &[&str]) -> Session {
        Self::start_with_env(dir, args, &[])
    }

    pub fn start_with_env(dir: &std::path::Path, args: &[&str], env: &[(&str, String)]) -> Session {
        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .unwrap();
        let mut cmd = CommandBuilder::new(BIN);
        cmd.args(args);
        cmd.env("TERM", "xterm-256color");
        cmd.env("XDG_RUNTIME_DIR", dir);
        // Test-PTYs beantworten keine Farbabfrage; Tests mit Farbe setzen das selbst.
        cmd.env("BRIDGET_COLOR", "aus");
        cmd.env_remove("BRIDGET_LANG");
        cmd.env_remove("TMUX");
        // Nie ins echte Gedächtnis (Dumps, duo-Einleitung); Tests mit Gedächtnis setzen es selbst.
        cmd.env("BRIDGET_MEMORY", dir.join("kein-gedaechtnis"));
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let writer = pair.master.take_writer().unwrap();
        Session { master: pair.master, writer, child, output: rx, seen: Vec::new() }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
    }

    /// Wartet, bis `needle` in der bisherigen Ausgabe auftaucht.
    pub fn expect(&mut self, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if contains(&self.seen, needle.as_bytes()) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.output.recv_timeout(left) {
                Ok(chunk) => self.seen.extend(chunk),
                Err(_) => panic!(
                    "{needle:?} nicht gesehen. Ausgabe bisher:\n{}",
                    String::from_utf8_lossy(&self.seen)
                ),
            }
        }
    }

    pub fn wait(&mut self) -> portable_pty::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            // Ausgabe weiter abholen, damit nichts blockiert.
            while let Ok(chunk) = self.output.try_recv() {
                self.seen.extend(chunk);
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "bridget endet nicht");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

impl Session {
    /// Wartet auf `anchor` und danach auf `needle` hinter dessen erstem Vorkommen.
    pub fn expect_after(&mut self, anchor: &str, needle: &str) {
        self.expect(anchor);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let at = self.seen.windows(anchor.len()).position(|w| w == anchor.as_bytes()).unwrap();
            if contains(&self.seen[at + anchor.len()..], needle.as_bytes()) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.output.recv_timeout(left) {
                Ok(chunk) => self.seen.extend(chunk),
                Err(_) => panic!(
                    "{needle:?} nach {anchor:?} nicht gesehen. Ausgabe bisher:\n{}",
                    String::from_utf8_lossy(&self.seen)
                ),
            }
        }
    }

    /// Prüft, dass `needle` während `dur` nicht auftaucht.
    pub fn expect_absent(&mut self, needle: &str, dur: Duration) {
        let deadline = Instant::now() + dur;
        while Instant::now() < deadline {
            if let Ok(chunk) = self.output.recv_timeout(Duration::from_millis(20)) {
                self.seen.extend(chunk);
            }
            assert!(
                !contains(&self.seen, needle.as_bytes()),
                "{needle:?} hätte nicht kommen dürfen. Ausgabe:\n{}",
                String::from_utf8_lossy(&self.seen)
            );
        }
    }
}

/// Steuer-CLI im gleichen Runtime-Ordner aufrufen.
pub fn ctl(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(BIN)
        .args(args)
        .env("XDG_RUNTIME_DIR", dir)
        .env_remove("BRIDGET_LANG")
        .output()
        .unwrap()
}

pub fn wait_for_socket(dir: &std::path::Path, name: &str) {
    let path = dir.join("bridget").join(format!("{name}.sock"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(Instant::now() < deadline, "Socket {path:?} fehlt");
        thread::sleep(Duration::from_millis(20));
    }
    thread::sleep(Duration::from_millis(200)); // Programm darin soll auch laufen
}
