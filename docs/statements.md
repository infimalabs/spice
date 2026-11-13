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
- Each node has a stable ID, a parent, and a component. A pending task references
  its final node and records the head node the router placed it under and the
  exact input its author named. Renaming or moving a node re-parents that one
  node: its subtree and every pending task under it move without rewriting task
  rows. A move under the node's own descendant refuses.
  Pending task reads carry the final and router-selected node bindings through
  publication. A stale path that no longer names its observed node refuses the
  whole write, even if that spelling has been reused for a new node. Read the
  current statement before retrying; an explicit topic reassignment chooses a
  new placement rather than retaining the old bindings.
  Parent walks detect cycles by repeated node identity, not by path depth;
  deep acyclic topics remain readable after creation and subtree moves.
- A public handle's stem is the last topic component uppercased; every private
  descendant and unplaced task uses `TASK`. A stale stem resolves with a notice
  naming the current handle. `task list --topic /a` lists the `/a` subtree; a
  UUID filter selects that private namespace.
- Operators inspect stable IDs, descriptions and ordered routing/metadata
  evidence with `spice topic show /a/b`; `--after SEQ` resumes a history page.
  Reads only look up existing nodes and never create them.
  `spice topic describe /a/b "description"` creates or describes a node when
  the description is nonempty; clearing a missing path leaves it absent.
  Only an operator may preview or apply
  `spice topic move /a/b /destination/name`, which renames or moves the entire
  subtree and creates missing destination ancestors. Topic creation through
  the draft advance and every task statement or placement command retain their
  ordinary actor policy. A move cannot enter the reserved `/oops` intake.
  Topic descriptions and moves first return a plan; repeat the exact command
  with `--apply DIGEST` to commit it. A changed topic authority invalidates
  that plan. Missing sources, occupied destinations, reserved OOPS
  destinations, and cycles refuse without publication. These commands take
  complete public or UUID-owned private paths. Public and private subtrees may
  move within or across namespaces; every descendant keeps its stable node ID,
  while destination ownership applies to the moved paths. Routing, description
  and subtree-move history belongs to stable node IDs, not spellings.
  An anonymous leaf is pruned in the same transaction that removes its last
  live association, and pruning repeats through newly empty ancestors. Only
  pending work is live: settlement snapshots the final and router-selected
  rendered paths, releases their node bindings, and prunes in that same guarded
  transaction. Reopening resolves those paths against the current tree and
  allocates a new identity when the old spelling was fully pruned; it never
  resurrects the retired identity.
  A node remains only while it has a nonempty description, a child, or a pending
  task's final or router-selected reference. Task facts keep their authored
  topic text, terminal task heads keep their rendered paths, and release
  landings keep their rendered topic path without retaining or independently
  targeting a node. Pruning removes only projection rows, never immutable facts.
- Default reads (`task list`, `task status`, dry `task next` counts) use the
  actor route: every activated row plus the reader's own origin-thread rows, so an
  authored topic is never invisible to a lane that may be allocated it. This
  route is a visibility boundary, not a query filter.

## Routing

