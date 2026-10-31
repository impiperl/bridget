# bridget

Bridget connects AI assistants and shells in the terminal, with tabs, hotkey-based text
sharing, and shared memory for Claude and Codex. Written in Rust.

Linux only: Bridget requires Unix PTYs and `SO_PEERCRED` to verify socket peers.

## Build and install

```bash
cargo build --release
cp target/release/bridget ~/.local/bin/
cargo test            # Unit and integration tests (run in test PTYs)
```

Or run `./install.sh` to do everything at once (`--help` lists the options). It builds
Bridget, installs it in `~/.local/bin`, and creates **shared memory** for Claude and Codex
in `~/.local/share/bridget/memory` (override with `--memory` or `BRIDGET_MEMORY`). New
sessions read `INDEX.md`, the rules in `regeln/`, and open handoffs; `GLOSSAR.md` describes
the entry format. The installer adds a reference to `~/.claude/CLAUDE.md` and
`~/.codex/AGENTS.md`, and grants access to the folder in `~/.claude/settings.json`
(backups are saved as `*.bak-bridget`). Use `--no-agents` to skip those configuration
changes. Existing memory files are never overwritten.

When a console or a `duo` conversation ends, including when its window is closed,
Bridget writes a dump to `sitzungen/`: the program, working directory, exit reason,
resume command (`claude --resume …`, `codex resume …`), and last output (up to 16 KiB).
Files use permissions `0600` and are removed after 14 days when the next dump is written.
Set `BRIDGET_NO_DUMP=1` to disable dumps; `duo` also supports `--no-memory`.

The same happens on `/compact` (including automatic compaction) and `/clear` in Claude
and Codex (also `/new` in Codex), through hooks registered by `install.sh` in
`~/.claude/settings.json` and `~/.codex/hooks.json`. They call `bridget hook claude|codex`
and apply to all sessions, including those outside Bridget. On the next Codex startup,
approve the hooks when prompted with “Hooks need review”; they will not run otherwise.

Memory folder names and metadata keys retain their original names for compatibility:

| Path | Purpose |
|---|---|
| `regeln/` | Rules |
| `wissen/` | Knowledge |
| `uebergaben/` | Handoffs |
| `archiv/` | Archived entries |
| `sitzungen/` | Session dumps |
| `NOTIZEN.md` | Scratchpad |
| `GLOSSAR.md` | Glossary and memory format |

## Interface language

The interface defaults to English. Select German for one invocation or through the
environment:

```bash
bridget --lang de pair claude codex
export BRIDGET_LANG=de
```

`--lang en|de` overrides `BRIDGET_LANG`. The setting applies to help, window titles,
the line picker, `pair`, `duo`, and Bridget subprocesses. English and German color names
are accepted regardless of the interface language.

## Quick start: `pair`

```bash
bridget pair claude codex      # New window: Claude on the left (A, blue), Codex on the right (B, green)
bridget pair                   # Two shells
bridget pair "claude --resume" bash --names X,Y --here   # In this terminal
```

This uses a dedicated tmux server (`tmux -L bridget`) and leaves your existing tmux
sessions alone. Both consoles can send to each other. Click to select a pane; hold
Shift and drag to select text for the terminal clipboard. Closing the window ends both
programs. The bottom bar shows the main pair shortcuts. The terminal is selected from
`$BRIDGET_TERMINAL`, then gnome-terminal, konsole, xfce4-terminal, or xterm.

## Workspace: `desk`

```bash
bridget desk claude codex          # Top: 1 = Claude, 2 = Codex (connected); bottom: terminal 3
bridget desk claude codex --tabs 3  # Bottom terminals: 3, 4, 5
bridget desk --here                # In this terminal
```

Like `pair`, with a full-width terminal area below. Its numbered tabs (3, 4, …) appear
in the bottom bar, with the visible tab highlighted. Each tab is a separate Bridget
console; hidden tabs keep running. When a tab exits, another takes its place. If `1`
is already taken by an existing workspace, console names become `2-1`, `2-2`, and so on.

**Switch consoles:** Click a visible pane or press `Alt+1`, `Alt+2`, `Alt+3`, and so on.
`F1` opens help. Pane borders show the position, console title, and `ACTIVE`/`inactive`
state; the bottom bar labels all available destinations. For terminal 10 and above,
or as a fallback, press `Ctrl-Space`, the number, then `Tab`.

| Key after `Ctrl-Space` | Action |
|---|---|
| `3 Enter` | Send the last output to console 3 once, keeping the 1⇄2 connection |
| `3 B`, `3 A` | Send the screen or selected lines to console 3 |
| `M` / `3 M` | Send the mouse selection to the partner or console 3 |
| `4 Tab` | Show and focus terminal 4 below (`1 Tab`/`2 Tab` switch to the top panes) |
| `Tab` | Next terminal tab |
| `N` | Open and focus a new terminal tab running `$SHELL` |
| `3 V` | Connect to console 3 |

