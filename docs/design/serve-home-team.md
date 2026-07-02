# Serve Home Team

Status: recommendation, 2026-06-21.

## Recommendation

Defer a durable home team for now.

Post-keystone, this is only a UX and identity decision. D8 gives the home-team
proposal ZERO metric motivation: team-id churn no longer fragments lane
counters, message counters, or metric cursors. Do not adopt a home team to
repair metrics, smooth metric rollups, or preserve metric history.

The UX case is real but not yet strong enough to justify a special permanent
team type. Current serve already guarantees at least one open lane group: when
the last team is closed or emptied, `_ensure_open_team_locked()` creates a new
empty `team-<uuid>`. A durable home team would mainly reduce visual identity
churn and preserve a familiar empty landing lane. That is useful, but it
requires exceptions to close, prune, split, and merge semantics that are more
expensive than the current annoyance.

## Current Behavior

Team creation is anonymous by default. `_create_team_locked()` uses
`team-<uuid>` when the caller does not provide a team id.

Serve keeps the UI from becoming teamless. `team_snapshot()` prunes zero
activity closed teams, then calls `_ensure_open_team_locked()`. If no open team
exists, `_ensure_open_team_locked()` creates a fresh empty team with default
config.

Closing a team is destructive for membership and identity. `close_team()` marks
the selected team closed, deletes its memberships, records `closeTeam`, and
then ensures a replacement open team exists.

Empty teams auto-close. Moving an agent out of a team calls
`_close_empty_teams_locked()` for previous teams. Removing the last agent also
closes the team and then ensures a replacement open team exists.

Merge and split are team-history operations, not home-lane operations.
`merge_teams()` moves members and team metric rows into the destination, closes
the source, and records restorable subgroup history. `split_team()` creates a
new team with the source config and moves selected agents into it.

Zero-activity prune removes closed teams that have no config, filters, shell
settings, renewals, metrics, or merge-subgroup history. This keeps the current
constant replacement behavior from accumulating dead anonymous teams.

## UX Read

A stable home lane would help in three cases:

- The operator closes the final lane and expects to see the same empty place
  return instead of a new `team-<uuid>` identity.
- The operator removes or moves the last agent from a group and wants a familiar
  default target for future imports.
- The operator thinks of one lane as the neutral staging area and wants its
  visual position and config to survive ordinary cleanup.

The current churn is mostly an identity and orientation problem. It does not
lose task state, metrics, agent identity, or transcript state. The replacement
team gets default config, so it can lose lane-local choices such as lifetime,
selected view, task filters, and shell settings if the operator expected those
choices to describe the default place rather than that specific disposable team.

That distinction matters: a durable home team is valuable only if the operator
intends the empty landing lane to carry durable configuration. If the desired
behavior is merely "there is always somewhere to drop an agent," the current
replacement team already provides that.

## Metric Boundary

This design record must not use metrics as a reason to adopt the home team.

Metrics now have their own durable agent and team storage paths. Merge and
split explicitly move team metric rows when that history matters. The old fear
that random replacement team ids fragment counters is no longer load-bearing.
If a metric appears wrong after the keystone, fix the metric ingestion,
identity, merge, or summary path directly. Do not introduce a permanent home
lane as an indirect metric stabilizer.

## Home Shape Options

### Store-Level Home

A single store-level home team is the least invasive shape. Use a stable id
such as `team-home` for the repository team store. It is the default empty lane
when no other open team exists.

This fits the current `team_snapshot()` and command service shape because those
paths do not carry worktree context. It also avoids creating many empty lanes
for every discovered worktree.

Tradeoff: it is not an agent's personal home. It is a shared neutral staging
lane.

### Per-Worktree Home

A per-worktree home team would derive from the serve target id, for example
`team-home-<target-id>`. It matches the mental model "this worktree has a home
lane."

