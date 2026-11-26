# App sidebar and task overview

## App sidebar

One URL-selected sidebar sits over mounted Chat at the right edge. Its shared
backdrop, header, Close, scroll, Escape, inert/ARIA and focus-return behavior
serves both Work and Spice. Contents swap without replaying the transition;
reduced motion removes it. Dismissal returns focus to a surviving launcher.

Attached accent rails use the **closed-edge** recipe: radius equals thickness.
Message cards, work total/task tabs and quota-account accents share it. The
closed composer strip's lower outer corners round without a strip-wide rail.
Structural dividers stay square. Standalone cards, media, dialogs, round
controls, pills and focus rings keep their radii; joined controls round only
outside endpoints.

## Task overview

Task activation opens the Work list for the complete selected task set: an
ordered, stable-deduplicated list derived from the locally admitted board in
its offered order. The Drafts, execution and Design totals form one contiguous
command-bar tab strip; each opens its complete global scope.

Work-scope selections use one canonical browser-location grammar:
`#/work` for all open execution work; `#/work/all` for all open work, combining
the execution and planning sets with no task in both;
`#/work/topic/{topic}` for open execution work under one exact topic path;
`#/work/drafts` for all open Drafts; `#/work/unplaced` for all open non-Design
work without an admitted topic, including Drafts, which remain Drafts until advanced;
`#/work/planning` for open Designs and their prospective Draft construction;
`#/work/planning/{topic}` for that planning set within one exact topic path;
and `#/work/team/{uuid}` for a team. Construction parents, not execution
dependencies, determine Design ownership. Activating a Plan cuts its ancestor's
gate; its later Draft children remain under the Plan's authority.
A topic is its complete absolute path, compared whole, never a leading
namespace: `/a` does not select `/a/b`. Team routes retain stable identity;
hydrated lane topology supplies current membership. All-open, all-work,
all-planning, and unplaced remain valid when empty. A missing topic or team
returns to Chat after its authority is available.

