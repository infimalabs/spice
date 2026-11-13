# Product model — core — Boundaries

[Product model — core](../model.md) · [Spice product model](../README.md)

## Counsel

[Counsel](../invariants/session.md#session-a-confirmed-maxim-reminder-returns-as-ordinary-steering)
uses ordinary steering. Reports provide evidence; they never add, remove, or
rescope active tracked bags. Changing active taste requires an
operator-accepted tracked change.

## The three lane-integration git boundaries

Agents never pull or push. Lane integration occurs at launch, claim, and phase
completion; activation is not a fourth boundary. Release, gate, proof, and
installation Git operations have separate owners.

- **Launch:** opportunistic fast-forward. A dirty, ahead, or diverged lane, an
  absent integration remote, an unreachable remote, or an uninspectable
  baseline keeps the lane's work and records a skip note; failure to advance
  cannot block launch.
- **Claim:** fast-forward-only to the baseline, recording the opening commit.
- **Phase completion:** baseline-first integration, or fast-forward when the
  baseline already descends from the lane.

A content conflict is presented as overlap with the baseline, not as an agent
responsibility to synchronize with an upstream. Typed footprint and admission
refusals remain distinct from raw synchronization failures.

The baseline is the branch the lane's own pairing declares in the repository's
configuration, `branch.<lane>.remote` and `branch.<lane>.merge`, read at every
scope beneath the lane's process-local Git shadow; the shadow points an
occupant's bare `git push` and `git pull` at the lane itself and never chooses
the baseline. A pairing on a named remote integrates through that remote's
tracking ref; a pairing on `.` integrates into this repository's own branch with
nothing fetched. A pairing is whole or absent: one half alone, or a merge that
names no branch, is a fault. A lane that declares no pairing integrates into
`origin`'s HEAD branch. A declared remote the repository does not define is a
fault; `origin` never stands in for it.

## Landing refusals

A landing may touch only the task's footprint: paths touched by its own
non-merge commits. A merge, including the lane's own integration of the
baseline, never widens it. A refusal names the excess and a repair recipe.

A conflict resolution that drifts outside the footprint publishes nothing, but
it commits the lane's integration of the baseline: the resolved tree with every
drifted path as the baseline holds it. The resolved content of those paths
stays staged, so the claimant either commits it as a non-merge commit that
brings it inside the footprint, such as a baseline caller adapted to a function
the task changed, or restores it from `HEAD`, and then completes again.

When integration synthesizes a tree different from the authored tree, the
configured cheap admission command must accept it before publication. This is
not a rerun of the whole verification suite. A nonempty command captured from
the lane stays fixed through publish-race retries; if the lane declared none,
each integrated tree may introduce its own command. Refusal leaves the
integrated tree at `HEAD` for repair and publishes nothing. Ordinary commit
gating is separate.

A content conflict installs a complete recoverable merge state or leaves the
pre-merge tree untouched, never stranded markers. A publish race preserves peer
paths, re-integrates against the new baseline, and retries within a bound.

## The rewind guard

A checked-out branch reference update cannot abandon commits already merged
upstream. The guard reads the current tip from the ref itself, not the caller's
declared old value.

The upstream is the reference the baseline's pairing names, read the same way,
never an alias, including as a fallback: process-local configuration can shadow
an alias. A `.` pairing protects the local branch it integrates into. No named
pairing is different from a named pairing that cannot resolve. Appends give up
no history and pass either way. A non-appending update with an unresolvable
named upstream refuses as unknown rather than clear. Refusal names the commits
at risk and requires forward work rather than amendment or reset backwards.

The guard is Git's `reference-transaction` hook, so it protects the branch,
not the tree: a refused `reset --hard` leaves HEAD unmoved, but the index and
worktree may already hold the rewind target. The refusal says whether they
match that target or were left untouched, and prescribes no destructive
recovery.

## Configuration layering

Precedence increases through compiled `builtin`, repository `tracked`,
Git-common machine-local `local`, and per-worktree `worktree` intent. Local and
worktree intent live in Git-directory state, never in the checkout:
`<git-common>/.spice/config.toml` and each worktree's own
`<git-dir>/.spice/worktree.toml`. Tracked `spice.toml` carries
repository policy only; machine-local settings live only in local config, and
per-worktree preferences (model, effort, voices) in local or worktree config.
A binding to one worktree's own build, the sealed proof runner, lives only in
that worktree's config. Command flags apply at the command boundary, not as
repository-configuration provenance.

[Provenance](../invariants/session.md#extension-every-effective-configuration-value-carries-its-source-provenance)
and [invalid configuration](../invariants/checks.md#checks-configuration-fails-loudly)
have their homes in the register. [Vendor resolution](../invariants/session.md#extension-vendor-resolution-is-explicit)
is a separate explicit-argument/environment/configuration contract.

## Capability tiers

Observe foreign transcripts; gate repository checks; steer one supervised agent;
or operate a fleet. Each tier is independently usable: a user may stop at any
rung.

Observation initializes nothing: no repository, worktree, team state, claims,
hooks, steering, or supervisor socket. It is read-only.

## Concurrency and serialization

Lifecycle decisions for one lane are serialized to prevent double launch.
Capacity, selection, claim, and start form one serialized decision. Sends within
one lane preserve operator order. Topology mutations carry an observed revision
and refuse a lost race so the client can re-pull. Inbox notices follow committed
mutation and cause fresh authenticated journal reads.

History scans run in a resident-wide bounded pool across every socket; queued
scans start in arrival order, and a request beyond the bounded unanswered
demand refuses as `stream::message-history-busy`. Scans run off the async
control path; a slow history read must not block a heartbeat, send, or sibling
lane's control operation.

### Stream ordering guarantees

The [stream contract](../invariants/identity.md#identity-a-resume-cursor-belongs-to-one-exact-stream)
owns snapshot, ordering, resumption, and generation/epoch fencing.
