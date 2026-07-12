# Task Documents

Status: implemented contract, 2026-07-12.

A **task document** is a markdown file that projects a goal onto the task
board as a fully connected dependency graph. Agents write task documents the
way they write any markdown — on the fly, mid-conversation, in one pass — and
pipe them in with `spice task ingest`. Piping the same document in again is a
no-op. Piping in a *tweaked* document reconciles: new work is created,
existing work is matched and preserved, new edges land, and nothing is ever
deleted. The board remains authoritative the moment rows exist; documents are
how graphs are born and grown, never how they are stored.

There is exactly one dialect, governed by one asymmetry: **reading is
forgiving, writing is strict.** Ingest accepts every unambiguous markdown
spelling of an intent — a bolded label, a criteria list, a checkbox, a
setext title — and puts each piece of content in the most specific
task-plane slot its shape implies. Descriptions and annotations are the
destinations of last resort. Export (`spice task ledger`) emits exactly one
normal form, so anything spice writes, spice re-reads to the same graph,
byte-stable from the first round trip on.

## Vocabulary

Every term below is used in exactly this sense throughout.

- **Task document** — a markdown file conforming to this dialect: structure
  by headings and lists, prose and field lines attached to nodes.
- **Node** — one task-to-be: minted by a heading or a list item. Carries
  title, slug, description, acceptance, annotations, priority, flow, due,
  tags, and edges.
- **Slug** — the node's identity within its family: the lowercase ASCII
  words of its title joined with `-`. Inline links contribute their text,
  never their URL, so retargeting a link does not re-identify a task.
- **Qualified slug** — a duplicated title's slug, extended with `--` and
  either its parent's slug or its distinguishing words; the parentless
  root alone keeps the bare slug (see Identity rules). `--` never occurs
  in a natural slug, so the qualified namespace is collision-free by
  construction.
- **Edge** (`A after B`) — A depends on B; B must complete before A is
  ready.
- **Root** — the node every other node descends from: the single parentless
  node, or a synthetic `Document root` when several nodes are parentless.
  The root completes last; its completion *is* the goal.
- **Rollup** — any node with children. A rollup depends on all of its
  children and becomes ready only when they are complete.
- **Leaf** — a node with no children. Execution always starts at leaves.
- **Origin** — the provenance token every task row carries: `ack:<key>`
  (minted by an acknowledged steering item) or `task:<handle>` (minted by
  a parent task). Origins are written once at creation and never edited.
- **Family** — all rows that carry a `taskdoc_id` attribute (see
  Matching) and share the document's `(project, origin)` pair. Apply
  matches nodes against family rows only.
- **Board** — the live task rows.
- **Apply** — what `spice task ingest` does: compute a plan that makes the
  family match the document, execute it, and report exactly what landed.
- **Plan** — the full set of apply verbs, computed and validated before any
  write. `--dry-run` prints the plan and stops.
- **Family edge** — an edge whose two endpoints are both family nodes: the
  only edges a document expresses and the only edges apply writes or
  diffs. Edges reaching rows outside the family belong to the board
  verbs; apply reads them for cycle validation and never changes or
  reports them.
- **Settled node** — a family row work has ever touched: status is not
  pending, its phase has advanced, or a claim has ever been taken on it.
  Settling is one-way — releasing a claim does not unsettle the row, so a
  board edit made under a claim stays durable. Documents never modify
  settled rows; they only report drift.
- **Creation surface** — the statements a document makes on a node:
  title, description, acceptance, family edges, priority, due, tags, and
  flow at creation. The surface apply equalizes while the node is
  unsettled.
- **Content column** — the column where a node's content begins: two
  columns past a bullet's marker, column zero for a heading's body.
  Indentation binds a line to the innermost node whose content column it
  reaches.
- **Loose row** — a family row the incoming document no longer lists.
  Reported, never deleted: the row, its fields, and its own outgoing
  edges stand. Edges from listed, unsettled nodes onto it follow those
  nodes' statements — an unlisted row cannot be named as an `After:`
  target. (Same word, same meaning as a loose commit: real work that the
  surrounding structure no longer names.)
- **Drift** — a difference between document and board that apply will not
  change, reported so the author can see it.
- **Warning** — a place where the classifier exercised judgment or
  discarded something: reported, never blocking. Like `loose` and `drift`,
  warnings are standing facts — the same document produces the same
  warnings until the author edits.
- **Normal form** — the single spelling the ledger writes: canonical
  labels, ATX headings (bold spans beyond level six), dash bullets,
  content-column indentation, collapsed blank runs, balanced fences,
  escapes where prose would otherwise read as structure.

## The Dialect

### Line classifier

A document is parsed in one top-to-bottom pass. Each line takes the first
matching rule:

| Line shape | Effect |
| --- | --- |
| inside an open fence | accumulate; the closer is a bare run of the opener's character, at least as long; on close the block — dedented to its opener — annotates the node the opener's indentation selects |
| ```` ``` ```` or `~~~` | open a fence (an unclosed fence swallows to EOF, is stored balanced, and warns) |
| `---` as the very first line | open frontmatter; the block through the closing `---` (or `...`) becomes one annotation on the root — unless a blank or heading inside proves the `---` was content, which replays the block as ordinary input, warned |
| whole-line `<!--` … `-->` | swallowed; a bare `<!--` swallows through the line carrying `-->` (warned at EOF if never closed); trailing prose after `-->` keeps the line prose |
| blank | paragraph break; closes a started acceptance capture and any annotation block |
| setext underline (`===`/`---` under a prose line) | the prose line becomes a heading: `===` → H1, `---` → H2 |
| thematic break (`---`, `***`, `___`, `- - -`) | dropped — a rule carries no information |
| line starting `\` + structural character, `12\.`, or `Label\: value` | **escaped prose**: description text; the backslash is markup and drops out of storage |
| ATX heading `#`–`######`, up to three leading spaces | **new node** — unless its slug names a field section (see Field sections); parent = nearest shallower heading |
| list item `-` `*` `+` `1.` `1)` | **new node**, criterion, or field, per List items below; parent = enclosing list item, else the current heading |
| field line (see Field lines) | field on the node indentation selects |
| sole bold span (`**Phase 1**` after a blank) | **new section node**, one level below the nearest real heading |
| line starting `>` or `\|`, a link definition (`[1]: url`), or a link-dominated line | annotation on the node indentation selects; contiguous same-shape lines form one block |
| anything else | description prose on the node indentation selects |

