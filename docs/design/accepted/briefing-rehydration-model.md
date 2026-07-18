# Briefing Rehydration Model

Status: implemented contract, 2026-07-18.

## Decision

The session briefing and sweep are one operation with one purpose: **rehydrate
the agent as much as possible, as fast as possible.** The briefing is a focus
query whose standing query is *"what rehydrates this agent right now."* Its
sections are the idiomatic presentation of ranked signal classes; they serve the
ranking, never the reverse.

The target model is:

- **Every candidate carries a value rank.** Each recoverable item — a steering
  row, a final answer, a commit, a command, a file touch, a compaction intent —
  is scored for actionability, memorability, and pertinence, recency-weighted.
- **The surface packs highest-value-first into a fixed budget.** Rank, then
  pack. Cutting to size always cuts the lowest-value item first, so a smaller
  budget degrades gracefully instead of dropping whichever section happened to
  render last.
- **The horizon bounds candidates; the budget bounds output.** Two independent
  clamps: a window horizon decides *what is eligible*, and a character budget
  decides *how much of the eligible set is rendered*.
- **Domain rank keys are the implementation, not a lesser design.** Each section
  ranks by a natural, deterministic, inspectable domain key (asks:
  steering-key recency + disposition; files: last-touch + failure hotspot;
  commands: failures-then-recency). These are the idiomatic realization of the
  same scoring problem that per-node semantic scoring solves elsewhere — chosen
  for determinism and debuggability, per the single-deterministic-path maxim,
  not because rehydration is a smaller problem.

`spice session briefing` becomes the degenerate short-horizon case of `spice
session sweep`: both walk a window horizon, rank every candidate in it, and pack
to budget. The briefing is "the last few windows"; the sweep is "up to the cap."

## Context

This record recovers a design model that existed before the spice integration
and was simplified away. The spice initial import (`8407dfa`, 2026-06-12)
replaced ranked, horizon-bounded, budget-packed briefing surfaces with flat
`[-N:]` tail slices and a single post-hoc whole-text clamp. The current code
shows the loss directly:

- `spice/sessions/briefing.py:105` `operator_asks` scrapes every non-scaffolding
  user message out of turn records, oldest first, with no rank.
- `spice/sessions/briefing.py:229` `_asks_lines` and `:238` `_finals_lines`
  render flat `asks[-DEFAULT_RECENT_ASKS:-1]` / `finals[-N-1:-1]` tail slices —
  recency by list position, no disposition, no overflow marker, and a literal
  `-` placeholder row when empty.
- `spice/sessions/briefing.py:158` `apply_output_budget` clamps the already
  assembled text by line and byte count as a final step, so size pressure cuts
  whatever is at the bottom rather than the lowest-value item.
- `spice/sessions/briefing.py:962` `render_sweep` re-implements window walking
  with its own `boundaries[-count:]` edges and the same flat `operator_asks`
  slicing, duplicating the briefing path instead of sharing it.

The purpose framing above is an operator correction (key
`20260708T032307331301Z`): the briefing is not a watered-down, different problem
from a focus query — it is the **same** problem. Rehydration *is* the focus
query, with an implicit standing query. Any earlier framing that treated the
briefing's fixed sections as the problem definition (and reserved real ranking
for a hypothetical `--focus` surface) is superseded. The sections are
presentation; the ranking is the problem.

### Lineage

The model being recovered has a clear ancestry, and the decision is to land on
the middle rung of it:

- **`~/devel/gotta` `src/gotta/plugins/session/analyze/`** (focus.py,
  semantic.py, lineage.py, overview.py) scores *every* semantic node and lineage
  candidate in tiers (5 = lineage, 4 = semantic kind + text, 3 = neighbor, 2 =
  weak text) with an adaptive `focus_match_threshold` cutting the tail. This is
  ranking taken to per-chunk extreme because gotta serves an interactive query
  flow where any chunk of any message might be the answer, so all candidates
  merge into one globally comparable ranked list.