This is a larger migration because `_ensure_open_team_locked()` currently has
no target context and ensures one open team for the whole store. Per-worktree
home teams would require target-aware snapshots or a separate reconciliation
pass that creates and hides many empty homes without flooding the UI.

Tradeoff: better identity, higher UI clutter risk.

### Per-Actor Home

A per-actor home team should be rejected. Actor identity can move from
`target:<id>` to `thread:<id>`, and renewal creates predecessor and successor
facts. Making a team id follow that actor lifecycle would couple team existence
to session churn and renewal history. It would also make a group concept behave
like a private lane, which fights merge and split semantics.

Tradeoff: it sounds personal, but it puts the home boundary on the least stable
identity axis.

## Semantic Conflicts

Close semantics need a new rule. Today closing the final team means "close this
team and create a replacement." With a home team, close must either refuse,
reset, or temporarily close and immediately reopen the same id. Refusing close
is clearest but makes one lane immortal. Resetting close is useful but must say
which fields reset: members, config, task filters, shell settings, renewal
state, and history.

Auto-close needs an exemption. `_close_empty_teams_locked()` currently closes
any empty team it is asked to check. A home team would need to remain open while
empty, or auto-close would erase the very stability it exists to provide.

Prune needs an exemption. Zero-activity prune should never delete the home row,
even if it has no config, events, metrics, renewals, or memberships. That
requires either a team kind column or a hard-coded reserved id.

Merge semantics need guardrails. Merging the home team into another team should
probably be disallowed or treated as "move members out of home, leave home
open." Merging another team into home is reasonable but must not let the home
row inherit restorable subgroup meaning that later prevents a clean reset.

Split semantics are mostly compatible. Splitting agents out of home can create
a normal child team. Splitting back into home should work only when the home has
the subgroup members and the operation does not close home.

Config semantics are the sharpest UX question. If home config persists, the
home team is a real durable workspace. If close resets config, the home team is
a neutral landing pad. Mixing those behaviors would be surprising.

## Migration Sketch If Adopted

Adopt only the store-level home first.

1. Add an explicit team kind, or reserve `team-home` and wrap the reservation in
   helpers. A schema column such as `kind TEXT NOT NULL DEFAULT 'normal'` is
   cleaner than spreading id checks through close, prune, merge, and UI code.
2. Replace `_ensure_open_team_locked()` with `_ensure_home_team_locked()` plus
   `_ensure_open_team_locked()`. The home helper should create or reopen the
   `home` kind team when no open normal team exists.
3. Make `_close_empty_teams_locked()` skip home teams. Empty home is a valid
   state.
4. Make `close_team(home)` a reset operation or a validation error. Prefer a
   reset if UX testing shows operators expect the close affordance to work;
   prefer refusal if close should always mean historical closure.
5. Exempt home from zero-activity prune. If using `kind`, prune only normal
   teams.
6. Disallow merge source `home`, or redefine it as "move home members to the
   destination and leave home open." Allow merge destination `home` only if the
   team keeps normal merge-subgroup history without blocking reset.
7. Keep split from home normal: selected agents move to a new normal team with
   copied config. Do not close home when it empties.
8. Add UI treatment that labels the home lane as home instead of exposing
   `team-home` as another arbitrary id.
9. Migrate existing stores lazily: on connection, create the home row if absent
   and no open teams exist. Do not rewrite existing normal teams to home.

Per-worktree home can be reconsidered only after the store-level version proves
useful. It needs target-aware team snapshots and a policy for hidden empty
homes. Per-actor home should stay out of scope.

## Decision Gate

Adopt later only if operator feedback shows that the neutral landing lane is
used as durable workspace identity, not only as a drop target. Evidence should
look like repeated confusion after closing the last lane, repeated reapplying
of default team config, or requests to "go back to the same empty lane."

Defer while the pain is only theoretical. The current system is internally
coherent: anonymous teams can be closed and pruned, an empty lane is always
available, and merge/split/close all operate on ordinary teams without a
special immortal row.
