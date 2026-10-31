#!/usr/bin/env bash
# Install Bridget: build and copy the binary to ~/.local/bin, create shared memory,
# and register it with Claude and Codex.
#
# Safe to rerun: existing memory files are never overwritten; the reference block in
# CLAUDE.md/AGENTS.md is replaced rather than appended again.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: ./install.sh [options]

  --bin DIR      Binary installation directory (default: ~/.local/bin)
  --memory DIR   Shared memory directory
                 (default: $BRIDGET_MEMORY, otherwise ~/.local/share/bridget/memory)
  --no-build     Skip building; use the existing target/release/bridget binary
  --no-agents    Leave ~/.claude and ~/.codex unchanged
  -h, --help     Show this help
EOF
}

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin_dir="$HOME/.local/bin"
memory="${BRIDGET_MEMORY:-${XDG_DATA_HOME:-$HOME/.local/share}/bridget/memory}"
build=1
agents=1

while [ $# -gt 0 ]; do
  case "$1" in
    --bin) bin_dir="${2:?--bin requires a directory}"; shift 2 ;;
    --memory) memory="${2:?--memory requires a directory}"; shift 2 ;;
    --no-build) build=0; shift ;;
    --no-agents) agents=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
note() { printf '    %s\n' "$*"; }

if [ "$(uname -s)" != "Linux" ]; then
  echo "Bridget runs on Linux only (Unix PTYs, SO_PEERCRED). On Windows, use WSL." >&2
  exit 1
fi

# --- 1. Binary --------------------------------------------------------------------------

if [ "$build" = 1 ]; then
  cargo="$(command -v cargo || true)"
  [ -z "$cargo" ] && [ -x "$HOME/.cargo/bin/cargo" ] && cargo="$HOME/.cargo/bin/cargo"
  if [ -z "$cargo" ]; then
    echo "cargo not found. Install Rust from https://rustup.rs or use --no-build." >&2
    exit 1
  fi
  say "Building Bridget (release)"
  (cd "$repo" && "$cargo" build --release)
fi

src="$repo/target/release/bridget"
[ -x "$src" ] || { echo "$src is missing. Run again without --no-build." >&2; exit 1; }

say "Installing to $bin_dir/bridget"
mkdir -p "$bin_dir"
install -m 0755 "$src" "$bin_dir/bridget"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) note "Note: $bin_dir is not in PATH." ;;
esac
command -v tmux >/dev/null || note "Note: tmux is missing; 'bridget pair' and 'desk' require it."

# --- 2. Shared memory ----------------------------------------------------------------------

say "Shared memory at $memory"
mkdir -p "$memory"
chmod 0700 "$memory"
for d in regeln wissen uebergaben archiv sitzungen; do
  mkdir -p "$memory/$d"
done

# Only create missing templates: after installation, the memory belongs to its users
# and assistants, not to the installer.
(cd "$repo/memory-template" && find . -type f) | while read -r f; do
  f="${f#./}"
  if [ -e "$memory/$f" ]; then
    note "kept: $f"
  else
    install -D -m 0600 "$repo/memory-template/$f" "$memory/$f"
    note "new:  $f"
  fi
done

# --- 3. Register with Claude and Codex -------------------------------------------------

if [ "$agents" = 0 ]; then
  say "Done (no changes to ~/.claude or ~/.codex)."
  exit 0
fi

if ! command -v python3 >/dev/null; then
  note "python3 is missing; add the CLAUDE.md/AGENTS.md references manually (see README)."
  exit 0
fi

# Replace the marked block or append it if missing. Before the first change, save
# a backup as <file>.bak-bridget.
put_block() {
  python3 - "$1" "$memory" <<'PY'
import os, re, shutil, sys
path, memory = sys.argv[1], sys.argv[2]
begin, end = "<!-- bridget-memory:begin -->", "<!-- bridget-memory:end -->"
block = f"""{begin}
## Shared memory (Bridget)

External memory shared by Claude and Codex: `{memory}`

- At session startup, read `{memory}/INDEX.md`, all files in `regeln/` (rules), and open
  handoffs in `uebergaben/`. Those rules apply like instructions in this file.
- Entry format and when to write: `{memory}/GLOSSAR.md`.
- Record useful information for future sessions there; no secrets.
- `{memory}/NOTIZEN.md` is a scratchpad: write, edit, or delete without asking.
- `{memory}/sitzungen/` contains dumps Bridget writes when a window closes or on
  `/compact` and `/clear` (last exchange, resume command). When continuing earlier
  work, read the newest relevant dumps.
{end}
"""
old = open(path, encoding="utf-8").read() if os.path.exists(path) else ""
pat = re.compile(re.escape(begin) + r".*?" + re.escape(end) + r"\n?", re.S)
if pat.search(old):
    new = pat.sub(lambda _: block, old, count=1)
else:
    new = old + ("\n" if old and not old.endswith("\n") else "") + ("\n" if old else "") + block
if new == old:
    print("    unchanged: " + path)
    sys.exit(0)
if old and not os.path.exists(path + ".bak-bridget"):
    shutil.copy2(path, path + ".bak-bridget")
os.makedirs(os.path.dirname(path), exist_ok=True)
open(path, "w", encoding="utf-8").write(new)
print("    updated: " + path)
PY
}

say "Registering shared memory with Claude and Codex"
put_block "$HOME/.claude/CLAUDE.md"
put_block "${CODEX_HOME:-$HOME/.codex}/AGENTS.md"

# Claude: grant the memory folder as an additional working directory so reading and
# writing there does not require a separate path approval.
python3 - "$HOME/.claude/settings.json" "$memory" <<'PY'
import json, os, shutil, sys
path, memory = sys.argv[1], sys.argv[2]
data = json.load(open(path, encoding="utf-8")) if os.path.exists(path) else {}
dirs = data.setdefault("permissions", {}).setdefault("additionalDirectories", [])
if memory in dirs:
    print("    unchanged: " + path)
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

# Hooks: on /compact and /clear, `bridget hook` writes a dump to sitzungen/.
# See src/hook.rs for event timing. Preserve other hooks; add or update one Bridget
# entry per event (identified by its command), and remove obsolete Bridget entries.
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
    print("    unchanged: " + path)
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
  note 'On the next Codex startup, approve the Bridget hooks at "Hooks need review"'
  note '(use "Trust all and continue" or "Review hooks"); otherwise they will not run.'
fi

# Codex: config.toml is not edited automatically because changing TOML without a
# parser is unsafe. Reads work everywhere; workspace-write needs this writable root.
codex_cfg="${CODEX_HOME:-$HOME/.codex}/config.toml"
if ! grep -qsF "$memory" "$codex_cfg"; then
  note "To let Codex write there, add this to $codex_cfg:"
  note "  [sandbox_workspace_write]"
  note "  writable_roots = [\"$memory\"]"
fi

cat <<EOF

Done. Shared memory: $memory
'bridget duo' includes the index and rules in both sides' initial context (disable with
--no-memory); headless runs are read-only.
When a session ends or its window closes, and on /compact and /clear (Claude, Codex),
Bridget writes a dump to sitzungen/ (disable with BRIDGET_NO_DUMP=1).
EOF
