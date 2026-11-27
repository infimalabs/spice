# Sealed Linux proof image

The `linux/amd64` proof environment contains the immutable Rust base, pinned
tooling, required Rust targets, vendored registry sources, and the `wasm-opt`
release build used by proof. It runs with networking disabled and fresh Cargo
home and target mounts.

```sh
spice dev xtask -- proof-image build
spice dev xtask -- proof-image load target/proof-image/spice-proof-image.tar
spice dev xtask -- proof-image verify
```

`build` fingerprints the sorted external package identities in `Cargo.lock`,
uses that digest as both the image's `org.spice.vendor-set` label and its one
deterministic local tag, and admits the exact OCI identity, platform, and label
reported by inspection. By default it writes the image archive with its
`.sha256` and `.identity` sidecars under `target/proof-image/`. `load` checks
the archive checksum and recorded identity, admits the loaded image's platform
and vendor set against the current `Cargo.lock`, then restores the deterministic
tag.
`verify` runs `build` and then `load` against the same archive.

A `Cargo.lock` change needs only:

```sh
spice dev xtask -- proof-image build
```

No tracked file records the resulting OCI digest. `proof-image build` and
`proof-image load` invoke Docker directly. Linux proof replay and sealed
staging instead name an absolute Docker or Podman executable as
`proof.container`. Their image-inspection admission resolves the current
vendor-set tag, admits its exact identity and Linux/amd64 platform, and retains
that identity in the staged payload's `proof_container` pin. A missing or stale
image refuses with `spice dev xtask -- proof-image build`; replay never
discovers this state from a later Cargo failure. Admitted replay uses a
read-only workspace, empty temporary caches, and no network.

## Container invocations

Containers are a release exception, never a development prerequisite. Exactly
five production surfaces invoke one:

- `proof-image build` builds, inspects, self-checks with networking disabled,
  and saves the Linux appliance whose architecture, filesystem, tool closure,
  and OCI identity the proof contract records.
- `proof-image load` checks, loads, inspects, and deterministically retags an
  archive without rebuilding it.
- [`proof/src/container.rs`](../../proof/src/container.rs) runs the
  `x86_64-unknown-linux-musl` battery in the exact image resolved from the
  current vendor-set tag. The OCI boundary supplies the read-only workspace,
  network namespace, fresh cache mounts, exact platform, and one attested
  scratch owner. The Darwin target uses the
  [sealed native runner](../proof-runner/README.md) instead.
- `stage-target x86_64-unknown-linux-musl` on a non-Linux host runs the native
  Linux `stage-target` in that same resolved image, launched by
  [`proof/src/container/stage.rs`](../../proof/src/container/stage.rs). The
  worktree and its Git common directory are read-only, `dist/stage` is the one
  writable worktree path, the Cargo home is a fresh tmpfs, the Cargo target is
  a fresh anonymous volume, and the network is disabled. A Linux host stages
  natively.
- `windows-cross` builds the Windows cross image from
  [`dist/windows-cross`](../windows-cross/Dockerfile) over this resolved
  image, adding Debian's pinned LLVM 19, the Windows Rust target, and a copy
  of the MSVC sysroot `windows-msvc` admitted under the operator's recorded
  licence acceptance. Because it carries that sysroot, the cross image stays
  local and is never published. It then runs the image's
  `windows-cross-build` under the `stage-target` boundary above, with
  `windows-cross/out` under the build root as the one writable path in place
  of `dist/stage`. That run cross-builds the candidate, `windows-hosted`,
  `xtask`, and every proof member's test executables for
  `x86_64-pc-windows-msvc`. The host packs the artifact root and writes its
  prepared manifest, which names every output's path and SHA-256 under a build
  key over the source archive, `Cargo.lock`, compiler, target, Cargo
  arguments, MSVC generation, and cross image.

All three sealed containers also mount an empty, non-executable tmpfs at `/tmp`.
Nothing in the build is pointed at it (`TMPDIR` stays under the target
directory the image fixes); it exists for Rosetta, which translates the amd64
image on an arm64 Docker host and aborts any thread whose context it must back
with a temporary file there when that file cannot be created.

Any further production invocation must declare its constraint here.

## Cleanup

The build context under `target/proof-image/context` is rebuilt each time. The
default archive and its checksum and identity sidecars are recovery artifacts;
`build.log` and `build.status` record the latest build attempt. All live under
`target/proof-image/`. Deleting the archive or either sidecar removes the load
route, so a missing image then requires another sealed `linux/amd64` build.
Restore a missing image with `proof-image load` rather than rebuilding. Never
use a Docker system, image, volume, or builder prune as worktree cleanup: they
are daemon-wide and may destroy shared evidence.
