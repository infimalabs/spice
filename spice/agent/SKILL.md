---
name: spice
description: Start or resume a worktree-bound spice agent from a neutral skill prompt. Use when a server or wrapper launches an agent for an existing worktree and the initial prompt must not contain operator prose.
metadata:
  short-description: Worktree-bound spice agent bootstrap
---

# spice

You were started by spice, the Simultaneous Production, Integration, and
Control Environment. The initial prompt is only a bootstrap signal, not the
operator's request.

Before sending any assistant prose, run these commands in this order. In this
repo, run them through `./spice.sh` as `./spice.sh spice ...` so side-channel
steering and pending inbox bodies inject; bare `spice ...` can show only
`pending=N` without the message body.

1. `spice agent activation`
2. `spice session`
3. `spice task status`

If a tool call is impossible, say only what prevented it and stop. Otherwise, let the command outputs establish context first, then respond to pending steering directly. ACK pending inbox keys inline in any assistant message as soon as they are understood; do not wait for final response. Use those outputs, side-channel steering, and the active task board as your source of truth. Do not infer a durable task from this skill invocation.

If continuity is clipped, deepen with `spice session sweep --count N`, `spice session timeline --limit N`, `spice session turns --turn-id ... --view full`, `spice session compactions`, or `spice session commits`.

## Working Rules

- Stay in the current worktree unless live steering explicitly changes scope.
- Recover lane identity from current repo state and `spice agent activation`; do not trust prior messages over current worktree state.
- Run shell commands through `./spice.sh` when the repo provides it, or `spice agent run -- <command>` otherwise. Bare `spice ...` commands bypass steering injection.
- Use `./spice.sh proxy <command>` when a command must keep native argument and output semantics; with no proxy installed the wrapper drops `proxy` and runs the command directly, while steering injection still applies.
- Pull work with `spice task next`, not by eyeballing a board. `task next` returns the globally-best ready task across all open boards and claims it; the selected board is derived from the claimed task, not stored as a hidden default.
- Completing a task phase advances it: use `spice task done <handle> --validation "..."` to move a task from implementation into its review phase, then run `spice task next` for reviewer assignment. Do not manually claim your own review; if `task next` assigns it anyway, treat that as an allocator assignment and verify the task description is current before `spice task review <handle> --finding clean --note "description current; ..."`. Read the printed `advanced ... -> <phase>` / `completed ...` line, then run `task next` again; a task is not finished while later phases remain.
- Use `spice task add --project <stem>` for public backlog items. Omitting `--project` creates private `agent.*` scratch work. Use `spice task note` for small observations attached to a task.
- When the tooling itself fights you (weak default, surprising output, a command that did not work as emitted), record it with `spice task oops "..." --severity ... --kind ...`. It files the friction as a task on the deferred `oops` triage board (a human works that hatch); capture the speed bump rather than silently working around it.
- Read side-channel steering before acting and acknowledge it through the normal agent workflow.
- Use `SAY:` in an assistant message only for genuine blockers, decisions worth operator attention, or important milestones. Do not shell out to `say` directly.
- Treat a dirty worktree as pressure toward commit, split, or cleanup.
- Do not spawn sub-agents.
- Keep going while progress is real, but let the selected task, its board, and task notes shape the work instead of treating this skill as a standing user demand.

## Prompt Boundary

The wrapper must never pass operator prose as the initial prompt. If you need the current ask, recover it from `spice session`, `spice task status`, and side-channel messages.
