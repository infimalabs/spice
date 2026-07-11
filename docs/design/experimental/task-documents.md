# Task Documents

Status: recommendation, 2026-07-11.
Replaces `docs/design/accepted/task-markdown-dag.md` when accepted.

A **task document** is a markdown file that projects a goal onto the task
board as a fully connected dependency graph. Agents write task documents the
way they write any markdown — on the fly, mid-conversation, in one pass — and
pipe them in with `spice task ingest`. Piping the same document in again is a
no-op. Piping in a *tweaked* document reconciles: new work is created,
existing work is matched and preserved, new edges land, and nothing is ever
deleted. The board remains authoritative the moment rows exist; documents are
how graphs are born and grown, never how they are stored.

There is exactly one dialect. It is the markdown you would write anyway.

## Vocabulary

Every term below is used in exactly this sense throughout.

- **Task document** — a markdown file conforming to this dialect: structure
  by headings and lists, prose and field lines attached to nodes.
- **Node** — one task-to-be: minted by a heading or a list item. Carries
  title, slug, description, acceptance, annotations, priority, flow, and
  edges.
- **Slug** — the node's identity within its family: the lowercase ASCII
  words of its title joined with `-`. Slugs are a pure function of the
  title.
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
- **Family** — all rows that carry a `taskdoc-id` annotation (see
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
- **Settled node** — a family row that work has touched *now or durably*:
  status is not pending, or its phase has advanced, or it holds an active
  claim — a claim released without a phase advance leaves the node
  unsettled again. Documents never modify settled rows; they only report
  drift.
- **Loose row** — a family row the incoming document no longer lists.
  Reported, never deleted. (Same word, same meaning as a loose commit:
  real work that the surrounding structure no longer names.)
- **Drift** — a difference between document and board that apply will not
  change, reported so the author can see it.

## The Dialect

### Line classifier

A document is parsed in one top-to-bottom pass. Each line takes the first
matching rule:

| Line shape | Effect |
| --- | --- |
| inside an open ``` fence | accumulate; on close, the whole fence becomes an annotation on the current node |
| ```` ``` ```` | open a fence |
| blank | paragraph break in the current node's description |
| ATX heading `#`–`######` | **new node**; parent = nearest shallower heading |
| list item `-` `*` `+` `1.` `1)` | **new node**; parent = enclosing list item, else the current heading |
| field line `Label: value` where Label ∈ {Acceptance, After, Priority, Flow} | field on the current node |
| starts with `>`, `\|`, or `---`, or contains an inline `[text](url)` | annotation on the current node |
| anything else | description prose on the current node |

The **current node** is whatever the most recent heading or list line
created. Before the first structural line there is no current node yet:
preamble content — prose, field lines, annotations — accrues to the
document and lands on the root once the root is known. Prose never
attaches by visual proximity to a section — it attaches to the most
recent node. Write accordingly (see The Authoring Contract).

### Structure lines

**Headings.** `#` through `######`. A deeper heading is a child of the
nearest shallower one.

**List items.** Any standard marker: `-`, `*`, `+`, `1.`, `1)`. Indent
depth (spaces; a tab counts as four) nests items: a deeper item is a child
of the enclosing item. A list's top-level items are children of the current
heading.

Headings and list items are the *only* structure. Everything else is
content.

### Field lines

A field line is a plain line — never a bullet — of the form `Label: value`,
attached to the current node. Labels are case-insensitive and exact. The
set is closed:

| Field | Value | Meaning |
| --- | --- | --- |
| `Acceptance:` | one criterion | Appends one acceptance item. Repeat the line per criterion. |
| `After:` | one or more slugs, comma-separated | Adds dependency edges to nodes elsewhere in the document. Repeatable; targets may appear later in the document. |
| `Priority:` | `high`, `medium`, `low`, `none` | Sets this node's priority, replacing the document default. |
| `Flow:` | comma-separated phases from the approved set (`design`, `plan`, `todo`, `verify`, `review`) | Sets this node's phase flow explicitly. |

`After:` targets are slugs, not titles: `After: freeze-main`. A slug that
matches no node in the document is a refusal (see Refusals). `After:` is how a
document says what nesting cannot — a shared prerequisite, a diamond, a
section that follows its siblings.

Near-misses are content, not fields: `**Acceptance:** x` (bolded),
`Acceptance criteria: x` (wrong label), and `- Acceptance: x` (a bullet —
the list rule wins, minting a node titled "Acceptance: x"). The labels are exact
and the line must be plain.

### Content lines

**Description.** Plain prose attaches to the current node's description,
preserving paragraph breaks. The description is the node's working body.

**Annotations.** Blockquotes, table rows, thematic breaks, lines containing
inline links, and whole code fences attach to the current node as durable
notes. They survive to the board as row annotations, verbatim.

### Inert constructs

These are legal markdown and legal in a task document, but they carry no
task meaning — they land as description or annotation text, verbatim:

- **Bold/italic pseudo-headings** (`**Phase 1**`) — not structure.
- **Setext headings** (`Title` over `====`) — not structure; use ATX `#`.
- **Checkbox markers** (`- [ ]`, `- [x]`) — the bracket is title text; a
  task document describes work to create, not work's status. Leave
  checkboxes out.
- **HTML comments** — visible description text, not directives.
- **Reference-style links** (`[text][1]` and `[1]: url` definitions) —
  description text; use inline `[text](url)` for link annotations.
- **Indented code** (4-space) — description text; use fences for code
  annotations.
- **YAML frontmatter** — there is none. A leading `---` block is preamble
  annotation text like any other; identity lives outside the document
  (see Families And Provenance), so there is nothing for frontmatter to
  say.

## Graph Construction

### Edge direction

Containment is dependency: a parent node depends on (`after`) each of its
children. `After:` lines add edges on top. Dependencies point from goal
toward prerequisite, so:

- Leaves are ready immediately; siblings run in parallel.
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
component.

### Identity rules

Slugs make reconcile possible, so titles carry identity obligations:

- **Titles are unique within a document.** Two nodes with the same slug
  are a refusal (`document-root` counts when synthesized). Distinct work
  deserves distinct names.
- **Titles contain at least one ASCII word** (`[a-z0-9]+` after lowering).
  A title that slugs to nothing is a refusal. Non-ASCII text is welcome in
  titles; it cannot be the *only* thing there.
- **Titles are stable.** The slug is the node's identity across applies.
  Retitling a node makes a new node and lets go of the old one — sometimes
  that is what you mean; the report will show both verbs. This holds for
  the root like any other node: rename the goal and the family survives,
  because family identity does not live in any title (see Families And
  Provenance).

### Document validation

Parsing refuses (see Refusals) when: the document is empty; a slug is duplicated; a
title has no ASCII word; an `After:` target matches no node; or the
document graph (containment plus `After:`) contains a cycle. A document
that parses is a valid, fully connected DAG.

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
  machinery, no in-document markers.

One origin, one document — and the document is the **complete statement**
of its family's creation surface. What it lists exists; what it stops
listing goes loose; what it cannot express (other families' rows, edges
that leave the family, runtime state) it cannot disturb. Because every
applied row round-trips through `taskdoc-id` and the family pin, the
board itself is the only memory apply needs: there is no stash, no
lineage file, no state beside the rows (see The Board Is The Memory).

### Matching

Within the family, matching is by node slug, recorded on each created row
as a system-owned `taskdoc-id: <slug>` annotation. A slug matching
exactly one family row is that row. A slug matching more than one family
row is a refusal — ambiguity is the one thing apply will not guess about.
Completed family rows still match (as already-satisfied work); deleted
rows never match. Rows outside the family are invisible: a bullet titled
"Add tests" here never collides with an "Add tests" from another origin,
another project, or last quarter.

Documents never contain `taskdoc-id:` text; agents never write it by
hand. Matching is by title, and title stability (see Identity rules) is
the whole mechanism.

## Apply Semantics

`spice task ingest PATH --project <project> [--origin <origin>]` is
**apply**: make the family match the document, creating what is missing,
preserving what exists, touching nothing that work has settled, deleting
nothing ever.

### The plan

Apply computes a complete plan before writing anything:

1. Parse and validate the document.
2. Resolve project and origin; load the family; match nodes to rows by
   slug.
3. Compute a verb for every node and every difference.
4. Validate the **union graph** — the document's edges plus every edge
   already on the board (a document edge can close a cycle through a
   chain that leaves the family). A cycle anywhere in the union is a
   refusal.
5. Execute. Any refusal fires before the first write.

`--dry-run` stops before execution and prints the plan as a report.

### The board is the memory

Apply is a function of exactly two inputs: the document and the board.
There is no third thing — no stash of the last document, no lineage
file, no mode switch. The round-trip law (see Ledger Export) is what makes this
possible: the family's rows *are* the last statement, reconstructable at
any time, so diffing the incoming document against the family answers
every question a separate memory could:

- A matched, unsettled node's creation surface is made **equal to the
  document's statement, verbatim** — additions, removals, rewordings,
  and reorderings are all one rule. There is no union step and no
  removal-intent puzzle: the document is the complete statement, so an
  acceptance item or family edge it no longer states is gone by
  authorship, not by inference.
- A matched, settled node is never modified; each difference is a
  `drift` line. New annotations still append — they are a log, not
  creation surface.
- A family row the document no longer lists is `loose`: untouched,
  fields and edges alike. No memory is needed to say so — the
  `taskdoc-id` proves the row is document-born, and the
  complete-statement rule makes "unlisted" mean "let go".
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
| annotations | append-only log | append new, dedup by content | append new |
| flow | creation-only | set at create; later changes are drift | drift |
| edges leaving the family | board-owned | never touched | never touched |
| project, origin | family identity | fixed at first apply | — |
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

The first line of every successful report is `root <handle>`. Order is
deterministic: `created` in creation order (post-order over dependencies),
then `reused`, then `updated`, then edge verbs, then `loose` and `drift`
(each in family creation order). Exit code 0. A refusal is a single
`spice: <sentence>` on stderr, exit code 2, zero writes.

`loose` and `drift` are standing facts, not events: they repeat on every
apply, verbatim, until the author resolves them — re-list the node, match
the field, or reach for the board verbs. A report is a statement of where
document and board stand, not a changelog.

### Laws

- **Reentrancy.** Apply reads the document and the board and nothing
  else. Same document, same board: same plan, same report — from any
  worktree, any agent, any time.
- **Idempotence.** Applying the same document twice: the second apply
  writes nothing — every matched row reports `reused`, `loose` and
  `drift` lines repeat verbatim, and the board is byte-identical.
- **Monotone safety.** No apply ever deletes a row, reopens completed
  work, or modifies a settled row.
- **Truthful report.** Refusals fire before the first write. After
  execution the report states exactly what landed: a row that settles
  between plan and write demotes its planned update to a `drift` line —
  everything else still lands.
- **Recovery by re-apply.** An interrupted apply leaves the family
  partway; applying the same document again converges — rows already
  created match and report `reused`, and the remainder lands.
- **Convergence.** Apply, then ledger: the exported document parses
  to the same graph as the applied document, minus drift the report
  already named.

## Ledger Export

`spice task ledger HANDLE` exports a row and its dependency closure as a
task document in this same dialect — root title as the H1, containment
rendered as nesting, non-tree edges as `After:` lines, acceptance and
priority and flow as field lines, descriptions as prose, annotations in
place. Child order is stable (board creation order), so export is
deterministic: same board, same bytes.

The round-trip law: `parse(ledger(apply(D)))` and `parse(D)` describe the
same graph. Export emits only this dialect, so anything spice writes,
spice can re-ingest — the export is an ordinary task document for the
same family, and the natural way to make big edits: ledger out, reshape,
apply back with the same origin.

Runtime state (claims, phase positions, review evidence, validation) never
appears in a task document, exported or authored. Documents describe work;
the board holds its history.

## Refusals

Everything that can fail fails loudly, completely, and before any write:

| Refusal | Trigger |
| --- | --- |
| `task document is empty` | empty or whitespace-only document |
| `duplicate title in document: <slug>` | two nodes slug identically |
| `title has no ASCII words: <title>` | slug would be empty |
| `unknown After target: <slug>` | `After:` names no node in the document |
| `dependency cycle at <slug>` | cycle in the document graph or the union graph |
| `<slug> is ambiguous in family: <handle>, <handle>` | node slug matches multiple family rows |
| `missing project` | no `--project` and no active claim to supply one |
| `missing origin` | no `--origin` and no active claim to supply one |
| `invalid flow phase: <phase>` | `Flow:` names an unapproved phase |

A refusal names the offending slug and, where recovery is not obvious, the
way out. A refusal never leaves partial writes — it fires before the first
one; an apply interrupted mid-flight is recovered by applying again
(the recovery law), never by hand.

## The Authoring Contract

Ten rules. An agent that follows them can free-write a document
mid-conversation, pipe it in, keep working, tweak it, and pipe it in again
— and the board will track the document for exactly as long as the
document deserves to lead.

- **One origin, one document.** Reuse the origin to evolve this graph;
  mint or pick a fresh origin for a new effort; point a new family's
  origin at the work it grows from (`task:<handle>`). Identity is
  provenance — never something you write inside the document.
- **Structure is headings and list nesting. Nothing else.** No bold
  labels, no setext, no frontmatter, no checkboxes, no HTML.
- **One task, one line.** Each bullet or heading is a unit of work with
  an imperative, specific, ASCII-bearing title, unique in the document.
  An H1 naming the goal is good practice — it names the root — but a
  bare list is a complete document.
- **Nest for sequence, `After:` for everything else.** "Publish notes
  needs the tag" — make `tag-the-candidate` a child of `publish-notes`,
  or write `After: tag-the-candidate` under it. Siblings without edges
  run in parallel; that is a feature, so only sequence what truly must
  wait.
- **Give ready work `Acceptance:` lines.** One criterion per line, exact
  label, plain line, directly under its node. A node with no acceptance
  and no `Flow:` routes to a `plan` phase and will demand decomposition
  before execution — use that deliberately for unshaped work.
- **Titles are identity — keep them stable.** Rewording a title creates
  a new task and leaves the old row loose. Reword descriptions and
  acceptance freely; rename tasks only when you mean replace.
- **Attach prose immediately.** Everything binds to the most recent
  node. Put a node's body directly under its line; put goal-level
  context at the top of the document before the first node; never write
  section prose after a list.
- **Tweak and re-pipe freely; read the report.** Adding a bullet, an
  edge, a criterion, a paragraph — all reconcile. `created`/`updated`/
  `edge-added` confirm your change; `drift` means the board owns that
  field now; `loose` means the document let go of a row the board still
  holds — deal with loose rows deliberately (usually: fine to leave).
- **The document leads until work begins.** Once a task is claimed or
  advancing, the board owns it; stop steering settled work through the
  document and use the task verbs (`note`, `edit`, `depends`, `review`).
- **Never write identity markers.** No `taskdoc-id:` text, no comment
  tags, no metadata blocks. If you find yourself inventing markers to
  control matching, stop — matching is by title within your origin's
  family, and the one-origin and stable-titles rules are the whole
  mechanism.

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

### Nesting is sequence

```markdown
# Release prep

- Cut the release branch
  - Freeze main
  - Tag the candidate
- Publish notes
```

`cut-the-release-branch` waits for its two children (which run in
parallel); `publish-notes` is independent; the root waits for everything.

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

> Context: pilot customer sends 2GB files.
```

Note the blockquote at the end: it attaches to the most recent node
(`stream-rows-instead-of-slurping`), not to the goal. To annotate the
goal, write the quote at the top, before the first bullet (the
attach-prose rule).

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
list flavors, nested ordered steps, every field line, prose, quote,
table, link, and fence:

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

Sixteen nodes, a four-deep tree, plus two `After:` cross-edges making
`rollout` wait for both sibling tracks — a shape no nesting could draw.
The fence annotates `metrics-scrape`; the table annotates `rollout` and
the closing prose is its description; every acceptance line sits on its
own node.

## Non-Goals

- **No structured input dialect.** Task documents are markdown. If a
  machine-to-machine format is ever needed, it will be a plain `.json`
  file on its own flag — never embedded in a document.
- **No structured report.** The apply report is lines for reading, not a
  payload for parsing. Tooling that wants graph state queries the board;
  a machine-readable report would invite agents to build on the receipt
  instead of the truth.
- **No in-document identity.** No frontmatter, no id markers, no comment
  tags, no reserved headings. Identity is `(project, origin)`, full stop.
- **No pruning.** Documents cannot delete. `spice task delete --reason`
  is a human-sized verb and stays one.
- **No status in documents.** Checkboxes, done-markers, claim owners,
  phases: the board holds state; ledger holds history; documents hold
  intent.
- **No per-node project.** One document, one project, one family.
- **No fuzzy matching.** Slug equality only. No title similarity, no
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
and the family-edge domain rule in the diff. Origin and project
resolution reuse the creation-path resolvers. Validation moves wholly
into parse/plan; the settled check re-runs at write time (the
truthful-report law); every refusal in the Refusals table gets a
positive test asserting its message and its zero-write guarantee. The
prior design record is replaced outright; its
verbs (`ingest`, `ledger`) and its core stance — board authoritative,
markdown as projection — carry forward unchanged.

## Appendix — Reference Card

```
STRUCTURE   # heading = section/goal      ## deeper = child section
            - item   * item   + item   1. item   (indent = nesting)
FIELDS      Acceptance: <criterion>         (repeat per criterion)
            After: <slug>[, <slug>...]      (cross-edges, slugs only)
            Priority: high|medium|low|none
            Flow: <phase>[,<phase>...]
CONTENT     prose -> description      > | --- [t](url) -> annotations
            ``` fenced code ``` -> annotation
INERT       **bold**  setext  - [ ]  <!-- -->  [ref][1]  frontmatter
IDENTITY    family = (project, origin); node = slug of title;
            stable title = same task; reuse origin = same graph
APPLY       created reused updated edge-added edge-dropped loose drift
LAWS        idempotent + reentrant (document + board, nothing else);
            never deletes; never touches settled work; refusals before
            writes, exit 2; interrupted applies converge on re-apply
```
