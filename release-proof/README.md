# Release-proof source boundary

This directory is the hermetic input layer for the local Docker or Podman
release proof. From a clean worktree, create a new context with:

```sh
scripts/release-proof-source /tmp/spice-release-proof-context
```

The exporter expands `git archive HEAD`, so ignored and untracked checkout
state never enters the context. The new output directory must be outside the
source worktree and may not already exist. The exporter records the original
source commit, tree, and commit timestamp in `.release-proof/source.json`.

`Containerfile` accepts only that exported context. During the image build,
`init-source.py` proves that the exported tracked tree still equals the
recorded source tree, then creates a deterministic synthetic commit. The
synthetic repository keeps the source's SHA-1 or SHA-256 object format and
restores every archived tracked path even when a tracked ignore rule matches
it; ignored checkout-only residue is absent before staging begins. The
source and synthetic identities live separately in
`.git/release-proof-identities.json`; repository-aware code sees a real clean
Git worktree without mistaking the synthetic commit for release provenance.

The Linux base is the multi-architecture Playwright 1.61.0 Noble manifest
pinned in `toolchain.json`. Python, Node, Chromium, Git, Taskwarrior, uv, and
packaging versions are resolved inside the built image and written to
`.git/release-proof-toolchain.json`. No operator home, package cache,
credential directory, source bind mount, or container-engine socket is part of
this boundary.
