//! Feedback belongs in tmux's reserved status area, never in a child program's PTY.
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Feedback {
    note: String,
    until: Option<Instant>,
    transfer: Option<Instant>,
    shown: String,
}

impl Feedback {
    pub fn note(&mut self, note: &str) {
        self.note = note.chars().filter(|c| !c.is_control()).collect();
        self.until = Some(Instant::now() + Duration::from_secs(4));
    }

    pub fn transfer(&mut self) {
        if std::env::var("BRIDGET_ANIMATION").as_deref() != Ok("0") {
            self.transfer = Some(Instant::now());
        }
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let frame = self.transfer.and_then(|start| {
            let step = now.duration_since(start).as_millis() / 180;
            ["●────→", "─●───→", "──●──→", "───●─→", "────●→"].get(step as usize).copied()
        });
        if frame.is_none() {
            self.transfer = None;
        }
        let note = if self.until.is_some_and(|until| now < until) { &self.note } else { "" };
        let text = match frame {
            Some(frame) => format!("{frame} {note}"),
            None => note.to_string(),
        };
        if text == self.shown {
            return;
        }
        self.shown = text.clone();
        if let Ok(pane) = std::env::var("TMUX_PANE") {
            // Literal user text must not become tmux formatting commands.
            let text = text.replace('#', "##");
            let key = format!("@bridget_feedback_{}", pane.trim_start_matches('%'));
            let _ = std::process::Command::new("tmux")
                .args(["set-option", "-g", &key, &text])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    pub fn clear(&self) {
        if let Ok(pane) = std::env::var("TMUX_PANE") {
            let key = format!("@bridget_feedback_{}", pane.trim_start_matches('%'));
            let _ = std::process::Command::new("tmux")
                .args(["set-option", "-gu", &key])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

/// tmux 2.7 has no pane options. Global user options keyed by pane ID provide the
/// same isolation; the status format selects only the currently focused pane.
pub fn status_left(mode: &str, panes: &[&str]) -> String {
    let mut format = format!("#[fg=colour81,bold] bridget {mode} #[default] ");
    for pane in panes {
        let id = pane.trim_start_matches('%');
        format.push_str(&format!("#{{?#{{==:#{{pane_id}},{pane}}},#{{@bridget_feedback_{id}}},}}"));
    }
    format
}
