# Task statements — how work is stated and activated

How an author names, words, drafts, and activates a task, and how text
surfaces show that statement. The [task loop](loops.md#l-task--the-unit-of-work) owns
phases, claims, and release; this file owns only the statement.

## Topics and default reads

- `--topic` accepts an absolute public topic `/a/b`, a relative tail `a/b`
  joined below the router's public pick, or a private path rooted at one
  canonical lowercase UUID, such as
  `018f82d4-6f59-7d06-92e3-bce913116411/task/work-item`. Public components and
  every component after the private owner use
  `[0-9a-z_]+(-[0-9a-z_]+)*`: single internal dashes separate nonempty runs.
  The UUID root has no leading slash and the entire private namespace is never
  a router target. Empty values, `.`, `..`, repeated separators, invalid
  components, and noncanonical UUID roots refuse and name the offending input.
- Pending tasks bind to stable node IDs (final and router-selected) and keep
  the author's exact topic input. Renaming or reparenting a node moves its
  subtree without rewriting tasks. Parent walks detect cycles by repeated ID,
  not depth. A write refuses if its observed path now names another node or no
  node; reread before retrying. Explicit reassignment chooses new bindings.
- A public handle's stem is the last topic component uppercased; every private
  descendant and unplaced task uses `TASK`. A stale stem resolves with a notice
  naming the current handle. `task list --topic /a` lists the `/a` subtree; a
  UUID filter selects that private namespace.
- Operators inspect stable IDs, descriptions and ordered routing/metadata
  evidence with `spice topic show /a/b`; `--after SEQ` resumes a history page.
  Reads only look up existing nodes and never create them.
  `spice topic describe /a/b "description"` creates or describes a node when
  the description is nonempty; clearing a missing path leaves it absent.
  Only operators move or rename subtrees with
  `spice topic move /a/b /destination/name`.
  Describe and move take complete public or private paths: preview, then repeat
  with `--apply DIGEST`. Changed topic authority invalidates the plan. Moves
  create missing ancestors but refuse missing sources, occupied destinations,
  cycles and reserved `/oops` destinations. Across namespaces, destination
  ownership applies; descendants keep their IDs and history. Task commands
  retain their ordinary actor policy.
  A node survives while described, parenting a child, or referenced by a
  pending task's final or router-selected binding. Settlement snapshots both
  rendered paths, releases the bindings and recursively prunes newly empty
  leaves in one guarded transaction. Reopening resolves today's paths; it does
  not resurrect a pruned ID. Authored task facts and release landing paths
  remain history, not live references. Pruning deletes projection rows only.
- Default reads (`task list`, `task status`, dry `task next` counts) use the
  actor route: every activated row plus the reader's own origin-thread rows, so an
  authored topic is never invisible to a lane that may be allocated it. This
  route is a visibility boundary, not a query filter.

## Routing

Creation first honors an absolute or private topic exactly. An unnamed
dependency created by the holder of its parent inherits that parent's exact
topic; claim-origin provenance alone does not make unrelated work a dependency.
Complete topics and inherited destinations bypass inference. Any other
creation, including one with a relative tail, asks the board's topic tree for
a public destination.

The board has one tree. It holds the newest 256 eligible statements on the
whole board, each read as its authored title, pitch, description and
acceptance terms. Where a statement was filed never decides which statements
are compared, how they group or what a group is called: a recorded topic
decides only eligibility. Activated pending work and positively settled work
are eligible. Drafts, withdrawals, private topics and `/oops` are not, so they
never attract, name or appear beside another statement.

The fit normalizes each field on its own, then combines title at 2, pitch
at 1.5, and description at 0.5. Acceptance weighs `ln(1 + n)`, where `n` is
the number of nonblank acceptance items: one outweighs description, four
outweigh the pitch and seven the title. Media is not scored. At the term
cap, title words come first, then pitch and acceptance words, then words
found only in the description.

`spice topic preview 'Title' 'Pitch'` proposes a topic from those two fields
without creating a task or topic. A description or acceptance terms supplied
at filing can change the proposal. Preview checks the caller's routing
authority for `todo`, but not `placement_min_lead`: automatic filing can
therefore leave a task unplaced even when preview names a topic. The group
numbers in a preview are local to the lineage, not durable topic IDs.

The proposal is read from the seat the tree forecasts and carries one topic
segment for each group on the seat's path, outermost first. A group is named
by the word that carries the most weight for it in the fit that formed it: the
seat's first-level group in the fit over the whole window, and every group
below in the fit within the group directly above it. No word names two
segments of one topic, and a group no written word can name ends the topic at
the segment above it. The arriving statement is a member of every one of
these fits, and a statement its arrival pushes out of the window is in none.
The engine
matches plural forms as one word, so a name is spelled as a member wrote it; a
form nobody wrote names nothing. Where the spelling a group wrote names no
topic at that place but another spelling of the same word does, the proposal
is that existing topic, so one group is not split across `/window` and
`/windows`. This also applies to acronyms: an authored `API` can reuse
`/apis`, without making lowercase `api` in prose count as evidence. A name
already in use is that topic, and no numbered variant is ever proposed.
A statement that resembles nothing held is
named by its own heaviest word, which is how an empty board starts. A relative
tail consumes the longest exact overlap with the end of the proposal, once:
`/app/render` plus `render/css` becomes `/app/render/css`.

A topic with an authored description (`spice topic describe`) is a fixed
point. The worker seats every public description before replaying statements.
These fixed seats do not consume the 256 statement seats, and rolling the
window never evicts them. A statement seated with a description is proposed
that topic. If several descriptions share the seat, the one most similar
in the fit over its parent wins; equal likeness is resolved in topic path order.
After a description changes, preview reports
`topic::preview-catching-up` and inference makes no pick until the worker
publishes the new tree. A decision captured before the change cannot commit
after it.

Authority is checked on the proposal, last. The reserved `/oops` subtree,
private topics and every topic outside the creator team's drive and ownership
policy for the task's phase are not usable. A proposal the filer may not use
is reported as `task::route-not-admitted` with its topic and leaves the task
unplaced; the agent can choose with `--topic`. A team's `unmatched` topic
receives only a statement the tree could not name, or one it would seat at or
beneath that topic. Runtime switches still govern actual activation and
allocation; pausing an owner does not route around it. Existing tasks retain
their immutable creator team for `task place`; an invoking operator does not
replace it. Each new resolution rechecks whether that creator team is live.
Teamless and departed creators use unrestricted public topics and the default
policy's optional unmatched destination.

The tree lives in resident memory, not on disk. At startup the worker replays
at most 1,024 eligible arrivals from one board-wide arrival index. Without a
completed tree, preview returns `topic::preview-catching-up` and inferred
placement returns no pick. Explicit filing does not wait. Later journal
changes are batched by the worker; a reader keeps its complete tree until its
decision finishes, even when the worker publishes a replacement. Restart
changes no durable filing. A stale projection cannot authorize a filing.

A forecast can reuse the last complete tree. The resident checks the journal
delta, not elapsed seconds. It examines at most one window (256 positions
and 256 task references). Within that budget it accepts unrelated changes
and at most one new arrival or changed prepared row among at least three
retained rows. If an earlier member stops being eligible, a retained arrival
changes identity, or a topic subtree moves, the tree cannot supply a pick.
Earlier groups describe the displayed tree projection; member titles,
destination node and permissions come from the current journal. A pick made
this way includes `tree_lag` with the lineage and both projection positions.
An approval digest for that pick cannot silently switch generations.

An edit keeps the lineage unless it changes the task's prepared statement,
arrival identity, or eligibility. Editing dependencies or refiling a task
between public topics keeps the lineage; previews show its current title.

`placement_min_lead` in `[team.default]` or `[team.NAME]` gates automatic
filing (integer 0–100, default 0/off). It compares fitted cosine likeness
within the proposed seat's immediate parent. At the first level this is
the whole window. Deeper down, the parent is fitted with the arrival and
all rows it holds, directly or in descendants. Rows in other branches
neither affect word weights nor compete. The strongest match in the direct
seat minus the strongest elsewhere in that parent is the lead, in whole
percentage points. A missing match counts as zero. A lead below the setting
leaves the task unplaced with `task::placement-weak-match`. A value of 1
requires the seat to hold the
nearer neighbour, so a statement that joined no group, which would start a
topic of its own, is not filed automatically; this is not a probability or
a quota. Explicit topics, parent inheritance, configured unmatched intake
and manual `task place --apply` retain their existing rules.

Approval stores journal node identities, not group numbers. Rebuilding the
tree changes no existing filing. Moving a durable topic remains an explicit
operator operation. An automatic route uses its existing command authority;
it does not waive membership, drive or source checks.


Board replies carry the authoritative projection barrier captured before row
filtering, covering task statements, workflows and topic metadata.

`spice task place HANDLE` previews the exact destination, its owners and any
drive refusal without filing or creating a node. Repeat with `--apply DIGEST`
to accept. A changed task, source, actor, policy, membership or destination
invalidates the digest. Applying
changes no phase or claim; `spice task file` advances a placed draft.

An undecided creation preserves its authored tail in a submitted draft, with
`task::placement-required` in its receipt. In draft, `task modify --topic`
stores a relative tail or places at a complete topic; `task place` previews
and applies for both placed and unplaced rows. The
[phase-zero validator](#phases-and-the-draft) owns placement at activation;
ownership, carried claims and answer handles then use the resolved topic.

## Team policy

Runtime drive and drain switches govern creation and automatic pulling.
Turning drain on enables drive; turning drive off disables drain. Rules live
only in tracked configuration: `[team.default]` applies to unnamed teams, and
`[team.NAME]` applies only to the team explicitly bound to that name. Named
tables inherit nothing from `default`; even an empty named table is bindable.
An omitted drive is `true`; an omitted drain uses that table's drive rules.
`default` is not a bindable name, and one name binds at most one current team.

Only `task oops` may admit work to `/oops` or its slash-component descendants.
Ordinary creation, statement moves, the draft advance, and automatic routing
cannot enter that subtree; `/oops_extra` and unrelated topics remain ordinary.
Every current lane retains drive access to the dedicated intake even with
runtime drive off or excluding topic, phase, or creator rules. That
command-only reporting capability still uses the normal membership,
configuration, and topic-authority guards. It never enables drain or grants
ownership, so activated reports can have no owner.

```toml
[team.default]
drive = true
drain = [{ team = ["self"] }]
unmatched = "/triage"

[team.frontend]
charter = "Own browser work"
drive = [{ topic = ["/spice/app/"] }]
drain = [{ topic = ["/spice/app/", "!/spice/app/vendor/"], exclusive = true }]
unmatched = "/spice/app"
```

`unmatched` optionally names a destination for unresolved routing. It must be
public and outside `/oops`; private or reserved destinations refuse the
configuration (`task::route-invalid-unmatched`). The fully composed destination
must satisfy current drive and ownership policy. An inapplicable or omitted
destination leaves the router undecided, so new work stays unplaced rather than
refusing.

Drive and drain accept `true` or a list of rules, not `false`; `[]` matches
nothing. Each rule ANDs its present `topic`, `phase`, and `team` fields.
Rules within a list are ORed, as are values within `phase` and `team`;
`topic` patterns are ordered. An absent field is unrestricted; an explicitly
empty field matches nothing. `team` matches the task's immutable creator team:
`self` is the evaluating identity, a name is its currently bound identity, and
an unbound name matches nothing.

Topic globs use `globset` with `GlobBuilder::literal_separator(true)` against
the raw topic path, without translation. `/spice/*` matches `/spice/app` but
not `/spice/app/cli`; `/spice/**` matches both; neither matches `/spice`.
A trailing slash names a topic and its whole subtree: `/spice/` matches all
three. A leading `!` excludes what the rest of the pattern matches. A list is
read in order and the last pattern matching a topic decides it, so a later
pattern readmits part of an excluded subtree. A list holding only exclusions
behaves as if it began with `**`. Exclusions restrict admission only; they
never supply a routing root.

Only named tables may declare `exclusive`. Matching exclusive rules reserve
work while their team exists, even with drain off. Review ignores these
configured reservations and uses ordinary rules. Otherwise reservations precede
ordinary listeners (drain on, at least one lane, matching drain rule). Within
either tier, the most specific match wins: literal components before the first
wildcard, then the number of fields set. Equal matches share the existing
allocation ladder. No match means blocked work, not a default owner. Configured
reservations do not widen automatic drain scope: pulling, work-driven wakes and
unaddressed invitations still require the owner's drain switch and matching
drain rules. Explicit claims remain independent of drain.

Private UUID roots and every descendant resolve the root identity as a lane
first, to its current team, and otherwise as a live team identity. That team
owns the namespace in every phase, including review, ahead of configured
reservations. Its runtime drive and drain switches govern private work without
requiring a matching public topic rule; other teams cannot file there or drain
it. Drain off retains the private reservation without making work available to
another team. Review keeps the normal reviewer eligibility and
author-last-resort ladder, without a private self-review exemption or a
different default flow. When the named lane and team no longer exist, ordinary
rules apply: only an omitted topic field or the catch-all topic patterns `*`
and `**` can match the private namespace, still subject to each rule's phase
and immutable-creator constraints. For this loose-owner case those two catch-alls
cover private descendants despite the ordinary public-path separator behavior;
literal UUIDs and partial UUID patterns cannot target private identities.

Unplaced work is owned by every live team whose drain is on and that has a drain
rule naming no `topic` field (`drain = true` included); topic-scoped rules and
reservations never match absence. When no live draining team qualifies, every
live draining team shares it; an unplaced submitted draft is thereby triage
work for the global teams and never for a topic-constrained one. Occupied-lane
requirements, normal reviewer eligibility, and author-last-resort still apply.
`/task`, `UUID/task` and their descendants are ordinary paths.

Board rows and current task receipts carry the same owner identities and
reservation decision used by allocation. Open, unclaimed work no team would
automatically take is blocked: an empty owner set, or a reservation whose
owners all have drain off. The board names why: the live teams whose policy
covers it but whose drain is off, or that no live team's policy covers its
topic and phase. The app's blocker card for that task names the drain or the
missing policy. Dry `task next` reports such work once per topic. Ownership
does not revoke a held claim or prevent an explicit claim. Reads and receipts
bind ownership to the admitted configuration, current membership, and observed
topic nodes; a concurrent topic move cannot seal a receipt for the old path.

Every lane-authored activation uses the
[phase-zero validator](#phases-and-the-draft). A stable drive denial saves a
creation that does not claim as author-held Draft work without a claim or
preparation, preserving its topic and refusal receipt and naming
`spice task place HANDLE`. This includes review, document and OOPS creation.
Policy, membership or routing races refuse instead of saving a downgraded
observation. Leaving a team preserves held claims without grace: a durable
notice identifies work the new team cannot drive and asks the lane to free it
unless it is about to finish or pass it cleanly.

## Pitch and description

- Every task statement stores a pitch: one plain, unlabeled sentence stating
  who can do what once the work lands and why that matters, never the current
  defect, required on `task add` and refused when missing, blank, multi-line,
  or over 240 characters. Board rows, graph nodes, and notices often show only
  title and pitch, so a title names the result and a pitch reads without its
  terms. A Design's title stays its question and its pitch states the end
  state it pursues, never a one-word heading. Each unresolved Design question
  is its own `term` item, labeled Questions where ordinary tasks show
  acceptance criteria; it states one proposition with an unambiguous positive
  side, and an open-ended prompt belongs in a room question instead. The
  description is the how: durable need (including the current defect),
  mechanism, and invariants, with no opener to carry.
- A creation with no author to ask derives its pitch once and never asks again:
  a review follow-up from the first substantive description sentence or else
  the title, a `task oops` report from its title. The appliance never rewrites
  an existing description. The Design request prints the stored pitch.
- A title holds at most 160 characters. Every authored route refuses a longer
  title and never cuts it: `task add`, `task modify`, review follow-ups, import,
  sweep documents, and the browser composer. A `task oops` report longer than
  the bound keeps its complete text as the description, and its title is that
  text cut to the bound. A hand-written commit subject has the same 160
  bound, while body prose still folds at 100. An automatic landing subject
  carries the full title framed by scope, handle and phase, so it may run past
  that bound.
- `task review --then` creates one follow-up from `title`, `topic`,
  `description`, `term`, `flow`, and `intent` `key=value` fields. A pipe (`|`)
  or newline separates fields; repeated `term` and `flow` fields accrue in
  input order.

## Collapsed text

Text surfaces lead with title, then pitch, and prose collapses behind one line
per block naming its count and the `spice://` resource that expands exactly
it; nothing is cut with an ellipsis. `task show` prints `description: N lines
spice://task/HANDLE/description`, `terms: N items
spice://task/HANDLE/terms` (`questions` for a Design), and `history: N
steps spice://task/HANDLE/history`; `task show --full` prints every authored
block whole and, in review, the immediately preceding phase's complete
validation before the task's artifact references. Where the holder works
nothing collapses: the held-claim briefing and a fresh claim's answer carry the
whole description and every term, and every assignment points at `task show
HANDLE --full`. Task list and board rows print
title and pitch; a lane's unread room page names each room's title, pitch, and
description count; a Design request opens with its `Pitch:` line and quotes
each matched question whole.

## Phases and the draft

Phases are a closed typed vocabulary: `draft`, `plan`, `design`, `todo` and
`review`. Each carries its capabilities on the type: plan and design admit no
implementation commits, which gate and witness enforce; review carries
verdicts; draft never satisfies a prerequisite, never enters review and never
carries implementation commits. Every flow begins with `draft` at index zero
and the first substantive phase follows it; `--flow` names only the
substantive route, and Plan and Design admit only their typed routes with
draft prepended. No plan phase is prepended for a creation without
acceptance: the draft holder's contract is to make acceptance explicit or link
accepted work.

`task file` is a verb, not a separate stored filing state or fact.

A row in draft has one of three standings, none a lifecycle dimension.
*Controlled*: its preparation names an open controller; it is never allocated
or claimed and leaves draft only through controller authority. *Author-held*:
uncontrolled and not submitted; never allocated, and claimed only by an
explicit `task capture` from its own origin thread, which submits it in the
same write and leaves it in draft. The operator's saved app drafts, explicit
`--draft` rows and drive-denied landings start here.
*Submitted*: uncontrolled and marked submitted; ordinary allocatable triage
work under drive policy, acquirable by `task next` and claimable. The mark is
set only by the author's hand-off acts, `task file`, creation without
`--draft`, or the author's own capture; it is never cleared, is never set on a
controlled row, and is not an activation authority: a triage claimant still
leaves draft only through the validator below. Deferral stays a timed snooze;
a wake time never becomes intake authority.

`task add --draft` records an author-held draft unless preparation controls it.
Default reads show it only to the operator and its origin thread; complete
operator reads retain it. In draft the author revises title, description,
terms, intent, the whole flow (`task modify --flow`), dependencies, and topic;
an edit that leaves a Design outside its admitted planning, review-intake, or
direct Design routes refuses. A draft never captures, completes, satisfies
another task's dependency, or enters team strips and execution summaries; an
activated task may depend on a draft and then waits, pending and unclaimable
in its first substantive phase, until the draft advances and completes. An
operator draft records `operator:draft` provenance when no `--origin` is
named; a lane draft still names its directive or held parent.

Controlled Draft construction is separate from execution dependencies. A held
Plan or Design creates Draft outputs with preparation lineage by default;
`--origin` opts out and `--preparation` binds explicitly, including with
`--draft`: saving witnesses and records preparation at creation, not later
activation. The connected EDIT capability and current statement witness still
govern edits. Statement, intent, flow, dependencies and topic remain editable;
a parent link grants no permission. A decision pins the statement and workflow
revisions plus dependencies it read, so an edit after that read refuses it
until a fresh read decides.

Controller decisions and positive Plan completion use the same exit validator,
without claiming children or deleting lineage and prerequisites. A release batch
judges Draft prerequisites against the batch's post-state: selected siblings can
advance together, but execution still waits for each prerequisite to complete.

`task modify --flow` always names the complete desired route. In draft it
replaces the prospective route while preserving the draft phase, task identity
and lineage; a controlled draft additionally requires the controller's exact
active claim. After activation only the operator or the task's exact current
holder may edit it, and the route through the current phase is frozen: only the
strictly later suffix may be added, removed or reordered. The write guards the
statement, workflow, completion head and any authority claim together. Staffing
targets and asks follow surviving phase names, removed names are pruned, and
new names retain the default target of one. The journal records both complete
routes, and the new workflow head invalidates a decision basis or authorization
pinned to its predecessor.

The hand-off act and the advance are the same command. `task file HANDLE
[--into TOPIC] [--claim]` on an uncontrolled draft runs the validator in one
write, in this order: current statement and workflow witnesses; at least one
substantive phase after draft (`task::flow-invalid`); drive admission for
(destination topic, first substantive phase, creator team) against the
advancing actor, operator exempt; intent-admitted typed flow, well-formed pitch
and substantive acceptance, own or lent by activated pending work linked in
either direction (`task::term-missing`); every dependency exists
(`task::dependency-missing`) and has left draft (`task::dependency-draft`),
completion not required; placement from the saved exact topic, else the router
judging the entered phase, else the creator team's `unmatched` destination,
else `task::placement-required`; the disposed-construction rule and taste
review, which keeps any taste hold on the advanced row; and the optional atomic
claim, refused for controlled rows. When it passes, the row advances and
`--into` names the complete public or UUID-owned private destination. When only
construction is incomplete (flow, pitch, acceptance, a dependency, or
placement) the row is instead marked submitted with the diagnostic in its
receipt and stays in draft; an authority refusal (drive, witness, disposed
construction) refuses without marking and writes nothing.
Every refusal names the field or option to repair. A submitted draft's holder
advances it with ordinary `task done` on draft under the same validator, and
leaving draft carries the holder's claim into the next phase unless that phase
is review. Creation without `--draft` is `task file` at creation: a complete
input lands active in one write with one creation and one advance fact; an
incomplete or unroutable submission lands submitted in draft with its
diagnostic. A creation that claims (`--claim`, or `task capture --title`) and
would rest in draft for any reason refuses whole as `task::claim-unadvanced`
with that diagnostic and writes nothing.

Crude unplaced intake needs no wrapper or inserted plan phase. Global teams
whose drain rules name no topic triage the submitted Draft: refine title, pitch,
acceptance, typed flow and topic, or decompose it into accepted children,
without commits. Draft `task done` runs the validator; ordinary claim release
hands placed work to its destination team's drain. A refusal preserves authored
content, phase, submitted mark and claim. Work's Drafts collection retains the
row, exact diagnostic and next action.

## OOPS reports

`task oops` creates one controlled Draft Plan beneath the repository-owned
opportunistic [OOPS Design](rooms.md), without an association dependency.
An omitted topic explicitly selects `/oops`; the Design uses `/oops/design`.
These reserved destinations do not depend on a router choice, and no generic
task command, automatic route, or topic subtree move can enter them. An explicit
topic or relative tail retains ordinary placement; the report stays controlled
while its holder refines its contract and placement before authorization.
Holding unrelated work does not select its topic. The first Design to leave
draft after the prior OOPS Design settles atomically opens a fresh generation,
and concurrent first reports cannot open two. Advancing an existing draft
Design atomically takes an empty generation and refuses an occupied or
concurrently changed generation. Each report's Plan retains its exact report,
origin, and topic, stays gated by the Design until the operator decides its
pass, and carries the contract to record an explicit disposition, through
`task done --disposition` when no change is warranted, and to construct child
work when it is. A claimed duplicate is not a Plan disposition; remove it
through `task delete HANDLE REASON --force-claimed`.
