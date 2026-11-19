# Design rooms

A Design's room carries its read view, the operator's pass decisions,
coordination, and lane contributions. Work's **planning** scope and
Design permalinks open that existing task view.

## Design standing

Every read derives standing from the Design's workflow, claim, and resident
pass cycle. Board rows, `spice task show`, and the browser agree:

Standing terms run from durable to volatile: lifecycle first, then deferral,
then pass cycle or claim state. The most volatile term therefore comes last,
limiting label jitter as the Design moves.

- **open, deferred**: open and deferred. No lane can possess it and it invites
  no lane until the operator wakes it or the resident wakes it at its deferral
  instant.
- **open, claimed**: open, undeferred, and claimed by a lane for one pass.
- **open, CYCLE**: open and undeferred, named by its pass cycle, so
  allocation and capture may reach it once that cycle allows.
- **concluded done**: the operator concluded it done.
- **concluded collapsed**: the operator collapsed it, with the durable reason
  that collapse recorded.
- **deleted**.

A concluded or deleted Design keeps its terminal standing when read. The work
overview and dependency cards label a deferred Design **Deferred** while that
wait remains in force.

## Reading a Design

Every Design, open or terminal, opens for reading in its task overview:
Work's **planning** scope, a Design-intent task's detail, and a Design permalink
all open the same view.
Below the task's state, its **Design room** section is the
task room every task detail shows, with its composer and its conversation
newest first, extended with the room's [standing](#design-standing) and its
retained decision history, resolved through the Design's birth
stamp under its current handle. The **Conversation** disclosure starts
collapsed and shows its publication count; expanding it reveals the complete
newest-first history. Live updates preserve that expansion; another Design
starts collapsed. A completed pass awaiting the operator's decision opens in
its **Yours** row in Work rather than in a separate panel. The
conversation stays readable in every standing. Reading neither joins,
acknowledges, authorizes, nor wakes anything. It commits no
journal fact. Operator decision controls appear only on an open Design.
Lanes refine a Design's statement through ordinary task edit authority, `spice
task modify --pitch` for its problem sentence (the why), `task modify
--description` or a `spice://task/HANDLE/description` write for the how, never
from the browser, which hosts only principal sessions.

## Concluding a Design

Only the operator concludes a Design. A completed pass that offers no output
opens in the Design's **Yours** row. It names the pass fact, then shows the
accepted decisions and comments plus released output commits and final
review, and the controlled work a collapse leaves unfinished. The statement,
workflow, and claim heads that conclusion will check follow in one compact
fact-style row rather than a sidebar column. One **Reason** field serves
three actions: **Conclude as done** carries no comment and is disabled while
the reason has text; **Collapse the Design** and **Return to the room** need
it.

The board's recent-finished window remains capped at 20. Every accepted output
of the mounted Design hydrates through the existing bounded targeted
task-detail queue, even while a board row supplies immediate standing. An
identity outside the window distinguishes loading, a resident read refusal,
and an authoritative unknown-task result; an admitted detail supplies its
complete statement, terminal, completion, and review evidence without widening
the board listing.
The workflow's positive clean settlement is the authority for final-review
wording. A review Git context contributes its fact and repository boundary
only; it does not turn a finding or an intermediate review into a clean one.

Review evidence names the latest recorded review fact, reviewer, verdict,
completion status, and note or finding. Only a clean review recorded as
completed is a clean final review. A pending clean review stays intermediate
even if a later phase completes the task; a finding never implies success.
This evidence remains visible without a repository boundary. Commit evidence
is separate, and no recorded review is distinct from loading or a read refusal.

Concluding or collapsing atomically records that decision and the guarded
Design conclusion; returning records only the material reason and keeps the
Design open. Sending locks the row, refusal retains the reason, and only
committed board authority moves the row.

## Deciding a Design pass