To select text, drag with the mouse (tmux selection), then press `Ctrl-Space M`. If the
program inside the pane captures the mouse, tmux cannot select text; Shift-drag still
uses the terminal's own selection. A number before the action key also works outside
`desk`: `Ctrl-Space 3 Enter` sends to the console named `3`.

## Copy and paste

Click the destination console, then use your terminal's paste shortcut, usually
`Ctrl-Shift-V`. For the terminal's context menu, try `Shift` + right-click; an ordinary
right-click may be captured by tmux or the program inside the pane. To copy text out,
hold Shift while dragging to select it, then press `Ctrl-Shift-C`.

## Bridge

```bash
bridget A -- claude             # Console 1
bridget B                       # Console 2 ($SHELL)
bridget B --connect A -- codex   # Connect to A at startup
```

**Console colors:** The background is lightly tinted: blue for the first console, green
for the second, and so on. In `duo`, side A is blue and B is green. Choose a color with
`--color blue|green|purple|orange|red|cyan|#rrggbb|off` (or `BRIDGET_COLOR`). The original
color is restored on exit. This requires a terminal that responds to `OSC 11`
(gnome-terminal, xterm, konsole, VS Code); otherwise the background stays unchanged.
The window title gets a label: `[A]`, or `[A⇄B]` when connected.

Shortcuts take two steps: press and release `Ctrl-Space`, then press the action key.
This keeps them separate from shortcuts used by the programs inside the panes.
An action menu appears after the prefix and shows the current destination. Enter a
number to change the destination, or `Esc` to close the menu. Fast key sequences also
work; help and the line picker replace the menu when opened.

Pair and desk use subtle borders and a cyan highlight for the active console. Brief
feedback also appears in the bottom status bar. Sending text moves a dot across the
bridge once; set `BRIDGET_ANIMATION=0` to disable the animation. It does not change
the output of the programs running inside the panes.

| Key after `Ctrl-Space` | Action |
|---|---|
| `Enter` | Send the last output (shell: since the last Enter; Claude/Codex: the last response, including text beyond the screen) |
| `A` | Select lines from scrollback with `Space`, then send with `Enter` |
| `B` | Send the visible screen |
| `M` | Send the mouse selection (tmux only) |
| `V` | Connect (choose a number if several consoles are available) |
| `T` | Disconnect |
| `Ctrl-V` | Insert held multiline text as a single line |
| `H` or `?` | Open the help overlay |
| `Ctrl-Space` | Send a literal `Ctrl-Space` to the program |

A number before the action key selects the destination: `Ctrl-Space 3 Enter` sends once
to console `3` without changing the existing connection (see `desk`).

The original `Ctrl-]` prefix and `L/P/S/C/D` action keys still work.

Sending text **never presses Enter** in the receiving program. Multiline text is only
inserted through bracketed paste. If the destination does not support it, the text is
held back. For Bash, add `set enable-bracketed-paste on` to `~/.inputrc`; for Vim 8.0,
configure `t_BE`/`t_BD`/`t_PS`/`t_PE` in `~/.vimrc`.

External commands: `bridget ls`, `bridget link A B`, `bridget unlink A`,
`bridget send A --last|--screen`, `bridget config [PROG] [--init]`.

## Exit a session

Bridget forwards exit commands to the program running inside it. In Codex, type
**`/exit` or `/quit`** in the input field and press `Enter`, without a `Ctrl-Space`
prefix. Plain `exit` without a slash is a chat message there. In a shell, `exit` ends
the shell. Once the program ends, its Bridget wrapper also exits and saves the session
dump as described above.

## duo

```bash
bridget duo --a claude --b codex --rounds 6 "Review docs/plan.md together"
bridget duo join <ID>     # Second console: show side B
```

`Space` pauses/resumes · `i` injects a message while paused · `u` takes over one side
in an interactive session while paused · `q` ends the conversation.

Codex takeovers use `--no-alt-screen` (inline), keeping the session in terminal scrollback
so long responses remain readable and can be copied in full.

Headless runs are read-only: Claude uses `--restricted --tools Read Grep Glob
--strict-mcp-config`; Codex uses the `read-only` sandbox without user configuration or
rules. Codex can read beyond the working directory. Changes are possible only during
an interactive takeover, through the CLI's normal conversation. Use `--transcript` to
save a log in `~/.local/state/bridget/` (permissions `0600`, removed after 14 days).

## Security

- Sockets live in `$XDG_RUNTIME_DIR/bridget` (must be owned by you with permissions
  `0700`, otherwise Bridget stops). Peers are verified with `SO_PEERCRED`; no network
  port is opened.
- Names match `[A-Za-z0-9_-]{1,32}`; messages are limited to 1 MiB and text to 256 KiB.
- Shared text is sanitized: only `\n` and `\t` control characters are kept, with no
  escape sequences.
