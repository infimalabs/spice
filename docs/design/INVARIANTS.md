# Design Invariants

This document records behavior contracts that implementation work must
preserve. The root design file stays compact; this file carries the detailed
checklist.

## Steering And ACKs

- Inbox publish is atomic; keys are direct-child UTC names with collision
  suffixes.
- Inbox items carry priority and continue/stop notes, expire after 24 hours,
  and are never retired by a bare read.
- ACK grammar requires a standalone ACK token and a valid key shape; fillers,
  separators, dropped `Z` aliases, directive scrubbing, and segment splitting
  are parsed deliberately.
- Unacknowledged steering redisplays on shell interaction and escalates by
  priority after repeated silent assistant messages.

## Wrapper And Side Channel

- `spice agent run` preserves proxy passthrough semantics and worktree env for
  nested harness calls.
- The supervisor exports the per-process git shadow once; child commands inherit
  the shadow rather than recomputing it.
- Side-channel startup uses an explicit hello protocol.
- Context-meter cache and repeat windows prevent noisy pressure spam while
  still surfacing rollover risk.
- The layered `rtk.executable` setting is one trusted executable basename or
  absolute path. Resolution does no availability lookup; every probe and
  rewrite invokes the exact winning identity.
- RTK owns rewrite selection and its canonical `rtk` frontend. Exit `0` or
  Exit `3` with non-empty stdout rewrites, Exit `1` with empty stdout is a
  no-match, and other or malformed outcomes are diagnosed and discarded so the
  original native command runs unchanged.
- Spice remaps the canonical frontend to the configured executable, owns only
  the built-in `common` wrapper's finite post-selection routes and the
  thread-scoped `RTK_DB_PATH` location at
  `<worktree-git-dir>/.spice/agents/<thread>/rtk/history.db`, and publishes
  health telemetry through activation `rtk_status`, Doctor, and bounded stderr
  diagnostics. RTK owns the history contents.

## Lifecycle

- Agent startup is guarded by an ensure lock and supervisor state publication
  contract.
- Ambient thread ids are refused at launch.
- The prompt boundary rule holds: initial prompt is neutral; the real ask is
  recovered from live control-plane state.
- Renewal uses ordinary steering and an explicit rehydration template.

## Watchdog And Maxims

- Assistant prose scanning is keyed on driver-specific transcript markers.
- Maxims are deduped by content-derived reminder key within a compaction epoch.
  Compaction may make the same reminder eligible to reappear because the agent
  may have lost the earlier steering; it never changes the reminder text.
- Judge text excludes generated diff/patch/tool-output bodies.
- `[MAXIM]` and watchdog echoes are suppressed so the system does not judge
  itself for quoting itself.
- Built-in maxims stay near-universal engineering preferences. They do not
  encode context runway or pressure policy; calm handoff behavior belongs in
  task and rehydration surfaces, not in maxim text.

## Tasks

- Task identity is the incepted handle; phase slots are bounded; claim TTL is
  3600 seconds; claim context links cover the surrounding five minutes.
- One actor has at most one active claim.
- Same-author review is guarded unless the allocator explicitly assigns it.
- The oops board is the hidden `.oops` project, backed by hidden metadata and
  deferred by a far-future wait date.
- Priority classes derive SLA due dates.
- Git sync happens only at task boundaries.
- The measured historical sequences distinguish an early hook rejection from
  the two corrupt terminal states. A rejecting `reference-transaction` hook
  receives `prepared ORIG_HEAD` and `aborted ORIG_HEAD`; porcelain merge exits
  before it writes markers or `MERGE_HEAD`, so that hook is not the cause of a
  marker-bearing checkout. The marker-first state instead follows a successful
  conflict materialization (markers and `MERGE_HEAD`) plus later cleanup that
  removes the merge parent without restoring the checkout. The ref-first state
  follows a generated merge becoming `HEAD` before its tree reaches the index
  and worktree, exposing merge parents while `git write-tree` and the checkout
  still describe the agent parent. Deterministic Git fixtures freeze all three
  checkpoints rather than depending on process timing.
- Task completion computes each baseline merge with `merge-tree` before it
  mutates the checked-out repository. A conflict is installed in this order:
  marker-bearing tree, complete higher-stage index, merge message and original
  head, then `MERGE_HEAD`. Therefore the visible terminal state is either the
  untouched pre-merge tree or a recoverable merge carrying the baseline parent;
  a reference hook cannot strand loose markers between those states.