Lines indented four or more columns past their container's content column
are **indented code**: content, never structure, fields, or annotations —
pasted YAML, console output, and nested markdown are safe by the same
boundary CommonMark uses.

**Attachment follows indentation, not recency.** A line with no blank
above it continues whatever the previous line attached to (lazy
continuation). After a blank,
a line binds to the innermost open node whose content column it reaches:
prose indented under a bullet belongs to the bullet; column-0 prose after
a list belongs to the section. Preamble content — anything before the
first structural line — lands on the root once the root is known.

### Structure lines

**Headings.** `#` through `######`. A deeper heading is a child of the
nearest shallower one. Setext headings (`Title` over `===` or `---`) are
the same structure in another spelling. A paragraph consisting of a single
bold span (`**Phase 2: Rollout**`) after a blank line is a section one
level below the nearest real heading — the shape agents write constantly —
and earns a warning naming the promotion. A bold span under an H6 is a
level-seven node, and the ledger spells depth beyond six the same way
(see The Ledger), so nesting survives however deep sections go.

**List items.** Any standard marker: `-`, `*`, `+`, `1.`, `1)`. Indent
depth (spaces; a tab counts as four) nests items: a deeper item is a child
of the enclosing item. A list's top-level items are children of the current
heading. Three refinements:

- **Ordered runs start at 1 and chain.** `1.` opens an ordered run; any
  number continues an open run at the same indent. Each ordered item gains
  an `after` edge on its predecessor: markdown has exactly one construct
  whose semantic is sequence, and this is it. A numbered line that neither
  starts at 1 nor continues an open run is prose (`1985. It was a long
  night…`), warned. Items captured as acceptance criteria are exempt —
  criteria are not tasks, so their numbering carries no dependency
  meaning.
- **Checkbox markers are stripped — before every other reading.**
  `- [ ] Fix the parser` titles the node "Fix the parser". A `[x]` is
  discarded with a warning: a task document describes work to create, and
  the board owns status.
- **A bullet that is exactly a field line feeds its parent.**
  `- Acceptance: no crash on empty input` is a criterion on the enclosing
  node, not a task named "Acceptance: no crash…" — same for every label
  in the field set, and (markers stripping first) for checkboxed
  spellings like `- [x] Priority: high`. With no enclosing node at all, a
  field bullet feeds the root. An ordered field bullet feeds its parent
  without advancing the chain.

Headings and list items are the *only* structure. Everything else is
content.

### Field lines

A field line is a plain line of the form `Label: value`, attached to the
node its indentation selects. Labels are case-insensitive. Emphasis around
a label is tolerated on read (`**Acceptance:** x` is a field line); the
ledger writes the bare canonical label. The field set is closed:

| Field | Value | Accepted spellings |
| --- | --- | --- |
| `Acceptance:` | one criterion; repeat the line per criterion | `Acceptance criteria`, `Success criteria`, `Done when`, `Definition of done`, `AC` |
| `After:` | one or more slugs or titles, comma-separated; repeatable; forward references welcome | `Depends on`, `Blocked by`, `Prerequisites` |
| `Priority:` | `high`, `medium`, `low`, `none`; anything else refuses | — |
| `Flow:` | comma-separated phases from the approved set (`design`, `plan`, `todo`, `verify`, `review`); an unapproved phase refuses | — |
| `Due:` | a date the board's date parser accepts | `Deadline` |
| `Tags:` | comma- or space-separated tags | `Labels` |

`After:` targets may be slugs (`freeze-main`), qualified slugs
(`phase-1--freeze-main`), or titles (`Freeze Main`) — titles are slugified
before lookup, so slug equality is still the only match. An `After:` whose
comma-separated targets are not slug-shaped is a hard-wrapped sentence,
not a field: `After: that window closes, we archive the remainder.` stays
prose, and the report says so (`after-prose`). A target that matches no
node refuses; a bare target that names a duplicated title refuses as
ambiguous — use the qualified slug.

Repeating `Priority:`, `Flow:`, or `Due:` on one node keeps the last
value and warns; `Tags:` lines accumulate. A label-shaped line outside
the field set (`Owner: mei`, `Estimate: 3d`) is description prose, warned
when it looks like it wanted to be a field.

**Acceptance lists.** A bare `Acceptance:` (or any acceptance spelling,
emphasized or not) followed by a list captures that list as criteria, one
per item — numbered, dashed, or checkboxed alike. At most blank lines may
sit between the intro and its first criterion; any other line cancels the
capture and warns (`empty-acceptance`). A blank line after at least one
criterion closes the capture, so a following task list is never eaten.
This and the plain repeated field line are the same statement in two
spellings; the ledger writes the field-line form.

**Field sections.** A heading whose slug is `acceptance`,
`acceptance-criteria`, `ac`, `done-when`, `definition-of-done`, or
`success-criteria` is not a node: its list items are criteria on the
nearest shallower heading's node. Headings slugging to `dependencies`,
`depends-on`, `prerequisites`, or `blocked-by` feed `After:` targets the
same way. Headings slugging to `notes`, `context`, `background`,
`references`, `links`, `open-questions`, or `risks` feed the parent:
prose to its description, list items to a blockquote annotation. Each
earns a warning naming what fed what. Setext and bold-span spellings of
these headings open the same sections; any heading, at any depth, closes
one.

### Content lines

**Description.** Plain prose attaches to the description of the node its
indentation selects, preserving paragraph breaks. The description is the
node's working body. Storage is in normal form: lines are kept relative to
the node's content column (so indented code survives), tabs expand to
four columns, and blank runs collapse to one.

