# Product model — interface contracts

Crossing and interface contracts.

## Task ledgers

`spice doc ledger HANDLE` emits one task as canonical dialect Markdown,
interleaving current title, description, terms, flow, priority, and notes with
workflow steps. The same task heads produce the same bytes on every machine; one
reversible escape rule makes emission a byte-for-byte fixed point. Bounded
pages name the next byte offset and complete digest; continuation refuses a
changed ledger. `--output` exports it completely. The ledger is a recovery view,
not a board serialization: it exports neither family nor dependency graph, and
unrepresentable tasks refuse `doc::ledger-unrepresentable`.

## Completion targets

`spice task target HANDLE PHASE TARGET` sets a positive distinct-lane target.
Explicit edits are operator-only, including Draft future phases and lowering.
Invitation growth commits with its filling claim and counts every participating
lane once, including a holder whose arrival is already recorded. Registered
lanes do not otherwise cap targets; reseating earns no extra arrival.

Satisfying an existing round with no active claim applies its owed closure in
that transaction; no further arrival is needed. Phase and verdict rules remain.
Empty `task next` reports partially arrived rounds by phase, count, target and
querying actor's arrival, distinguishing these from real dependency blockers.

[Recruitment rules](loops.md) cover one live offer per claim and sender
cancellation.

## The JSON outcome

Every finite `spice --json` call emits one newline-terminated `Outcome`. A
reply is `{"kind":"reply","body":...}`; reads add snapshot `cursor`, mutations
add committed `seq` ranges, and daemonless replies omit both. Refusals and
faults use their own `kind`. JSON changes shape, never exit code or stream:
replies go to stdout, refusals and faults to stderr.

Text renders journal positions as condensed base-52 codes; text operands
(`--after`, thread references) admit only those codes. A new journal begins at
$52^3$ (`1000`), giving positions at least four significant characters; retained
journals never renumber. JSON and wire positions remain integers; persisted
control strings retain decimal spelling.

## Room and Design commands