A completed pass whose Design offers outputs opens in its **Yours** row in
Work. The offered outputs are the Design's pending, acceptance-bearing
controlled Drafts. The row names the pass fact and the current tally: undecided, or accepted and rejected counts, plus **with comment** when
the overall comment has text. Each output card keeps its handle-linked
acceptance criteria directly between its pitch and its Accept/Reject controls,
so every criterion and verdict are visible together without selecting an
output. Each card pairs a short multiline comment with a compact vertical
Accept/Reject stack in a two-column grid. The field fills the remaining width;
the verdict stays beside it even on narrow screens. Both lock while a decision
is pending. Nothing starts decided.
**Accept all** and **Reject all** appear after the last output, just above the
overall comment, when there are several outputs.

One **Comment** serves both outcomes:

- **Accept every output**, **Reject every output**, or **Record N accepted, M
  rejected**
  resolves every output and records the optional overall comment. The verdict
  button remains disabled until every output has a verdict. Only accepted
  outputs are released; rejected outputs are closed with their controlled
  pending work. Rejecting every output releases nothing and readies another
  pass.
- **Return to the room** records that same comment as the required reason,
  with every item comment, without resolving outputs: the verdicts are set
  aside, not cleared. It is enabled exactly when the comment contains
  non-whitespace text.

Both buttons use the existing preparation preview and approval dispatch. That
preview records the basis the operator read, the Design statement head, its
room head, and every offered output's revision, and witnesses the Design's
statement and workflow heads, that room head, every offered output, accepted
output transitions, and absence of the new decision. An edit or room
publication after that read refuses the decision as stale. The GUI keeps the serialized
request internal. Resolution records verdicts, comments, and authorizations in
one write; no later Authorize action exists.

Submission marks the Design's decision pending before the socket write and locks
the decision fields beneath a dashed sending notice. Phone width pins the
action bar to the viewport bottom while the row is open. A successful reply
only establishes that the write landed: the row moves when committed board
authority reflects the result. A refusal stays beside the unchanged verdicts
and comments for another submission; it does not silently replace a stale
authority witness or discard the operator's review. The draft, its answer,
and the lock belong to that one pass: the Design's next completed pass opens
an empty, unlocked draft without a reload.

Conclusion and collapse decisions use the same preview, witness, pending, and
refusal machinery as resolutions. Decisions and authorizations remain
readable in the room's conversation, not a separate surface.

## Preparation authority

A room for a controlling task also exposes its Design pass decisions and
execution-authorization records in immutable journal order. Each record
preserves its basis, source link, every output verdict
and comment, the overall comment, and accepted outputs; review evidence and
execution authorization remain distinct. The same authority is available through
bounded `spice task authority HANDLE` and `spice://task/HANDLE/design` pages.
Reading or opening these records joins, acknowledges, and changes nothing.

## Stopping a held Design

The operator puts a lane on an open Design by telling it to `spice task
capture HANDLE`; the room records no coordination. Deferring a held Design,
through its task detail's Deferral fact, sends its holder one stop direction
asking it to checkpoint and `spice task free`. The standing shows that stop
request until that exact claim is completed, freed, or lapsed; it never
force-frees work or releases child claims and unrelated direction.

## Room standing

Each design's menu and read view open with the room's standing, derived from
the room snapshot, its live facts, the board, and lane topology on every render:

- **Next action**: the one next useful action that authority supports, as one
  sentence naming who acts next and on what, never a command line.
- **Lanes**: a block beneath the standing, the menu's only block and the first
  of the read view's strip, listing each involved lane with whether it is
  seated, vacant, or unknown, plus one chip per reason, each naming its source:
  - authored message and contribution history;
  - a recorded Design request the lane has not answered or declined;
  - native work;
  - a claim-bound stop request;
  - a leave, read as a compact **Left** chip that carries the lane's clause in
    its title, while the room's Left card shows the clause in full; a leave
    is the decline of a Design request the lane owed.
  Nothing ages. The lane's next room publication, a contribution or a leave,
  answers a Design request it postdates and ends that request's listening; a
  lane owing no request has nothing to leave. Authored messages and
  contributions never create listening; only a later Design request restores
  it. History and native duties stay.