**Annotations.** Blockquotes, table rows, link definitions,
link-dominated lines, and whole fences attach as durable notes. Contiguous
lines of the same shape form **one** annotation block — a five-line
blockquote is one note, not five. A sentence that merely contains a link
stays description; only lines that are mostly link annotate. Annotations
survive to the board as row annotations in normal form.

**Escapes.** The classifier honors CommonMark's backslash escape at the
start of a line: `\- not a bullet`, `\# not a heading`, `1985\. a year`
(`12\)` alike), `After\: a sentence`, `\[a link](url)` for a
link-dominated line, and `\**Priority:** high` for an emphasis-wrapped
field shape are all prose, and the backslash — markup, not content —
drops out of the stored text.
The ledger writes these escapes wherever a stored line would otherwise
re-classify as structure, a field (bare or emphasized), or an annotation;
without them no exporter of this dialect could satisfy the round-trip
law.

### Discarded constructs

Legal markdown that carries no task meaning is discarded, not misparsed:

- **Thematic breaks** — visual separators; dropped, at any list depth.
- **HTML comments** — directives to no one; whole-line and multi-line
  bodies are swallowed (a bullet inside a comment is not work), a comment
  left open warns at EOF, and trailing prose after `-->` keeps its line
  prose.
- **Checked markers** — `[x]` is status, and the board owns status;
  stripped with a warning, wherever the checkbox appears.
- **YAML frontmatter** — a leading `---` block through `---` or `...` is
  stored as one annotation on the root, uninterpreted; a blank or heading
  inside aborts the block back to ordinary content, warned. Identity
  lives outside the document (see Families And Provenance), so there is
  nothing for frontmatter to say.

## Graph Construction

### Edge direction

Containment is dependency: a parent node depends on (`after`) each of its
children. Ordered runs chain siblings; `After:` lines add edges on top.
Dependencies point from goal toward prerequisite, so:

- Leaves are ready immediately; unordered siblings run in parallel.
- Ordered siblings run in sequence — that is what the numbers say.
- A rollup becomes ready when its last child completes.
- The root completes last. The graph reads as "to finish the goal, finish
  its sections; to finish a section, finish its items."

### Root resolution

A single parentless node is the root. When several nodes are parentless —
a bare list, or several top-level headings — a synthetic root titled
`Document root` (slug `document-root`) is added, depending on each of
them. Preamble content lands on the root either way. Every applied
document is therefore a weakly connected DAG — by construction, not by
validation. There is no such thing as a floating task or a second
component. The slug `document-root` is reserved for the synthetic root: a
parentless task titled `Document root` beside other parentless nodes
refuses; nested under a parent, the title qualifies like any duplicate.

### Identity rules

Slugs make reconcile possible, so titles carry identity obligations:

- **Titles contain at least one ASCII word** (`[a-z0-9]+` after lowering).
  A title that slugs to nothing is a refusal. Non-ASCII text is welcome in
  titles; it cannot be the *only* thing there.
- **Duplicated titles take qualified slugs — every occurrence,
  symmetrically.** Structure qualifies first: an occurrence whose parent
  distinguishes it takes `<parent-slug>--<slug>` (`phase-2--update-changelog`).
  Occurrences sharing a parent take their distinguishing words instead —
  the slug-words of the full title, links included, that none of their
  rivals carry (`update-the-guide--v1`, `update-the-guide--v2`). A
  parentless occurrence has no parent to name: root position is its
  qualifier, so it keeps the bare slug. Each qualification warns. When
  neither structure nor wording distinguishes two occurrences, the
  document refuses: distinct work deserves distinct names, and nesting
  or wording counts as a name. Structure-qualified
  slugs never re-key as the document grows — the qualifier is the parent;
  a wording-qualified slug re-keys only when a new occurrence claims a
  rival's distinguishing words, and every qualification warns, so a
  re-key is always visible in the report. Qualification is export-closed:
  edges always name qualified slugs.
- **Titles are stable.** The slug is the node's identity across applies.
  Retitling a node makes a new node and lets go of the old one — sometimes
  that is what you mean; the report will show both verbs. This holds for
  the root like any other node: rename the goal and the family survives,
  because family identity does not live in any title (see Families And
  Provenance). The moment a title becomes duplicated its occurrences
  re-key to qualified slugs — one loose row per occurrence, and a
  structure-qualified slug then holds steady however the document grows.

### Document validation

Parsing refuses (see Refusals) when: the document is empty; a title has no
ASCII words; duplicated titles cannot be distinguished; a parentless title
collides with the synthetic root; a `Priority:` or `Flow:` value is not in
its approved set; an `After:` target matches no node or names a duplicated
title bare; or the document graph (containment plus chains plus `After:`)
contains a cycle. A document that parses is a valid, fully connected DAG.

## Families And Provenance

### Identity is origin

Family identity is `(project, origin)` — the project the graph lives in
and the provenance token the whole graph shares. Nothing inside the
document carries identity: not the title of any node, not any marker, not
any metadata block. The document stays pure intent; identity rides on
provenance, exactly as it does everywhere else on the board.

This is an invariant where no in-document key can be: origins are minted
once by the steering loop (`ack:<key>`) or by parenthood (`task:<handle>`),
written at creation, and never edited. Any title, any heading, any slug
can be reworded tomorrow — and when the root's title changes, the blast
radius is one node's identity, not the family's.

### Origin resolution

`spice task ingest` resolves origin exactly as task creation does
everywhere: `--origin` explicit, or inherited from the active claim. That
gives an agent as many families as it needs, for free:

- **Evolve a graph:** reuse the origin. Same `(project, origin)`, tweaked
  document, apply reconciles.
- **Start a graph:** use a fresh origin. Every acknowledged change request
  mints one; a claimed task supplies one.
- **Chain efforts:** point the new family's origin at prior work —
  `--origin task:<root-handle>` of the graph it grows out of. Provenance
  chains through the origin grammar the board already has; no new
  machinery, no in-document markers. The chain names a row, not a title —
  retitle that goal later and the chained origin still points at the
  original root row, which is what provenance means.