Tasks use `#/work/task/STAMP[/VIEW]`; ordered explicit selections use
`#/work/selection?task=A&task=B[&related=C]`; Spice uses `#/spice`.
Destinations push; selecting the current destination does nothing. Scope chips,
filter and graph adjustments replace. Back/Forward rehydrate the URL with Chat
mounted and restore the entry's own presentation: its open Work row, panel
scroll, Chat reading positions, graph viewport and node budget, passive Jobs
read and focused element, never a draft, request or credential. Close, Escape
and backdrop unwind only an owned panel chain to its Chat opener; a
direct-linked panel replaces to Chat. See the
[navigation checks](BROWSER.md#navigation) for grammar and refusal boundaries.

The sidebar's header puts its **Work** label and filter on the first line, then
wraps the scope chips **all**, **planning**, and **execution** below it. Each
chip counts open tasks by default and adds that scope's settled rows while
**Settled** is enabled. It opens its canonical route above; the list's
collapsed **done** group is unchanged.
The current chip is marked. A topic, team,
or single-task selection shows one context chip naming it in their
place; its × opens all work.

Above the rows, Work renders a relationship graph from the same filtered,
scope-selected roots, including yours roots kept **outside this filter**.
Its census separates roots shown, total selected roots, related nodes and
relationships. Bounded typed enrichment never truncates or reorders the list.
**Drafts** (default on) and **Settled** filter graph nodes and incident edges,
not list rows; a single-task address always draws its own task.
**Prerequisites** and **Dependents** toggle independently, including neither,
without changing depth or selected roots. Loading or refusal retains the graph.

Dependencies, Lineage, Reviews and Lifecycle share one bounded projection.
The default-on **Controllers** toggle overlays each preparation controller
directly to its controlled task with a dotted line, lighter than a dependency.
Activating a selected node opens and focuses its accordion row, expanding
**done** first for a terminal task and closing the prior row. An on-board
neighbor extends the URL selection and opens its row. Off-board neighbors are
context only and disappear when no visible incident edge attributes them;
external evidence never creates another detail surface.

At phone width, graph controls stack into 44px targets and its viewport is
capped before the scrolling list. Neither zoom nor long evidence may widen or
hide the rows.

The body lists every selected open task once, grouped by who has it, in this
order: **◆ yours**, only the operator can move it; **● working**, a live claim
names its lane; **○ ready**, anyone could take it; **⊘ waiting**, naming the
unsettled prerequisites it stands behind, or a resource pool; **◷ deferred**,
with the time until it returns; and **◌ draft**. The existing open-board read
also carries at most the 20 most recently finished rows, newest workflow first.
Those matching the current scope close the list under one collapsed
**✓ done** disclosure whose count is the number of rows it reveals.
Finished task and concluded-Design rows open the same in-place, read-only
detail with history. An exact task link remains readable after its row leaves
the bounded finished window: the selection keeps one **◌ history** row while
its targeted detail loads, changes that marker to **✓ history** once settled,
then renders the retained contract, standing, and history. Its lifecycle track
comes from that detail and its exact dependency-map root survives board
eviction. A prerequisite without admitted standing reads neutrally as a
prerequisite, never as a new wait. Only an exact `task::detail-unknown` becomes
**◌ lookup** and reads as not found; board absence alone does not. Each group
heads with its glyph, word, and count, an empty
group is omitted, and tasks keep the board's offered order within it. With
nothing yours, one **✓ nothing waits on you** line stands where that group
would. A task is yours when the [operator waits](#operator-acts) include it,
whatever its own standing.

Each row is one native button with `aria-expanded`: its glyph, its handle in
its kind's color (Design or execution), its title, its phase on its kind's
track, its holder, and its age. At desktop width a row is one line; on a phone
it reads two, where it stands and then its title, and every control the list
owns keeps a 44px target. Gold marks only yours rows and the operator's act.
Exactly one row stands open: the first yours task opens with the list, else a
lone selected task; opening a row closes any other, and the open row's own
button closes it. A new selection returns the open row to that default.

The filter matches a handle, a title, or the name of the lane holding the task,
ignoring case, and belongs to its Work address. It never hides a yours
task: one it rejects is listed under **◆ outside this filter**. When nothing
else matches, the list says so and offers **Clear filter**. An empty selection
reads **No open work**.

Work uses the board's exclusive standings: a live claim is **In flight**, a
lapsed claim **Stale**, and an explicit set-aside **Deferred**. The resident gate
then chooses **Waiting**, **Awaiting decision**, or **Blocked** before unclaimed
review, environment **Held**, and **Ready**, keeping counts equal to `spice board`.
**Waiting** means dependencies can advance; **Blocked** means missing authority,
a missing prerequisite, a cycle, or another wall requiring human repair.
A Design cycle never hides a wall from its dependents. The Design itself stays
**Awaiting decision** when that act is available, **Deferred** while deferred,
or **Blocked** in another cycle. Stale and awaiting-decision rows are transparent
to dependency walks: the underlying gate determines their dependents' standing.

A first detail read reserves its full sheet height; status and pending buttons
relabel in place. See
[largest footprint](../docs/invariants/lifecycle.md#surface-a-changing-region-holds-its-largest-footprint)
and [retained detail feedback](../docs/invariants/lifecycle.md#surface-retained-task-detail-does-not-move-for-background-feedback).
Lifecycle and action results use separate out-of-flow toast slots.

An open row shows the full title, live task handles, pitch, track, claimant,
review standing, and typed relationships. A relationship target already in the
selection focuses its row; another target replaces the selection. Blocker and
Draft rows keep the same title-and-pitch statement treatment.

A yours Design awaiting a decision renders its completed pass in place of the
generic detail body. A pass offering outputs carries output verdicts; one
offering none carries conclude, collapse, and return with accepted decision
evidence, released output evidence, and compact statement, workflow, and
claim witnesses. All variants share pending/refusal state and the
phone-pinned action bar; immutable decision history remains in the room.

The detail body pairs conversation with compact facts for status,
deferral, claim, lease, topic, origin, creator, and repository, stacking
conversation first below 920px. A mono link row leads the body, above the
facts, and opens room, acceptance or Design questions, description, facts, and
Design history in place under a crumb back; nothing above it changes with the
selection, so it never moves. These views retain exact decisions and output
standing. Task and lane identities navigate; journal fact codes and steering
origins remain text.

The task-owned room composer opens from one **Observe** control. **Add**
publishes an observation. Offered **Close**, **Conclude**, **Collapse**, or
**Delete** actions witness the displayed heads and confirm first. Close,
Delete, and a Conclude with a draft publish the draft as an observation and
settle only after it lands; a blank Conclude settles at once, and Collapse
settles at once with the draft as the Design's recorded collapse reason, which
it requires. Refused drafts are preserved. Settled work has no composer.
Connection loss keeps the exact draft read-only until reconnect, and generation
replacement hands it forward without posting it. Bounded detail reads hydrate
the complete selection; late answers never create hidden state.

A pending activated task's **Deferral** fact changes when it returns in place, on
the fact's own line. **Clear**, shown only while the task is deferred, wakes it
through `task::wake`; **Defer**, labelled **Change deferral** while deferred,
opens the browser's date-time picker, and the chosen local time defers the
task until then through `task::defer`.
Each witnesses the workflow and deferral heads the fact was drawn from, so a
change made against a stale view refuses. A landed change re-reads the board,
so the fact shows the new time, or a dash once cleared, without a reload. The
outcome, or the refusal, stays on the same line, in full on hover. Either
button is disabled when the runtime does not offer its command.
Deferring an open Design is the one way to stop it: no lane may possess it,
its invitations end, and a lane already holding it is sent one stop direction
asking it to checkpoint and `spice task free`. Explicit **Clear** makes it
possessable again, subject to its pass cycle.

## Operator acts

The operator waits include every task on the board that needs an operator act
now: an unclaimed, undeferred Design awaiting the
operator's decision on its completed pass, read as **Awaiting your decision**; a
task whose unsatisfied environmental requirement (trust grant, journal schema,
appliance version) needs the operator, unless that task is already set aside
(closed, Draft, parked, or deferred); and each
human-actionable Blocked gate or the task where its dependency chain stops. A
deferred task keeps its own explicit standing without becoming an operator wait
merely for waiting; it appears only when a Blocked dependency chain reaches it.
Every task behind a blocked or decision tab mark is listed here or holds behind
a listed wall. A Design ready for a pass marks its tab as a notice but never
appears here: a lane, not the operator, acts next. A
dependency on work an agent can claim or is working, or on an open
Design whose underlying gate can advance, is ordinary **Waiting** and never
counts. A Design awaiting your decision counts once as itself, never through
its outputs.
Each operator wait reads yours once, in board order, however many reasons or held
tasks reach it, and leaves on the board update that records the operator's
act. An opened yours row leads with that act: why only the operator can move
the task, the requirement or wall that holds it, and the tasks held behind it.

Trust decisions name one exact digest. An uncaptured or pended requirement
offers compact **Reject** and **Approve** controls. The browser sends the task,
statement revision, digest, requested ruling, and any current pended trust
head; it never supplies a filesystem path. The resident resolves the task's
one current linked producing worktree, re-reads its executable configuration,
and applies the ruling only when those bytes still produce that digest and the
operator, resident epoch, statement, claim, residency, and trust head remain
current at commit. Approval records verified provenance and grants the digest.
Rejection records durable revoked standing and leaves the requirement
unsatisfied. Granted and revoked standing are terminal on the card rather than
offering a second ruling.

The controls stay disabled while one ruling is pending and until the board
refresh catches up. A stale, ambiguous, unavailable, or unauthorized ruling
changes no authority and leaves a bounded refusal beside the same controls for
retry against current state. Journal-schema and appliance-version blockers
carry no controls: the app never renders an action the resident cannot
perform.

## Drafts and OOPS intake

Work's **Drafts** collection shows three standings: **controlled** (constructed
under an open controller, never allocated or claimed), **held** (saved app
drafts, explicit `--draft` rows and drive-denied landings, never allocated), or
**submitted** (handed off for ordinary triage under drive policy).

Activation follows the shared
[phase-zero validator](../docs/statements.md#phases-and-the-draft).
Refusal keeps the row, authored content, submitted mark and claim visible,
with its diagnostic and next action.

`spice task oops` records a controlled Draft Plan under the current
repository-owned OOPS Design. A pass refines the report into an
acceptance-bearing output the operator decides. The construction reference creates no execution
dependency, and there is no separate OOPS lifecycle state or action.

The global composer is the left companion to the right Spice sidebar. Its
circular operator-avatar launcher exactly mirrors the Spice button's size and
browser-edge position. The operator's face fills it inside a hairline rim,
without an initials band; a small badge carries the personal inbox's open count.
Beside it, **Ask**, **Task**, **Plan**, and **Design** hang as tabs matching
**Draft** and **Help**, the current destination wearing its accent, without a
strip-wide divider.

Choosing the launcher, a destination, its shortcut, or a quote expands that
same composer into a full-height sheet: writing, the To row with Send/Create,
then **Principals** and the independently scrolling personal inbox. On desktop
the non-modal sheets coexist. On a phone, Spice temporarily hides the left
sheet and closing Spice restores it. Opening and dismissal preserve the
laid-out composer and return focus outside.

**Ask** is the default; expanded, its writing line reads **Ask or steer
(required)**. Beside **To**, a wrapping row offers **Best-matched
agent**, pressed while none is chosen, and a checkbox per current agent. With
none chosen, the resident picks the best eligible agent from retained
conversation profiles, possibly starting a vacant one; acceptance closes the
sheet and focuses that agent's Chat composer, or toasts if the roster lacks
it; Back returns. One chosen agent is a named send. Several each get the whole
composition once, as one fanout needing all reachable; the composer holds
until every branch settles, then one toast names who was asked and why any
refused. Refusal keeps the composition with only refused agents chosen; full
acceptance clears it. Only best-matched Asks navigate.

The composer and Draft editor report progress and every acceptance, creation,
save, or refusal through the shared toast; neither owns a status line.
Command- or Control-Enter submits from any control. Adding Shift broadcasts an
Ask's text and direct images as one fanout to every occupied agent, ignoring
the choice while retaining drafted quotes; work modes do not broadcast. A held
stroke that would press Send/Create rings it as team SEND's command ring does,
in theme ink for an Ask's broadcast.

**Ask**, **Task**, **Plan**, and **Design** share one textarea and the strip's
horizontal glass selector. Kind, recipient, image count, and resizing preserve
editor geometry; Ask's recipients wrap beside the action.
See the [composer checks](BROWSER.md#draft-composer-parity).
The selected thumb and operator tab identify Ask as neutral, Task green, Plan
blue, and Design plum; the open sheet's inline-end edge mirrors the Spice sidebar's.
Quotes sit above the editor, and direct
images use the team composers' bounded thumbnail strip over
its lower inline end. Images can also be chosen, pasted, or dropped.

Draft defaults off, is disabled for Ask, and holds until reload. Send/Create
lights only when submission is possible: Ask needs text, a quote, or an image;
work needs title and pitch. With Draft off, Create files the work at once,
read-only from its admitted row; with Draft on it records an editable Draft. A
narrow closed tab ellipsizes its prompt.

For new work, the textarea's first line is the title, its next paragraph the
pitch, and later paragraphs plus quotes the description. Images become Draft
artifacts and are admitted as one set before sending, so refusal creates
nothing. Creation supplies no topic: advance from draft routes through the
saved topic, router, or creator team's unmatched destination. It also supplies
no terms, Design questions, or flow controls, leaving a submitted draft with
`task::term-missing` until an agent or editor adds the contract. Task defaults
to `todo,review`, Plan to `plan,review`, and Design to `design`, each after the
draft phase.

Recording opens task detail's full editor with its contract and images.
**Save Draft** writes only changed title, pitch, intent or description, never
acceptance or flow, and replaces images only when their set differs. Board,
room, connection and detail refreshes never rewrite edited text, selection
or focus. Reopening loads admitted images. A newer revision updates only
untouched fields or values matching the landed Save; the next Save witnesses
that revision. Refusal keeps all edits for retry. Opening another Draft starts
from its latest admitted content.
The editor's **Create** advances the draft as a complete ordinary Task, Plan,
or Design. A construction refusal marks the draft submitted, which closes the
editor; an authority refusal writes nothing. Success replaces the draft with
its activated task, which the allocator can offer at once.
Task detail shows the editor only on a controlled or held Draft that no lane
claims. A submitted Draft, or one a lane holds, is read-only, without the
editor, **Save Draft** or **Create**: a holder reshapes and files it, and the
editor returns once that claim is freed or lapses.
