# Agent doctrine

This repository develops spice and is also operated by it. The harness's own
rules apply to the agents working here.

## Control plane

- Run shell commands normally. Spice shell startup hooks reexec zsh/bash
  commands through `spice agent run -- <command>`, which injects pending inbox
  steering and keep-working guidance on stderr before the requested command.
- `spice` is the canonical control plane. Start every session with:
  1. `spice agent activation`
  2. `spice session briefing`
  3. `spice task status`
- `spice session briefing` is the primary rehydration product after a renewal
  or compaction. Trust machine-readable artifacts over chat memory.

## Steering and ACKs

- Operator steering arrives as inbox items. Reading does not retire them;
  retire an item by ACKing its key in an assistant message:
  `ACK <key> [<key> ...]: <what you understood and did>`.
- When steering asks for task capture, add an inline batch line that starts on
  its own line, using the same key=value format as task-add batch input. If you
  are also ACKing, write the ACK prose first, then a separate TASK line:
  `ACK <key>: captured the request.`
  `TASK title=... | project=<stem.child> | acceptance=...`. Capture first, then
  return to `spice task next`; task creation is not allocator selection.
- Keep-working guidance means continue through the allocator. A phase boundary
  keeps the lane active: after `task done` or `task review`, run
  `spice task next` and keep working until the allocator reports no work or a
  real blocker exists.

## Tasks

- Pull work with `spice task next` — the allocator owns selection; do not
  eyeball the board.
- Complete phases with `spice task done … --validation` and reviews with
  `spice task review …`, then run `spice task next` again. Authored reviews
  come through allocator assignment; if `task next` assigns it, review it.
- Record tooling friction with `spice task oops` and keep working.
- Git sync belongs to task boundaries (claim fast-forwards, done publishes);
  do not pull/push as ordinary development behavior.

## Commit hygiene

- Never add `Co-Authored-By` trailers to commit messages. The commit-msg hook
  rejects them; no commit that includes one will land. Write commits without
  attribution trailers.

## Code health

- The pre-commit gate is the constitution; never bypass it. Fix exactly what
  it reports.
- Do not add negative tests or negative assertions.
- Keep driving while progress is real; when outcomes oscillate, instrument
  instead of endlessly tuning.
- Do not spawn sub-agents. Preserve the prompt boundary.
