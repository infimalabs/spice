# Sealed Darwin proof runner

`build.sh` seals the `aarch64-apple-darwin` half of release proof: the `proof`
crate's `spice-proof-darwin-runner`, compiled with one pin document naming the
source commit, the vendored `Cargo.lock` closure, the SDK, developer, and Cargo
cache directories, and the path and bytes of each boundary tool (Cargo, rustc,
Git, and `sandbox-exec`). Run it from the repository root:

```sh
dist/proof-runner/build.sh
```

An optional first argument names the Cargo target directory both runner builds
share; it defaults to the worktree's `target/`. The hosted macOS job passes its
bootstrap target so the runner reuses the release dependency graph already
built for `xtask` and `spice`.

It requires native arm64 macOS, a tracked tree at `HEAD`, and no non-ignored
untracked files outside release output. It writes the runner, its vendored
closure, and a readable copy of the pins under `target/proof-native-darwin/`,
then exercises the runner protocol end to end and reports the runner's
identity.

`build.sh` also binds the runner's absolute path as `proof.runner` in the
worktree that sealed it. It uses `spice config proof.runner ABSOLUTE_PATH` to
plan the write and repeats that command with `--apply=DIGEST` to apply it to
the worktree's Git-directory `.spice/worktree.toml`. The binding is admitted
only there; a `proof.runner` entry in Git-common `.spice/config.toml` is refused,
so worktrees sealing runners from different commits cannot repoint one another.
`spice dev xtask -- stage-target aarch64-apple-darwin` refuses until the
configured runner attests, and proof replay admits it by digesting the exact
bytes it is about to execute.

Pins are compiled into the executable, so a runner built without them refuses
and editing them changes its identity. Before every execution the runner
digests its own executable against the identity it was invoked under,
revalidates every pin, emits its attestation on stderr, and runs the battery
under `sandbox-exec`. The sandbox denies off-host network routes (while
retaining loopback because the battery's own corpus binds it), makes the
repository and Git-common directory read-only, makes host Cargo caches and the
library prefixes probed by `-sys` build scripts unreadable, denies access to
neighbouring checkouts, and removes its entire private scratch root afterward.
Cleanup restores owner access only on real directories inside that private
root, so member-created read-only trees cannot leak the root and symlink targets
are never traversed.

The profile opens with `(allow default)`, so it denies named things and still
denies less than the container half, which has no host filesystem at all: it
establishes that the prefixes host dependencies are found under are
unreachable, not that the host is absent. The rationale for each rule lives
with the profile in
[`proof/src/bin/spice-proof-darwin-runner/main.rs`](../../proof/src/bin/spice-proof-darwin-runner/main.rs).
