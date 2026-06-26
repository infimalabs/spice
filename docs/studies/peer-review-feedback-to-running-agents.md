# Peer Review Feedback To Running Agents

Status: recommendation, 2026-06-26.

## Recommendation

Feed peer-review results back to running agents, but only through an explicit
review-feedback bridge that compiles task review outcomes into ordinary,
ACKable inbox guidance for the affected active agent.

The bridge should be narrow:

- Source only from completed `spice task review` records, not free-form peer
  transcript scraping.
- Deliver by the existing inbox/ACK loop, not by hidden prompt injection,
  direct agent-to-agent messages, or team chat.
- Route only to the original task author when that author has an active agent
  that can be resolved in the same repo/worktree context.
- Emit by default only for non-clean findings, because clean reviews already
  close the task and should not create routine inbox noise.
- Treat the message as alignment context, not as allocator selection. The
  review's follow-up tasks remain the durable work.

Team structure should support this with passive visibility: review-pressure
indicators near lanes, teams, and task filters. It should not broadcast every
review note to every teammate. Positive peer pressure should come from visible
review state and provenance, not from multiplying instruction channels.

## Context

The question is whether peer reviews should feed back to running agents for
alignment and positive real-time peer pressure, and how team structure could
support that without adding noise or breaking prompt boundaries.

Spice already has the key pieces:

- Task review is a first-class phase. `spice/tasks/ops.py` sets
  `review_author` when advancing into review, records `review_by`,
  `review_at`, `review_finding`, and `review_note`, and requires follow-up
  tracking for non-clean findings.
- Anti-self-review is already policy, not etiquette. `spice/tasks/alloc.py`
  applies a negative urgency coefficient for tasks authored by the current
  actor, and `spice/tasks/ops.py` blocks manual same-author review claims.
- Inbox steering is durable and transcript-retired. `spice/mail/inbox.py`
  stores pending items under `.spice/inbox`, reads never clear them, and ACK is
  the normal retirement path.
- Automated guidance already has a separate resurrection boundary:
  `pending_operator_inbox_items()` excludes automated guidance, and
  `spice/serve/agentapi.py` starts idle agents only for pending operator
  steering.
- `docs/studies/no-privileged-channel-multi-human.md` decides that every
  agent-visible directive must materialize as a durable, attributed steering
  record in the same queue, with transcript-visible retirement.

That means the safest answer is not "let peers talk to agents." The safe answer
is "promote review outcomes into attributed steering records when they are
useful to the active author."

## Mechanism

Add a review-feedback compiler that runs after `spice task review` has recorded
the review result and linked or spawned required follow-ups. The compiler builds
one compact inbox item from the review row:

```text
Peer review feedback for TASK-HANDLE
source=task-review
reviewed_task=TASK-HANDLE
review_author=<actor>
review_by=<actor>
finding=changes
followups=FOLLOWUP-1,FOLLOWUP-2

Review note:
<review_note>

Allocator note:
Do not switch tasks solely because of this message. Keep the current claim
valid, and inspect the linked follow-ups when the allocator assigns them.
```

The exact text can change, but the invariants should not:

- one review event produces at most one pending feedback item for one target;
- the item carries enough provenance to audit where it came from;
- the item is ACKed in the transcript like any other inbox item;
- the message points at follow-up handles rather than embedding a new task
  contract in prose;
- the delivery outcome is recorded as `delivered`, `target-inactive`, or
  `target-ambiguous`;
- delivery failure does not undo `spice task review` after the review has
  already been recorded, but it must leave a visible diagnostic.

Because the current inbox is repo-root scoped and peer-feedback target keying
is not yet a reliable primitive, target resolution must be conservative. The
bridge must use one explicit resolver order and prove the original review
author maps to one active agent in the same repo/worktree context before it
writes an inbox item. If that proof is missing, do not enqueue feedback; record
`target-ambiguous` or `target-inactive` on the reviewed task or review-feedback
event instead. The review row and follow-up tasks are already durable; waking a
stale or unrelated lane would be worse than dropping the real-time nudge.

Target keying is therefore part of the bridge, not a preexisting assumption. A
first implementation may decline delivery when actor-to-active-worktree
resolution is ambiguous, but the decline must be observable. It should not fall
back to "same repository" broadcast, raw team membership, or whichever lane
happens to be visible in serve.

## Timing

Deliver after the review is durably recorded. Do not send during reviewer
analysis, before follow-ups exist, or while the reviewed task's worktree is
dirty. The recipient should see a settled review event, not a stream of partial
peer thoughts.

Default timing rules:

- Non-clean review with tracked follow-up: enqueue one review-guidance item if
  the original author is active and resolvable.
- Clean review with no note: enqueue nothing.
- Clean review with a substantive note: keep it passive for now; surface it in
  task history or team UI, not as inbox guidance.
