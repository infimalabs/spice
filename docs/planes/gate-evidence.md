# Product model — supporting planes — Gate evidence

[Product model — supporting planes](../planes.md) · [Spice product model](../README.md)

Gate evidence separates proof of one immutable action from admission of one
prospective commit, so work is credited without running tests inside a Git
hook. The digests, store layout, and action declarations live in `gate/src` and
`spice/src/gate_command`.

## Identities

Two identities have different jobs and never substitute for each other.

- `candidate_tree` is the tree of the exact index pre-commit observed. It binds
  an admission to the complete prospective commit and never keys proof. A
  tracked symbolic link enters it by path and recorded target, and nothing
  follows the link.
- `action_key` identifies one action and only the inputs that can change its
  result. Equal keys reuse a receipt across branches and linked worktrees even
  when unrelated paths differ. Branch, commit, and mainline identity enter a key
  only for an ancestry-sensitive action.

Producer, timestamps, and trust standing are credit and authorization metadata;
they never enter an `action_key`, so identical work earns credit wherever it
ran. Reuse requires exact key equality and a canonical receipt from the
proof-aware runner. A shell transcript, test output, a copied exit status, or a
hand-written receipt is not proof: none establishes source, declaration,
command, environment, tool, and result as one execution.

A receipt also carries diagnostics that never enter its `action_key`, input
manifest, or typed result: the lease wait before execution, per-phase wall time
(the app arm's clippy and trunk; a test shard's workspace prebuild and test
execution), Cargo's fresh and rebuilt artifact counts, a bounded tail of the
trunk log, and a producer naming the executing appliance's executable digest,
target, profile, and run. Phase time belongs to the receipt whose execution
spent it, so a prebuild an earlier shard already completed shows as fresh units
rather than a share of that shard's compile. Two receipts that differ only in
diagnostics prove the same thing, and a receipt without diagnostics still
decodes and reuses.

A packaged toolchain arm's action specification carries the commands its
declaration states, so changing what an arm runs changes its key. The app arm
runs WASM clippy and a development `trunk build` without `--release`; optimized
bundling belongs to release and install builds, which keep `--release`.

## Action classes

A `cacheable` action's result is determined by staged content and declared
non-file inputs. An `ancestry-sensitive` action is reusable only with its exact
parent, merge-parent, or committed-baseline identities in its key. A `live-only`
action reads mutable state and runs at pre-commit every time. An action that
cannot prove its complete dependency closure, or name every mutable decision
input, is live-only; it never emits an under-keyed reusable receipt.

## Proof and admission

Cargo test proof actions are planned by `plan_cargo_gate_shards_at` and executed
by `run_cargo_gate_shard_at` against the selected source tree. Each shard retains
its own action identity, input closure, and receipt.

Within one aggregate proof, contiguous typecheck actions (`app-web-typecheck`
and Cargo typecheck) and contiguous Cargo test actions may run two at a time.
Every other action remains a serial
barrier. The dispatcher reserves three logical CPUs of load headroom per action
and falls back to one action when the host's one-minute load leaves less; action
identities, receipts, retained output, and canonical report order do not change.

Pre-commit and bare `spice gate prove` never execute a missing proof. A miss
names each action with its `spice gate prove ACTION` command and one
`spice gate prove --missing` aggregate. Each executed action's complete output
is retained as `.spice/output/gate/DIGEST.log` under the Git common
directory, and a failed action's refusal carries a bounded tail and that path.
A shell pipeline can mask the prover's exit status; preserve the status rather
than inferring success from a filter or the last progress line.

`spice gate prove --all` selects as if every staged path changed, so an
unchanged commit still owes every applicable reusable action and Cargo shard.
It reuses each current receipt, proves each missing one, and lists every
receipt. Live-only actions publish no receipt and stay with pre-commit.

Compiler output in `.spice/output/cargo/` and gate output in
`.spice/output/gate/` each keep up to seven days of captures within 64 MiB per
directory. Older captures go first under byte pressure; the current complete
capture is never removed or truncated, even if it alone exceeds 64 MiB.
Reusing identical output refreshes its retention age. There is no file-count
cutoff: small, nonempty logs within the age/byte budget remain. Empty
predecessor logs and empty directories are removed during the same ordinary
directory pass. The current capture stays available even when empty.
Publication and predecessor pruning hold one filesystem lock per capture
directory, so concurrent commands cannot prune a current file before its
retaining command returns.

Build-output reclamation reports count only units with nonzero allocated data,
name their paths and bytes, and include elapsed inventory and removal time. A
pass that removes nothing and meets no busy or recently used unit prints
nothing. Lock files and their directories remain for Cargo and rustc
synchronization; empty lock-only units are not counted as reclaimed caches. A
unit directory a live writer refills before removal stays without failing the
pass. Each pass runs under the 24 GiB per-worktree bound, the ten-minute
cooldown, one-hour recent-use protection, and exclusive locks.

