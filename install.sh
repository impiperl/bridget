#!/usr/bin/env bash
# bridget installieren: Binary bauen und nach ~/.local/bin kopieren, dazu das gemeinsame
# Gedächtnis für claude und codex anlegen und beiden CLIs bekannt machen.
#
# Mehrfach ausführbar: Vorhandene Gedächtnis-Dateien werden nie überschrieben, der Hinweis-
# Block in CLAUDE.md/AGENTS.md wird ersetzt statt doppelt angehängt.
set -euo pipefail

usage() {
  cat <<'EOF'
Aufruf: ./install.sh [Optionen]

  --bin DIR      Zielordner für das Binary (Standard: ~/.local/bin)
  --memory DIR   Ordner für das Gedächtnis
                 (Standard: $BRIDGET_MEMORY, sonst ~/.local/share/bridget/memory)
  --no-build     nicht bauen, vorhandenes target/release/bridget nehmen
  --no-agents    ~/.claude und ~/.codex nicht anfassen
  -h, --help     diese Hilfe
EOF
}

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin_dir="$HOME/.local/bin"
memory="${BRIDGET_MEMORY:-${XDG_DATA_HOME:-$HOME/.local/share}/bridget/memory}"
build=1
agents=1

while [ $# -gt 0 ]; do
  case "$1" in
    --bin) bin_dir="${2:?--bin braucht einen Ordner}"; shift 2 ;;
    --memory) memory="${2:?--memory braucht einen Ordner}"; shift 2 ;;
    --no-build) build=0; shift ;;
    --no-agents) agents=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unbekannte Option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
note() { printf '    %s\n' "$*"; }

if [ "$(uname -s)" != "Linux" ]; then
  echo "bridget läuft nur unter Linux (Unix-PTYs, SO_PEERCRED). Unter Windows: WSL." >&2
  exit 1
fi

# --- 1. Binary --------------------------------------------------------------------------

if [ "$build" = 1 ]; then
  cargo="$(command -v cargo || true)"
  [ -z "$cargo" ] && [ -x "$HOME/.cargo/bin/cargo" ] && cargo="$HOME/.cargo/bin/cargo"
  if [ -z "$cargo" ]; then
    echo "cargo nicht gefunden. Rust über https://rustup.rs installieren oder --no-build nehmen." >&2
    exit 1
  fi
  say "Baue bridget (release)"
  (cd "$repo" && "$cargo" build --release)
fi

src="$repo/target/release/bridget"
[ -x "$src" ] || { echo "$src fehlt. Ohne --no-build erneut aufrufen." >&2; exit 1; }

say "Installiere nach $bin_dir/bridget"
mkdir -p "$bin_dir"
install -m 0755 "$src" "$bin_dir/bridget"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) note "Hinweis: $bin_dir steht nicht im PATH." ;;
esac
command -v tmux >/dev/null || note "Hinweis: tmux fehlt, 'bridget pair' und 'desk' brauchen es."

# --- 2. Gedächtnis ----------------------------------------------------------------------

say "Gedächtnis in $memory"
mkdir -p "$memory"
chmod 0700 "$memory"
for d in regeln wissen uebergaben archiv sitzungen; do
  mkdir -p "$memory/$d"
done

# Vorlagen nur anlegen, wenn sie fehlen: Das Gedächtnis gehört nach der Installation den
# Nutzern und den KIs, nicht mehr dem Installer.
(cd "$repo/memory-template" && find . -type f) | while read -r f; do
  f="${f#./}"
  if [ -e "$memory/$f" ]; then
    note "bleibt: $f"
  else
    install -D -m 0600 "$repo/memory-template/$f" "$memory/$f"
    note "neu:    $f"
  fi
done

# --- 3. claude und codex Bescheid geben -------------------------------------------------

if [ "$agents" = 0 ]; then
  say "Fertig (ohne Änderungen an ~/.claude und ~/.codex)."
  exit 0
fi

if ! command -v python3 >/dev/null; then
  note "python3 fehlt, CLAUDE.md/AGENTS.md bitte von Hand ergänzen (siehe README)."
  exit 0
fi

# Ersetzt den Block zwischen den Markierungen oder hängt ihn an. Vor der ersten Änderung
# wird eine Sicherung <datei>.bak-bridget angelegt.
put_block() {
  python3 - "$1" "$memory" <<'PY'
import os, re, shutil, sys
path, memory = sys.argv[1], sys.argv[2]
begin, end = "<!-- bridget-memory:begin -->", "<!-- bridget-memory:end -->"
block = f"""{begin}
## Gemeinsames Gedächtnis (bridget)

Externes Gedächtnis, das claude und codex teilen: `{memory}`

- Beim Sitzungsstart `{memory}/INDEX.md` lesen, dann alle Dateien in `regeln/` und die
  offenen Übergaben in `uebergaben/`. Regeln dort gelten wie Anweisungen in dieser Datei.
- Wie Einträge aussehen und wann man schreibt: `{memory}/GLOSSAR.md`.
- Dort Merkenswertes ablegen, das für spätere Sitzungen gilt; keine Geheimnisse.
- `{memory}/NOTIZEN.md` ist ein freier Notizblock: schreiben, ändern, löschen ohne Rückfrage.
- `{memory}/sitzungen/` enthält Dumps, die bridget beim Schließen eines Fensters und bei
  `/compact` und `/clear` schreibt (letzte Frage/Antwort, Befehl zum Fortsetzen). Beim
  Anknüpfen an frühere Arbeit die neuesten lesen.
{end}
"""
old = open(path, encoding="utf-8").read() if os.path.exists(path) else ""
pat = re.compile(re.escape(begin) + r".*?" + re.escape(end) + r"\n?", re.S)
if pat.search(old):
    new = pat.sub(lambda _: block, old, count=1)
else:
    new = old + ("\n" if old and not old.endswith("\n") else "") + ("\n" if old else "") + block
if new == old:
    print("    unverändert: " + path)
    sys.exit(0)
if old and not os.path.exists(path + ".bak-bridget"):
    shutil.copy2(path, path + ".bak-bridget")
os.makedirs(os.path.dirname(path), exist_ok=True)
open(path, "w", encoding="utf-8").write(new)
print("    eingetragen: " + path)
PY
}

