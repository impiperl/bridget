//! `bridget duo join ID`: zeigt Seite B in einer zweiten Konsole.

use std::io::BufReader;
use std::sync::mpsc;
use std::thread;

use anyhow::{Result, bail};

use super::ui::{Console, Keys, LineInput, LineResult, Show};
use super::{FromView, ToView, after_takeover_help, choice_from_key, duo_dir, run_interactive};
use crate::terminal::RawGuard;
use crate::{name, proto, runtime};

enum Ev {
    Keys(Vec<u8>),
    Msg(ToView),
    Gone,
}

enum Mode {
    Normal,
    Inject(LineInput),
    AfterTakeover,
}

pub fn join(short: &str) -> Result<i32> {
    name::validate(short)?;
    let dir = duo_dir()?;
    let stream = runtime::connect(&dir, &format!("duo-{short}")).map_err(|_| {
        anyhow::anyhow!(if crate::locale::german() {
            format!("Kein laufendes duo-Gespräch mit der ID {short:?}")
        } else {
            format!("No running duo conversation with ID {short:?}")
        })
    })?;
    let mut writer = stream.try_clone()?;
    proto::write_json(&mut writer, &FromView::Join { conv: short.to_string() })?;
    let mut reader = BufReader::new(stream);
    let side_name = match proto::read_json::<ToView>(&mut reader)? {
        Some(ToView::Welcome { side_name, .. }) => side_name,
        Some(ToView::Bye { reason }) => bail!("{reason}"),
        other => bail!(
            "{}",
            if crate::locale::german() {
                format!("unerwartete Antwort: {other:?}")
            } else {
                format!("unexpected response: {other:?}")
            }
        ),
    };

    let _guard = RawGuard::enable()?;
    let _marker = crate::color::Marker::apply(crate::color::for_duo(1)?);
    let (tx, rx) = mpsc::channel();
    let keys = {
        let (ktx, krx) = mpsc::channel::<Vec<u8>>();
        let tx2 = tx.clone();
        thread::spawn(move || {
            for k in krx {
                if tx2.send(Ev::Keys(k)).is_err() {
                    return;
                }
            }
        });
        Keys::start(ktx)
    };
    {
        let tx = tx.clone();
        thread::spawn(move || {
            while let Ok(Some(msg)) = proto::read_json::<ToView>(&mut reader) {
                if tx.send(Ev::Msg(msg)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Ev::Gone);
        });
    }

    let mut console = Console::new();
    console.show(&Show::Note(if crate::locale::german() {
        format!("bridget duo {short} · diese Konsole zeigt {side_name} · Space Pause · i Nachricht · u übernehmen · q Ende")
    } else {
        format!("bridget duo {short} · this console shows {side_name} · Space Pause · i Message · u Take over · q End")
    }));
    let mut mode = Mode::Normal;
    let send =
        |w: &mut std::os::unix::net::UnixStream, m: FromView| proto::write_json(w, &m).is_ok();

    for ev in rx {
        match ev {
            Ev::Gone => {
                console.show(&Show::Note(
                    tr!("Connection to conversation ended", "Verbindung zum Gespräch beendet")
                        .into(),
                ));
                break;
            }
            Ev::Msg(ToView::Show { item }) => console.show(&item),
            Ev::Msg(ToView::Bye { reason }) => {
                console.show(&Show::Note(reason));
                break;
            }
            Ev::Msg(ToView::TakeoverGranted { command, cwd }) => {
                keys.suspend();
                let result = run_interactive(&command, &cwd);
                keys.resume();
                if let Err(e) = result {
                    console.show(&Show::Note(format!("⚠ {e:#}")));
                }
                mode = Mode::AfterTakeover;
                console.show(&Show::Note(after_takeover_help().into()));
            }
            Ev::Msg(ToView::Welcome { .. }) => {}
            Ev::Keys(bytes) => {
                mode = match mode {
                    Mode::Inject(mut input) => match input.feed(&bytes) {
                        LineResult::More => Mode::Inject(input),
                        LineResult::Done(text) => {
                            send(&mut writer, FromView::Inject { text });
                            Mode::Normal
                        }
                        LineResult::Cancel => Mode::Normal,
                    },
                    Mode::AfterTakeover => match bytes.first().copied().and_then(choice_from_key) {
                        Some(choice) => {
                            send(&mut writer, FromView::TakeoverDone { choice });
                            Mode::Normal
                        }
                        None => {
                            console.show(&Show::Note(after_takeover_help().into()));
                            Mode::AfterTakeover
                        }
                    },
                    Mode::Normal => {
                        let mut next = Mode::Normal;
                        for &b in &bytes {
                            match b {
                                b'i' => {
                                    console.show(&Show::Note(
                                        tr!(
                                            "Message (Enter sends, Esc cancels; only while paused):",
                                            "Nachricht (Enter senden, Esc abbrechen; nur in einer Pause):"
                                        ).into(),
                                    ));
                                    next = Mode::Inject(LineInput::new());
                                    break;
                                }
                                b'u' => {
                                    send(&mut writer, FromView::Takeover);
                                }
                                b' ' | b'q' | 0x03 => {
                                    send(&mut writer, FromView::Key { key: b });
                                }
                                _ => {}
                            }
                        }
                        next
                    }
                };
            }
        }
    }
    Ok(0)
}
