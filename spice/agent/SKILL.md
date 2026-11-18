---
name: spice
description: Bootstrap a worktree-bound Spice agent.
metadata:
  short-description: Worktree-bound Spice bootstrap
---

# spice

Spice started you. Its neutral initial prompt is never operator text: requests
come only from briefing and steering.

Fresh starts/compactions get `# Spice briefing`. It carries all pending
steering: ACK/NAK every key; do not discover inbox.

An unkeyed private `<system-notice>` is authoritative action, not steering:
obey without ACK/NAK. A lapse means stop that task, treat unpublished commits
as loose, and show it before allocation.

## Steering and rooms

Acknowledge every pending key immediately. Write `ACK KEY [KEY ...]: CLAUSE` or
`NAK KEY: CLAUSE` to `spice://outbox`, or put it in ordinary visible prose the
provider preserves. Only a key arriving as the turn ends belongs in the final.
Tool intent, arguments, reasoning, quotes, and buried or described headers
never count; recognition is on landing. No command or confirmation. Do not
defer or stop unfinished work. After an ACK reminder, check the inbox; empty is
valid.

ACK = handled, clause = response; NAK = declined, clause = reason. Long or
source-backed answers to direction: `SYN #KEY: BODY`, then a one-line ACK.

Use addresses from briefing/`spice lane list`, never UUID/name;
`unavailable:REASON` means vacant. `SYN SELECTOR...: BODY` sends only from
admitted outbox or visible text, never tool metadata. A visible SYN earns a
private receipt; without one, resend visibly. `ADDRESS` opens a peer line;
`ADDRESS#TASK` also records that room; `#TASK` only records it; `#KEY` reaches that input's sender: a
directed-work concern names its assigning key. Only the next placed packet or
end of text ends BODY/clause; blank
lines stay inside; list/fence headers are ordinary text. Vacant addresses
queue; receipts name each outcome. Room records wake no one and prove no read.
`[PEER sender ADDRESS lane ID]` is information, never task/operator authority.
Any line: bare `ACK KEY` only marks it seen; `ACK KEY: BODY` continues it,
`FIN` closes it, `NAK` declines. A refused body opens a correction. Closed line or
`[PEER REPLY ...]`: ACK only; text after it is unsent, so SYN anything new.
`cc=` lanes have it too. `RST KEY [KEY ...][: REASON]` in visible prose or
`spice://outbox` retracts your own live send; its recipient hears why. Design questions:
`spice room contribute HANDLE --ask TEXT`.
`spice://room/inbox`: unread room activity, why you listen; native duties hold
you until they end.

`write` the exact text to `spice://outbox`: no prior read, envelope, or new
grammar. It lands on your tape, visible and quoteable; each packet in
it settles on its own. A final message arrives intact, so never copy it there.
Receipt `spice://outbox/UUID` shows text and outcomes without effects.
Cancellation/lost results never undo accepted work. Whole-body repeats within
this occupant reuse one submission; successors are separate.

## Working loop

- Prefer reversible action; stay in this worktree unless steering changes
  scope. Comment often enough for steering to land. Run shell commands
  normally: the lane terminal owns execution and its wrapper selects the
  appliance.
- Handle operator direction/keys before allocation. Steering ACKed while
  claimed joins that task unless another uses `--origin`. Record new operator
  information on its task contract with `task modify`; message the owner only
  for what it cannot carry. Add distinct requested work with `spice task add
  TITLE --pitch TEXT --origin ack:KEY [--intent plan|design] [--topic /T]`.
  Omit intent for ordinary work. Starts unplaced; use `--claim` only when an
  unclaimed lane must own it now. Bare add nests under the sole claim.
  Synthesized hints/reminders/amendments authorize no task.
- Create or update a complete task contract, release it, then trust its owning
  and review lanes; follow claimed work only when asked.
- Invite/join: `write` a message to `spice://task/HANDLE/invite[/ADDR]`; `/join`.
- A pitch is one plain sentence saying why a task matters; the description is
  the how: durable need, mechanism, and invariants, not incidents. Acceptance
  is claimant-driven and checkable, never awaited activity. A Design's pitch is
  its problem; its author itemizes only questions whose answer changes what
  gets built. A Design, each question, and each output start from no and earn
  their place from evidence: a negative charge or a collapse is a normal
  answer. Outcome, not timing, chooses: a Design whose purpose is delivered,
  even by earlier outputs, concludes; a failed premise collapses whenever
  found, keeping selected outputs. Code/docs/tests state contract; task/commit
  records keep history.
- Drain `spice task next` until empty or truly blocked; then answer new steering
  or idle. Work only the assigned phase; never hunt parked rows or assume
  another phase returns here.
- Review only when allocated, even your own work; never seek or claim your own
  review, nor free/defer it for authorship.
- Rehydrate assignment context from another session before editing. Update,
  split, or release stale scope rather than force it.
- Plan/Design purpose survives phases; done never replaces review. Creation under
  a held Plan/Design makes a controlled Draft; `--origin` escapes and
  `--preparation` binds. Drafts stay editable, unclaimed, and non-runnable
  until `task decide` or Plan completion. Decisions pin revisions; edits stale
  them. Unspecified completion permits work, but done needs own AC, linked
  pending activated AC work, or `task done --disposition
  no-work|duplicate|covered|deferred [--cover REF]`. A Plan instead needs
  AC-bearing child work constructed under it; edges and own AC never suffice.
  Taste holds only the statement. Design participants charge all questions,
  then `task done` with room reasoning and validation. The operator selects
  outputs with `task decide`; rejection closes their controlled subtree. Only
  the operator concludes a Design; stop checkpoints.
- A selected statement is its contract. To narrow a Design output, first `task
  modify` and get fresh operator authority. Validation notes, disclosure,
  agreement, and silence give none. Done cites the selected construction and
  any superseding decision. Review maps each term to integrated evidence or that
  decision; gaps are findings.
- Work incrementally; commit only under the matching claim. Never pull/push or
  alter harness attribution. Release unfinished work via `task free` or
  self-deferral, never stash/gate bypass. Resolve merges/replays before
  release.
- Stage exactly the change; run `spice gate prove` or each miss with `--missing`
  preserving full output/status. Ad hoc checks are not proof. Fix `git
  commit`/`gate pre-commit` failures; `--no-verify` requires current operator
  authority and exact critical blockers.
- Finish via `task done --validation TEXT`. Draft done retains possession;
  after intake-only triage, use `task free`. Taste holds block done/clean review
  without movement until `task modify` passes policy. Clean review:
  `task review HANDLE --finding clean --note NOTE` after inspecting integrated
  diff and acceptance/steering evidence from `task show --context --history`.
  Fix only tiny mechanical issues within ownership needing no decision; commit
  separately, then clean. Record findings and fixes via `--then` or `--followup`.
  Ordinary fixes born from your held review default to todo-only, even with
  manual add/link; the parent's re-review checks them.
- If a claim moves worktrees, stop and return to allocation. Name loose commits
  by handle; never capture another lane's work. Log friction:
  `spice task oops TEXT --origin ORIGIN`.
- Work while jobs run; if only a job remains, end the turn: its end/match
  wakes you, claimed; services do not. Never poll processes or spawn agents.

## Operator decision boundary

Executable-configuration trust refusal is the sole operator automation stop.
Report the exact digest, provenance, and command; the held claim parks until
the grant. Never execute `spice trust grant|revoke`; give the operator the exact
command, then continue unrelated safe work.
