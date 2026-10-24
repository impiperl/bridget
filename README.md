# bridget

Aufsatz für Konsolenprogramme. Text per Hotkey zwischen zwei Konsolen schicken, und zwei
KI-CLIs (`claude`, `codex`) kontrolliert miteinander reden lassen.

Nur Linux: bridget braucht Unix-PTYs und `SO_PEERCRED` für die Socket-Prüfung.

## Bauen

```bash
cargo build --release
cp target/release/bridget ~/.local/bin/
cargo test            # Unit- und Integrationstests (laufen in Test-PTYs)
```

Oder alles auf einmal: `./install.sh` (`--help` für Optionen). Das baut, installiert nach
`~/.local/bin` und legt ein **gemeinsames Gedächtnis** für claude und codex an
(`~/.local/share/bridget/memory`, anders mit `--memory` oder `BRIDGET_MEMORY`). Neue
Sitzungen lesen dort `INDEX.md`, die Regeln in `regeln/` und offene Übergaben; wie Einträge
aussehen, steht in `GLOSSAR.md`. Der Installer trägt einen Verweis in `~/.claude/CLAUDE.md`
und `~/.codex/AGENTS.md` ein und gibt den Ordner in `~/.claude/settings.json` frei
(Sicherung jeweils als `*.bak-bridget`). `--no-agents` lässt beides weg. Vorhandene
Gedächtnis-Dateien werden nie überschrieben.

Beim Beenden einer Konsole oder eines `duo`-Gesprächs, auch wenn das Fenster geschlossen wird,
schreibt bridget einen Dump nach `sitzungen/`: Programm, Ordner, wie es endete, den Befehl
zum Fortsetzen (`claude --resume …`, `codex resume …`) und die letzte Ausgabe (höchstens
16 KiB). Dateien `0600`, nach 14 Tagen gelöscht. `BRIDGET_NO_DUMP=1` schaltet das ab, bei
`duo` auch `--no-memory`.

Dasselbe bei `/compact` (auch automatischem Kompaktieren) und `/clear` in claude und codex
(dort auch `/new`), über Hooks, die `install.sh` einträgt (`~/.claude/settings.json`,
`~/.codex/hooks.json`). Sie rufen `bridget hook claude|codex` auf und gelten für alle
Sitzungen, nicht nur in bridget-Konsolen. codex fragt beim ersten Start danach
„Hooks need review“: freigeben, sonst laufen sie nicht.

## Sprache

Die Bedienoberfläche ist standardmäßig englisch. Deutsch gilt wahlweise für einen
Aufruf oder dauerhaft über die Umgebung:

```bash
bridget --lang de pair claude codex
export BRIDGET_LANG=de
```

`--lang en|de` hat Vorrang vor `BRIDGET_LANG`. Die Sprache gilt auch für Hilfe,
Fenstertitel, Auswahlmodus, `pair`, `duo` und gestartete Unterprozesse von Bridget.
Englische und deutsche Farbnamen werden unabhängig von der Oberflächensprache akzeptiert.

## Schnellstart: `pair`

```bash
bridget pair claude codex      # neues Fenster: links claude (A, blau), rechts codex (B, grün)
bridget pair                   # zwei Shells
bridget pair "claude --resume" bash --names X,Y --here   # in dieser Konsole
```

Läuft über tmux auf einem eigenen Server (`tmux -L bridget`), die eigenen tmux-Sitzungen
bleiben unberührt. Beide Konsolen sind gleich verbunden. Ein Klick wählt den Pane, Shift
plus Ziehen markiert Text für die Zwischenablage. Wird das Fenster geschlossen, enden beide
Programme. Eine Leiste am unteren Rand zeigt die wichtigsten Pair-Shortcuts. Fenster:
`$BRIDGET_TERMINAL`, sonst gnome-terminal, konsole, xfce4-terminal, xterm.

## Arbeitsplatz: `desk`

```bash
bridget desk claude codex          # oben 1 = claude, 2 = codex (verbunden), unten Terminal 3
bridget desk claude codex --tabs 3 # unten Terminal 3, 4, 5
bridget desk --here                # in dieser Konsole
```

Wie `pair`, aber mit einem Terminal-Bereich unten über die ganze Breite. Seine Tabs sind
nummeriert (3, 4, …) und stehen in der Leiste unten, der sichtbare ist hervorgehoben. Jeder
Tab ist eine eigene bridget-Konsole; die versteckten laufen weiter. Endet ein Tab, rückt der
nächste nach. Ist `1` schon belegt (zweiter `desk`), heißen die Konsolen `2-1`, `2-2` …

| Taste nach `Ctrl-Space` | Wirkung |
|---|---|
| `3 Enter` | letzte Ausgabe an Konsole 3 (einmalig, die Verbindung 1⇄2 bleibt) |
| `3 B`, `3 A` | Bildschirm bzw. Auswahl an Konsole 3 |
| `M` / `3 M` | mit der Maus markierten Text an den Partner bzw. an Konsole 3 |
| `4 Tab` | Terminal 4 unten zeigen und hineinwechseln (`1 Tab`/`2 Tab`: nach oben) |
| `Tab` | nächster Terminal-Tab |
| `N` | neuer Terminal-Tab mit `$SHELL` |
| `3 V` | fest mit Konsole 3 verbinden |