Inheritance follows the *active claim*: two pipes under different claims
are two families. Evolving one graph across claim boundaries means
passing `--origin` explicitly — that is what the flag is for.

One origin, one document — and the document is the **complete statement**
of its family's creation surface. What it lists exists; what it stops
listing goes loose; what it cannot express (other families' rows, edges
that leave the family, runtime state) it cannot disturb. Because every
applied row round-trips through `taskdoc_id` and the family pin, the
board itself is the only memory apply needs: there is no stash, no
lineage file, no state beside the rows (see The Board Is The Memory).

### Matching

Within the family, matching is by node slug, recorded on each created row
as a system-owned `taskdoc_id` attribute, written atomically with the
row's creation. Its sibling `taskdoc_parent` records containment —
parenthood must live on the row, because nesting and cross-edges land
identically in the depends set, and without it no export could rebuild
the document's shape (see The Ledger). A slug matching exactly one family
row is that row. A slug matching more than one family row is a refusal —
ambiguity is the one thing apply will not guess about. Completed family
rows still match (as already-satisfied work); deleted rows never match.
Rows outside the family are invisible: a bullet titled "Add tests" here
never collides with an "Add tests" from another origin, another project,
or last quarter.

Documents never contain these attributes; agents never write them by
hand. Matching is by title, and title stability (see Identity rules) is
the whole mechanism.

## Apply Semantics

`spice task ingest PATH --project <project> [--origin <origin>]` — with
`-` reading stdin, because agents pipe — is **apply**: make the family
match the document, creating what is missing, preserving what exists,
touching nothing that work has settled, deleting nothing ever.

### The plan

Apply computes a complete plan before writing anything:

1. Parse and validate the document.
2. Resolve project and origin; load the family; match nodes to rows by
   slug.
3. Compute a verb for every node and every difference.
4. Validate the **post-state graph** — the board's edges as the plan
   will leave them: planned family-edge drops removed, planned adds
   included, every edge beyond the family kept (a document edge can
   close a cycle through a chain that leaves the family). A cycle
   anywhere is a refusal — and when it runs through board-owned edges
   outside the family, the way out is the board verbs, not the document.
5. Execute. Any refusal fires before the first write.

`--dry-run` stops before execution and prints the plan as a report.

### The board is the memory

Apply is a function of exactly two inputs: the document and the board.
There is no third thing — no stash of the last document, no lineage
file, no mode switch. The round-trip law (see The Ledger) is what makes this
possible: the family's rows *are* the last statement, reconstructable at
any time, so diffing the incoming document against the family answers
every question a separate memory could:

- A matched, unsettled node's creation surface is made **equal to the
  document's statement, in normal form** — additions, removals,
  rewordings, and reorderings are all one rule. There is no union step
  and no removal-intent puzzle: the document is the complete statement,
  so an acceptance item or family edge it no longer states is gone by
  authorship, not by inference.
- A matched, settled node is never modified; each difference is a
  `drift` line. New annotations still append — they are a log, not
  creation surface.
- A family row the document no longer lists is `loose`: the row, its
  fields, and its own outgoing edges stand untouched; edges from listed,
  unsettled nodes onto it follow those nodes' statements. No memory is
  needed to say so — the `taskdoc_id` proves the row is document-born,
  and the complete-statement rule makes "unlisted" mean "let go".
- Edges are diffed only over the **family-edge domain**. Where the
  dependent node is unsettled, its family-edge set is made equal to the
  document; where it is settled, differences become drift. An edge tying
  a family row to outside work — sequencing added with `spice task
  depends` against another effort — is not expressible in a document
  and is therefore never added, dropped, or reported by apply.

The consequence to hold onto: board-side edits to an *unsettled* family
node's creation surface are provisional — the next apply restores the
document's statement, because that is what document ownership means. To
make such an edit durable, fold it into the document; work that has
begun is already protected by the settled boundary.

### Field policy

| Field | Class | Unsettled node | Settled node |
| --- | --- | --- | --- |
| title / slug | identity | immutable — a new slug is a new node | immutable |
| family edges (`after`) | statement | made equal to the document | differences become drift |
| acceptance | statement | made equal to the document | drift |
| description | statement | made equal to the document | drift |
| priority | statement | made equal to the document | drift |
| due | statement | made equal to the document | drift |
| tags | statement | made equal to the document | drift |
| annotations | append-only log | append new, dedup by block | append new, dedup by block |
| flow | creation-only | set at create; later changes are drift | drift |
| edges leaving the family | board-owned | never touched | never touched |
| taskdoc_id, taskdoc_parent | system | written at create, never edited | never edited |
| project, origin | family identity | fixed at first apply | — |
| wait, scheduled, until | board-owned | never touched | never touched |
| claims, phases, review, validation | runtime | never touched | never touched |

The settled boundary is the single ownership rule: **the document owns a
node until work begins; the board owns it after.** A document can reshape
tomorrow's work freely and can never rewrite yesterday's.

### Verbs

Every plan line is one of:

| Verb | Meaning |
| --- | --- |
| `created <slug> <handle>` | new row minted (leaves first, root last) |
| `reused <slug> <handle>` | matched, nothing to change |
| `updated <slug> <handle> <field>` | a statement field made equal to the document on an unsettled row |
| `edge-added <slug> -> <slug>` | new dependency landed |
| `edge-dropped <slug> -> <slug>` | a family edge the document no longer states, removed |
| `loose <slug> <handle>` | family row the document no longer lists; untouched |
| `drift <slug> <handle> <field>` | document and board disagree where the board owns the field |
| `warn <line> <code> <message>` | the classifier exercised judgment or discarded something |

The first line of every successful report is `root <handle>`. Order is
deterministic: `created` in creation order (post-order over dependencies),
then `reused`, then `updated`, then edge verbs, then `loose`, `drift`, and
`warn` (each in stable order). Exit code 0. A refusal is a single
`spice: <sentence>` on stderr, exit code 2, zero writes.

