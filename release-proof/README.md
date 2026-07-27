# Local release-proof appliance

Install and start either Docker or Podman, check out the clean commit to prove,
then run exactly one local command:

```sh
scripts/release-proof --engine docker --output /tmp/spice-release-proof
# or: --engine podman
```

The output path must not exist and must be outside the worktree. Success writes
exactly the tested wheel, tested source distribution, and Linux
`release-proof.json`; on macOS it also writes the source-bound
`release-proof-macos.json`. The command validates the complete file inventory,
regular-file types, source commit and tree, schema, byte sizes, and SHA-256
digests before publishing the directory atomically.

Failure exits nonzero and, whenever the output path is safe to create, writes
only `release-proof-failure.json` plus bounded redacted logs under `failures/`.
The status names the failed phase, selected engine and version, source identity
when known, exact run-scoped cleanup outcome, and each diagnostic's size and
SHA-256. An unsafe or pre-existing output is never overwritten; that preflight
status is emitted as JSON on stderr instead.

The command uses the shared Docker/Podman lifecycle `build`, `create`, `cp`,
`container rm`, and `image rm`. It never starts the scratch artifact carrier,
mounts a source directory, passes a container-engine socket, reads files from
the operator home into the context, accepts a credential/build-argument option,
logs in, pushes, uploads, or prunes. Publication and signing remain separate
credential-bearing operations over the already validated output directory.

## Source and Linux boundary

`scripts/release-proof-source` is the appliance's lower-level hermetic input
step. The orchestrator invokes it in a private temporary directory; it can also
be inspected directly with `scripts/release-proof-source OUTPUT_DIRECTORY`.
The exporter expands `git archive HEAD`, so ignored and untracked checkout
state never enters the context. The new output directory must be outside the
source worktree and may not already exist. The exporter records the original
source commit, tree, and commit timestamp in `.release-proof/source.json`.
It also resolves the release this tree upgrades from—the newest tag reachable
from `HEAD` that sorts strictly below the version the tree declares in
`pyproject.toml`—and writes its peeled commit plus the tagged Python schema
source for the team, ACK, maxim-metrics, and projection stores to
`.release-proof/prior-stores.json`. The exporter also builds that tagged
release and carries its wheel plus digest in
`.release-proof/prior-artifact/`. The schema provenance carries source text
only—never a generated SQLite file—and explicitly classifies a store that did
not exist in the predecessor.

`Containerfile` accepts only that exported context. During the image build,
`init-source.py` proves that the exported tracked tree still equals the
recorded source tree, then creates a deterministic synthetic commit. The
synthetic repository keeps the source's SHA-1 or SHA-256 object format and
restores every archived tracked path even when a tracked ignore rule matches
it; ignored checkout-only residue is absent before staging begins. The
initializer excludes the reserved provenance paths while it validates the
original tracked tree, then carries them into the synthetic commit. The source
and synthetic identities live separately in
`.git/release-proof-identities.json`; repository-aware code sees a real clean
Git worktree without mistaking the synthetic commit for release provenance.

The Linux base is the multi-architecture Playwright 1.61.0 Noble manifest
pinned in `toolchain.json`. Python, Node, Chromium, Git, Taskwarrior, uv, and
packaging versions are resolved inside the built image and written to
`.git/release-proof-toolchain.json`. No operator home, package cache,
credential directory, source bind mount, or container-engine socket is part of
this boundary.

The final build step runs the full Python suite, Ruff, both prior-store upgrade
rehearsals, every release-safe browser smoke, and the committed deterministic
mutation cohort before it creates `/proof/artifacts`. The source-level
rehearsal generates temporary databases from the tagged source and opens the
exact four-store inventory with current writers. The installed-artifact
rehearsal installs the carried predecessor wheel and the release wheel in the
same isolated virtual environment. Through those installed packages it resolves
every governed and excluded store path inside a scratch repository, seeds
nonempty team, ACK, maxim, and task authority facts, requires forward in-place
authority migrations without file replacement, writes post-upgrade facts, and
rebuilds the projection store. It materializes committed `HEAD` into a fresh
build tree so ignored host residue cannot enter either artifact, builds the
canonical sdist and wheel exactly once, checks both with Twine, installs that
exact wheel for import and console-command probes, then rebuilds a comparison
wheel from that exact sdist. `release-proof.json` records the source identity,
toolchain, test counts, upgrade evidence, browser scenarios, mutation cohort,
carried artifact digests, rehearsal checks, and every member-level comparison.
ZIP container-byte reproducibility is explicitly deferred; any missing, extra,
or content-changed wheel member is reported by path and SHA-256.

A direct host invocation of `rehearse.py` exercises the same artifact chain but
does not manufacture the container-only source-identity or resolved-toolchain
records. Its receipt marks both records as absent and labels the claim boundary
`host-artifact-rehearsal`; the container appliance remains the only path that
can emit the full Linux release claim. Git-private records follow the checkout's
`.git` directory or required `gitdir:` pointer, so linked worktrees use their
real Git directory instead of treating the marker file as a directory.

Failed gates write deterministic logs capped at eight files and 64 KiB per
file. Userinfo and sensitive query or fragment URL values, plus values from
credential-like environment names, are redacted before persistence.

## Host-native macOS remainder

The container claim remains Linux-only. On macOS the public appliance command
automatically runs the companion after the Linux artifacts validate. The
companion remains directly inspectable with:

```sh
python3 release-proof/hostnative.py --evidence-dir /path/to/artifacts
```

The companion requires the checkout's full `HEAD` to equal
`release-proof.json`'s original source commit. It drives a real, bounded
filesystem write through the production kqueue watcher, probes system
appearance and speech synthesis, then writes the matching source identity and
results to `release-proof-macos.json` beside the byte-unchanged Linux report.
