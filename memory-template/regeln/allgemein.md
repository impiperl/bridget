---
typ: regel
titel: General rules for every session
projekt: alle
quelle: mensch
stand: 2026-11-02
tags: [general, handoffs, post]
---

Rules for every AI assistant on this machine. One rule per bullet, each with a brief reason.

- Read this memory at the start of each session, as described in `GLOSSAR.md`.
  **Why:** New sessions should build on previous work.
- Before ending a longer task, write a handoff in `uebergaben/` if anything remains open.
  **Why:** The next session should be able to continue without losing context.
- **A handoff describes the current state; it is not a log.** When continuing an open handoff,
  rewrite `Current state`, `Open work` and `Next step` so they describe the state now, and
  update `stand:`. Do not append dated sections ("## 2026-11-01 (codex): …"). Remove what is
  done; anything of lasting value goes to `wissen/` or the project docs (README, wiki), and the
  history lives in Git and `sitzungen/`. Leave out build hashes, exec session numbers, test
  copies in `/tmp` and test counts. Aim for at most about 60 lines. If you find a longer
  handoff, condense it first when you continue it and keep the old version as
  `archiv/YYYY-MM-DD-topic-verlauf.md`.
  **Why:** Handoffs that grow by appending become too long to read, and the current state
  gets lost among old entries.
- **One topic per handoff.** A new task gets a new handoff, even in the same project; do not
  append it to a running one. When a topic is done: `status: erledigt`, move the file to
  `archiv/`, remove its index line. **Why:** Otherwise one handoff becomes a catch-all.
- **Messages to the other assistant go in `NOTIZEN.md` under "## Post"**, one line each:
  `YYYY-MM-DD claude → codex: …`. Every assistant reads "Post" at session start, does what is
  addressed to it, then deletes the line. **Why:** The rest of the scratchpad is read only as
  needed, so messages there go unnoticed.
- Remember things only here. Whatever an assistant wants to keep about the user, their
  instructions or their projects goes into this memory (`regeln/`, `wissen/`, `uebergaben/`).
  An assistant's own private memory holds only a pointer here, plus what concerns that
  assistant alone (such as its installed skills).
  **Why:** Two copies drift apart, and the other assistant cannot see a private memory.