`loose`, `drift`, and `warn` are standing facts, not events: they repeat
on every apply, verbatim, until the author resolves them — re-list the
node, match the field, reword the line, or reach for the board verbs. A
report is a statement of where document and board stand, not a changelog,
at any scale: a 200-node steady-state apply is 200 `reused` lines and its
standing facts — boring, grep-able, and correct. The warning channel is
what lets reading stay forgiving without becoming silent: every heuristic
that guesses also says so, in the report, every time.

### Laws

- **Reentrancy.** Apply reads the document and the board and nothing
  else. Same document, same board: same plan, same report — from any
  worktree, any agent, any time.
- **Idempotence.** Applying the same document twice: the second apply
  writes nothing — every matched row reports `reused`; `loose`, `drift`,
  and `warn` lines repeat verbatim; the board is byte-identical.
- **Monotone safety.** No apply ever deletes a row, reopens completed
  work, or modifies a settled row.
- **Truthful report.** Refusals fire before the first write. After
  execution the report states exactly what landed: a row that settles
  between plan and write demotes its planned statement verbs —
  `updated`, `edge-added`, and `edge-dropped` alike — to `drift` lines;
  everything else still lands.
- **Recovery by re-apply.** An interrupted apply leaves the family
  partway; applying the same document again converges — rows already
  created match and report `reused`, and the remainder lands.
- **One writer at a time.** Apply holds no lock; two agents applying
  into one family race. The damage is bounded — matched slugs reconcile
  idempotently, and racing creations surface as the family-ambiguity
  refusal on the next apply — and recovery is deliberate: `spice task
  delete` the extra row, or re-point one document's origin.
- **Convergence.** Apply, then ledger: the exported document parses to
  the same graph as the applied document, minus drift the report already
  named.

## The Ledger

`spice task ledger HANDLE` exports the named row's family — the rows
sharing its `(project, origin)` — as a task document in **normal form**,
the strict half of the dialect's one asymmetry. Containment is rebuilt
from `taskdoc_parent`; the remaining family edges become `After:` lines;
edges that leave the family are not expressible in a document and do not
export, exactly as apply never touches them. Normal form means:

- Canonical spellings only: ATX headings through level six and bold-span
  sections beyond, `-` bullets, plain field lines with canonical labels
  (`Acceptance:` per criterion, one `After:` line of comma-separated
  qualified slugs, `Priority:`, `Flow:`, `Due:`, `Tags:`).
- Containment rendered as nesting; every non-tree edge as an `After:`
  line; child order stable (board creation order).
- Node content at its content column: fields first, then description
  paragraphs, then annotation blocks. Bare bullets pack into tight lists;
  bullets with content take surrounding blanks.
- Blank runs collapsed; fences balanced; frontmatter (if any) first.
- A backslash escape on any stored line that would otherwise re-classify
  as structure, a field, or an annotation.

Two laws bind the ledger to the classifier:

- **Round trip:** `parse(ledger(apply(D)))` and `parse(D)` describe the
  same graph — every slug, title, parenthood, field, acceptance list,
  description, annotation block, and cross-edge.
- **Fixed point:** exporting what an export re-imports is byte-identical:
  `ledger` output is already in normal form and never churns. This is
  what makes the editing loop — ledger out, reshape, apply back — safe to
  automate: a ledger diff shows the author's edits and nothing else.

One honest caveat: annotations extracted from between description
paragraphs cannot be re-interleaved; the first cycle moves them after the
description, and every cycle thereafter is stable. Runtime state (claims,
phase positions, review evidence, validation) never appears in a task
document, exported or authored. Documents describe work; the board holds
its history.

## Refusals

Everything that can fail fails loudly, completely, and before any write:

| Refusal | Trigger |
| --- | --- |
| `task document is empty` | empty or whitespace-only document |
| `task document has no nodes (content but no structure)` | content, but no heading or list item anywhere |
| `title has no ASCII words: <title> (line <n>)` | slug would be empty |
| `duplicate title in document: <slug> (lines <n> and <m>; nothing distinguishes them)` | colliding titles that neither structure nor wording can qualify; fires once per title |
| `title collides with the synthetic root: <title> (line <n>); rename or nest it` | a parentless task titled `Document root` in a document that needs the synthetic root |
| `unknown After target: <target> (line <n>)` | `After:` names no node in the document |
| `ambiguous After target: <target> (title is duplicated; use a qualified slug)` | a bare `After:` target names a duplicated title |
| `invalid priority: <value>` | `Priority:` outside `high`/`medium`/`low`/`none` |
| `invalid flow phase: <phase>` | `Flow:` names an unapproved phase |
| `dependency cycle at <slug>` | cycle in the document graph or the post-state graph |
| `acceptance criterion on <slug> contains '\|'` | a criterion the row surface's pipe join cannot round-trip; escape or reword |
| `<slug> is ambiguous in family: <handle>, <handle>` | node slug matches multiple family rows |
| `missing project` | no `--project` and no active claim to supply one |
| `missing origin` | no `--origin` and no active claim to supply one |

A refusal names the offending slug and, where recovery is not obvious, the
way out. A refusal never leaves partial writes — it fires before the first
one; an apply interrupted mid-flight is recovered by applying again
(the recovery law), never by hand.

## Warnings

The report's third voice, between refusal and silence. Warnings never
block and never change the graph; they say where the classifier exercised
judgment so the author can confirm or reword:

| Code | Fires when |
| --- | --- |
| `bold-heading` | a sole bold span was promoted to a section |
| `field-section` | a heading fed its parent instead of minting a node |
| `dup-qualified` | a duplicated title took its qualified slug |
| `checked-discarded` | a `[x]` marker was stripped; the board owns status |
| `ordered-start` | a numbered line neither started a list at 1 nor continued one; kept as prose |
| `indent-code` | a list-shaped line inside indented code was kept as content |
| `field-repeat` | `Priority:`/`Flow:`/`Due:` repeated on one node; last value won |
| `fieldish-prose` | a label-shaped line outside the field set landed in a description |
| `after-prose` | an `After:`-shaped line whose targets are not slug-shaped stayed prose |
| `empty-acceptance` | an acceptance intro captured no criteria |
| `url-title` | a title carries a URL (its slug does not) |
| `long-title` | a title long enough to deserve decomposition |
| `unclosed-fence` | a fence ran to EOF and was stored balanced |
| `unclosed-comment` | an HTML comment ran to EOF |
| `unclosed-frontmatter` | a frontmatter block ran to EOF; replayed as content |
| `frontmatter-abort` | a blank or heading inside frontmatter; replayed as content |

