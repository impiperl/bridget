//! Vollbild-Hilfe für die interaktive Brücke.
//!
//! Die Darstellung bleibt bewusst statisch und zeilenorientiert: So ist sie auch in
//! kleinen Terminals lesbar und braucht keine zusätzliche TUI-Abhängigkeit.

use unicode_width::UnicodeWidthChar;

pub fn render(name: &str, partner: Option<&str>, rows: u16, cols: u16) -> String {
    let rows = usize::from(rows.max(1));
    let cols = usize::from(cols.max(1));
    let label = partner.map_or_else(|| name.to_string(), |p| format!("{name} ⇄ {p}"));

    let mut lines = vec![
        format!("{}  ·  {label}", tr!("bridget Help", "bridget Hilfe")),
        String::new(),
        tr!("Shortcuts use two keys:", "Shortcuts bestehen aus zwei Tasten:").into(),
        tr!(
            "press and release Ctrl-Space, then press the action key.",
            "erst Ctrl-Space drücken und loslassen, dann die Aktion."
        )
        .into(),
        String::new(),
        tr!("SEND", "SENDEN").into(),
        tr!(
            "[Enter]  Send last response  (recommended)",
            "[Enter]  Letzte Antwort senden  (empfohlen)"
        )
        .into(),
        tr!("[A]      Select and send lines", "[A]      Zeilen auswählen und senden").into(),
        tr!("[B]      Send visible screen", "[B]      Sichtbaren Bildschirm senden").into(),
        tr!("[M]      Send mouse selection (tmux)", "[M]      Mausmarkierung senden (tmux)").into(),
        tr!(
            "[3] Enter  Number first: send to console 3",
            "[3] Enter  Nummer davor: an Konsole 3 senden"
        )
        .into(),
        String::new(),
        tr!("CONNECTION", "VERBINDUNG").into(),
        tr!("[V]      Connect to a console", "[V]      Mit einer Konsole verbinden").into(),
        tr!("[T]      Disconnect", "[T]      Verbindung trennen").into(),
        String::new(),
        tr!("DESK", "DESK").into(),
        tr!(
            "[n] Tab  Show terminal n (without n: next)",
            "[n] Tab  Terminal n zeigen (ohne n: nächstes)"
        )
        .into(),
        tr!("[N]      New terminal tab", "[N]      Neuer Terminal-Tab").into(),
        String::new(),
        tr!("MORE", "WEITERES").into(),
        tr!(
            "[Ctrl-V] Insert held text as one line",
            "[Ctrl-V] Zurückgehaltenen Text einzeilig einfügen"
        )
        .into(),
        tr!("[H] [?]  Open this help", "[H] [?]  Diese Hilfe öffnen").into(),
        String::new(),
        tr!(
            "Legacy keys L/P/S/C/D still work.",
            "Bisherige Tasten L/P/S/C/D funktionieren weiterhin."
        )
        .into(),
        tr!("Esc, Enter, H, ? or Q closes help.", "Esc, Enter, H, ? oder Q schließt die Hilfe.")
            .into(),
    ];

    // In kleinen Terminals zuerst Leerzeilen und den Kompatibilitätshinweis opfern.
    if rows < lines.len() + 4 {
        lines.retain(|line| !line.is_empty());
    }
    if rows < lines.len() + 4 {
        lines.retain(|line| {
            !line.starts_with("Legacy keys") && !line.starts_with("Bisherige Tasten")
        });
    }

    let content_width = lines.iter().map(|line| width(line)).max().unwrap_or(1);
    let box_width = (content_width + 4).min(cols.saturating_sub(2).max(1));
    let inner_width = box_width.saturating_sub(4);
    let visible = lines.len().min(rows.saturating_sub(2));
    let box_height = visible + 2;
    let top = (rows.saturating_sub(box_height)) / 2 + 1;
    let left = (cols.saturating_sub(box_width)) / 2 + 1;

    let mut out = String::from("\x1b[?25l\x1b[0m\x1b[H\x1b[2J");
    if box_width < 4 || rows < 3 {
        out.push_str(&format!("\x1b[1;1H\x1b[1;36m{}\x1b[0m", fit(&lines[0], cols)));
        return out;
    }

    out.push_str(&format!("\x1b[{top};{left}H\x1b[36m┌{}┐\x1b[0m", "─".repeat(box_width - 2)));
    for (i, line) in lines.into_iter().take(visible).enumerate() {
        let style = if i == 0 {
            "\x1b[1;36m"
        } else if matches!(
            line.as_str(),
            "SEND" | "CONNECTION" | "DESK" | "MORE" | "SENDEN" | "VERBINDUNG" | "WEITERES"
        ) {
            "\x1b[1;33m"
        } else {
            "\x1b[0m"
        };
        out.push_str(&format!(
            "\x1b[{};{}H\x1b[36m│\x1b[0m {style}{}\x1b[0m \x1b[36m│\x1b[0m",
            top + i + 1,
            left,
            fit(&line, inner_width)
        ));
    }
    out.push_str(&format!(
        "\x1b[{};{}H\x1b[36m└{}┘\x1b[0m",
        top + visible + 1,
        left,
        "─".repeat(box_width - 2)
    ));
    out
}

fn width(line: &str) -> usize {
    line.chars().map(|c| c.width().unwrap_or(0)).sum()
}

fn fit(line: &str, cols: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in line.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > cols {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push_str(&" ".repeat(cols - used));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_primary_actions_and_connection() {
        let screen = render("A", Some("B"), 24, 80);
        assert!(screen.contains("bridget Help  ·  A ⇄ B"));
        assert!(screen.contains("[Enter]  Send last response"));
        assert!(screen.contains("[A]      Select and send lines"));
        assert!(screen.contains("Ctrl-Space"));
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        let screen = render("A", None, 2, 3);
        assert!(screen.contains("bri"));
    }
}
