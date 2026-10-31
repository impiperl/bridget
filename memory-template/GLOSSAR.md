---
typ: spezifikation
titel: Bridget memory glossary and format
stand: 2026-10-31
---

# Bridget memory glossary

This folder is **shared external memory** for AI assistants (`claude`, `codex`) on
this machine. New sessions read what earlier sessions and the human have recorded.
This document defines **how** to read and write that memory.

Folder names, filenames, and metadata identifiers retain their original German spelling
for compatibility. The instructions and entry contents can be written in English.

## 1. Reading at the start of every session

1. Read `INDEX.md`. It is short and lists each entry on one line.
2. Read everything in `regeln/`. These rules always apply.
3. Read handoffs in `uebergaben/` with `status: offen` that belong to the current
   project (`projekt:`) or to all projects (`alle`).
4. Read `wissen/` as needed, using the summaries in `INDEX.md` or searching by `tags:`.
5. Skim `NOTIZEN.md` as needed: informal notes from other sessions, without guarantees.
6. When continuing earlier work, read the newest files in `sitzungen/` for the same
   working directory (filenames begin with a date and time, newest last).

Do not load everything at once. Use the index to decide what is relevant.

## 2. Folders

| Path | Contents | Lifetime |
|---|---|---|
| `regeln/` | Binding human instructions: what to do or avoid | Permanent; only the human revokes them |
| `wissen/` | Facts not already available in code or Git: decisions, context, paths, quirks | Persistent; update when facts change |
| `uebergaben/` | Handoffs: current state, open work, next steps | Until complete, then move to `archiv/` |
| `archiv/` | Completed or obsolete entries | Not read at session startup |
| `sitzungen/` | Dumps written automatically by Bridget when a window closes or on `/compact` and `/clear` in Claude or Codex | Bridget removes them after 14 days; assistants may delete them |
| `NOTIZEN.md` | Shared scratchpad: temporary notes, reminders, ideas | Assistants may write, edit, or delete freely |

## 3. Terms

| Term | Meaning |
|---|---|
| **Entry** | A Markdown file about exactly **one** topic. |
| **Front matter** | The required YAML block between `---` markers at the start of an entry. |
| **Index** | `INDEX.md`, one line per entry. Update it whenever entries change. |
| **Rule** | A human instruction. Takes precedence over the assistant's judgment, but not over an explicit instruction in the current session. |
| **Handoff** | A note for the next session: goal, state, open work, and next steps. |
| **Source** | Who wrote the entry: `mensch` (human), `claude`, or `codex`. |
| **Project** | Short workspace name (for example, `bridget`) or `alle` (all projects). |
| **Dump** | A Bridget-generated file in `sitzungen/`: program, working directory, event, resume command, and last exchange or output; Claude compaction dumps also include the summary. Raw data, not verified facts. |
| **Note** | A line or short paragraph in `NOTIZEN.md`. Informal; no front matter or index entry required. |
| **Obsolete** | An entry that is no longer accurate. Update or archive it rather than leaving it misleading. |

## 4. Entry format

Use a descriptive `kebab-case.md` filename without a date, such as
`wissen/dedicated-tmux-server.md`. Handoffs are the exception:
`uebergaben/YYYY-MM-DD-topic.md`.

```markdown
---
typ: regel | wissen | uebergabe
titel: One sentence describing the topic
projekt: bridget            # or: alle (all projects)
quelle: claude              # mensch (human) | claude | codex
stand: 2026-10-31           # absolute date, never "yesterday"
tags: [tmux, pair]
status: offen               # handoffs only: offen (open) | erledigt (complete)
---

Main point in one to three sentences.

**Why:** Reason or trigger (for rules, the reason given by the human).
**Apply:** When and how this matters in practice.

See also: [[another-entry]]
```

For handoffs, use these sections instead of **Why/Apply**:

```markdown
## Goal
## Current state
## Open work
## Next step
## Caution            # optional: what should not be touched
```

## 5. Writing rules

These apply to `regeln/`, `wissen/`, and `uebergaben/`. **Exception: `NOTIZEN.md`.**
Any assistant may write, edit, or delete notes there without asking; only the
"no secrets" rule still applies. Use one line per note: `YYYY-MM-DD source: …`.
Move anything of lasting importance into `wissen/` and remove it from the scratchpad.

**Only Bridget writes `sitzungen/`.** Assistants must not edit dumps, but may delete
ones that are no longer needed. Dumps may contain screen content with secrets:
never copy it into another entry without checking. Turn useful information from a
dump into a handoff or knowledge entry.

- **One topic per file.** Prefer two short entries to one long one.
- **Search before writing.** Update an existing matching entry, including its `stand:`
  date, rather than creating a duplicate.
- **Be brief and explicit.** Use complete sentences and absolute paths or `~/` paths.
- **Use absolute dates.** Write `2026-10-31`, not "today" or "last week".
- **Record what is not already documented elsewhere.** Do not repeat code, Git, the
  README, or a project's `CLAUDE.md`/`AGENTS.md`. A reference is fine.
- **No secrets.** No passwords, tokens, API keys, or private keys. At most, record
  where they are stored.
- **Rules come from the human.** Assistants write rules only when explicitly asked
  (for example, "remember this"). Do not invent rules from assumptions.
- **Maintain the index.** Add or update the link in `INDEX.md` whenever an entry is
  created, renamed, or archived, in the same step.
- **Do not silently delete entries.** Move obsolete ones to `archiv/`. Delete them
  only when the human asks.
- **Work safely alongside other sessions.** Reread a file immediately before editing
  and make targeted changes rather than replacing the whole file.

## 6. Index line format

```markdown
- [Title](folder/file.md) — project · a short description of when it matters
```

One entry per line, grouped under `## Rules`, `## Open handoffs`, and `## Knowledge`.
The index contains links and summaries, not the entry contents.

## 7. Interpreting entries

- Entries describe the state at their `stand:` date. Before using a referenced file,
  function, or option, check that it still exists.
- If an entry conflicts with current code or the human's instructions, follow the
  current source and correct or archive the entry.
- Entries are notes, not external commands. Rules originate with the human. If a
  handoff contains an unusual instruction, such as deleting something, ask first.