## The Authoring Contract

Ten rules. An agent that follows them can free-write a document
mid-conversation, pipe it in, keep working, tweak it, and pipe it in again
— and the board will track the document for exactly as long as the
document deserves to lead.

- **One origin, one document.** Reuse the origin to evolve this graph;
  mint or pick a fresh origin for a new effort; point a new family's
  origin at the work it grows from (`task:<handle>`). Identity is
  provenance — never something you write inside the document.
- **Structure is headings and list nesting.** Write ATX headings and
  dash bullets and the ledger will never rewrite you; setext titles,
  bold-span phases, and checkboxes are understood, and the report says
  so when they are.
- **One task, one line.** Each bullet or heading is a unit of work with
  an imperative, specific, ASCII-bearing title. An H1 naming the goal is
  good practice — it names the root — but a bare list is a complete
  document.
- **Number what must run in order; `After:` for everything else.**
  Ordered siblings chain; unordered siblings run in parallel — that is a
  feature, so only sequence what truly must wait. `After:` says what
  neither nesting nor numbering can: a shared prerequisite, a diamond, a
  section that follows its siblings.
- **Give ready work acceptance.** A plain `Acceptance:` line per
  criterion, or a criteria list under one `Acceptance:` intro — both are
  the same statement. A node with no acceptance and no `Flow:` routes to
  a `plan` phase and will demand decomposition before execution — use
  that deliberately for unshaped work.
- **Titles are identity — keep them stable.** Rewording a title creates
  a new task and leaves the old row loose. Reword descriptions and
  acceptance freely; rename tasks only when you mean replace. Duplicated
  titles qualify by section or by their own distinguishing words — but a
  title only you can tell apart is a title only you can re-find.
- **Attach prose where it belongs and indent it there.** Content binds
  by indentation: a bullet's body sits indented under the bullet;
  section prose sits at the section's margin; goal-level context goes at
  the top of the document.
- **Tweak and re-pipe freely; read the report.** Adding a bullet, an
  edge, a criterion, a paragraph — all reconcile. `created`/`updated`/
  `edge-added` confirm your change; `drift` means the board owns that
  field now; `loose` means the document let go of a row the board still
  holds; `warn` means the classifier made a call you can confirm or
  reword. Deal with loose rows deliberately — usually fine to leave, but
  a loose *rollup* is still a claimable row; delete what the family
  should truly forget.
- **The document leads until work begins.** Once a task is claimed or
  advancing, the board owns it; stop steering settled work through the
  document and use the task verbs (`note`, `edit`, `depends`, `review`).
- **Never write identity markers.** No `taskdoc_id` attributes, no
  comment tags, no metadata blocks. If you find yourself inventing
  markers to control matching, stop — matching is by title within your
  origin's family, and the one-origin and stable-titles rules are the
  whole mechanism.

## The Ladder

Worked examples, smallest to largest. Reports show the plan verbs; handles
and origins are illustrative.

### Blank page

```
(empty file)
```

```
spice: task document is empty
```

The empty document is a refusal, not an empty graph.

### The smallest document

```markdown
- Ship the login fix
```

```
$ spice task ingest note.md --project task.login --origin ack:20260710T163000Z
root LOGIN-1kD2xPQr
created ship-the-login-fix LOGIN-1kD2xPQr
```

One bullet is a complete task document: a single-node graph whose root is
its only leaf. Identity came from the flags, not the text — the same
bullet under a different origin is a different family.

### A bare list

```markdown
- Fix the session timeout
- Update the login form copy
- Add a regression test
```

```
document-root ─┬─> fix-the-session-timeout
               ├─> update-the-login-form-copy
               └─> add-a-regression-test
```

Three parallel leaves under a synthetic root that completes last. Naming
the goal with an H1 is better practice (the next rung), but the basic
list works.

### A named goal

```markdown
# Login hardening

Tighten the whole login path before the audit.

- Fix the session timeout
- Update the login form copy
- Add a regression test
```

Same shape, but the root is now `login-hardening` and carries the
preamble prose as its description. The H1 names the goal; it does not
identify the family — the origin does.

### Numbers are sequence

```markdown
# Deploy 2.0

1. Freeze main
2. Cut the release branch
3. Run the soak test
4. Promote to fleet
```

```
deploy-2-0 ─┬─> freeze-main <── cut-the-release-branch <── run-the-soak-test <── promote-to-fleet
            ├─> cut-the-release-branch
            ├─> run-the-soak-test
            └─> promote-to-fleet
```

Four chained steps: each waits for its predecessor, the root waits for
all. Write dashes when siblings may run in parallel; write numbers when
they must not.

### Cross-edges: what nesting cannot say

```markdown
# Cut the release

## Backend ready

After: migrate-schema

## Frontend ready

After: migrate-schema

## Migrate schema

Shared prerequisite for both tracks.
Acceptance: schema version bumped and deployed
```

```
cut-the-release ─┬─> backend-ready ──┐
                 ├─> frontend-ready ─┴─> migrate-schema
                 └─> migrate-schema
```

A diamond in plain markdown: both tracks name the shared prerequisite
with `After:`. Sibling sections plus `After:` lines express any DAG.

### Acceptance, both spellings

```markdown
# Harden the importer

- Reject unknown columns

  Acceptance:
  - unknown column aborts with a named error
  - error names the offending header
  - exit code is 2

- Stream rows
  Acceptance: memory stays flat for 1M-row files
```

