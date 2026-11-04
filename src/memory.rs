//! Gemeinsames Gedächtnis für claude und codex (angelegt von `install.sh`).
//!
//! Ort: `$BRIDGET_MEMORY`, sonst `$XDG_DATA_HOME/bridget/memory`, sonst
//! `~/.local/share/bridget/memory`. Gibt es den Ordner nicht, ist das Gedächtnis aus.
//!
//! `duo` startet die CLIs headless ohne User-Config, sie sehen den Verweis in
//! `CLAUDE.md`/`AGENTS.md` dort nicht. Deshalb kommen Index und Regeln direkt in die
//! Einleitung, der Rest wird bei Bedarf gelesen (claude per `--add-dir`).

use std::path::{Path, PathBuf};

/// Höchstgröße für Index und Regeln zusammen in der Einleitung.
const MAX_INLINE: usize = 32 * 1024;

pub fn dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let dir = env("BRIDGET_MEMORY")
        .or_else(|| env("XDG_DATA_HOME").map(|d| d.join("bridget/memory")))
        .or_else(|| env("HOME").map(|h| h.join(".local/share/bridget/memory")))?;
    dir.is_dir().then_some(dir)
}

/// Einleitungsteil: wo das Gedächtnis liegt, dazu `INDEX.md` und alle `regeln/*.md`.
/// `writable`: ob die Seite dort schreiben darf (duo, Standard).
pub fn preface(dir: &Path, writable: bool) -> String {
    let mut files = vec![dir.join("INDEX.md")];
    let mut rules: Vec<PathBuf> = std::fs::read_dir(dir.join("regeln"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    rules.sort();
    files.extend(rules);

    let d = dir.display();
    let de = crate::locale::german();
    let mut out = if de {
        format!(
            "Gemeinsames Gedächtnis (bridget) unter {d}. Die Regeln darin gelten für dich. Index und Regeln stehen unten; offene Übergaben in {d}/uebergaben/ und Einträge in {d}/wissen/ liest du bei Bedarf selbst."
        )
    } else {
        format!(
            "Shared memory (bridget) at {d}. Its rules apply to you. Index and rules follow below; read open handoffs in {d}/uebergaben/ and entries in {d}/wissen/ yourself when needed."
        )
    };
    out.push(' ');
    out.push_str(&match (writable, de) {
        (false, true) => format!("Hier schreibst du nichts hinein; was festgehalten werden soll, schlägst du dem Menschen vor (Format: {d}/GLOSSAR.md)."),
        (false, false) => format!("Do not write there; propose anything worth keeping to the human (format: {d}/GLOSSAR.md)."),
        (true, true) => format!("Du darfst hier schreiben, nach {d}/GLOSSAR.md: Übergaben, wissen/, NOTIZEN.md und INDEX.md; regeln/ nur, wenn der Mensch es ausdrücklich sagt. Lies eine Datei direkt vor dem Ändern neu, andere Sitzungen schreiben parallel. Sag im Gespräch kurz, was du eingetragen hast, die andere Seite sieht es sonst nicht."),
        (true, false) => format!("You may write there, following {d}/GLOSSAR.md: handoffs, wissen/, NOTIZEN.md and INDEX.md; regeln/ only when the human explicitly says so. Re-read a file right before changing it, other sessions write in parallel. Say briefly in the conversation what you recorded, the other side will not see it otherwise."),
    });
    let mut used = 0;
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let rel = f.strip_prefix(dir).unwrap_or(&f).display().to_string();
        if used + text.len() > MAX_INLINE {
            out.push_str(&format!(
                "\n\n{}",
                tr!(
                    "[further files omitted for size, read them from the folder]",
                    "[weitere Dateien aus Platzgründen weggelassen, bitte im Ordner lesen]"
                )
            ));
            break;
        }
        used += text.len();
        out.push_str(&format!("\n\n--- {rel} ---\n{}", text.trim_end()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preface_inlines_index_and_rules_in_order() {
        let tmp = std::env::temp_dir().join(format!("bridget-mem-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("regeln")).unwrap();
        std::fs::write(tmp.join("INDEX.md"), "index\n").unwrap();
        std::fs::write(tmp.join("regeln/b.md"), "regel b\n").unwrap();
        std::fs::write(tmp.join("regeln/a.md"), "regel a\n").unwrap();
        std::fs::write(tmp.join("regeln/x.txt"), "nicht\n").unwrap();
        let p = preface(&tmp, false);
        assert!(p.contains("propose") || p.contains("schlägst"), "{p}");
        let w = preface(&tmp, true);
        assert!(w.contains("regeln/ only") || w.contains("regeln/ nur"), "{w}");
        let (i, a, b) = (
            p.find("--- INDEX.md").unwrap(),
            p.find("regel a").unwrap(),
            p.find("regel b").unwrap(),
        );
        assert!(i < a && a < b, "{p}");
        assert!(!p.contains("nicht"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