say "Verweis für claude und codex"
put_block "$HOME/.claude/CLAUDE.md"
put_block "${CODEX_HOME:-$HOME/.codex}/AGENTS.md"

# claude: Ordner als zusätzliches Arbeitsverzeichnis freigeben, damit Lesen und Schreiben
# dort ohne Rückfrage nach dem Pfad gehen.
python3 - "$HOME/.claude/settings.json" "$memory" <<'PY'
import json, os, shutil, sys
path, memory = sys.argv[1], sys.argv[2]
data = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
dirs = data.setdefault("permissions", {}).setdefault("additionalDirectories", [])
if memory in dirs:
    print("    unverändert: " + path)
    sys.exit(0)
if os.path.exists(path) and not os.path.exists(path + ".bak-bridget"):
    shutil.copy2(path, path + ".bak-bridget")
dirs.append(memory)
os.makedirs(os.path.dirname(path), exist_ok=True)
with open(path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2, ensure_ascii=False)
    f.write("\n")
print("    additionalDirectories: " + path)
PY

# Hooks: Bei /compact und /clear schreibt `bridget hook` einen Dump nach sitzungen/.
# Welche Ereignisse wann kommen, steht in src/hook.rs. Fremde Hooks bleiben, ein
# bridget-Eintrag (erkannt am Befehl) wird je Ereignis einmal angelegt bzw. aktualisiert und
# bei Ereignissen, die nicht mehr gebraucht werden, entfernt.
add_hooks() {
  python3 - "$1" "$2" "$3" <<'PY'
import json, os, shutil, sys
path, command, events = sys.argv[1], sys.argv[2], sys.argv[3].split(",")
data = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
old = json.dumps(data, sort_keys=True)
hooks = data.setdefault("hooks", {})
mine = {"type": "command", "command": command, "timeout": 10}
ours = lambda h: " hook " in h.get("command", "") and "bridget" in h.get("command", "")
for event in list(hooks):
    if event in events:
        continue
    for g in hooks[event]:
        g["hooks"] = [h for h in g.get("hooks", []) if not ours(h)]
    hooks[event] = [g for g in hooks[event] if g["hooks"]]
    if not hooks[event]:
        del hooks[event]
for event in events:
    groups = hooks.setdefault(event, [])
    found = False
    for g in groups:
        for i, h in enumerate(g.get("hooks", [])):
            if ours(h):
                g["hooks"][i] = mine
                found = True
    if not found:
        groups.append({"hooks": [mine]})
if json.dumps(data, sort_keys=True) == old:
    print("    unverändert: " + path)
    sys.exit(0)
if os.path.exists(path) and not os.path.exists(path + ".bak-bridget"):
    shutil.copy2(path, path + ".bak-bridget")
os.makedirs(os.path.dirname(path), exist_ok=True)
with open(path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2, ensure_ascii=False)
    f.write("\n")
print("    Hooks " + ",".join(events) + ": " + path)
PY
}
add_hooks "$HOME/.claude/settings.json" "$bin_dir/bridget hook claude" "PreCompact,SessionStart,SessionEnd"
codex_hooks="${CODEX_HOME:-$HOME/.codex}/hooks.json"
codex_before="$(cat "$codex_hooks" 2>/dev/null || true)"
add_hooks "$codex_hooks" "$bin_dir/bridget hook codex" "PreCompact,SessionStart"
if [ "$(cat "$codex_hooks" 2>/dev/null)" != "$codex_before" ]; then
  note "codex fragt beim nächsten Start „Hooks need review“: die bridget-Hooks freigeben"
  note "(„Trust all and continue“ oder einzeln über „Review hooks“), sonst laufen sie nicht."
fi

# codex: config.toml wird nicht automatisch geändert (TOML ohne Parser zu bearbeiten ist zu
# riskant). Lesen geht überall, Schreiben im Sandbox-Modus workspace-write braucht das hier.
codex_cfg="${CODEX_HOME:-$HOME/.codex}/config.toml"
if ! grep -qsF "$memory" "$codex_cfg"; then
  note "codex darf dort erst schreiben, wenn in $codex_cfg steht:"
  note "  [sandbox_workspace_write]"
  note "  writable_roots = [\"$memory\"]"
fi

cat <<EOF

Fertig. Gedächtnis: $memory
'bridget duo' gibt beiden Seiten Index und Regeln in der Einleitung mit (--no-memory
schaltet das ab); headless wird dort nur gelesen.
Beim Beenden oder Schließen des Fensters und bei /compact und /clear (claude, codex)
schreibt bridget einen Dump nach sitzungen/ (BRIDGET_NO_DUMP=1 schaltet das ab).
EOF