Three nodes — the goal and two tasks — four criteria, zero extras: the
criteria list under the bare intro and the plain field line are the same
statement. The blank line after the last criterion is what hands the next
bullet back to the task list. The ledger writes both tasks in the
field-line spelling.

### The full field set

```markdown
# Importer hardening

Stabilize the CSV importer before the pilot.

- Reject unknown columns
  Acceptance: unknown column aborts with a named error
  Acceptance: error names the offending header
  Priority: high
- Stream rows instead of slurping
  Keep memory flat for 1M-row files.
  After: reject-unknown-columns
  Due: 2026-08-01
  Tags: importer, perf

> Context: pilot customer sends 2GB files.
```

Fields bind to the bullet they sit under; the closing blockquote sits at
column 0, so it annotates the goal — attachment follows indentation, and
the graph reads the way the page looks.

### Duplicates qualify, or refuse

```markdown
# Rollout plan

## Phase 1

- Ship to canary
- Update changelog

## Phase 2

- Ship to fleet
- Update changelog
```

The two chores become `phase-1--update-changelog` and
`phase-2--update-changelog`, each warned. Two bullets differing only in
their link targets qualify by their distinguishing words
(`update-the-guide--v1`, `update-the-guide--v2`). Two identical bullets
under one parent refuse: nothing distinguishes them, so nothing can
re-find them either.

### The apply sequence — the reason this dialect exists

Day one, working under a claim, the agent pipes in (origin and project
inherited from the claim):

```markdown
# Q3 importer hardening

- Reject unknown columns
  Acceptance: unknown column aborts with a named error
- Stream rows instead of slurping
```

```
$ spice task ingest plan.md
root IMPORT-1kD3Qb2n
created reject-unknown-columns IMPORT-1kD3Qa8v
created stream-rows-instead-of-slurping IMPORT-1kD3Qb1c
created q3-importer-hardening IMPORT-1kD3Qb2n
```

Work proceeds; `reject-unknown-columns` gets claimed and completed. The
agent learns streaming must wait for the column work, adds a criterion,
and adds a new task — then pipes the tweaked document in again:

```markdown
# Q3 importer hardening

- Reject unknown columns
  Acceptance: unknown column aborts with a named error
- Stream rows instead of slurping
  Acceptance: memory stays flat for 1M-row files
  After: reject-unknown-columns
- Add a 2GB fixture to CI
```

```
root IMPORT-1kD3Qb2n
created add-a-2gb-fixture-to-ci IMPORT-1kD5x8Wm
reused reject-unknown-columns IMPORT-1kD3Qa8v
updated stream-rows-instead-of-slurping IMPORT-1kD3Qb1c acceptance
edge-added stream-rows-instead-of-slurping -> reject-unknown-columns
edge-added q3-importer-hardening -> add-a-2gb-fixture-to-ci
```

Nothing was deleted, nothing refused, the completed row matched as
satisfied work, and the tweaks landed as verbs. This loop — generate,
pipe, work, tweak, pipe — is the entire point.

### Loose rows and drift

The agent later drops the fixture bullet and rewords the streaming task's
description while that task is actively claimed:

```
root IMPORT-1kD3Qb2n
reused reject-unknown-columns IMPORT-1kD3Qa8v
reused stream-rows-instead-of-slurping IMPORT-1kD3Qb1c
loose add-a-2gb-fixture-to-ci IMPORT-1kD5x8Wm
drift stream-rows-instead-of-slurping IMPORT-1kD3Qb1c description
```

The claimed row is settled — the board owns its description now; the
document's rewording is reported, not applied. The dropped bullet's row
survives untouched, reported loose. Both lines are information, not
failure — and both will repeat on every apply until resolved, because a
report states where things stand, not what changed a moment ago.

### Renaming the goal

The agent retitles the H1 from `# Q3 importer hardening` to
`# Importer pilot readiness` and re-applies (the streaming task is still
claimed, its description still reworded):

```
root IMPORT-1kD6mBqT
created importer-pilot-readiness IMPORT-1kD6mBqT
reused reject-unknown-columns IMPORT-1kD3Qa8v
reused stream-rows-instead-of-slurping IMPORT-1kD3Qb1c
edge-added importer-pilot-readiness -> reject-unknown-columns
edge-added importer-pilot-readiness -> stream-rows-instead-of-slurping
loose q3-importer-hardening IMPORT-1kD3Qb2n
loose add-a-2gb-fixture-to-ci IMPORT-1kD5x8Wm
drift stream-rows-instead-of-slurping IMPORT-1kD3Qb1c description
```

Title is identity, so the renamed root is a new node and the old rollup
goes loose — but the family and its matched rows survive untouched,
because identity lives in `(project, origin)`, not in any title. The
blast radius of a rename is one node, by design. The fixture row's
`loose` line from the previous rung repeats too — still in the family,
still unlisted, still standing.

### Everything at once

One document exercising the whole dialect — three heading levels, both
list flavors, nested ordered steps, field lines, prose, quote, table,
link, and fence:

````markdown
# Q3 platform hardening

The umbrella goal for the quarter.

Acceptance: all tracks verified in staging

## Storage layer