Direct proof and the installed pre-commit hook normalize Git-only routing and
shell prompt metadata before they identify a compiler environment. Windows'
hidden per-drive working directories (`=C:`, for example) are location-only,
like `PWD`, and never partition proof identity. Routing exclusions include
Git for Windows' injected `PLINK_PROTOCOL`. Native `gate pre-commit` removes
every leading copy of `GIT_EXEC_PATH` from `PATH`, accepting both its native
and MSYS spelling, then removes Git's repository-local routing before any
proof environment or child is captured. On macOS it also removes only the
default compiler paths injected by Command Line Tools Git or full Xcode Git;
nondefault and unrelated toolchain paths remain inputs. The same staged action
therefore has one environment identity outside and inside the hook. Rust and
app compile arms use `HOME` only when the captured compiler environment
supplies it. They neither embed the release builder's home nor synthesize a
runtime replacement; when `HOME` is absent, Cargo uses its own host resolution
or reports its own failure.

Live Git observations resolve the first regular `git` (`git.exe` on Windows)
on the normalized `PATH` to one absolute executable. A `git-core` entry is used
exactly where `PATH` placed it. Git for Windows' leading `mingw64/bin` and
`usr/bin` still route to that installation's public `cmd/git.exe`; shell-added
user `bin` and public `cmd` routing collapse from environment authority when
that same public directory contains Git. The exact selected Git path and bytes
remain bound separately. A direct producer and Git-invoked hook therefore
select and prove the same tool. Only an absent `PATH` candidate admits
`/usr/bin/git` on Unix or `Git\cmd\git.exe` beneath the shell-resolved Windows
Program Files folder, announced with that
path. A selected tool's resolution, launch, or command failure never tries the
platform default. The selected path and executable bytes participate in proof
authority, so changing Git cannot reuse evidence from another tool.

Windows proof environments canonicalize case-insensitive variable names.
Git Bash's `MSYSTEM` terminal defaults, redundant `HOME`/`USERPROFILE`, and
redundant `TMPDIR`/`TEMP` pairs are routing rather than compiler authority, so
the hook consumes the same semantic environment its parent producer supplied.

Pre-commit verifies every owed receipt, runs the live-only checks, and publishes
one admission for the candidate tree. Several admissions may exist for one tree
when branches or worktrees reach equal content from different parents; each
tree and first-parent pair indexes its newest admission. The
`reference-transaction` hook consumes that admission for each local branch
update to a new commit. A candidate whose tree has admissions but none for its
first parent, or whose indexed admission has expired, refuses
`gate-evidence::admission-candidate-mismatch`; a tree with no admission
directory consumes nothing, so a commit made without pre-commit lands with no
admission credited. Moving a ref within existing history (an unchanged tip, or
a commit already reachable from a local branch, remote-tracking ref, or tag)
needs no admission; rewinds that abandon published history refuse. No
reference-transaction state runs a test or waits for a prover.

## Evidence store

Receipts, action indexes, leases, admissions, and fingerprints live under
`.spice/cache/gate/vN/` in the Git common directory, where `N` is the store
version. Every digest-keyed directory fans out like Git's loose objects: a
record keyed `abcdef…` lives at `DIR/ab/cdef…`. A receipt is canonical minified
JSON named by its own digest, so it is never edited. Beside `objects/`,
`receipt-stats/` counts how often `gate prove` reused each receipt and how often
a committed admission consumed it, with the last time of each;
`spice gate prove --verbose` shows those counts under each action. Counting is
best effort and never fails a proof or a commit, and collection removes a
receipt's counts before the receipt. A new store version starts empty and
re-proves every action once; the previous version's directory is never read,
and deleting it reclaims its space.

## Package declarations

A workspace package whose build script or proc macro runs at compile time is
cacheable only with a closed-world declaration:

```toml
[package.metadata.spice.cargo-proof]
version = 1
complete = true
files = []        # package-relative selected-tree paths
environment = []  # exact ambient variable names
calls = []        # call paths and `.method` names the scanner admits
```

`complete = true` is the package owner's assertion that these lists, extended
by literal `cargo::rerun-if-changed` and `cargo::rerun-if-env-changed` output,
cover every ambient read the compile-time code makes. A missing or incomplete
declaration, a path outside the selected tree, an undeclared read, an
unlisted call the scanner cannot bound, or code that reaches process, network,
or dynamic authority makes the shard non-cacheable.

Rust text no target compiles (parser input, golden samples, corpora a test opens
at run time) is declared as data, and a change under it selects only that
package's own test targets:

```toml
[package.metadata.spice]
data = ["tests/fixture"]
```

A declaration covering source a target compiles refuses. Any other changed Rust
path no target claims refuses, because an unclaimed path is also what a module
the scanner failed to follow looks like. A deleted path that no target claims in
the current or base inventory is treated as data without a declaration.
