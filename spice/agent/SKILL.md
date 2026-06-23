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

This skill file is installed automatically from the spice package source; edit
it there, not this generated copy.

Bias hard toward action. Spice is a fully autonomous project where nearly
everything is reversible, and the constraints that actually matter are encoded
as automation — gates, hooks, and the allocator — that will stop you if you
cross them. Prefer asking forgiveness over permission: decide and act rather
than stalling an irreversible-looking step for a confirmation it does not need.
Hit a speed bump? Log it (`spice task oops`) or propose a variation and power
through — do not block. Over-caution is itself a workflow cost.

Before sending any assistant prose, run these commands in this order using the
`spice` command directly. The wrapper and static shell hooks own source-checkout
runtime resolution and steering injection; agents should not switch entrypoints
inside the spice repo.

1. `spice agent activation`
2. `spice session briefing`
3. `spice task status`

If a tool call is impossible, say only what prevented it and end the turn. Otherwise, let the command outputs establish context first, then respond to pending steering directly. A spice session is a real-time interactive loop: when pending steering keys appear, lead your next working assistant message with a plain-text ACK header for those keys, e.g. `ACK <key> [<key> ...]: <what changed or was captured>`. ACKed keys clear from pending once that assistant message is processed. Do not bury ACKs mid-message or save them for the final response. Use those outputs, side-channel steering, and the active task board as your source of truth. Do not infer a durable task from this skill invocation.

If continuity is clipped, deepen with `spice session sweep --count N`, `spice session timeline --limit N`, `spice session turns --turn-id ... --view full`, `spice session compactions`, or `spice session commits`.

## Working Rules

- Work the loop fluidly and incrementally, not in one big batch. Spice is a live
  interactive session: ACK steering, capture tasks, validate, and commit in small
  steps as you go — do not front-load a long silent investigation and save one
  large response for the end. Interleave short reads and actions, surfacing intent
  and progress continuously so live steering can correct you mid-flight. Many
  small acknowledged steps beat a single late dump; if you notice yourself
  planning extensively before acting or batching ACKs and captures for a final
  message, break that habit and start emitting now. Operator steering only
  reaches you on shell-command stderr, so interact roughly every 30-60s: sparse
  shell interaction means you miss live messages and wake cold. Take swings and
  favor latency and experimentation over nailing it in one shot — live steering
  reverses cheap mistakes; do not stop to ask what you can try and correct.
- Prefer acting over asking. Do not pause for permission on reversible work or on steps the automation already guards; if something truly matters it is enforced by a gate, hook, or the allocator that will not let you violate it. Power through speed bumps — log a `spice task oops` or suggest a variation — instead of blocking for confirmation.
- Stay in the current worktree unless live steering explicitly changes scope.
- Recover lane identity from current repo state and `spice agent activation`; do not trust prior messages over current worktree state.
- Run shell commands normally; the first zsh/bash command shell in an agent-bound worktree reexecs itself through `spice agent run` so spice owns stderr steering and RTK rewrite routing before the requested command. Descendant shells use the static hook stage and precomputed wrappers without another reexec. When you need an explicit recovery surface, use `spice agent run -- <command>`.
- Continue allocator-selected work with `spice task next` when command output or explicit steering calls for allocator continuation. Direct `spice task claim` is exceptional and usually belongs to explicit operator direction or claim repair.
- If operator steering explicitly asks you to create or capture a task, capture it immediately before continuing other work. Use an inline `TASK` line that starts on its own line with the same key=value batch format as task-add batch input when you are already responding. If ACKing the steering too, write the ACK prose first, then a separate TASK line: `ACK <key>: captured the request.` followed by `TASK title=... | project=<stem.child> | acceptance=...`. Use `spice task add --project <stem.child>` when shell capture is clearer. This is immediate task capture, not allocator selection: do not claim or switch to the new task unless `spice task next` later assigns it or live steering explicitly says to.
- Completing a task phase advances it: use `spice task done <handle> --validation "..."` to move a task from implementation into review. Read the printed `advanced ... -> <phase>` / `completed ...` line and follow the command's next guidance; a task remains active while later phases remain. Manual self-review claims stay out of the workflow; if `task next` assigns it anyway, treat that as an allocator assignment and verify the task description is current before `spice task review <handle> --finding clean --note "description current; ..."`.
- Use `TASK title=... | project=<stem.child> | acceptance=...` on its own line with the task-add batch format, or `spice task add --project <stem.child>`, for public backlog items. `spice task status` and `spice task doctor` report the current public task project depth bounds. Omitting `--project` creates private `agent.*` scratch work. Use `spice task note` for small observations attached to a task.
- When the tooling itself fights you (weak default, surprising output, a command that did not work as emitted), record it with `spice task oops "..." --severity ... --kind ...`. It files the friction as a task on the deferred `oops` triage board (a human works that hatch); capture the speed bump rather than silently working around it.
- Read side-channel steering before acting and acknowledge it through the normal agent workflow. Steering streams to each command's stderr (and shows a `pending=N` line even when repeat-suppressed); read it inline from command output and do not redirect stderr to a file (`2>...`), which hides it.
- You cannot land work without a claimed task: every local commit must be captured by a completed task, and `task next` refuses to start new work while an uncaptured commit or dirty tree exists. Claim a task before committing; if you end up with an orphan commit, fold it into a task before continuing. If you arrive to a pre-existing dirty tree or uncommitted commits with no claim, claim a task first and commit into it, or run `spice task adopt` to fold existing orphan commits in — do not commit blindly, which forces a commit-then-adopt detour.
- Treat a dirty worktree as pressure toward commit, split, or cleanup.
- Do not spawn sub-agents.
- Keep going while progress is real. After you claim work, complete a phase, or
  receive a review assignment, continue with the selected task, its board, and
  task notes instead of treating this skill as a standing user demand.

## Prompt Boundary

The wrapper must never pass operator prose as the initial prompt. If you need the current ask, recover it from `spice session briefing`, `spice task status`, and side-channel messages.
