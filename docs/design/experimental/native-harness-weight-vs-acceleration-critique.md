# Power Suit, Not Weighted Blanket: A Native-Harness Critique

Status: critique, 2026-07-06. Deliverable for PRESENT-1kBMcWYz. Capture-first —
this is observation, not a fix; it feeds STEERIN-1kBMc2Gk, WIRING-1kBMc8BV,
PRESENT-1kBMcRQG, and PRESENT-1kBMbq1z (already captured this session).

## Method

Every item below is something I (this session's agent, running Claude Code as
the native harness, spice-wrapped on top) directly felt while working roughly
ten tasks end to end — claiming, implementing, reviewing, committing. Nothing
here is inferred from spice's own documentation; it's what actually landed in
front of me, turn after turn, and how it read.

## What reads as weight

**Repeated verbatim instruction text on every steering delivery.** The
PostToolUse "Inbox Steering" block re-prints the full ACK/NACK mechanics
("Real-time N/ACK loop: put a plain-text ACK or reasoned NACK header...",
"Persistence: acknowledged or refused keys clear once processed...") on
*every single tool call*, whether or not there's a new pending key. I
internalized this once, from the skill file, in the first turn. Seeing it
re-explained dozens of times afterward is pure repetition — it doesn't
adapt to "this agent has already demonstrated it knows the format." A
harness that trusts what it already taught me would show the *keys* every
time and the *instructions* only until I've correctly ACKed once or twice.

**A generic reminder that doesn't know spice has its own task system.**
Claude Code's own "the task tools haven't been used recently... consider
using TaskCreate" reminder fired repeatedly throughout this session, while
I was *continuously* actively using `spice task next` / `task done` /
`task review` — a full, working, better-fitted task system for this
environment. The reminder isn't wrong in general, but here it's
undifferentiated: it has no signal that a domain-specific task system is
already carrying that exact weight, so it nags past a solved problem. This
is the sharpest concrete example of "prescriptive without payoff" this
session produced.

**The shell "Working state" banner, mostly unchanging.** Prepended to every
Bash result: `🌶️ Working state: claim X todo for Ns; last maxim Y.` Useful
the first time I see a claim change; low-signal on the fortieth nearly
identical repetition in the same task phase. It reads as ambient status,
not new information, most of the time.

**Supervisor feedback that repeats without acknowledging correction.** The
`prose.starved count=12` warning fired identically across several turns
even as I was actively adding more narration in response to it. A count
that only climbs, never resets on visible improvement, reads as "you are
still failing" rather than "here's whether the last correction landed" —
weight without feedback on whether the weight is working.

## What reads as acceleration

**`spice task next` as a single, trustworthy next-action affordance.**
Across roughly ten hand-offs this session, I never had to decide what to
work on — one command, one clear answer, every time. This is the single
best "power suit" feature in the whole environment: it removes decision
paralysis entirely and replaces it with drive.

**Task records that hand you the exact recovery command, not a description
of one.** `context_check`/`rehydrate` on every task record gives a literal
copy-pasteable `spice session briefing <thread> --start ... --end ...`
instead of "you should verify context is fresh" prose. Same for
`review_diff_command` on review-phase tasks — it told me precisely how to
view a task-merge diff correctly (`git show -m --first-parent --stat
--patch <sha>`) rather than making me rediscover that merge commits need
`-m` to show the real patch. Precision beats a hint every time.

**One command that judges *and* tracks.** `spice task review --finding
changes --then "title=... | ..."` records a verdict and creates durable
follow-up tracking in a single atomic step. When I found real gaps in two
different reviews this session, filing the follow-up cost nothing extra —
no context-switch to a separate ticketing flow.

**Gate failures that name the exact fix.** Pre-commit failures (local-path
literal, taste word, env-name ledger, private-internal-coupling
allowlist) all named the file, line, and the specific missing entry or
rephrase — never "something is wrong, figure it out." Every one of them, I
fixed and recommitted within a minute, no guessing.

## The pattern underneath both lists

Every "weight" item above is *repetition of something already established*
or *a generic signal blind to a more specific one already in play*.
Every "acceleration" item is *the exact next fact or command, once, with
nothing to decode*. The fix implied for STEERIN-1kBMc2Gk / WIRING-1kBMc8BV /
PRESENT-1kBMcRQG isn't "say less" — it's "stop re-teaching what's already
landed, and stop being generically prescriptive where a specific system
already has the answer."