Agreement, silence, message count, elapsed time, and ACK, FIN, or NAK line
controls never read as progress or approval; the section states only what
moves the Design. It only
renders inside an explicitly opened menu or read view: a status update
changes its text in place and never mounts a room, moves a console, or wakes a
lane.

## Design contributions

A lane contributes to an open Design's problem space with one typed record:
a signed vote from -2 to +2 on one proposition stated by a Design question
item, or an open research question, with its text and the journal,
contribution, transcript, artifact, or repository sources it cites. A
chargeable item states one proposition whose positive side is unambiguous; an
open-ended prompt belongs in a room question instead. A ±1 charge is an
opinion; 0 and ±2 cite at least one source. The app reads each charge to the
operator as a vote: strongly for, for, evidence, against, or strongly against.
A contribution is
lane-authored evidence for the operator's judgment, never authority: it
moves no task, claim, decision, or authorization,
and it wakes nobody. Lanes contribute with `spice room contribute` from their
own sessions; the operator reads contributions and decides through the
ordinary Design controls.

The Design read view shows each contribution once, in the room conversation
at its journal position, with its lane, time, vote, text, and every cited
source; the Conversation disclosure's count is the sheet's only contribution
count. The question item a contribution weighs reads as a `Q N` link that
focuses item N in the sheet's Contract list, so the item's words appear in
that list and nowhere else on the card. The resident resolves each cited
source for the shown Design, and the card says whether it is available or why
not (missing, hidden, changed, unreadable, or retained in an unsupported
shape), the same states any actor's `spice room history` prints. It also
gives the item's text as the link's title while the statement the
contribution witnessed is still current, and otherwise says the item belongs
to an earlier statement, because the journal keeps no superseded statement
text.
Both come from the `room::contribution-read` read, which any authenticated
session may issue and the app issues once per contribution of a shown Design
that cites a source or names an item, at most eight in flight, and again
after a reconnect; it resolves through the same resident path as
`spice room history`. This read-only surface
never creates or answers the session-profile Design requests delivered through
a lane's own session.

A Design's read view shows its Standing across the room directly under the room
heading. Beneath it one strip carries the **Lanes**, **Maturity**, and
**History** summaries in that order, each sized to its own words and kept in
place whatever is open; a narrow room wraps whole summaries onto further rows.
Activating a summary opens its block, and only that block, in one full-width
panel beneath the strip with its line items balanced across as many columns as
the width fits; activating it again closes it. Every mounted strip names its
own panel, so a Design shown in both its console menu and its read view never
points one surface's control at the other's panel. While the room is still
loading, the strip carries History alone.

The **Maturity** block derives from the bounded per-Design measure the room
projection folds incrementally from streamed requests and contributions, never
by folding rendered room history. It shows the state word and summary, recent
statement-head request yield, and folded older-head totals; each current
question item's state (quiet, unopposed,
contested, or concurred) with its votes and lanes reads beside that question
in the sheet's Contract list. Fresh has no contributions; steeped means at
least `min(N, ceil(2 ln(1 + N)))` of the N current items have received a
nonzero charge, some item concurs, and none is contested: every question of one
to four, four of five or six, five of seven to eleven; the summary otherwise
names the item and the vote it needs next. Steeped maturity makes the Design's
first pass ready for a lane, which the standing's pass cycle shows, and narrows
its invitation reach to the 0.16 floor, so fewer lanes are asked. While a
completed pass awaits the operator's decision, the 0.24 decision floor supersedes
the maturity floor without disabling matching. Maturity is not a danger or
workflow state and never blocks or authorizes a Design decision.

An open Design's view offers the reader no contribution control. A lane learns
of an open Design matching its claimed work from the resident's private notice
and `spice room awareness`. Each names the Design's title and stored pitch,
the claimed task and its matched terms, each distinct term once and at most
six, along with the statement revision and the typed `room contribute`
commands, and states that the invitation creates no assignment and requires no
response.