Creation first honors an absolute or private topic exactly. An unnamed
dependency created by the holder of its parent inherits that parent's exact
topic; claim-origin provenance alone does not make unrelated work a dependency.
Otherwise a relative tail asks the router for a public node and appends that
tail; omitted topics route as the draft section below describes. Ownership
never redirects that destination. Creation receipts report the placed topic,
its owners, and the informational router pick (or why no pick is available),
including when an explicit destination differs. See
[team policy](#team-policy) for drive-denied creation.

The router scores every described or pending-task-referenced public topic in one
TF-IDF fit. A task query contains its authored title, pitch, description and
acceptance or questions; creation and `task place` use the same field set.
Topic documents contain the node description and those same fields from at most
the 64 newest pending tasks for that node, without adding the pitch again.
Settled tasks and statements moved away from a node remain inspectable history
but supply no routing target or lexical weight. UUID-owned private namespaces
are never targets, and source code supplies no descriptions.
Routing uses the shared [matching vocabulary](rooms.md): short technical terms
such as `CSS/css`, `LSP/lsp`, `x86` and `p95` can distinguish public targets,
but rejected short flags and numeric fragments contribute no routing signal.
Private documents cannot supply a target or break a public tie. Empty queries
follow the same zero-overlap rules as other statements.

Routing is one source-bound hierarchical decision. The authority filter comes
first: the reserved `/oops` subtree, the policy's declared unmatched topic and
every node outside the creator team's drive roots are not candidates. Roots are
the literal public prefixes of the creator team's drive topic rules. An
unrestricted drive rule or a glob without a literal prefix (such as `**`)
starts at the public roots; an empty rule list supplies none.
Structural nodes need not be targets: eligible descendants remain reachable
across structural gaps. Final composition must satisfy exact drive and ownership
policy for the task's phase, not merely its routing root. Runtime switches still
govern actual activation and allocation; pausing an owner does not route around it.
Existing tasks retain their immutable creator team for `task place`; an invoking
operator does not replace it. Each new resolution rechecks whether that creator
team is live. Teamless and departed creators use unrestricted public roots
and the default policy's optional unmatched destination.

Evidence tiers then decide each level of the tree in a fixed order. The
`statement` tier scores the authored statement against topic descriptions and
the current task corpus. Only when that pass leaves a tie does the `origin`
tier add a bounded view (2048 bytes) of the task's origin: the parent task's
title, pitch and description, or the steering directive's text. The view
records its exact source revision and bytes; publication refuses a changed
origin rather than reusing its earlier evidence. Document and operator-Draft
provenance add nothing. Only after both tiers tie does the `path` tier compare
an authored relative tail against contiguous existing path components, then
use path cosine for unresolved choices. A full suffix match comes first;
otherwise the longest existing initial tail segment can locate a parent for a
new leaf. Earlier statement or origin decisions constrain the subtree: a suffix
elsewhere cannot override them. Node age, UUID order, popularity, recency and
room or session prose never break a tie.

Roots: the unique maximum at the earliest deciding tier wins. Unresolved roots,
an empty candidate forest, or a composed destination without drive/ownership
admission use an ordinarily admitted configured `unmatched` destination;
without one the router is undecided and names none. No topic is built in. An
authored tail composes under `unmatched` too.

Descent: a child must strictly improve on its parent at the deciding tier; a
unique best improving child is entered,
children that tie after improving stop at the parent with `children_tied`,
children equal to the parent through every tier stop with `children_equal`,
children all below their parent stop with `children_below`, and a node
without routable children is a `leaf`. No score floor or routing-driven phase
is added. Explicit complete destinations retain their authored path; their
counterfactual router result is informational.

Relative tails preserve their exact order and remain separate from the inferred
prefix. A unique existing match is reused; ambiguous matches compose at the
explained prefix rather than selecting a branch. Composition consumes the
largest exact overlap between the prefix's final components and the tail's first
components: `/app/render` plus `render/css` becomes `/app/render/css`;
`/app/css` plus `css/css` becomes `/app/css/css`; `UUID/app` plus `app/css`
becomes `UUID/app/css`. The owner UUID is never consumed. `/app` plus `app`
remains `/app`, recording overlap one and zero remainder.

The separate `structure` receipt records the authored tail, inferred prefix,
matched path and component count, composition overlap, and an overlap, meet,
known gap, unresolved or ambiguous connection. Gap components come only from
an existing connecting path; no middle names are guessed. An unslashed
`app/render` remains relative even if `/app/render` exists; an earlier-tier
choice elsewhere reports that root-anchored alternative. Structural evidence
does not fabricate statement cosine.

Every pick carries its evidence: `confidence` (`evidenced` when the statement
alone chose and stopped every step; `secondary` when origin or path was
needed to choose or stop; `low` for zero consulted overlap, an undecided child
stop or the unmatched topic), the weakest consulted `tier`, the `stop` reason,
the strongest consulted `overlap` in thousandths, the strongest `residual`
statement terms the destination's corpus lacks (at most 8), and a read-only
`suffix` recommendation naming the existing parent, those residual terms
spelled as candidate components, and the topic revision it was computed
against. The recommendation creates, renames, moves, merges, splits and prunes
nothing; topology remains an explicit operator action.

Restricted and unrestricted picks use the same scored node set after authority
removes only the current creator's configured unmatched subtree. A task placed
on an unmatched destination never contributes semantic corpus for any creator,
but the shared topic's description, path and deliberately placed work remain
available to other teams. The routing observation binds the chosen node or
unmatched path, topic revision, configured policy and team membership, exact
authored statement fields and relative tail, actual phase, bounded origin bytes
and source revision, and both decisions. Nothing is cached across observations.

Publication guards the captured tree, descriptions, task corpus, configured
policy, team membership and origin revision. Automatic creation recaptures a
stale routing snapshot or corpus generation internally from the unchanged
request; it never rewrites caller witnesses or retries violated source,
membership or policy assumptions. Explicit `task place --apply` still refuses
a stale preview. Later corpus, usage or topology changes affect only future
routing; activated tasks move only through explicit guarded `task place`.
Re-placement preserves a stored relative `TaskTopic.named` tail and composes it
once under the newly inferred prefix without changing phase. Editing the task
to an explicit complete topic can replace that tail.

Board replies carry the authoritative projection barrier captured before row
filtering, covering task statements, workflows and topic metadata.

`spice task place HANDLE` previews the restricted router pick and, when it
differs, the unrestricted pick from that same corpus. The receipt names the
destination's owners, current phase and any drive refusal. An available
`--apply=DIGEST` moves the saved topic without changing phase or releasing
claims. A draft stays in draft; `spice task file` is the separate advance.
An activated task cannot move to a drive-denied destination. The digest binds the
task, actor, destination, policy, membership and corpus observation; changed
inputs require a fresh preview, and a refused apply writes nothing.

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
reservation decision used by allocation. An empty owner set blocks automatic
work; dry `task next` reports it once per topic. Ownership does not revoke a
held claim or prevent an explicit claim. Reads and receipts bind ownership
to the admitted configuration, current membership, and observed topic nodes;
a concurrent topic move cannot seal a receipt for the old path.

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

- Every task statement stores a pitch: one plain, unlabeled sentence saying
  why the task matters, required on `task add` and refused when missing, blank,
  multi-line, or over 160 characters; a Design's pitch is its problem
  sentence. The description is the how: durable need, mechanism, and
  invariants, with no opener to carry.
- A creation with no author to ask derives its pitch once and never asks again:
  a review follow-up from the first substantive description sentence or else
  the title, a `task oops` report from its title. The appliance never rewrites
  an existing description. The Design request prints the stored pitch.
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
steps spice://task/HANDLE/history`; `task show --full` prints every block
whole. Where the holder works nothing collapses: the held-claim briefing and a
fresh claim's answer carry the whole description and every term, and every
assignment points at `task show HANDLE --full`. Task list and board rows print
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
uncontrolled and not submitted; never allocated. The operator's saved app
drafts, explicit `--draft` rows and drive-denied landings start here.
*Submitted*: uncontrolled and marked submitted; ordinary allocatable triage
work under drive policy, acquirable by `task next` and claimable. The mark is
set only by the author's single hand-off act, `task file` or creation without
`--draft`, is never cleared, is never set on a controlled row, and is not an
activation authority: a triage claimant still leaves draft only through the
validator below. Deferral stays a timed snooze; a wake time never becomes
intake authority.

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
