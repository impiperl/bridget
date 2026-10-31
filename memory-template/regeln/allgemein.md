---
typ: regel
titel: General rules for every session
projekt: alle
quelle: mensch
stand: 2026-10-31
tags: [general]
---

Rules for every AI assistant on this machine. One rule per bullet, each with a brief reason.

- Read this memory at the start of each session, as described in `GLOSSAR.md`.
  **Why:** New sessions should build on previous work.
- Before ending a longer task, write a handoff in `uebergaben/` if anything remains open.
  **Why:** The next session should be able to continue without losing context.
- Remember things only here. Whatever an assistant wants to keep about the user, their
  instructions or their projects goes into this memory (`regeln/`, `wissen/`, `uebergaben/`).
  An assistant's own private memory holds only a pointer here, plus what concerns that
  assistant alone (such as its installed skills).
  **Why:** Two copies drift apart, and the other assistant cannot see a private memory.