The [room loop](rooms.md#l-room--conversation-at-a-task) states what these
commands mean; this section fixes their operand bodies and bounds.

### Design command bodies

`task decide` takes one canonical JSON object. Unknown fields refuse,
nothing is lenient, and a malformed body refuses with
`preparation::argument-invalid`, giving the parse detail and a complete body
that command accepts. `task adopt` takes two task handles.

- **Values.** A task is its bare birth stamp, without the topic prefix
  (`dVzdhh8Nx` for `REPOSIT-dVzdhh8Nx`): exactly nine characters. Decision
  identities are native base-52 sequences (`"1"`, `"2"`, …), new within their
  controller. A source is `{"kind": "journal", "fact": POSITION}` (a position
  `task authority` prints), `{"kind": "contribution", "room": STAMP,
  "publication": STAMP}` (the `source` a `room contribute` answer prints,
  pasted unchanged), `{"kind": "artifact", "digest": HEX}`, or a transcript
  hunk.
- **`decide DECISION_JSON`.** `decision` (`{"controller": STAMP, "decision":
  SEQUENCE}`), `source`, `basis`, and one outcome over the controller's
  completed pass. `basis` is what the operator read: `{"statement": POSITION,
  "room": POSITION?, "outputs": [{"task": STAMP, "statement": POSITION,
  "workflow": POSITION, "dependencies": [STAMP, …]}, …]}`, the Design
  statement head, its room's newest publication (absent for an empty room),
  and every offered output at its exact revision. The offered outputs are
  every pending, acceptance-bearing Draft the Design controls. Before the
  pass completes the decision refuses `preparation::pass-open`; a moved
  statement, room, or offered output refuses `preparation::statement-moved`,
  `room-moved`, or `output-revised`, so read again and decide again. With
  offered outputs the outcome is `{"kind": "resolved", "outputs": [{"output":
  STAMP, "verdict": "accepted"|"rejected", "comment": TEXT?}, …], "comment":
  TEXT?}`, naming every offered output exactly once, else
  `preparation::decision-output-mismatch`. Comments are optional but nonblank
  when present. Exactly the accepted outputs receive execution authorization;
  rejecting all outputs authorizes nothing. With none offered the outcome is
  `{"kind": "concluded"}` or `{"kind": "collapsed", "reason": TEXT}`, staging
  the corresponding guarded Design conclusion in the same transaction. Any
  pass can instead use `{"kind": "returned", "reason": TEXT}`; the nonblank
  material feedback records durably without authorizing or concluding.

### Limits and their repairs

| Bound | Value | Refusal and repair |
| --- | --- | --- |
| Design possession | undeferred open Design | A deferred Design refuses capture as `claim::capture-ineligible` and steal as `claim::steal-ineligible`: run `spice task wake DESIGN` (`--claim` to take it). Operator deferral of a held Design asks its holder to checkpoint and free; a repeat asks again only when no stop stands for that claim. |
| Steeping Design | open, undeferred Design whose cycle is not yet ready for a research pass | allocation never offers it; capture, steal, or recovery by name claims it before it steeps, a steal taking it from any holder, and its holder investigates every remaining question before `spice task done`; `claim::design-held` on capture, steal, or recovery of a Design awaiting decision or waiting on selected work, naming the Design, its cycle, and that cycle's next action |
| Route fanout per publication | 32 | the packet refuses whole |
| Publication content | 64 KiB | the packet refuses whole; an observation refuses `room::post-too-large` |
| Design contribution text | 4096 bytes, nonempty, stored without leading or trailing whitespace | `room::contribution-empty` or `room::contribution-too-long`; the command normalizes outer whitespace, while a noncanonical journal fact is refused |
| Design contribution charge and question item | a charge from -2 to +2 weighing one question item of the Design statement the command witnessed, or a question; `--question HEX` is a prefix, at least six hex digits long, of the item's text-digest identity naming exactly one current question (shown as six hex digits, longer only while shared); 0 and ±2 cite a source | `room::contribution-kind-missing`, `room::contribution-charge-invalid`, `room::contribution-question-invalid`, `room::contribution-question-missing`, `room::contribution-question-stale`, `room::contribution-question-ambiguous`, or `room::contribution-uncited`; stale and ambiguous list the current questions with handles |
| Design contribution citation key | a task handle or stamp, a publication stamp in this room, a base-52 journal code, an artifact digest, or repository source `[COMMIT:]PATH[:L1-L2]` (`/`, `.`, or `:` marks source; `COMMIT` is a full id or unique prefix of four or more hex digits, else the lane's HEAD) linked by commit, path, blob, and 1-based inclusive lines | `room::contribution-cite-unknown` or `room::contribution-cite-invalid`; admission re-proves each link, refusing `room::contribution-source-unavailable`, and a read shows a source unavailable once its store no longer yields it |
| Sources per Design contribution | 8 distinct exact authorities | `room::contribution-sources-exceeded` or `room::contribution-source-duplicate`; two transcript links to one locus and text range are one authority however their accounted boundary differs, and repository source is one authority per commit, path, and line range |
| Contribution room and proof | an open Design, by a lane, with one source witness per cited authority | `room::contribution-not-design`, `room::contribution-design-concluded`, `room::contribution-lane-only`, or `room::contribution-source-witness`; the journal refuses a contribution without its matching successful command receipt |
| Exact Design maturity statement heads | current plus three recent heads; older heads fold into one aggregate | display the aggregate instead of expanding history |
| Distinct lanes per Design maturity set | 64 sorted identities | display the retained count with a bounded marker; a lane beyond an item's bound cannot count as conceding |
| Design maturity question-item series | 32 per Design, least recently charged dropped first | a dropped item reads as quiet until charged again |
| Post to a deleted room | any anchor that is not deleted, settled included | `room::anchor-deleted`, naming the standing: a deleted anchor's room stays readable, and its open receipts still settle |
| Terminal Design or negative Plan settlement | every pending downstream dependency branch closes in the same transaction | a held branch refuses `task::controlled-descendant-claimed` and names its holder unless explicit force is permitted |
| Decision controller | an open Design | `preparation::controller-not-design` or `preparation::controller-concluded`: a Plan's outputs follow its positive review |
| Selecting Design outputs | no pending Design dependency, including review corrections | `preparation::controller-blocked`: settle the dependency first, so a finding keeps the Design pending |

## Bounded reads

A read shows current, relevant state by default; history and detail are
explicit flags, and no flag makes terminal output unbounded. Every
output-bearing surface admits through the one contract in `wire::output`:

- **Limits.** A surface declares interactive defaults and maxima for rows
  (`--limit`), aggregate encoded bytes (`--max-bytes`), and per-field text
  (`--max-text`), all under one repository ceiling. A request above the
  surface maximum is refused, not clamped. Zero rows is a count-only read.
- **Relevance-first admission.** Records are offered current and actionable
  first. Every record is charged its full encoded length, the first included,
  and the first record that does not fit closes admission, so a page boundary
  is a single cursor.
- **No hidden omission.** A partial answer states what it withheld, the limit
  that closed admission, the next record's size, and the exact continuation
  command (`--after CURSOR`); a record too large for the surface continues
  through a file export. Clipped text keeps a UTF-8 boundary and reports its
  withheld bytes.
- **Exact export.** Complete documents and evidence are written atomically to
  an explicitly named file with a receipt naming its path, length, and digest;
  `-` is refused.

Surfaces that refine the contract:

- `spice inbox` and `spice consent` are authority queues: every pending key or
  request is indexed outside every limit, so no item hides behind a page.
- `spice task show` never clips the fields `spice task modify` replaces, so a
  field read there is exactly what a replacement overwrites. `--context` and
  `--history` add metadata, workflow steps, notes, and the task's room
  publications newest first under the shared limits, resuming inside the room
  at a `room:PUBLICATION` cursor; any principal that may read the task reads
  them there, while `spice journal` stays operator-only. A claim answer's
  `room_context` carries the same newest room rows under the interactive
  defaults, so the next phase's holder and the reviewer receive the task's
  conversation. Its `next:` command uses the same admitted transition as
  mutation replies: deferred work names `task wake`, while blocked work names
  no task action. A lane whose team has drain enabled can still name allocator
  continuation; open Designs retain their shared cycle guidance.
- `spice board` is a posture — counts, the reader's own claim outside every
  budget, and the ready, review, and blocked rows on its route — not a dump.
  The complete board is only `spice board [FAMILY] --output FILE`.
- `spice retro` and `spice watch` show decoded reasoning only as a count of
  withheld bytes, in every form including export. A cursor from a replaced
  tape refuses rather than replaying against the replacement. Bare `watch`
  is one snapshot; `--follow` requires a bounded deadline and blocks on native
  filesystem notifications rather than rescanning.
- `task artifact show` never writes raw payload bytes to the terminal; exact
  extraction is `--output PATH`, verified against the retained digest, and a
  payload that fails its digest is neither shown nor extracted.
- A task holds at most 32 artifact citations. Every `task artifact add`,
  `list` page, and `remove` answer carries the task's `budget`: its `count`,
  the `bound`, and the `remaining` slots, whole-task even on a partial page;
  the add refusal at the bound names `task artifact remove`. Removal drops one
  citation of a pending task, deleting the payload only when no remaining
  citation shares its digest; it refuses a completed or deleted task, an
  unknown id, and an artifact its room cites. A removed id is never issued
  again.
- Backup and restore share one bundle bound on members and aggregate bytes;
  restore refuses an oversized manifest before reading a member, and an
  oversized current authority before changing anything.

Mutation replies are outside this contract: they stay complete and concise.

## Authored outbox

Answer steering in the final message when the turn ends there, else through
`spice://outbox` between tool calls, since a provider may fold mid-turn prose
into reasoning. A refused outbox write reports its refusal, not a prose retry.
A tool call's intent never answers: a placed header there retires nothing and
draws one private correction naming the outbox.

An admitted OMP host captures raw UTF-8 prose on the lane tape before dispatch;
no preparation read or packet envelope is needed. The host, not the body,
supplies the serving repository, exact occupant, and tape coordinates. A retired
host cannot borrow its successor's identity.

The tape remains authoritative; journal admissions and judgments reference it
without another message-body store. Ordinary Chat, history, and quotes retain
the author and exact source. Real ACK/NAK headers gain ordinary prose's quoted
direction context and authenticated disposition live and on restoration;
unknown keys gain none. Other tool arguments, reasoning, results, and quoted
examples remain inert.

Status strips show admitted outbox bodies and retries as ordinary prose with
stable identity and author label, requiring exact body/occupant proof. Only
the canonical message owns its Chat card and speech.

Bodies admit at most 256 KiB and 256 packet positions, using native
SYN/ACK/FIN/NAK/RST grammar and recipient rules. Each effect commits with its
immutable judgment. Refused or withheld packets neither undo earlier successes
nor block valid neighbors. Partial success returns a submission with settled
and unresolved positions and any interruption, never an atomic-batch rollback.

`RST KEY [KEY ...][: REASON]` is the sender's control: each key that names a
live send of the writing lane is canceled through the same guarded transition
the operator's `steer cancel` uses, retaining the optional reason. Any other
key refuses alone as `steer::rst-unavailable`. RST settles only through the
outbox; the recipient's late ACK, FIN or NAK refuses as `steer::canceled`.

Write receipts name a canonical quote and `spice://outbox/UUID`. They report
`reused: false` for fresh admission or `reused: true` for an existing
submission. Inspection omits this write-only status. Exact-author inspection
returns exact text and outcomes without acknowledging, publishing, preparing,
or waking. The reader's occupant is frozen against successor access. Private
mutation disclosure survives a canceled or lost result; accepted bytes and
committed effects remain accepted.

Exact complete bytes deduplicate within one repository and occupant birth,
including concurrent writes, helper retries/replacement, cancellation, and
resident restart; digest hits still compare bytes. Recovery handles only
unjudged positions. Successors have separate scopes. This intentionally
suppresses identical outbox repeats, not ordinary visible-prose repetitions.

A nonblank body written to `spice://task/HANDLE/invite[/ADDR]` is the same
kind of capture, admitted in the invitation's own journal write. That write,
never a header in the body, addresses it: the lane the invitation reached and
the task's room. A refused invitation admits nothing. One captured record
admits once, and the same words in another invitation are another submission.
The trimmed body settles once as a peer message to that lane's occupant
recorded in the room, with an outbox submission's receipt, inspection, quote
and startup recovery; a lane no proved occupant has held gets the room record
alone, which its new claim reads through room catch-up. The invitation itself
carries only its task line, and the write's answer ends with the message's
receipt. A message that cannot settle leaves the invitation standing and its
submission unresolved.

## Browser Work deep links

Work is a durable task-set location over the always-mounted Chat surface, not a
separate page. `#/work` names all open execution work; its canonical
refinements name all open work, one exact topic, all Drafts or their unplaced
subset, all or topic-scoped Design planning, one team UUID, one task, or an
ordered explicit selection. A topic segment is its complete admitted label,
never a root or prefix. Team URLs retain stable identity; current topology
supplies membership. A noncanonical spelling replaces to its one canonical
address, and an unrecognized part is dropped and named. A missing typed
identity or a malformed location returns to `#/chat` with a visible refusal.
The complete route grammar and overview behavior live in
[`app/OVERVIEW.md`](../app/OVERVIEW.md#task-overview); the command bar's
[global totals](../app/CONSOLE.md#task-strips) open these scopes.

## Kept jobs

`job list` shows jobs owed to the lane: running or ended but unheard.
Delivered jobs appear only in `history`, newest first with exact continuation.
Each job records its launch task or no task; task overviews show only jobs
belonging to their selected identities.

`wait` observes finite jobs without stopping them. An admitted exact claim
keeps the process idle after its turn; a match or ending reactivates that
occupant. Without a retained process, the lane stays vacant until an unheard
match or ending wakes a successor holding the claim after the delivery grace.

Services never keep a lane idle or end on their own. A claimed service lasts
until that exact claim win ends or `stop`, surviving occupant succession.
An unclaimed service stops with its launching turn. Readiness comes from output
or the served surface; `wait` without a pattern returns immediately for a
running service. Seat starts list running kept commands, silently when none
exist. Lane reads give kind (`command`, `service`, `watch`, or `watched-service`)
and standing (`running` or `unheard`); watched kinds carry an armed `until`.

`until` is one regex on `job wait` and on `shell`, compiled before any wait or
launch; an invalid pattern refuses, as does `until` on another `job`
operation. Both read the merged stdout/stderr log once, in order, holding only
the incomplete line, clipped to 64 KiB; log growth wakes reads without
polling. Output past the log's cap is not searched; a verdict with no match
says so.

`wait` with `until` follows any job, services included, to a matching line,
leaving it running; earlier lines count. Timeout, a message, or the job's
ending also returns.

`shell` with `until` returns a job id at once. Its keeper publishes the first
matching line, clipped to 1 KiB, as one durable notice and disarms; the job
keeps running and its ending is told after the match. A service's match never
wakes the lane: it reaches a seated occupant or else the next seat.

## Principals

A principal is a durable external identity born by `principal::admitted`, with
an optional name/email profile, roles, and a connection capability. The
[identity invariants](invariants/identity.md#identity-a-principal-is-born-once-by-an-explicit-admission)
govern it.

These commands require the operator role:
- `spice principal list`: birth order, identity, code, birth position, admission
  instant, roles, and profile.
- `spice principal admit [--name NAME] [--email EMAIL] [--operator]`: admission
  and a once-only capability; `ORIGIN/#CAPABILITY` connects as that principal.
- `spice principal profile PRINCIPAL [--name NAME] [--email EMAIL]`: replace
  supplied fields.
- `spice principal role grant|revoke PRINCIPAL operator`: change the role.
- `spice principal deauthorize PRINCIPAL`: retire its admission capability and
  end its sessions.

The app's global **Principals** menu uses these commands. Rows show avatar,
initials, name, code, identity, email, roles, capability standing, and admission
time, with profile, role, and capability controls; the connected principal's row
contains their inbox. App admission requires a name, allows email and the
operator role, and shows the opening capability link and copy control once.
It is not stored or shown on replay. Nonoperators see only their own row and
inbox because the resident denies listing.

Codes are vowel-free base-52 admission positions, not routes. SYN addressing
uses concerns (`#KEY` or `#TASK`); a key implies its sending principal, never a
separate principal selector.

## Extension surfaces

Repository configuration may mount a command under the `spice` namespace;
[built-in verbs win at every depth](invariants/session.md#extension-built-in-verbs-and-registered-actions-win-at-every-depth),
so a dotted mount extends a built-in verb only with a novel action name. The
compiled registry is the only executable extension authority: configuration
cannot discover or load executable values into the vendor, wrapper, or gate
namespaces.

## Diagnostics

`spice doctor` summarizes the closed board, journal, steering, lane, transport,
installation, and release roster through bounded journal indexes, independent
of history length. `spice journal verify` owns whole-journal checks.
Host probes never enter hook verdicts.

`spice demo` uses the real renderer and an isolated seeded root, without model
calls or caller-repository mutation. Canned turns are room posts in the seeded
task sheet; simulated lanes never run or show agent prose in the team plane.
Only `--open` opens a display. Ava holds the primary task through live claim
authority, so task strips, ownership accents, roster, overview, and board agree
on one team-owned row.

Interruption stops the demo resident but preserves its root. Every reply prints
`spice demo --reuse ROOT`, which restarts over the lanes' state. Reuse requires
unchanged seed manifest/transcript, seeded task/lanes/turns in the journal, and
HEAD descending from the seed commit; branch, policy, residency, and journal
may advance. Failed identity checks name a repair; a running resident refuses
`resident::already-up`.

## Media

An attachment is part of the operating record, not a transient upload: it
stays browsable beside the message it arrived with. A steering directive keeps
image bytes only in its attachment values; its prose places them with bounded,
digest-bound `attachment:` markers, which display projections expand from the
same record and which are never fetch authority. A send whose prose duplicates
an attachment's data URL refuses before journal mutation.

Speech is the viewing device's own system synthesis: the browser's
`speechSynthesis`, or under `spice app` the desktop shell's synthesizer, which
offers the downloaded voices the embedded web view withholds (see
[the console's speech surface](../app/CONSOLE.md#speech)); nothing renders
audio on the resident. One global queue speaks entries sequentially and never
overlaps them. Narrate alone keeps one page-level Media Session playing while
any team selects it, including idle gaps between queue entries; Speak and Quiet
do not. Platform pause and stop controls stop that same queue.

Browser synthesis partitions long text at authored boundaries into at most
3,000 UTF-16 code units per platform utterance, preserving the complete text in
source order and starting each remainder once from its current predecessor's
end. Only the browser chain's final end settles its queue entry, while any
error or stop cancels the active utterance and every remainder. A failure is a
notice that never blocks the visible stream; the transcript remains the record.

`say.voices` is an ordered list of voice names, scoped to the agent's worktree:
each device speaks an agent's messages with the first listed name it publishes,
and the agent keeps that voice for the page. With an empty list, an Apple
mobile browser uses the device default voice because its published voices do
not carry macOS tier metadata. That is an iPhone, iPad, iPod, or iPadOS
presenting its desktop-mode Macintosh user agent, recognized by a `Mobile/`
token or by `navigator.maxTouchPoints` above one, which a Mac reports as zero.
Other browsers spread roster agents across published macOS premium voices
first: Safari's `com.apple.voice.premium.` identifier or Chromium's exact
` (Premium)` name suffix, served locally. The pool keeps the default premium
voice's language, or the browser's language when none is default; within it the
pool is the premium voices, else the enhanced voices
(`com.apple.voice.enhanced.` or ` (Enhanced)`), else every speaking voice the
browser serves locally in that language, never a macOS novelty voice
(`com.apple.speech.synthesis.voice.`: Bells, Bubbles, Zarvox, ...), and the
browser default speaks only when the language has no such voice.

An agent keeps the voice its lane hashes to unless a teammate already holds
it, so teammates sound different until a team outnumbers the voices, and
agents on different teams may share. The first time a Macintosh page steps
below premium, one toast names the language, how many voices the browser
publishes, the languages its premium voices do speak, what speaks instead,
and the remedy: download a premium voice or set `say.voices`. Other platforms
have no premium tier to miss and run the same descent silently. A speaker
outside the roster with no list uses the browser default. A browser that has
not yet published voices is waited for under a bound when configured or
desktop spreading needs the list; a browser that never publishes any voice
refuses, and a published list holding none of the configured names refuses,
naming the voices the browser offers.
