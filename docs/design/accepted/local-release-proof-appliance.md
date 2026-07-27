# Local Release-Proof Appliance

Status: implemented contract, 2026-07-21. Deliverable for
`HARDENI-1kFzZX30`.

## Decision

Spice will expose one local command that turns a clean Git commit into tested
release artifacts and bounded machine-readable evidence using either Docker or
Podman:

```text
scripts/release-proof --engine docker|podman --output OUTPUT_DIRECTORY
```

The command is a verification boundary, not a runtime deployment or a
publication surface. It builds from an immutable archive of `HEAD`, runs the
Linux release rehearsal inside a pinned container build, exports the resulting
wheel, source distribution, and evidence, validates them on the host, and
removes only the disposable engine objects it created.

On macOS, the command then runs a separate host-native companion against the
same source identity. On other hosts it makes only the Linux-portable claim.
Uploading artifacts, authenticating to a registry, and publishing to PyPI or
GitHub remain separate credential-bearing operations.

## Constitution

The implementation is bound by these rules:

1. **A clean commit is the only source.** The command rejects a dirty worktree,
   resolves the full `HEAD` commit and tree, and delegates source materialization
   to `scripts/release-proof-source`. The container never sees untracked files
   or a writable source checkout.
2. **The engine choice is closed.** `--engine` accepts exactly `docker` or
   `podman`; arbitrary executables and command fragments are rejected. The
   orchestrator uses only the portable `build`, `create`, `cp`, and removal
   lifecycle documented below.
3. **Proof inputs exclude operator authority.** No operator home, package
   cache, SSH directory, Git credential store, environment file, publication
   token, engine socket, or writable source mount enters the proof container.
   The command does not accept a secret, credential, build-argument, volume, or
   socket option.
4. **Proof outputs are files, not a service.** The final image is an
   artifact-carrier image with no deployed process. A stopped container exists
   only long enough to copy `/artifacts` to host staging.
5. **Output is new and transactional.** `OUTPUT_DIRECTORY` must not exist and
   must be outside the source worktree. A successful run publishes one complete
   validated directory. A failed run publishes only bounded, redacted failure
   evidence and never a partially trusted artifact set.
6. **Claims remain platform-scoped.** `release-proof.json` records only the
   Linux container proof. `release-proof-macos.json`, when present, records only
   the macOS host-native remainder and is source-bound to the Linux report. The
   companion never edits the Linux report.
7. **Diagnostics are bounded and safe to retain.** Every captured engine or
   validation stream passes through the shared release-proof redaction and size
   limits before it is written. Raw subprocess output is never copied into the
   result directory.
8. **Cleanup is exact and local.** Image and container names contain the full
   source identity plus an unguessable run identifier. Cleanup names those exact
   objects. The command never prunes, removes by glob, or touches another run's
   resources.
9. **Publication stays outside.** No proof phase logs in, pushes an image,
   uploads a distribution, signs with operator credentials, or mutates a remote
   system. A future publisher may consume the exact validated directory but
   cannot be folded into this command.

## Boundary and Data Flow

```text
clean host HEAD
    |
    | scripts/release-proof-source
    | (git archive + current identity + tagged prior-store source)
    v
private, read-only-by-convention temporary context outside the worktree
    |
    | docker|podman build (COPY only; no source mount or engine socket)
    v
pinned Linux proof stage -> tests -> builds -> release-proof.json
    |
    | final scratch stage copies only /proof/artifacts
    v
stopped, run-scoped artifact-carrier container
    |
    | docker|podman cp /artifacts/. to private host staging
    v
host validation -> atomic successful output publication
    |
    +-- Darwin only: source-bound host-native probes
```

The host orchestrator may communicate with the selected engine through the
operator's normal CLI configuration. That host-side access is unavoidable and
is deliberately not delegated into the build. The engine socket itself never
appears in the build context, image, container mounts, or command arguments.

The temporary source context and output staging directory are created outside
the worktree with owner-only permissions. They are deleted in a `finally` path.
Interrupt and termination handling preserves the same exact-object cleanup
rule.

## Command Contract

### Preconditions

Before invoking the engine, the orchestrator verifies all of the following:

- it is running inside the repository containing the proof definition;
- `HEAD` resolves to a full commit and the worktree is clean;
- the selected engine executable exists and its version probe succeeds;
- the output path is absolute after resolution, absent, and outside the
  worktree;
- the repository exporter produces a context whose
  `.release-proof/source.json` names the resolved commit and tree; and
- `.release-proof/prior-stores.json` binds the release the tree upgrades from —
  the newest tag reachable from `HEAD` sorting strictly below the version the
  tree declares — to its peeled commit and the source-only schema surfaces for
  the exact team, ACK, maxim-metrics, and projection store inventory; and