Markieren: einfach mit der Maus ziehen (tmux-Markierung), dann `Ctrl-Space M`. Fängt das
Programm im Pane die Maus selbst ab, markiert tmux nichts; Shift plus Ziehen bleibt für die
Zwischenablage des Terminals. Die Nummer vor der Taste geht überall, auch außerhalb von
`desk`: `Ctrl-Space 3 Enter` schickt an die Konsole mit dem Namen `3`.

## Brücke

```bash
bridget A -- claude   # Konsole 1
bridget B             # Konsole 2 ($SHELL)
bridget B --connect A -- codex   # verbindet sich beim Start selbst mit A
```

**Markierung:** Der Hintergrund wird leicht eingefärbt. Die erste Konsole ist blau, die
zweite grün usw., bei `duo` ist Seite A blau und B grün. Eine eigene Farbe gibt
`--color blau|grün|lila|orange|rot|türkis|#rrggbb|aus` (oder `BRIDGET_COLOR`). Beim Ende
wird die alte Farbe zurückgesetzt. Das braucht ein Terminal, das `OSC 11` beantwortet
(gnome-terminal, xterm, konsole, VS Code); sonst bleibt alles wie es ist. Der Fenstertitel
bekommt ein Kennzeichen: `[A]` bzw. verbunden `[A⇄B]`.

Shortcuts sind zweistufig: zuerst `Ctrl-Space` drücken und loslassen, danach die
Aktionstaste. Dadurch geraten sie nicht mit den Programmen in den beiden Panes aneinander.

| Taste nach `Ctrl-Space` | Wirkung |
|---|---|
| `Enter` | letzte Ausgabe senden (Shell: seit dem letzten Enter; claude/codex: letzte Antwort, auch über den Bildschirm hinaus) |
| `A` | Auswahlmodus: Zeilen aus Verlauf markieren (`Space`), mit `Enter` senden |
| `B` | sichtbaren Bildschirm senden |
| `M` | mit der Maus markierten Text senden (nur in tmux) |
| `V` | verbinden (bei mehreren Konsolen: Ziffer wählen) |
| `T` | trennen |
| `Ctrl-V` | zurückgehaltenen mehrzeiligen Text als eine Zeile einfügen |
| `H` oder `?` | Hilfe als eigenes Fenster öffnen |
| `Ctrl-Space` | echtes `Ctrl-Space` ans Programm senden |

Eine Nummer vor der Taste wählt das Ziel: `Ctrl-Space 3 Enter` schickt einmalig an Konsole
`3`, ohne die bestehende Verbindung zu ändern (siehe `desk`).

Das bisherige Präfix `Ctrl-]` und die Aktionstasten `L/P/S/C/D` funktionieren weiterhin.

Gesendeter Text kommt **nie mit Enter** an. Mehrzeiliger Text wird nur per Bracketed Paste
eingefügt. Kann das Zielprogramm das nicht, wird er zurückgehalten. Abhilfe für bash:
`set enable-bracketed-paste on` in `~/.inputrc`; für vim 8.0 `t_BE`/`t_BD`/`t_PS`/`t_PE`
in der `~/.vimrc`.

Von außen: `bridget ls`, `bridget link A B`, `bridget unlink A`,
`bridget send A --last|--screen`, `bridget config [PROG] [--init]`.

## duo

```bash
bridget duo --a claude --b codex --rounds 6 "Prüft gemeinsam docs/plan.md"
bridget duo join <ID>     # zweite Konsole: zeigt Seite B
```

`Space` Pause/weiter · `i` Nachricht einwerfen (in der Pause) · `u` Seite übernehmen
(interaktive Sitzung, in der Pause) · `q` Ende.

Übernommenes codex läuft mit `--no-alt-screen` (inline), damit die Sitzung im Scrollback
des Terminals bleibt und lange Antworten ganz lesbar und kopierbar sind.

Headless wird nur gelesen: claude mit `--restricted --tools Read Grep Glob
--strict-mcp-config`, codex mit Sandbox `read-only` und ohne User-Config/Regeln. codex darf
dabei überall lesen, nicht nur im Arbeitsverzeichnis. Geändert wird nur bei einer Übernahme,
im normalen Dialog der CLI. Mit `--transcript` wird nach `~/.local/state/bridget/`
protokolliert (0600, nach 14 Tagen gelöscht).

## Sicherheit

- Sockets nur in `$XDG_RUNTIME_DIR/bridget` (muss dir gehören, 0700, sonst Abbruch),
  Gegenstelle per `SO_PEERCRED` geprüft, kein Netzwerk-Port.
- Namen `[A-Za-z0-9_-]{1,32}`; Nachrichten ≤ 1 MiB, Texte ≤ 256 KiB.
- Übertragener Text wird bereinigt: nur `\n` und `\t` bleiben, keine Escape-Sequenzen.