- **`~/devel/Grit-main` gritctl** folded that into section-level structure:
  `c2d925c7e` ("Fold briefing row ranking model", `gritctl/session/briefing/
  rows.py`) gave each section its own rank key (recency-first via
  `reverse_text_rank_key` on `last_touch_ts`, failure hotspots, failed
  commands), `BriefingRowWindow.overflow_count` for explicit `+N more` overflow,
  `_is_placeholder_briefing_value` to suppress dash/unknown rows, and
  `pack_briefing_fact_lines` char-budget packing. `55b5a6c03` ("make briefings
  horizon-ranked", 2026-04-30) added `adaptive_horizon_start_index` with a
  `horizon_basis` label naming which rule bound the window.
- **spice initial import** dropped the ranking and kept flat slices.

The decision is to **recover the gritctl-tier model** (per-section rank keys,
dual-axis horizon, explicit overflow, placeholder suppression, char-budget
packing) onto the supervised-era signal sources (the ACK DB and coordination
plane), and to **reject gotta's per-chunk semantic-graph scoring**. The
briefing's sections are fixed by contract, so ranking factorizes: each section
has a natural domain key that yields the same rows under a size clamp as global
scoring would, without cross-section score calibration, threshold tuning, or a
semantic-graph build that would blow a seconds-scale budget on multi-MB
transcripts. Rank keys are deterministic and inspectable (sort by `last_touch`
desc); composite scores are debugging-opaque.

## Evaluation

### Message-shape taxonomy

Rehydration signal is not one stream; it is a small set of signal classes, each
with a shape and a domain rank key. The taxonomy is the ranked candidate model:

| Signal class | Source | Rank key (highest-value first) |
| --- | --- | --- |
| **Steering row (ask)** | ACK DB (keys + dispositions) | pending before refused before consumed; then recency |
| **Final** | turn `final_answers` | recency; the closing final of the newest window is top |
| **Commit** | `collect_commit_records` | recency; task-captured before loose |
| **Command** | command audit records | failures first, then recency (a failed command rehydrates more than a successful one) |
| **File touch** | turn `touched_files` | last-touch recency + failure/complexity hotspot weight |
| **Compaction intent** | `CompactionRecord` (`assistant_before` / `user_after`) | the intent that bracketed each window boundary |
| **Coordination state** | claim/task board (claimed task + phase) | the live claim first; it is the single most rehydrating fact |

Sections are the idiomatic *presentation* of these classes. Each section gets a
named row-cap constant, ranks its candidates by the class key, renders the top
rows, and emits an explicit overflow marker for the remainder. No section
renders a placeholder (`-`, `unknown`, empty) row; an empty class is omitted.

The ask signal draws from two planes, never from transcript user-messages: the
**steering/ACK plane** (the ACK DB) and the **coordination plane** (the claimed
task and phase). The claimed task/phase is not decoration around the asks — it
is a first-class rehydration signal, often the highest-value one, because it
states what this agent is doing right now.

**Message-shape classification is one deterministic path.** Assigning a
candidate to a signal class is not a fuzzy match. Each source shape maps to
exactly one class by an explicit rule; an unknown or non-human shape **fails
loud** rather than being silently bucketed as an ask. Per the
single-deterministic-path maxim, a violated assumption about message shape must
surface immediately, not degrade into a mislabeled row — the ask section must
never fill with material that was never a steering ask.

### ACK DB as the ask source

The asks-of-record are the steering keys and their dispositions, not user
messages scraped from turns. `operator_asks` +
`is_scaffolding_text` (`briefing.py:105`) reconstruct "what the operator asked"
by pattern-matching turn text; in a supervised, steering-driven session the
authoritative record already exists in the ACK DB (`spiceacks.sqlite3`, with
`key`, `text`, `ack_content`, `disposition`, `archived_at`, and `lineage`
columns — accessed via `spice/mail/ackstate.py`, `spice/mail/acks.py`, and
surfaced today as the `Inbox` section through `inbox_ack_state_context_rows`).
The decision: **source the ask class from the ACK DB.** A steering key carries
its own `text`, timestamp, `disposition` (pending / acked / nacked / refused /
archived), and `lineage`, which is exactly the rank key the ask section needs —
no text heuristic, no scaffolding filter, no guessing which user message was a
real ask.

The **coordination plane is the second authoritative source**: the claimed task
and phase come from the task board, not from transcript reconstruction. Together
the ACK DB and the coordination plane fully supply the ask/coordination signal,
which is why the turn-scraping path is deleted rather than kept as a fallback.

### Dual-axis horizon: 3-window default, 5-window cap

The window horizon is bounded on **two axes**, and the surface labels which axis
bound it (`horizon_basis`), recovering `adaptive_horizon_start_index`:

- **Compaction count.** Default horizon is the **last 3 compaction windows**;
  the **hard cap is 5**. (Grit-main used 12; the operator's current constraint
  tightens it to 3 default / 5 max — a briefing is a rehydration surface, not an
  archive.) `spice session sweep --count N` selects within the cap.
- **Wall-clock floor (timeout).** A count alone is wrong when recent windows are
  tiny: three fast compactions can span seconds and rehydrate nothing. So the
  horizon also honors a **minimum wall-clock floor** — if the count-selected
  windows fall inside the floor, the horizon extends back in time until it
  reaches the floor (Grit-main's `min_seconds`, e.g. 4h), still clamped by the
  5-window hard cap. The window that actually bound the horizon is named in
  `horizon_basis` (`compaction_count`, `wall_clock_floor`, or `hard_cap`), so a
  reader always knows *why* the window ended where it did.

### Timeout semantics

"Timeout" appears on two axes and both are made explicit rather than implicit:

- **Horizon timeout (the wall-clock floor above).** The floor is the timeout
  that keeps a too-recent window from under-rehydrating; `horizon_basis`
  reports when the floor, not the count, is the binding constraint.
- **Ask disposition timeout.** A steering key that is never explicitly ACKed or
  NACKed ages: pending keys redisplay on a timeout and eventually archive. That
  disposition-plus-age is the ask rank key — an un-timed-out pending key ranks
  above a refused one, which ranks above a long-consumed one. The briefing reads
  disposition from the ACK DB rather than inferring freshness from position in a
  turn list.
- **Recency eviction.** Recency is king, so the surface is self-expiring:
  asks and finals that fall out of the window horizon or lose the value-ordered
  pack are simply not rendered. Nothing stale persists by inertia; an item stays
  only by ranking high enough to earn its budget.

### Briefing / sweep convergence

`render_briefing` and `render_sweep` collapse into one pipeline:

1. resolve the window horizon (dual-axis, capped),
2. collect candidates per signal class across those windows,
3. rank each class by its domain key,
4. pack highest-value-first into the character budget with explicit overflow,
5. render sections, suppressing placeholders.

The briefing is this pipeline at the default 3-window horizon; the sweep is the
same pipeline at up to 5 windows (or `--count`). They stop being two code paths
with independently drifting slice logic. `apply_output_budget`'s post-hoc
whole-text clamp is replaced by the rank-then-pack step, so size pressure always
sheds the lowest-value candidate rather than the last-rendered line.

## Constraints / Non-Goals

- **Not using gotta's per-chunk semantic scoring.** No semantic graph, no
  cross-section score calibration, no adaptive threshold. Section-level rank
  keys deliver the same cut quality deterministically. The one capability given
  up is query-relative relevance *across* sections; the standing rehydration
  query does not need it. If a literal `spice session briefing --focus <topic>`
  surface is ever built, that surface — and only that surface — is where
  gotta-style scoring would return.
- **Rank keys stay deterministic and inspectable.** No opaque composite scores
  in the shipped path; a reviewer must be able to reproduce the ordering by
  sorting on the named key.
- **No silent truncation.** Every clamp (row cap, overflow, horizon bound)
  renders an explicit marker or basis label. A shortened surface must read as
  shortened, never as complete.
- **Not changing what a compaction is or how transcripts are collected.** This
  record is about ranking, horizon, and packing over the existing
  `TurnRecord` / `CompactionRecord` / ACK-DB sources, not about the transcript
  format.
- **The record is authority; code is execution.** This document fixes the
  implemented contract, while the code and focused tests named below prove the
  current execution. Future changes must update both in one reviewed boundary.

## Examples

- **Renewed agent, three fast windows.** The last 3 compactions span 90 seconds
  of a stuck retry loop. Count says "3 windows"; the wall-clock floor extends the
  horizon back 4h to the real work. `horizon_basis=wall_clock_floor` tells the
  agent the window was time-bound, not count-bound.
- **Ask ranking.** Two pending steering keys and six long-consumed ones are in
  the horizon. The ask section shows both pending keys (disposition-first),
  then `+6 more` — not the six most recent by list position with the pending
  ones clamped off the bottom.
- **Budget pressure.** A narrow `--max-bytes` drops the lowest-value candidates
  first: weak file touches and successful commands go before any pending ask or
  the newest final, because packing is value-ordered rather than section-ordered.

## Implementation Evidence

The former implementation battery is complete. Current authority is code and
focused behavior, rather than the original task handles:

- `spice/sessions/briefing.py` owns typed candidates, per-class rank keys,
  ACK-state and coordination inputs, value-ordered packing, overflow markers,
  placeholder suppression, and the shared briefing/sweep render pipeline.
- `spice/sessions/slices.py` owns the three-window default, five-window cap,
  wall-clock floor, and explicit `horizon_basis` result.
- Session briefing, invariant, and slice tests pin ranking, horizon selection,
  budget degradation, source classification, and briefing/sweep convergence.

Future changes modify this implemented contract directly; the deleted battery
is not a live work queue.