- the reserved `.release-proof` source path and other exporter invariants pass.

No precondition failure builds an image. It still produces a bounded failure
report at the requested output path when that path is safe to create.

### Portable engine lifecycle

For an engine executable `E`, unique image `I`, unique container `C`, exported
context `S`, and private output staging `O`, the only material lifecycle is:

```text
E build --file S/release-proof/Containerfile --tag I S
E create --name C I
E cp C:/artifacts/. O
E container rm C
E image rm I
```

Cleanup commands run when their corresponding object was created, even if a
later step fails. Their failures are reported but do not trigger broad cleanup.
The implementation may use the engines' accepted short aliases (`rm`, `rmi`)
only if parity tests prove identical Docker and Podman behavior; the explicit
noun forms above are the preferred contract.

This deliberately avoids BuildKit-only output flags, engine mounts, privileged
mode, a running container, and engine-specific APIs. Docker documents that
[`docker container create`](https://docs.docker.com/reference/cli/docker/container/create/)
creates without starting and that
[`docker container cp`](https://docs.docker.com/reference/cli/docker/container/cp/)
supports running or stopped containers. Podman documents the corresponding
[`podman create`](https://docs.podman.io/en/latest/markdown/podman-create.1.html)
and states that
[`podman cp`](https://docs.podman.io/en/latest/markdown/podman-cp.1.html)
also supports running or stopped containers. Copying the contents of a fixed
directory from a stopped container is therefore the shared export seam.

### Container shape

`release-proof/Containerfile` becomes a multi-stage definition:

- the existing Linux proof stage retains the digest-pinned Playwright base,
  isolated `HOME` and caches, non-root proof user, synthetic Git repository,
  tests, browser proof, mutation checks, package build, and content comparison;
- the proof stage writes only the release bundle beneath `/proof/artifacts`;
  and
- a final `FROM scratch` carrier stage copies `/proof/artifacts` to
  `/artifacts`.

The final stage has no shell, entrypoint, exposed port, health check, writable
source, or runtime responsibility. `create` does not start it; it merely gives
both engines a common object from which `cp` can export files.

The base image digest and declared package/tool pins remain the reproducibility
inputs. Network access during image construction remains necessary for pinned
APT, npm, and Python dependencies. This design does not mislabel that build as
offline or hermetic; it excludes operator credentials and mutable source state,
not all network reads.

## Result Contract

### Success

Before publishing a successful output directory, the orchestrator requires:

- exactly one wheel and one source distribution with the expected project
  names;
- one `release-proof.json` that validates as the current evidence schema;
- a report source commit and tree equal to the preflight and exported source
  identities;
- artifact filenames, byte sizes, and SHA-256 digests equal to the report;
- successful Linux test, prior-store upgrade, browser, mutation, build,
  metadata, installation, and sdist-to-wheel comparison results; and
- no symlink, device, socket, unexpected directory, or undeclared top-level
  file in the copied bundle.

The validator reads regular files without following symlinks. It rejects an
unexpected inventory rather than quietly copying it forward. Only after all
checks pass is host staging renamed to the requested output directory.

On Darwin, successful host-native validation adds the sibling
`release-proof-macos.json` before the same atomic publication. On non-Darwin,
the successful output contains the Linux bundle alone and the Linux report's
platform boundary continues to name the host-native companion as a separate
requirement rather than implying it ran.

### Failure

The image build is the proof execution. If a `RUN` step fails, its in-container
`/proof/artifacts/failures` directory is not reliably exportable from the
unfinished image on both engines. The portable failure contract is therefore
the orchestrator's captured build stream, not a claim that a failed layer can
be copied out.

Every failed phase writes a small machine-readable status report naming:

- schema version, UTC start/end timestamps, selected engine and its reported
  version;
- source commit and tree when preflight established them;
- failed phase and exit status or signal;
- whether image/container cleanup succeeded; and
- the deterministic names, retained byte sizes, truncation state, and SHA-256
  digests of bounded diagnostic files.

Diagnostics use the existing release-proof limits: at most eight retained
files and 64 KiB per file. Environment-derived secrets and every credential
channel in URLs (userinfo, sensitive query values, and sensitive fragment
values) are redacted before persistence. The process exit remains nonzero.
Failure publication uses a separate staging directory and never includes a
wheel or source distribution that did not pass host validation.

## Linux and macOS Claims

The Linux proof is portable across a Docker or Podman host because its tested
system is the digest-pinned Linux image. It can claim only the behavior actually
exercised there.

The macOS companion is deliberately host-only. It must:

- require Darwin and verify that host `HEAD` exactly equals
  `release-proof.json`'s source commit before running;
- exercise a real bounded Darwin kqueue/FSEvents notification through the
  production watcher path, rather than a monkeypatched stand-in;
- probe the host appearance and speech surfaces already named by the evidence
  contract;
- preserve the Linux report byte-for-byte; and
- write a sibling report that repeats the bound source and Linux-report digest.

The companion cannot be treated as valid until
`HARDENI-1kG0kf2K` repairs the currently known source-binding, real-event, and
URL-redaction integrity gaps. That task is an implementation dependency, not an
exception to this design.

Running the command on Linux does not fail merely because no macOS companion is
possible. Running it on macOS does fail if the Linux proof succeeds but the
required host-native companion does not, because the local macOS command is
responsible for the complete claim it advertises.

## Orchestrator Shape

The public script is a thin executable entrypoint into a Python orchestrator.
Python owns path containment, subprocess argument arrays, process-group
timeouts, redaction, hashing, JSON serialization, signal cleanup, and atomic
publication. Shell interpolation does not assemble engine commands.

Each material phase has a named deadline and terminates the complete child
process group when exceeded. The image build budget is intentionally longer
than individual validation commands but remains finite and appears in failure
evidence. An interrupt returns the conventional nonzero status only after
attempting exact-object cleanup and persisting bounded diagnostics.

The run identifier is generated locally and appears only in ephemeral engine
object names and the failure report. It is not a reproducibility input. The
commit, tree, Containerfile digest, pinned base digest, toolchain pins, and
artifact hashes remain the evidence-bearing identities.

## Verification Battery

Implementation is accepted only when all of these observable checks pass:

1. **Argument and path tests** cover the exact engine allowlist, missing
   engines, dirty sources, unsafe/existing outputs, and source/export identity
   mismatches.
2. **Engine transcript parity tests** use fake `docker` and `podman`
   executables and require the same ordered build/create/copy/remove lifecycle,
   unique names, list-valued arguments, and absence of mounts, sockets,
   credentials, publication, privileged mode, or arbitrary engine flags.
3. **Carrier-image tests** verify the multi-stage definition, scratch final
   stage, fixed `/artifacts` inventory, and absence of runtime behavior.
4. **Success validation tests** reject every filename, type, count, size,
   digest, source identity, schema, or claim mismatch before atomic publication.
5. **Failure tests** cover preflight, build, create, copy, validation,
   host-native, timeout, signal, and cleanup failures; they prove diagnostic
   bounds and redaction for environment values and URL userinfo/query/fragment
   credentials.
6. **Concurrency tests** run independent fake-engine proofs for the same commit
   and prove their exact cleanup cannot collide.
7. **Real engine smoke tests** run the full command once with Docker and once
   with Podman on Linux-capable hosts, validate both bundles, and compare their
   artifact hashes and evidence claims. A development host with neither engine
   may run the deterministic fake-engine battery but cannot claim this final
   portability check.
8. **Darwin integration tests** exercise an actual bounded host event and bind
   the companion to the exact container commit and report digest. Non-Darwin
   tests prove the Linux-only result makes no macOS claim.
9. **Existing release gates** continue to pass, including tagged prior-store
   generation and current-writer upgrade rehearsal, package installation,
   browser evidence, mutation evidence, sdist rebuild comparison, redaction,
   and host-native tests.

## Non-Goals

- Replacing hosted CI as an independent environment or review signal. The
  appliance removes dependence on a maintainer checkout for the proof itself;
  CI may still invoke the same command.
- Running Spice as a deployed container service.
- Supporting arbitrary OCI engines, remote builders, Kubernetes, Docker
  Compose, BuildKit-specific exporters, or registry-based artifact transfer.
- Providing an offline dependency mirror or claiming bit-for-bit image
  reproducibility beyond the pinned inputs and recorded artifact comparison.
- Publishing, signing with operator credentials, generating release notes, or
  mutating a remote release.
- Treating a Linux container as proof of macOS-only APIs, or treating simulated
  host tests as native platform evidence.

## Consequences

The release proof becomes a single reproducible local operation with a narrow,
auditable authority boundary. Docker and Podman share the same stopped-container
export seam, while engine-specific convenience features stay out of the trust
contract. Failed builds still leave useful bounded evidence without promising
unportable access to intermediate layers. The strict source identity chain
connects host commit, exported archive, container report, artifacts, and the
optional macOS companion, so evidence from different checkouts cannot be
silently combined.

The tradeoff is an extra carrier layer and a stopped container during export.
That small lifecycle is preferable to engine-specific output mechanisms because
both engines document it, it does not execute the carrier, and it keeps the
result boundary visible as ordinary files.