- A clean task merge is synthesized with the baseline as first parent and the
  task line as second parent. Its complete tree reaches the index and worktree
  before one compare-and-swap update of the checked-out branch. A rejected or
  raced ref transaction restores the actual current head with `read-tree`,
  which does not invoke another ref hook. A publish race repeats this sequence
  against the newly fetched baseline, preserving every peer path before retry.

## Serve

- Message keys are `timestamp#offset`.
- Transcript tail scans use bounded chunks and caps.
- Presence records do not consume visible-message budget, but the newest useful
  presence record is retained.
- Image attachments collapse paired view-image output.
- Activity states decay through active, active-ish, and inactive windows.
- Empty ordinary messages are rejected.
- Team revisions are monotonic.
- Lifetime vocabulary is exactly Steer, Drive, Drain.
- Speech narrates edges: explicit ACK utterances win; fallback narration reads
  only compact prose; image markdown is described, not read; every prose message
  keeps a manual play affordance.

## Storage Classes And Shape Changes

The storage split is asymmetric: losing a projection costs a replay; losing
authority costs facts no replay can recover. The five SQLite stores therefore
have these fixed classes:

| Store | Storage class | Role |
| --- | --- | --- |
| `taskchampion.sqlite3` | Durable authority | Task documents and their TaskChampion mutation history |
| `spiceteams.sqlite3` | Durable authority | Team topology, configuration, membership, renewal, identity, and revisioned events |
| `spiceacks.sqlite3` | Durable authority | Directive publications, ACK/refusal dispositions, and their provenance |
| `spicemaxims.sqlite3` | Durable authority | Maxim evaluations, gate decisions, and reminder-publication events |
| `spiceprojections.sqlite3` | Rebuildable projection | Agent-activity materializations derived from typed driver-transcript facts |

- An authority migration is a singular, row-preserving, forward step from one
  supported source shape to the current shape, not an accumulating ladder of
  per-release arms. Refusing an older or otherwise unsupported shape without
  mutation is legitimate when the refusal names the release that owns the
  required one-time migration.
- A projection store carries no migration ladder. An unknown version, drifted
  shape, corrupt file, or missing file is discarded and recreated at the
  current shape, then explicitly replayed from its native source before its
  facts are served.
- Superseded code paths, compatibility branches, and aliases are deleted
  outright once callers use the current shape. That cleanup rule does not
  authorize deleting, resetting, or silently rewriting durable authority;
  conversely, a required durable-data migration does not justify retaining
  superseded runtime paths.

## Constitution

- Namespace package policy rejects `__init__.py` under configured package roots.
- Path shape enforces lowercase boundary names and rejects generic shard names.
- File pressure uses base and flex limits, sticky state, and rename-following.
- Routine complexity uses CCN and length limits with the same flex/sticky model.
- Magic numbers are diffed against a configured baseline so only regressions
  fail.
- Reachability is provider-backed and routes module findings separately from
  symbol findings.
- Commit messages wrap at the configured limit; literal `\n` is rejected;
  every Git trailer -- including Co-Authored-By -- rides through unless a repo
  configures an allowed or blocked trailer set, and a driver's native
  attribution is disabled to match when the repo would reject its trailer.
- Env policy requires `env-policy: allow` for tracked env literals and, by
  default, every env access site. Env-name ledger separately accounts exact
  env names.
- The fully-staged rule rejects partially staged files.
- Prefer asserting real, present invariants over absence, migration-residue, or
  vestige assertions: pin an observable property of a value that is actually
  produced, not the non-appearance of one. This is a strong preference, not a
  blanket ban on negatively-phrased comparisons -- `a != b` between two real
  produced outputs asserts a present differentiation invariant and is encouraged.
- A successful gate prunes sticky state that no longer measures over base.
- Dirty work renders the same pressure against the uncommitted tree as steering,
  not as a block.
- Repo-truth docs are capped by tracked policy and are part of the constitution,
  not a special case outside source control.

## Dependency Contract

Runtime dependencies are intentionally small: `watchfiles`, `ruff`, and
`lizard`. The agent shell optionally uses
[RTK](https://github.com/rtk-ai/rtk) `0.42.4` or newer to optimize command
output; unavailable or invalid RTK is reported and preserves native command
execution. Other external binaries degrade loudly rather than silently
changing semantics: Taskwarrior, the maxim judge, speech backends, and the
agent CLI. Development depends on `pytest`, `ruff`, and `lizard`.
