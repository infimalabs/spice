# Agent doctrine

This repository develops spice and is also operated by it. The harness's own
rules apply to the agents working here.

## Control plane

- Run shell commands normally. Spice shell startup hooks reexec zsh/bash
  commands through `spice agent run -- <command>`, which injects pending inbox
  steering and context pressure on stderr before the requested command.
- `spice` is the canonical control plane. Start every session with:
  1. `spice agent activation`
  2. `spice session`
  3. `spice task status`
- The no-arg `spice session` briefing is the primary rehydration product after
  a renewal or compaction. Trust machine-readable artifacts over chat memory.

## Steering and ACKs

- Operator steering arrives as inbox items. Reading does not retire them;
  retire an item by ACKing its key in an assistant message:
  `ACK <key> [<key> ...]: <what you understood and did>`.
- Use `SAY:` only for genuinely operator-worthy moments.
- Context-pressure warnings are informational. Keep working; do not finish
  prematurely because pressure is high.

## Tasks

- Pull work with `spice task next` — the allocator owns selection; do not
  eyeball the board.
- Complete phases with `spice task done … --validation` and reviews with
  `spice task review …`. Never claim your own authored review.
- Record tooling friction with `spice task oops` and keep working.
- Git sync belongs to task boundaries (claim fast-forwards, done publishes);
  do not pull/push as ordinary development behavior.

## Code health

- The pre-commit gate is the constitution; never bypass it. Fix exactly what
  it reports.
- Do not add negative tests or negative assertions.
- Keep driving while progress is real; when outcomes oscillate, instrument
  instead of endlessly tuning.
- Do not spawn sub-agents. Preserve the prompt boundary.