Owns durability. See [design doc](https://example.com/storage).

- Write-ahead log
  - Frame format
    Acceptance: fuzzer runs 1h clean
  - Recovery replay
    1. Replay in order
    2. Verify checksums
- Compaction
  Priority: low
  Compact only cold segments.

## API surface

### Public endpoints

- POST /ingest
- GET /status
  Acceptance: p99 under 50ms

### Internal endpoints

* Health probe
* Metrics scrape

  ```sh
  curl -fsS localhost:8080/healthz
  ```

## Rollout

After: storage-layer, api-surface

| stage  | gate        |
| ------ | ----------- |
| canary | error rate  |
| fleet  | p99 latency |

Ship canary first, then fleet.
````

Sixteen nodes, a four-deep tree, two `After:` cross-edges making
`rollout` wait for both sibling tracks, and one chain edge making
`verify-checksums` wait for `replay-in-order` — replay in order *says* in
order. The sentence with the link stays in `storage-layer`'s description;
the fence annotates `metrics-scrape` because its indentation reaches the
bullet's content column; the table is one annotation block on `rollout`
and the closing prose is its description; every acceptance line sits on
its own node.

## Non-Goals

- **No structured input dialect.** Task documents are markdown. If a
  machine-to-machine format is ever needed, it will be a plain `.json`
  file on its own flag — never embedded in a document.
- **No structured report.** The apply report is lines for reading, not a
  payload for parsing — its grammar is stable for eyes, not a contract.
  Tooling that wants graph state queries the board; a machine-readable
  report would invite agents to build on the receipt instead of the
  truth.
- **No in-document identity.** No frontmatter keys, no id markers, no
  comment tags, no reserved headings. Identity is `(project, origin)`,
  full stop.
- **No pruning.** Documents cannot delete. `spice task delete --reason`
  is a human-sized verb and stays one — and apply recreates any listed
  row someone deleted, so let the document drop a task before deleting
  its row.
- **No status in documents.** Checkboxes are read for their titles and
  their state is discarded; done-markers, claim owners, phases live on
  the board; ledger holds history; documents hold intent.
- **No tables as task sources.** A table is one annotation block. Minting
  work from cells is a guess the classifier refuses to make.
- **No wait/scheduled/until fields.** The task plane supports them; no
  authoring demand has been demonstrated. The field set grows by
  evidence, not symmetry.
- **No per-node project.** One document, one project, one family.
- **No fuzzy matching.** Slug equality only. Qualification is
  deterministic naming, not similarity; there is no title distance, no
  positional guessing.
- **No chain markers.** Provenance chaining is the origin grammar itself
  (`task:<handle>`); no extra chain field until real use demands one.
- **No memory beside the board.** Apply keeps no copy of past documents,
  no lineage files, no per-family state. Anything a future apply needs
  must be recoverable from the document and the rows — and it is (see
  The Board Is The Memory).
- **No steering settled work.** Documents shape the future; the task
  verbs govern the present.

## Implementation Notes

For the implementing battery, the seams are: `spice/tasks/markdown.py`
(dialect, plan, apply), `spice/tasks/cli.py` (`ingest --dry-run`,
`ledger`), family-scoped row matching in place of global board scans,
and the family-edge domain rule in the diff. Origin resolution reuses the
creation-path resolver; project inheritance from the active claim is new
surface. Creation defaults (auto-due SLAs, default priority) must not
fire for document-born rows — the document is the complete statement, and
a default it did not state would drift on the next apply. Ledger export
emits content annotations only: claim, review, and other system notes are
runtime and stay off the page. Validation moves wholly into parse/plan;
the settled check re-runs at write time (the truthful-report law); every
refusal in the Refusals table gets a positive test asserting its message
and its zero-write guarantee.

Hazards already hit while validating this design, so the battery does
not have to:

- **Pass titles and annotation text behind `--`.** Taskwarrior consumes
  attribute-shaped argv tokens: on 3.4.2, `task add due:eom cleanup pass`
  mints "cleanup pass" due end-of-month. A spaced title passed as one
  argv element happens to survive there, but the `--` guard is the
  version-proof defense — `create.py:_build_add_args` lacks it today.
- **The acceptance join.** Criteria stored joined with `" | "` round-trip
  wrong when a criterion contains a pipe; refuse at plan time (the
  Refusals table names the message).
- **Write `taskdoc_id` and `taskdoc_parent` in the same `add`.** UDAs are
  atomic with creation; an annotation written afterwards is a crash
  window that leaves the row outside its family, and a re-apply would
  then duplicate it.
- **Read documents as `utf-8-sig` with universal newlines**, `-` for
  stdin. A BOM otherwise breaks the first heading; CRLF otherwise leaks
  into stored text.
- **Dedupe a preamble `After:` that duplicates containment** before
  writing edges.
- **The settled re-check and the write are two board calls.** The gap is
  real but bounded: a claim landing between them sees one
  document-shaped edit, and the next apply reports the drift.

## Appendix — Reference Card

```
STRUCTURE   # heading (H1-H6; deeper: bold spans)   Setext over ===/---
            **Bold span** after blank (one level below the real heading)
            - item  * item  + item   (indent nests; content indents under)
            1. item  (ordered runs start at 1, chain in sequence)
            - [ ] item  (marker strips first; [x] warned)
FIELDS      Acceptance: <criterion>       (repeat per criterion, or:)
            Acceptance:                   (+ criteria list, blanks only between)
            After: <slug-or-title>[, ...] (cross-edges; forward refs ok)
            Priority: high|medium|low|none      Flow: <phase>[, ...]
            Due: <date>                         Tags: <tag>[, ...]
            labels case-insensitive; **emphasis** tolerated; synonyms:
            acceptance criteria/success criteria/done when/definition of
            done/AC, depends on/blocked by/prerequisites, deadline,
            labels; field-shaped bullets feed their parent (checkboxed
            too); ## Acceptance criteria / ## Notes sections feed theirs
CONTENT     prose -> description (binds by indentation; last resort)
            > quote  | table  [1]: url  link-only lines  ``` fences
            -> annotation blocks (coalesced; last resort)
            frontmatter --- ... --- -> one root annotation, uninterpreted
            \- \# 12\. Label\: \[link] \**Label:** -> escaped prose,
            backslash drops out
DISCARDED   --- rules   whole-line <!-- comments -->   [x] state
IDENTITY    family = (project, origin); node = slug of title (link text);
            duplicates qualify: parent--slug, else slug--own-words,
            else refuse; stable title = same task; reuse origin = same graph
APPLY       created reused updated edge-added edge-dropped loose drift warn
LEDGER      one normal form; round trip preserves the graph; export is a
            fixed point — a ledger diff shows only the author's edits
LAWS        idempotent + reentrant (document + board, nothing else);
            never deletes; never touches settled work; refusals before
            writes, exit 2; interrupted applies converge on re-apply
```