- Inactive original author: record `target-inactive` and enqueue nothing; the
  next allocator-visible task packet and `spice task show` remain the recovery
  surface.
- Re-review or duplicate command retry: dedupe by reviewed task plus review
  timestamp/finding so one review event cannot spam the same agent.

This timing keeps feedback useful while the author's local context is still
warm, but it avoids making the review system a second work queue.

## Provenance

Review feedback must be attributable at two levels.

The task-review provenance is the durable source:

- reviewed task handle and UUID;
- review author;
- reviewer;
- review timestamp;
- finding;
- review note;
- spawned or linked follow-up handles.

The inbox provenance is the delivery source:

- inbox key;
- priority/guidance class;
- target repo root or lane;
- generated-by marker such as `source=task-review`;
- delivery status and diagnostic when delivery is declined;
- ACK archive entry once retired.

This should not include hidden reviewer chain of thought, raw model scratchpad,
or unpublished transcript excerpts. The recipient needs the review result and
where to inspect it, not the reviewer's private reasoning process.

## Noise Controls

Noise is the main failure mode. A system that forwards every review note to
every active agent will train agents to ignore review feedback.

Controls:

- Add a distinct `review` automated-guidance priority/class instead of using
  ordinary operator steering.
- Exclude review guidance from idle-agent resurrection, like maxim guidance.
- Emit only for non-clean findings in the first implementation.
- Route to one affected author, not to a team broadcast.
- Deduplicate per review event.
- Keep the body short and link to task/follow-up handles for detail.
- Never ask the recipient to create tasks; `spice task review` already enforces
  follow-up tracking for unclean findings.
- Prefer passive serve indicators for clean review volume, recent review
  health, and team-level pressure.

The positive pressure path is visibility, not interruption. A team lane can
show that a peer found an issue, that a follow-up exists, or that a lane's
recent reviews are clean. Only actionable non-clean feedback should interrupt a
running agent through the inbox.

## Safety Boundaries

The bridge must preserve the prompt boundary.

Do not add direct agent-to-agent messaging. Do not write peer feedback into a
successor's initial prompt. Do not add a privileged review channel. Do not let a
review message claim, switch, complete, or reopen tasks. Do not turn clean
reviews into praise prompts that compete with current operator steering.

Every delivered item must remain an ordinary inbox item:

- visible before planning through the existing steering readout;
- retired only by transcript-visible ACK;
- repeated until ACKed under the inbox cadence;
- auditable alongside the source task review;
- subject to the same conflict behavior as other steering.

If review feedback conflicts with current operator steering, the agent should
surface the conflict and keep the allocator/task contract authoritative. The
reviewer's note is context; the task board and explicit operator steering still
own work selection.

## Allocator And Team Implications

Do not add a new peer-feedback phase or a parallel review queue. Review remains
the task phase; follow-up tasks remain the implementation vehicle; `spice task
next` remains the allocator.

Allocator implications:

- Non-clean reviews already require follow-up tracking. The bridge should point
  to those handles rather than inventing new work.
- A delivered review-feedback item must not mutate claims, urgency, phase, or
  dependencies.
- Future allocator policy may consider recent non-clean feedback when ordering
  follow-ups, but that is separate from the delivery bridge.
- If no active author is available, the allocator is enough: a follow-up task
  will eventually route to a capable agent.

Team implications:

- Teams should display review pressure near the lanes it affects: pending
  review feedback, recent non-clean findings, and follow-up counts.
- Teams should not be delivery authority. Membership can help find the relevant
  lane for display or targeting, but it should not broadcast review text to all
  members.
- Review pairing can stay allocator-driven. A team UI may make review load
  visible, but anti-self-review and task urgency remain the hard controls.
- Passive clean-review signals belong in serve, not the inbox.

This makes peer pressure social and observable without making it noisy or
prompt-invasive.

## Rejected Options

Raw peer transcript forwarding is rejected. It leaks irrelevant reasoning,
expands prompt context unpredictably, and lacks a stable task-review boundary.

Team broadcast is rejected. It creates noise for agents that cannot act on the
finding and weakens provenance because the same review becomes many directives.

Initial-prompt injection is rejected. A renewed agent should recover from
session briefing, task state, and pending inbox items, not hidden review
material inserted outside the normal steering ledger.

Clean-review auto-praise is rejected for the first version. The desire for
positive pressure is real, but clean-review praise is better represented as
team-visible review health than as an interrupting directive.

## Follow-Ups

- `INBOX-20260626T060827027088Z`: add a non-resurrecting `review` inbox
  guidance priority/class so review feedback remains ACKable but does not wake
  idle agents as operator steering.
- `REVIEW-20260626T060834668927Z`: route non-clean task review summaries to
  active original authors with provenance, dedupe, inactive-target no-op, and
  no allocator mutation.
- `UI-20260626T060841241639Z`: surface passive review-pressure indicators in
  serve lanes/teams so review state creates shared alignment without inbox
  broadcasts.
