//! Runtime-Ordner und Sockets.
//!
//! `$XDG_RUNTIME_DIR` muss existieren, dem eigenen User gehören und keine Rechte für
//! Gruppe/andere haben. Es gibt keinen Rückfall nach `/tmp`. Darunter liegt `bridget/`
//! (0700) mit einem Socket pro Konsole (0600).

use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::name;

pub fn uid() -> u32 {
    // SAFETY: getuid kann nicht fehlschlagen.
    unsafe { libc::getuid() }
}

/// Prüft, dass ein Ordner uns gehört und weder Gruppe noch andere Rechte haben.
fn check_private_dir(path: &Path, what: &str) -> Result<()> {
    let meta = fs::symlink_metadata(path).with_context(|| {
        if crate::locale::german() {
            format!("{what} {path:?} fehlt")
        } else {
            format!("{what} {path:?} is missing")
        }
    })?;
    if !meta.is_dir() {
        bail!(
            "{}",
            if crate::locale::german() {
                format!("{what} {path:?} ist kein Ordner")
            } else {
                format!("{what} {path:?} is not a directory")
            }
        );
    }
    if meta.uid() != uid() {
        bail!(
            "{}",
            if crate::locale::german() {
                format!("{what} {path:?} gehört nicht dem aktuellen user")
            } else {
                format!("{what} {path:?} is not owned by the current user")
            }
        );
    }
    if meta.mode() & 0o077 != 0 {
        bail!(
            "{}",
            if crate::locale::german() {
                format!(
                    "{what} {path:?} hat zu offene Rechte ({:o}), erwartet 0700",
                    meta.mode() & 0o777
                )
            } else {
                format!(
                    "{what} {path:?} has overly broad permissions ({:o}); expected 0700",
                    meta.mode() & 0o777
                )
            }
        );
    }
    Ok(())
}

/// Der eigene Unterordner `…/bridget`, wird bei Bedarf mit 0700 angelegt.
pub fn dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).context(tr!(
        "XDG_RUNTIME_DIR is not set; bridget requires a secure runtime directory",
        "XDG_RUNTIME_DIR ist nicht gesetzt, bridget braucht einen sicheren Runtime-Ordner"
    ))?;
    let base = PathBuf::from(base);
    check_private_dir(&base, "XDG_RUNTIME_DIR")?;

    let dir = base.join("bridget");
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            return Err(e).with_context(|| {
                if crate::locale::german() {
                    format!("{dir:?} anlegen")
                } else {
                    format!("create {dir:?}")
                }
            });
        }
    }
    check_private_dir(&dir, tr!("runtime directory", "Runtime-Ordner"))?;
    Ok(dir)
}

pub fn socket_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.sock"))
}

/// Bindet den Socket für `name`. Ist der Name belegt (jemand antwortet), ist das ein
/// Fehler; eine verwaiste Socket-Datei wird entfernt.
pub fn bind(dir: &Path, name: &str) -> Result<UnixListener> {
    name::validate(name)?;
    let path = socket_path(dir, name);
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            bail!(
                "{}",
                if crate::locale::german() {
                    format!("Der Name {name:?} wird schon von einer laufenden Konsole benutzt")
                } else {
                    format!("Name {name:?} is already used by a running console")
                }
            );
        }
        fs::remove_file(&path).with_context(|| {
            if crate::locale::german() {
                format!("verwaisten Socket {path:?} entfernen")
            } else {
                format!("remove stale socket {path:?}")
            }
        })?;
    }
    let listener = UnixListener::bind(&path).with_context(|| {
        if crate::locale::german() { format!("{path:?} binden") } else { format!("bind {path:?}") }
    })?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Verbindet mit der Konsole `name` und prüft, dass am anderen Ende derselbe User sitzt.
pub fn connect(dir: &Path, name: &str) -> Result<UnixStream> {
    name::validate(name)?;
    let path = socket_path(dir, name);
    let stream = UnixStream::connect(&path).with_context(|| {
        if crate::locale::german() {
            format!("Konsole {name:?} nicht erreichbar")
        } else {
            format!("console {name:?} is not reachable")
        }
    })?;
    check_peer(&stream)?;
    Ok(stream)
}

/// Namen aller Konsolen mit Socket-Datei (ob sie noch laufen, prüft erst `connect`).
pub fn list(dir: &Path) -> Result<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let file = e.file_name().into_string().ok()?;
            let name = file.strip_suffix(".sock")?.to_string();
            name::validate(&name).ok()?;
            Some(name)
        })
        .collect();
    names.sort();
    Ok(names)
}

/// Läuft unter diesem Namen gerade eine Konsole?
pub fn is_live(dir: &Path, name: &str) -> bool {
    connect(dir, name).is_ok()
}

/// Wie viele andere Konsolen laufen gerade? (für die Farbe nach Reihenfolge)
pub fn live_others(dir: &Path, me: &str) -> usize {
    list(dir).unwrap_or_default().iter().filter(|n| *n != me && is_live(dir, n)).count()
}

/// SO_PEERCRED: Nur Prozesse desselben Users dürfen sprechen.
pub fn check_peer(stream: &UnixStream) -> Result<()> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred und len sind gültige, passend große Puffer für SO_PEERCRED.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error())
            .context(tr!("read SO_PEERCRED", "SO_PEERCRED lesen"));
    }
    if cred.uid != uid() {
        bail!(
            "{}",
            if crate::locale::german() {
                format!("Gegenstelle gehört einem anderen User (uid {})", cred.uid)
            } else {
                format!("peer belongs to another user (uid {})", cred.uid)
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(mode: u32) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "bridget-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir(&d).unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(mode)).unwrap();
        d
    }

    #[test]
    fn rejects_open_dir() {
        let d = tempdir(0o755);
        assert!(check_private_dir(&d, "x").is_err());
        fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(check_private_dir(&d, "x").is_ok());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn bind_detects_busy_and_stale_sockets() {
        let d = tempdir(0o700);
        let l = bind(&d, "A").unwrap();
        let mode = fs::metadata(socket_path(&d, "A")).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(bind(&d, "A").is_err(), "belegter Name muss abgelehnt werden");
        drop(l); // Datei bleibt liegen, aber niemand hört zu
        assert!(bind(&d, "A").is_ok(), "verwaister Socket muss ersetzt werden");
        assert_eq!(list(&d).unwrap(), vec!["A".to_string()]);
        assert!(bind(&d, "../A").is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn peer_check_accepts_own_user() {
        let (a, _b) = UnixStream::pair().unwrap();
        check_peer(&a).unwrap();
    }
}
