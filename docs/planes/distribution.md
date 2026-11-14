# Product model — supporting planes — Distribution

[Product model — supporting planes](../planes.md) · [Spice product model](../README.md)

## Distribution

Distribution crosses authorities the appliance does not own outright: one
tracked Git release authority and an operator's repository. Immutable release
refs cannot be repaired by pretending an earlier effect did not happen, and
repository installation must preserve state the operator already owns. Exact
plans, durable receipts, convergence, and identity-aware reversal answer those
boundaries.

### External publication is a plan, not a verb

**Invariant —** Every release verb with externally visible publication effects
renders its complete ordered plan and performs none of those effects until
execution supplies that plan's exact digest. An absent or moved digest refuses
before effects; exact-plan execution is the only publication path. Local
mutations remain governed by their own transactional contracts.

**Invariant —** Plan identity covers the repository, action, version, source and
release commits, target matrix, ordered operations, notes content, artifacts,
proof receipts, sealed manifest, and captured preconditions. Step order is part
of it, because the ordering is the safety property. The correctness operation
means the complete canonical strict battery, never a mutable subset outside the
plan.

**Invariant —** Preconditions that can be evaluated without effect are evaluated
while the plan is built, so a knowable failure never strands the operator after
an effect.

**Invariant —** Inspection is free of consequence. Checking the gates, drafting
the notes, and previewing the range never bump, commit, tag, push, or publish.

**Invariant —** Release documents are complete only as files. Bare
`spice release notes` prints a bounded summary, and `--output FILE` writes the
complete document. A publish preview retains the complete plan under the Git
common directory before it prints, names that file, and ends with the exact
`--apply=KEY` command; that key is the only accepted authority.

**Invariant —** One executor and one plan and receipt grammar serve preview,
fresh execution, and every resumption. There is one release action and one
ordered operation graph. Before the first effect, admission proves the
converged prefix: each operation is met by its durable receipt or by
authoritative external state. An exact visible package meets every operation
through registry visibility, a tag at the release commit meets tagging, and an
announcement at the release commit meets announcing. A receipt whose own
registry, tag, or announcement state is absent, external state naming another
release, or a met operation after an unmet one refuses before any effect. A met
operation is receipted as already converged without crossing its effect, so
no resumption reruns the gate, republishes a visible package, retags, or
repeats an announcement. Every other operation records its exact prior state
before crossing its effect boundary and a typed receipt after convergence. A
journal written under another journal schema refuses before any effect,
naming the release that last wrote it, and is never decoded for execution.

### A versioned commit is sealed before publication

**Invariant —** Every landing is a version. A full semantic
`dist/version/base` pins it; otherwise that file supplies the major,
`dist/version/counters` supplies the additive minor, and the patch counts
committer seconds from the newest first-parent commit that raised that sum.
The history walk considers only counter-changing first-parent commits and
stops at that raise. Exported source and hosted snapshots carry HEAD's
canonical `dist/build.json`, so builds without usable history retain HEAD's
product and source identity; a hosted snapshot carrying uncommitted changes
marks that product `-dev`, as described under
[Version identity](../implementation.md#version-identity).
Release requires clean HEAD at the release commit, already integrated in the
tracked source branch, with requested and tracked product versions in
agreement and the executable's compiled counter sum equal to the checkout's.
Every other shape refuses. Release never edits version inputs,
synthesizes a commit, or infers task ownership from the board, and a red
candidate publishes nothing.

**Invariant —** A release whose package is not yet visible executes the
complete canonical strict battery over the exact source commit the plan names,
and every arm must admit. This host-side battery is distinct from sealed target
proof replay.

**Invariant —** The artifact gate proves composition and use without rebuilding
after authorization. The package materializes from sealed content-addressed
objects and must equal the previewed package and tree identities. The
compiled-host executable named by the sealed inventory must match the plan's
artifact digest, and a plan-keyed executable copy of it must succeed at
`--json version`.

### Proof is sealed per target and composed last

**Invariant —** A release has exactly three target payloads, in canonical
order: `aarch64-apple-darwin`, `x86_64-unknown-linux-musl`, and
`x86_64-pc-windows-msvc`. Each receipt-free payload binds its target, source
identity, the source archive it was built from, app generation G, the digest
of the spice rack its executable links, native executable, build pins, and
artifact inventory, and all three bind the source digest of the
already-versioned release commit and the same archive. Every target binds the
same G and rack digest. The inventory carries no app files: the executable
is the app's only carrier.

**Invariant —** One release stages every target from one source archive.
`spice dev xtask -- source-archive` is the one point staging reads Git: it
requires a checkout exactly at HEAD, with no tracked change, no untracked path
outside release output, and no index flag hiding either, and writes HEAD's
tree with its canonical `dist/build.json` to `dist/stage/source.tar`. A local
`stage-target` extracts that archive into a fresh scratch root under the build
root and builds there with no `.git`, so the version is the record the archive
carries and the rack `build.json` is byte-identical to the archive's; the
archive bytes are rechecked immediately before the target payload pointer is
published. The hosted Windows route commits the archive's exact tree,
`dist/build.json` included, as a parentless one-use commit; the signed request
and result bind the archive identity, which the guest hands to its own
`stage-target`, and the client refuses a returned payload recording any other
archive. The release plan names the archive identity once; manifest
publication refuses targets staged from two archives, and `spice release`
refuses a sealed manifest whose archive is not the one at the stage root.

**Invariant —** Every appliance executable produced by a current build path
links one spice rack: a ZIP whose `app/` members are the staged generation's
`generations/G.zip`, with stored streams copied unchanged, beside the root
`SKILL.md` from `spice/agent/SKILL.md`, the root `LICENSE.txt` from `LICENSE`,
the root `spice.toml` packaged defaults from `gate/defaults/spice.toml`, and
`docs/...` from `docs/`. The configuration reference is served read-only at
`spice://spice.toml`. The Darwin, Linux, and
Windows release builds, and `spice dev xtask -- install` on supported macOS,
Linux, and x86-64 Windows hosts, assemble it through one path and admit it as
G, so the same staged generation and tracked sources link the same bytes.
Worktree dSYMs exist only on macOS, so elsewhere that command publishes without
them. Each build links the rack as one retained read-only section:
`__TEXT,__spicezip` on Mach-O and `spicezip` on ELF and PE32+.

Rack paths carry no namespace, allowlist, or bound. The source table in
`xtask/src/app/rack.rs` alone maps tracked files and directories to rack
members. Fixed `spice://` CLI reads resolve first; other paths resolve by
linked rack membership or directory listing.

The standalone `rack` crate is a plain STORE ZIP: it uses `zip` with default
features disabled for ZIP indexing, header consistency and deterministic
writing, and interprets no member's name or bytes. Spice's convention in
`serve/src/canvas/gzip.rs` stores each member as one gzip stream named by its
logical path plus exactly one `.gz`, so a generic ZIP tool extracts named gzip
files while every rack, `spice://`, and HTTP path stays logical. Admission
indexes once; member paths and stored streams borrow the archive, so lookup
and HTTP serving do not copy or inflate them. Inflation checks the stream's
declared size and CRC on demand. The crate's writer and executable link/read
implementations remain opt-in.

A rack URL may qualify its first segment or trail its member path with
`@CODE`, the journal short code admitting the rack linked into that executable.
Only that generation is served. `spice://@CODE` lists root members; the
unqualified `spice://` remains the CLI resource index.

Every target's relocatable object defines `spicezip` and `spicezip_len`; the
final-image readers locate exactly one rack section and recover the rack bytes
from it. The Windows target inventory names `bin/spice.exe` and the matching
`symbols/spice.pdb`; both are package members, while `proof.json` is the
package's detached signature artifact.

The Mach-O and ELF builds then derive symbols, strip, and sign where
applicable. No step appends bytes after signing. The final image's section
metadata is the canonical external extraction boundary; no build-time file
offset is persisted. The running process stores no offset and parses no image:
the linker resolves both symbols to the mapped address and byte length. The
final executable is refused unless exactly one section carries the staged
archive and its one version report names both the admitted product version and
G exactly. Only these builds write the object carrying the archive and enable
the `canvas` feature that names its symbols, so no compile step reads a file
outside the selected tree. A resident whose executable carries no archive
refuses installed serving at startup.

The Darwin target inventories one schema-1 symbol artifact containing only the
captured `Info.plist` and complete `DWARF/spice` bytes. A short canonical
metadata header is followed directly by one deterministic deflate stream over
both members; their decoded lengths split the stream without duplicating either
member or expanding it through a text encoding. Either decoded member and their
combined decoded length are bounded at 402,653,184 bytes; the complete
compressed artifact is bounded at 268,435,456 bytes. Any other schema version
refuses. Capture admits `dsymutil`'s optional
`Relocations/<one-architecture>/spice.yml` by-product but does not publish it;
any other extra dSYM entry refuses staging.

Windows deliberately admits no embedded Authenticode certificate. The
executable, PDB, payload, linked app archive, build pins, provider run, runner
image, and toolchain are instead bound by the public GitHub Sigstore
attestation over the hosted result. The PE image is re-admitted after the
link, the only byte-changing Windows step, and its content digest is re-admitted
when packaged, so neither symbol capture nor detached signing can mutate the
image.

App and release builds resolve the compiler in Cargo order: `RUSTC`,
`CARGO_BUILD_RUSTC`, the nearest ancestor Cargo config naming `build.rustc`,
the selected Cargo home's config, then `PATH`. Within a config directory,
`config` precedes `config.toml`. Configuration is parsed as TOML, so comments,
inline tables and escaped strings retain their normal meaning; malformed TOML,
a non-table `build` or a non-string `build.rustc` refuses selection.

The release adapter opens each native executable and Windows PDB once, admits
the held regular object's cross-platform identity, reads its exact admitted
length through that handle, and then requires both the handle and its final
no-follow name to retain the same identity. A link, reparse point, mutation, or
name replacement therefore refuses before artifact publication.

The Windows appliance is complete: ordinary commands, resident, journal, board,
app serving, and distribution surfaces run through their native filesystem,
process, and local-IPC adapters.

The Windows release executable exposes `--version` and `--json version`, the
identity surface exercised by sealed proof, the repository installer described
under Windows installation, the resident `up` that serves the linked app, and
the repository lifecycle owner below. Unix process supervision commands are not
silently emulated: they remain absent until they have native Windows contracts.
Disposable repository commands run through `windows-hosted`, not through this
release entry point.

**Invariant —** Darwin staging gives the release executable a notarized
Developer ID signature before the pipeline reads the bytes it digests: after
the dSYM is derived and the image stripped, it signs with the hardened runtime
and a secure timestamp, submits an archive of the signed file to Apple
notarization, and finishes only when strict `codesign` verification and
Gatekeeper's primary-signature assessment accept the file as notarized. The
target payload, its inventory, and the symbol artifact therefore bind the
signed bytes, and no step changes the executable after they are read. The
signature and notarization are a step of the one staging build, never a
rebuild: Apple's timestamp makes two signings of the same image differ, so a
signed digest is never reproduced, and nothing reproduces it. The artifact
gate and proof replay verify the sealed digests of the bytes the stage wrote.
The bare executable cannot carry a stapled ticket, so a recipient's first
assessment is online.

The signer is named by two machine-local Git-common keys,
`release.darwin.identity` (a `Developer ID Application` keychain identity) and
`release.darwin.notary_profile` (a `notarytool` keychain profile). Only names
enter configuration; the certificate, private key, and notarization credential
stay in the operator's keychain. Darwin `stage-target` and `seal-darwin` admit
the pair against the keychain and the notary service before the first build.
With neither key set they refuse before any build unless `--unsigned` asks for
an executable Gatekeeper blocks on every other Mac; with only one set they
refuse by naming the other.

Darwin is the only target with a signing route. The Linux and Windows
executables ship unsigned, so every command that builds one must say so:
`stage-target x86_64-unknown-linux-musl`,
`stage-target x86_64-pc-windows-msvc` (including the command the hosted route
runs remotely), and `windows-appliance` refuse before any build unless given
`--unsigned`.

`spice dev xtask -- share-darwin` applies the same signing to a separate built
Darwin executable outside staging; its result is not a sealed artifact.

**Invariant —** Linux staging links and executes the Linux release on Linux.
A Linux host stages `x86_64-unknown-linux-musl` natively. Any other host runs
that same `stage-target` inside the `linux/amd64` proof image selected by the
current `Cargo.lock` vendor set through the Docker or Podman executable
`proof.container` names. One image-inspection admission resolves the
deterministic vendor-set tag, admits its exact OCI identity, Linux/amd64
platform, and `org.spice.vendor-set` label, and carries that exact identity into
the staged payload's `proof_container` pin. Every target shares that pin, so
Darwin staging and the hosted Windows client resolve the same image on the
host before building and hand it to a guest as `--proof-container`; staging
any target needs the current vendor-set image loaded. A lockfile change
requires only `spice dev xtask -- proof-image build`; no tracked file records
the resulting image digest. A stale or absent image refuses before app or
Rust compilation and names that build command.

The admitted image uses the linker its root Cargo config pins, and
`release:canvas-version` executes the built executable inside it. The host
first stages the app, and the image links that generation without compiling
the app again, so Linux carries the generation every other target links. The
image mounts the worktree and its Git common directory read-only, makes
`dist/stage` its one writable worktree path, and builds with a fresh Cargo
home and target and no network. No Cargo config above the worktree
and no host tool reaches that build. With `proof.container` unset or not an
admitted runtime, or with the current vendor-set image not loaded, staging
refuses before any build and names the missing piece.

**Invariant —** A native Darwin proof runner is sealed for exactly one
worktree: `dist/proof-runner/build.sh` bakes that worktree's HEAD commit and
vendor tree into the runner and binds it through `proof.runner`, which admits
only the worktree configuration layer; a Git-common value is reported as a
policy issue and never applied. A runner whose sealed commit differs from HEAD
refuses by naming both commits and the reseal, and the staging preflight names
the bound runner it asked. `spice dev xtask -- seal-darwin [--output DIR]
[--unsigned]` is the one-command point-in-time build: it seals and binds this
worktree's runner, cuts `dist/stage/source.tar` from HEAD so no earlier archive
is reused, stages and signs the Darwin target from it, reports the commit and
archive digest it compiled, and places the executable under `DIR`, writing
only this worktree's `target/`, `dist/stage/`,
Git-directory configuration, and `DIR`; the installed appliance, the resident,
and other worktrees are untouched.

**Invariant —** Proof replays the complete current battery against the
candidate checkout: Linux mounts it read-only in the pinned OCI environment,
Darwin passes it to the attested native runner, and Windows dispatches the
batch to one attested ephemeral GitHub-hosted runner. Replay is bound to
payload, environment, and battery, but does not prove that the checkout equals
the payload's source digest. Unsupported hosts, drift, infrastructure failure,
or a non-clean replay publish no receipt; a refused replay is a typed refusal.

The Windows proof boundary requires the configured repository visibility to be
`public` before it dispatches the release battery. A private repository returns
the typed hosted-attestation prerequisite and publishes no proof receipt;
cross-builds, Wine, the Windows 11 ARM developer VM, and self-hosted keys cannot
replace this boundary.

**Invariant —** A proof receipt is detached and canonical. Admission requires
target, payload digest, environment, battery digest, replay digest, and clean
standing to form one identity chain.

**Invariant —** The release manifest is post-proof: every supported target
with a clean, mutually consistent receipt, or no release. Content-addressed
objects publish before the sole manifest pointer, so a reader sees the prior or
the new complete seal, never a pointer to missing proof.

**Invariant —** Staged artifacts and target payload pointers, proof receipts,
and release manifest objects and pointer all publish through one rooted
regular-file boundary. A path must be relative and stay beneath its root; an
ancestor that is a symlink or not a directory, or a destination that is not a
regular file, refuses before mutation. Bytes and their exact mode land in a
private same-directory file that replaces the destination only once complete,
so an interrupted publication leaves the prior file or none.

### Ephemeral native Windows execution

Windows source lives in the single `windows-execution` Cargo package under
`windows/`. Its neutral `protocol` module owns the command, result, capture, and
limit types shared by both execution roles. The `hosted` module owns the
provider-attested disposable runner and client, while the `guest` module owns
the persistent developer-guest executor. Separate binaries retain their
different trust, dependency, and lifecycle boundaries without a second package
or nested manifest. Their tracked declarations live together under
`dist/windows/`.

The repository's Windows execution boundary is a fresh GitHub-hosted
`windows-2025` x86-64 machine running Windows Server 2025 Datacenter on the
provider's Visual Studio 2026 image. No dedicated Windows host is owned, kept
running, or administered. Wine, the Darwin-to-MSVC cross-toolchain, Linux
containers, and synthetic PE fixtures remain useful format/build checks but
never substitute for this native execution.

Run one argv, with no shell joining or reinterpretation, from a selected
worktree:

```sh
spice dev xtask -- windows-hosted -- PROGRAM ARG...
```

Stage the Windows release target through the same boundary:

```sh
spice dev xtask -- stage-target x86_64-pc-windows-msvc --unsigned
```

`PROGRAM` is either a path beneath the selected worktree or one admitted tool
token: `@runner`, `@cargo`, `@rustc`, `@git`, `@cl`, or `@rc`. Proof replay
sends its five commands as one bounded batch, so one signed provider run
identity covers the complete battery. An ad hoc command materializes the
selected worktree in a temporary Git index and creates a one-use commit of it;
staging instead commits the release source archive's tree, as described under
[Proof is sealed per target and composed last](#proof-is-sealed-per-target-and-composed-last). Either way the
request binds the identity of the archive those bytes came from and a
cross-platform SHA-256 identity of the commit's recursive Git listing, which
the guest verifies against its checkout before vouching for the archive, and
the client pushes only a temporary `refs/tags/spice-hosted-*` ref. It never
pushes or moves a branch. It deletes that ref and the result artifact after
the run.

GitHub registers `workflow_dispatch` only from the default branch. The
repository command therefore becomes live only after
`.github/workflows/windows.yml` has landed there; a checkout that has the
client but predates that landing refuses at dispatch rather than selecting
another workflow.

The supported provider repository is `github.com/infimalabs/spice`.
Authenticate the machine-local `gh` client and Git transport separately.
The Git credential needs Contents write permission to create and delete tags.
The GitHub CLI credential needs Actions read/write, Attestations read, Contents
read, and Metadata read for this repository; the workflow's short-lived
`GITHUB_TOKEN` gets Attestations write for signing and Contents write only so
its `always` cleanup can retire the exact one-use `spice-hosted-*` tag, plus
OIDC identity-token write. The client cancels the exact dispatched run on
`SIGINT` or `SIGTERM`; its non-interactive provider subprocesses run in their
own process group, so a terminal interrupt reaches only the client. After the
provider returns an exact run identity, every failure first confirms that run
is terminal, cancelling it if necessary; success, failure, cancellation, and
verification refusal then retire any result artifact for that exact run. The
one-use tag is retired whenever a push was attempted. Provider cleanup accepts
only that tag and is the backstop when the client disappears before it can act.
Tokens, mutable endpoints, and provider account configuration are never tracked
policy. Rotate or revoke GitHub credentials in the provider, then run
`gh auth logout --hostname github.com`; delete or rotate the corresponding SSH
key through GitHub and the local keychain.

`dist/windows/hosted.toml` is the tracked trust policy. Its `provider`,
`repository`, and `workflow` keys bind dispatch; `[image]` pins the
`windows-2025` label, image version, OS build, and `X64` architecture; and
`[toolchain]` pins Rust, Cargo, Git, MSVC, Windows SDK and Trunk, plus every
executable, `PATH`, include, and library location. The bootstrap Cargo invokes
the pinned `RUSTC` path under the same pinned `RUSTUP_TOOLCHAIN`; workflow-step
scope never lets it fall back to the runner image's mutable stable proxy. The
workflow pins every third-party action by full commit. Updating the image means
reviewing the matching `actions/runner-images` release manifest, changing every
affected tool/path pin together, and proving the runner smoke before landing.

The sealing step builds the guest from the selected source and executes the
sealed request in the disposable VM, then signs that exact
`windows-hosted.exe` and its result separately with GitHub's workload identity.
Nothing publishes unless both attestations succeed. Verification rejects
self-hosted attestations and binds both subjects to the one-use source ref and
commit. The guest hashes itself under
`spice.proof.native-runner.v1`; the client verifies both attestations, hashes
the downloaded runner, and refuses unless that independent identity equals the
one in the result. Runner or pin changes require the same native smoke before
landing.

Every dispatch has a fresh 256-bit challenge. The signed result must name that
challenge, this repository and workflow, the provider run and attempt, the
exact source archive, image, toolchain, runner digest, and every returned
staging artifact's path, length, and digest. The client verifies GitHub's
Sigstore/OIDC attestations before it reads the result as evidence. Changed
source, image, tools, runner, repository, workflow, run identity, challenge, or
artifact bytes refuse. A successful Windows stage copies content-addressed
objects first and the mutable target payload pointer last.

The guest gives the command batch a read-only source tree, one fresh Cargo home
and target directory, bounded output per member, bounded member runtimes, and a
machine-wide guest-firewall rule covering every non-loopback IPv4 and IPv6
destination. Each command root is created suspended, assigned to a kill-on-close
Windows Job Object, and resumed only after assignment, so no descendant can run
outside the owned tree. The guest passes each argv directly through
`CreateProcess`, clears ambient environment, and names every executable by an
admitted path. Result publication happens only after the rule is removed and
scratch teardown succeeds. Provider cancellation or the
75-minute workflow limit destroys the ephemeral VM. GitHub-hosted concurrency
and billing limits are the account's cost boundary; the repository requests
one machine per dispatch and defines no persistent or autoscaling fleet.

### Persistent Windows 11 ARM developer guest

Windows work iterates on the Apple-silicon development host before it reaches
the hosted boundary. One persistent Windows 11 Pro ARM64 guest runs the pinned
x64 cross-build under Windows' x64 emulation, so NTFS, Job Object, named-pipe,
process-tree, and install behavior come from a real Windows kernel. Its results
are developer evidence only: the guest publishes no receipt, satisfies no
release, sealing, or package-replay claim, and no gate or release path executes
or trusts it. Permanent tests stay fast and in process; every guest run is a
throwaway smoke.

**Hypervisor.** The guest runs under Homebrew QEMU (`qemu-system-aarch64
-machine virt -accel hvf`) with edk2 UEFI firmware. QEMU is driven entirely
from argv and its QMP control socket, so the repository owns the whole machine
definition, and its open-source binaries and firmware are pinned by digest.
qcow2 overlays make the recorded clean state a file operation. It boots Windows
11 ARM through Hypervisor.framework.

**First-time setup.** Install the pinned bottles once with
`brew install qemu wimlib`, then run:

```sh
spice dev xtask -- windows-guest provision
```

`dist/windows/guest.toml` is the recorded configuration. It pins:
- each host tool and firmware file by path and SHA-256;
- the machine shape;
- the Windows 11 Pro ARM64 ESD from Microsoft's Media Creation Tool catalog;
- the virtio-win NetKVM driver ISO;
- the Win32-OpenSSH ARM64 MSI;
- the x64 VC++ runtime installer named by the package manifest that
  `dist/windows/msvc.toml` pins. It supplies the `VCRUNTIME140.dll` that
  Rust MSVC executables load and that the hosted image gets from Visual Studio.

Provisioning:
1. Fetches each download over its pinned URL and refuses any size or digest
   mismatch.
2. Builds read-only FAT32 Setup media with wimlib. The media carries Windows
   PE, Setup, the Pro image, the network driver, OpenSSH, the x64 runtime, the
   host's guest SSH public key, `dist/windows/guest/setup.ps1`, and
   `dist/windows/guest/autounattend.xml` with a random local password.
3. Installs Windows unattended (this takes tens of minutes).
4. Seals the installed disk and UEFI variables read-only as the clean base.
5. Learns the guest's SSH host key on first boot.

Updating a pin means changing it in the declaration and running
`provision --replace`; a guest provisioned from other pins or templates is
refused until then. Machine-local state lives in the shared
`.spice/machine/windows/` directory and is never tracked. Sizing is 4 vCPUs,
8 GiB RAM, and a sparse 64 GiB NVMe disk. Provisioning needs about 25 GB of host
space for the download cache and media, and a working guest needs 10–20 GB.

**Commands.** Each subcommand runs as
`spice dev xtask -- windows-guest <subcommand>`:

| Subcommand | Effect |
| --- | --- |
| `provision [--replace]` | Build the clean base from the recorded configuration. |
| `start` | Boot the persistent overlay in the background; returns once SSH answers. |
| `stop` | Shut Windows down cleanly; QEMU is stopped forcibly only if shutdown stalls. |
| `status` | Report whether the guest is provisioned from the current pins and whether it is running. |
| `reset` | While stopped, return the disk and UEFI variables to the clean base. |
| `sync [--artifact PATH]... [--source PATH]...` | Replace `C:\spice\work` with the named artifacts in `bin\` and the named tracked source in `source\`. |
| `run [--timeout S] [--output-bytes N] -- PROGRAM [ARG...]` | Run one argv in `C:\spice\work`. |

When `lock.windows-guest` is configured, `start`, `stop`, `reset`, `sync`, and
`run` acquire that pool. Without it this optional coordination is disabled and
those guest commands run directly. A task declaration is strict:
`--resource windows-guest` refuses unless that pool is configured. Once activated,
the task retains its configured shard for its claim; an xtask launched by the
owning lane recognizes the lane/task owner evidence and uses that retained
possession, while another lane or an unscheduled process refuses. `provision`
and read-only `status` do not take the runtime pool.

Lock-pool configuration is machine-local in the Git-common
`.spice/config.toml`, shared by every worktree. Those local bytes participate in
the executable-configuration trust digest, so adding or changing a pool
requires the operator to grant the resulting digest before pre-commit gates
admit it.

The guest persists across `stop` and `start` until `reset`. It reaches the
host only through loopback forwards: SSH on 22622 (key-only, pinned host key)
and QMP on 22623. QEMU's `restrict=on` networking gives the guest no outbound
network once it is installed.

`sync` admits only regular files whose PE machine is x86-64 beneath one of the
two pinned cross-build roots, `target/x86_64-pc-windows-msvc/` for developer
builds and `target/release-appliance/x86_64-pc-windows-msvc/` for clean
release-archive builds; containment is tested on canonical paths, so a symlink
cannot escape either root. `--source` admits only paths `git ls-files` tracks.
Every sync first builds the guest executor
through `spice dev cargo` and installs it; the executor stays pinned beneath the
developer root.

The host owns every Cargo build. A clean release cycle is:

```sh
spice dev xtask -- windows-msvc
spice dev xtask -- source-archive
spice dev xtask -- windows-appliance --unsigned
spice dev xtask -- windows-guest sync --artifact target/release-appliance/x86_64-pc-windows-msvc/release/spice.exe
```

A dirty checkout instead uses this developer-guest cycle:

```sh
spice dev xtask -- windows-msvc
spice dev xtask -- windows-appliance --unsigned
spice dev xtask -- windows-guest sync --artifact target/x86_64-pc-windows-msvc/release/spice.exe
```

Without the hosted route, this local cross-build produces the Windows
executable. A clean `windows-appliance` build consumes the same
`dist/stage/source.tar` as Darwin and Linux: it verifies that the live checkout
equals HEAD, requires the archive record to name that HEAD, extracts the
archive beneath the build root, and builds the exported tree with no `.git`.
Its Cargo root is `target/release-appliance`, its executable is
`target/release-appliance/x86_64-pc-windows-msvc/release/spice.exe`, and the
completion line reports the archive digest beside that executable and the
record's product version. An absent archive, a record for another commit, or
archive movement during the build refuses. A tracked path the index marks
assume-unchanged or skip-worktree counts as dirty, because status cannot vouch
for its bytes; the rule reads those flags and never changes them. A checkout
that starts dirty stays in the developer-guest loop: it builds the live worktree
as `-dev` beneath `target/` and does not consume a release archive. The result
is not a staged payload and carries no receipt; Cargo's intermediate
executable may remain after a refusal.

Only the x64 PE crosses into the guest; the initial guest needs neither Rust nor
Visual Studio, and Windows 11 ARM executes it through Microsoft x64 emulation.
Keep the one qcow2 disk thin-provisioned and retain no VM snapshots. Use
`spice dev xtask -- windows-guest reset` to discard smoke state and
`spice dev xtask -- build-cache --apply` when repository-owned host build roots
need reclamation.

**Shared execution protocol.** `run` and the hosted runner use the neutral
`protocol` module's types and bounds: the program-plus-arguments `Command`, the
3600 s and 8 MiB defaults (also the maxima), the 256-argument limit, the base64
`Capture` of stdout and stderr with truncation flags, and the `Completion` exit
mapping (timeout reports 124). The command prints the captured bytes, reports
the exact Windows exit code on stderr, and exits with it, as `windows-hosted`
does.

The executor runs that argv like the hosted runner does:
- straight through `CreateProcess`, with no shell;
- with a cleared environment, keeping only a fixed Windows system allowlist,
  plus a `PATH` of `bin\` and the system directories and a private
  `TEMP`/`TMP`;
- inside a kill-on-close Job Object that bounds output and runtime and ends
  every descendant on timeout.

The hosted runner's own `--smoke-argv` mode runs unchanged in the guest after
`sync --artifact target/x86_64-pc-windows-msvc/release/windows-hosted.exe`. Its
attested batch mode does not run there: it requires GitHub's run identity and
an in-guest toolchain, and the guest must never imitate the former.

Journal restore and artifact backup treat their roots as filesystem authority,
not path prefixes. Every intermediate directory is held and opened relative to
its held parent; a link, reparse point, or non-directory at any component is
corruption. Preflight, copy, and the post-copy identity check all repeat that
confined descent, so replacing an intermediate component cannot redirect bytes
into or out of a bundle.

**Port smokes.** Cross-build the port's in-process test binary and run it under
the real Windows kernel. Cargo prints each test executable's path as
`target/x86_64-pc-windows-msvc/debug/deps/NAME-HASH.exe`. These are the
commands each port's native developer smoke must pass. Each `sync` replaces
the guest's `bin\`, so name every artifact a run needs in one `sync`.

The guest account is a local administrator, and its SSH session, including
every `run`, holds an elevated token with backup and restore privileges that
override file DACLs. An elevated owner can therefore remove entries from a
sealed directory. Prove such owner refusals from an unelevated token: over the
guest's SSH, run the same test argv under
`runas /trustlevel:0x20000 "cmd /c C:\spice\work\bin\NAME-HASH.exe FILTER > C:\spice\tmp\OUT.txt 2>&1"`,
which starts asynchronously, then read `OUT.txt`.

```sh
spice dev xtask -- windows-guest start

# Filesystem and Cargo isolation
spice dev cargo -- test --locked --no-run --target x86_64-pc-windows-msvc -p spice --lib
spice dev cargo -- test --locked --no-run --target x86_64-pc-windows-msvc -p sill -p atomic -p serve --lib
spice dev cargo -- build --locked --target x86_64-pc-windows-msvc -p spice
spice dev xtask -- windows-guest sync --artifact target/x86_64-pc-windows-msvc/debug/deps/spice-HASH.exe --artifact target/x86_64-pc-windows-msvc/debug/deps/sill-HASH.exe --artifact target/x86_64-pc-windows-msvc/debug/deps/atomic-HASH.exe --artifact target/x86_64-pc-windows-msvc/debug/deps/serve-HASH.exe --artifact target/x86_64-pc-windows-msvc/debug/spice.exe
spice dev xtask -- windows-guest run -- bin/spice-HASH.exe config:: cargo_isolation cargo_output agent_wrapper
spice dev xtask -- windows-guest run -- bin/sill-HASH.exe authority lock
spice dev xtask -- windows-guest run -- bin/atomic-HASH.exe
spice dev xtask -- windows-guest run -- bin/serve-HASH.exe discovery

# Journal backup, restore, and artifact member modes
spice dev cargo -- test --locked --no-run --target x86_64-pc-windows-msvc -p sill -p journal -p artifact --lib
spice dev cargo -- test --locked --no-run --target x86_64-pc-windows-msvc -p spice --bin spice
spice dev xtask -- windows-guest sync --artifact target/x86_64-pc-windows-msvc/debug/deps/sill-HASH.exe --artifact target/x86_64-pc-windows-msvc/debug/deps/journal-HASH.exe --artifact target/x86_64-pc-windows-msvc/debug/deps/artifact-HASH.exe --artifact target/x86_64-pc-windows-msvc/debug/deps/spice-HASH.exe
spice dev xtask -- windows-guest run -- bin/sill-HASH.exe authority
spice dev xtask -- windows-guest run -- bin/journal-HASH.exe
spice dev xtask -- windows-guest run -- bin/artifact-HASH.exe
spice dev xtask -- windows-guest run -- bin/spice-HASH.exe restore_command journal_command task_artifact_command

# Job Object process ownership
spice dev cargo -- test --locked --no-run --target x86_64-pc-windows-msvc -p sill
spice dev xtask -- windows-guest sync --artifact target/x86_64-pc-windows-msvc/debug/deps/sill-HASH.exe
spice dev xtask -- windows-guest run -- bin/sill-HASH.exe spawn

# Local IPC with daemon handoff
spice dev cargo -- test --locked --no-run --target x86_64-pc-windows-msvc -p door
spice dev cargo -- test --locked --no-run --target x86_64-pc-windows-msvc -p spice --bin spice
spice dev xtask -- windows-guest sync --artifact target/x86_64-pc-windows-msvc/debug/deps/door-HASH.exe --artifact target/x86_64-pc-windows-msvc/debug/deps/spice-HASH.exe
spice dev xtask -- windows-guest run -- bin/door-HASH.exe
spice dev xtask -- windows-guest run -- bin/spice-HASH.exe supervisor::

# Supervisor and keeper lifecycle on the Job boundary
spice dev cargo -- test --locked --no-run --target x86_64-pc-windows-msvc -p sill
spice dev xtask -- windows-guest sync --artifact target/x86_64-pc-windows-msvc/debug/deps/sill-HASH.exe
spice dev xtask -- windows-guest run -- bin/sill-HASH.exe

# Repository appliance install. The skipped test reads dSYM
# identity from metadata, which on Windows carries no file ID.
spice dev cargo -- test --locked --no-run --target x86_64-pc-windows-msvc -p spice -p sill --lib
spice dev xtask -- windows-guest sync --artifact target/x86_64-pc-windows-msvc/debug/deps/spice-HASH.exe --artifact target/x86_64-pc-windows-msvc/debug/deps/sill-HASH.exe
spice dev xtask -- windows-guest run -- bin/spice-HASH.exe install:: resident:: --skip install::symbols::tests::a_read_settles_only_on_one_unmoved_regular_file
spice dev xtask -- windows-guest run -- bin/sill-HASH.exe authority

spice dev xtask -- windows-guest stop
spice dev xtask -- windows-guest reset
```

Each port uses a throwaway end-to-end consumer beside its in-process tests; the
consumer is not retained in the permanent suite. The process-ownership consumer
starts a suspended child, proves `CTRL_BREAK` grace, forces an interrupt-ignoring
root plus descendant at its runtime bound, cancels a second tree, and independently
opens each descendant PID after completion to prove the Job reaped it. The
lifecycle consumer launches a two-process tree in a new group, reaches it only by
group id to interrupt and then force it, waits for the root's forced status, and
proves the emptied and an unknown group both refuse as not found. The
repository-install consumer needs Git for Windows under Program Files and the
`spice dev xtask -- windows-appliance --unsigned` image. It installs into a fresh
repository, runs the wrapper and hooks under Git's shell, reinstalls
convergently, and upgrades under a running resident that keeps serving the app.
It then reclaims the deferred image, resumes an apply killed mid-flight, and
proves an operator occupant is refused or restored exactly. The other port
smokes cover one prepare/select/cleanup cycle and the resident start,
lane-control round trip, and shutdown.

`reset` between ports keeps each run starting from the same clean state.

**Differences from the hosted job.** Each one keeps guest evidence below the
hosted boundary:

| Aspect | Guest | Hosted job |
| --- | --- | --- |
| Emulation | x64 code runs under Windows 11 ARM's x64 emulation (Prism) on an ARM64 kernel | Native x86-64 |
| CPU features | Emulation exposes a subset of x86-64 extensions and reports a different CPU identity; feature detection, intrinsics, and floating-point corner cases may differ | Real x86-64 |
| Timing | Translation warm-up, nested virtualization, and laptop power state skew durations, timeouts, and scheduling, so deadlines must not be calibrated from the guest | Provider timing |
| Installer architecture | Windows 11 Pro client build 26100.4349 with an ARM64 OS and ARM64 system components; x64 installers may select ARM64 or emulated paths | Windows Server 2025 Datacenter 26100 x64 |
| Attestation | No provider run identity, Sigstore attestation, challenge, or receipt; results are machine-local developer output | Attested and receipted |
| Configuration | Unactivated, with TPM and Secure Boot checks bypassed for Setup and no in-guest toolchain; networking is loopback only | Pinned image, toolchain, and firewall rule |

### Notes are read out of durable landing authority

**Invariant —** Notes derive from the journal; there is no maintained changelog.
Git first-parent ancestry only selects the exact integrated commit window. The
window runs from the highest-versioned prior tag reachable from the release
commit, addressed through its fully qualified tag ref so a same-named branch
cannot redefine it. With no reachable tag, a bounded window of recent commits
names that basis. The repository's true parentless root is the initial release
boundary rather than a selected landing; commit-object parentage proves that
identity, so a shallow boundary cannot impersonate it.

**Invariant —** Every other selected commit belongs to exactly one contributing
task landing projected from a durable task Git boundary. Task identity, current
title, current topic, review standing, and typed correction relationships come
from journal authority, never from commit subjects, trailers, or revert prose.
Missing or ambiguous typed authority refuses before draft or release mutation.

**Invariant —** Landings sharing a task collapse into one entry at the task's
first selected commit. A correction task folds into its reviewed parent. A
parent whose latest durable review is a finding remains suppressed until a
later clean review. Entries group by the parent's exact current topic, never
by touched paths, and use the parent's exact current title. Buckets appear in
first-seen order and keep entry order; equal titles merge only within a bucket
and retain every selected commit reference.

**Invariant —** Curation owns the region above the generated inventory. Plan
building regenerates the inventory against the release commit, so a curated
file never publishes a stale range. Notes refuse when untouched, when they still
carry the placeholder, or when the curated region holds only banner and
headings.

### The publication sequence

**Invariant —** The only external release provider is the tracked Git authority:
remote `origin`, source `refs/heads/main`, package
`refs/spice/packages/vVERSION`, tag `refs/tags/vVERSION`, and announcement
`refs/spice/releases/vVERSION`. Runtime configuration cannot redirect a step.

**Invariant —** Publication never moves the source branch. The package ref
converges first through a guarded no-effect rehearsal; tag and announcement
converge to the release commit only after bounded observation sees the exact
package object. Exhausting that observation is a terminal refusal, not
permission to advertise unobservable content.

**Invariant —** A version names exactly one package object and one release
commit. A package, tag, or announcement ref pointing elsewhere refuses rather
than moving; a matching ref is already converged and left untouched. An
interrupted release is completed by re-running it from the first incomplete
boundary, never unwound.

**Invariant —** An operator interruption terminates with a status distinct from a
gate failure.

### Installing into a repository the product does not own

One canonical status-bearing receipt owns the exact plan, fixed-size references
to first-captured prior images, durable progress, and reversal outcomes. The
referenced raw bytes live once in a receipt-owned content-addressed store.

**Invariant —** What the operator reviews is produced purely by observation.
Apply requires that plan's exact digest, re-resolves the plan and executable
repository configuration, and refuses before mutation when either moved. A
process that derives and applies its own plan, such as the development refresh,
may carry it into apply but still proves every bound source, generation,
destination, and receipt image before effect.

**Invariant —** Hook activation orders its worktree observations and per-worktree
configuration mutations by their recorded UTF-8 path strings, removing duplicate
recorded worktrees before either inventory is built. The common configuration
switch is last. Plan production and receipt validation use the same ordering.

**Invariant —** Preview never invokes maintenance. After admission, apply and
`--unapply` may run one bounded pass that reclaims only recognized stale
install-record publications; it is not a plan operation and never imports,
repairs, or deletes foreign state.

**Invariant —** Before the first destination effect, apply atomically accepts
one canonical `applying` receipt with the exact plan, repository identity, zero
progress, and fixed-size prior-image references, then durably publishes every
referenced raw payload before changing a destination. No plan change happens
until the receipt describes it, and each completion is durably published before
the next mutation, so a crash leaves at most the recorded frontier ahead of its
receipt. Terminal `complete` is published only after every stage completes; an
interrupted apply resumes only the same plan.

**Invariant —** The receipt records the exact prior image the installation may
restore: a fixed-size mode/content reference or absence; prior configuration
values or absence; and whether it introduced a parent directory. A refresh
preserves each destination's original reversal binding and transfers each
introduced directory to exactly one reversal while separately binding the
current parent used for publication. Referenced regular-file bytes are stored
once outside the repeatedly rewritten receipt, under their domain-separated
content identity. Prior state is captured once, at first application, and never
re-derived from the installation's own output.

**Invariant —** The installed executable is the app's only carrier.
Installation writes no app file. Its receipt binds the canvas generation G
named by the executable's linked archive, beside the executable operation. A
plan reads the selected executable once and admits that archive, inflating
only its shell. Upgrading the app is replacing the executable.

**Invariant —** Retirements are derived from the installed receipt, never
listed by name. A refresh reads every operation the receipt records, with its
asset tag open: a tag this build does not know still decodes, round-trips
canonically, and is ownership evidence. Each recorded operation whose path
this build no longer publishes becomes a retirement inheriting the recorded
parent binding and introduced parents, so the file and the directories its
install introduced leave once empty. One that replaced a prior image keeps
that reversal image, and every retirement the receipt already carries is
carried forward with the image its first install captured, so unapply
restores it; one that introduced its file is a discard no unapply restores.
A retirement removes a file only while it still holds the image the receipt
recorded. A drifted file stays in place, leaves receipt ownership, and is
named with its recorded and observed digests in the preview and the refresh
report; the refresh does not refuse. Once a receipt is complete or
uninstalled, a discard's recorded parent carries no authority: removing that
directory leaves the receipt readable, auditable, and refreshable.

**Invariant —** A plan this build derives is validated against its canonical
inventory and order. A settled receipt is validated intrinsically: unique
safe relative paths with no traversal, a known location, a known mode, and
canonical digests. A recorded operation that violates those bounds refuses
before any retirement is planned, whatever its tag. A receipt at a later
schema, or carrying a plan field this build does not decode, refuses before
any plan, so a later release's receipt is interpreted only when it differs
by file inventory alone.

**Invariant —** Generated artifacts live in a directory the product owns and are
activated by redirecting the host tool there, never by writing into the location
the repository already uses. Every generated artifact carries an ownership
digest, so an untouched installation is distinguishable from a locally modified
one and from a newer generated shape.

### Windows installation keeps the same contract under Windows names

**Invariant —** The Windows installer keeps the plan, receipt, and recovery
contract above. The executable publishes to `.spice-install\bin\spice.exe`.
Receipt destinations and introduced parents use `\`-separated relative names;
plan asset paths keep their logical `/` spelling. Modes are owner-only DACLs:
an executable image is `0o700`, every other file `0o600`, and a mode granting
group or other access refuses rather than being approximated. The same limit
binds prior state: a preview refuses, before anything is touched, an existing
target whose access reaches beyond its owner, because reversal could capture
its bytes but never restore that access.

**Invariant —** Package preview is lifecycle-effect-free and binds one keeper
standing. An explicit record is `Configured` on every host; an absent record is
`Default` on Windows and no standing on Linux or macOS. A converged plan — one
the preview reports as no change — stays effect-free under apply: it neither
registers, enables, restarts, nor recreates an owner, so an explicit `spice
down` or `spice down --keeper` survives reapplying the installed build. A
publishing apply revalidates the previewed standing before stopping a resident,
retains that policy while replacing the appliance, publishes every file,
retirement, and hook stage, and holds the receipt at `establishing` until a
required owner is registered and serving under the new executable; only then
does it settle `complete`. Unix package apply with no standing creates no
owner. A registration or launch failure leaves the `establishing` receipt and
reports the boundary; the next apply of the same plan resumes owner
establishment without republishing a file, and `--unapply` reverses an
`establishing` install like a complete one. A publication that fails restores
the owner it stopped; if restoration also fails, apply reports both failures as
`install::publication-and-owner-failed`. Reversal removes only a `Default`
owner, unregistering it and draining its resident before removing the
executable and private keeper artifacts. It refuses a `Configured` owner with
`install::configured-keeper-preserved` and `spice down --keeper` guidance, so
package reversal never deletes an explicit choice.
An interrupted boundary is derived from the install receipt, keeper record,
and native registration standing; rerunning the same direction converges
without a shell monitor or second installer path.

**Invariant —** A receipt generation is the volume serial and file ID of the
object a name held. An identifier wider than 64 bits, as ReFS may assign,
refuses rather than truncating into another object's identity.

**Invariant —** Every directory from the Git common root to a destination's
parent is held without delete sharing for the whole operation, so none can be
renamed or removed beneath it. A new name is admitted by a no-replace move. An
expected destination is replaced without a gap: a second hard link first holds
the prior object and is proved to be the expected image, the staged image then
atomically replaces the destination, and a failed post-replacement proof moves
the prior object back. Every crash point leaves the destination holding the
prior or the staged image, which resume already distinguishes.

**Invariant —** Windows refuses to unlink an image a running process maps, or
a file opened without delete sharing. Removal and retirement then move that
object to a fresh canonical `.spice-install-<pid>-<counter>.tmp` name in the
same directory instead of failing. Maintenance scans the runtime directory and
the installed executable's directory. It reclaims such a file once it is
owner-only, owned by the current principal, older than the grace period, and
its minting process has exited; a file still in use stays for a later pass.

**Invariant —** Hook activation and gate execution share one Windows Git
selection. The first regular `git.exe` on `PATH` is resolved to an absolute
path; only its absence admits `Git\cmd\git.exe` beneath the shell-resolved
Program Files folder, and that exceptional selection is announced. A selected
executable that fails is not replaced by the fallback. `core.hooksPath`, the
generated hook launchers, and the alternate `GIT_INDEX_FILE` of a task's
worktree snapshot or integration tree name local-drive paths in Git's `C:/...`
form, never the verbatim `\\?\C:\` form canonicalization yields, beside which
Git for Windows cannot create a lock. A path outside a local drive refuses.
The shared spelling helper borrows unchanged UTF-8 paths on other platforms.

**Invariant —** A file belongs to the current principal when its owner is a SID
the process token may assign as owner: the token user or an enabled owner
group, such as Administrators in an elevated token. Files an elevated install
creates therefore stay owner-only to every later process of that principal,
including hooks under Git for Windows' shell, whatever default owner the
launching runtime selected.

### Reversal is a claim about identity

Reversal restores only what it can still recognize as its own.

**Invariant —** `--unapply` is the explicit reversal direction and has no
preview digest. It advances the exact `complete` (or `establishing`) receipt
generation to `unapplying`, runs the exact inverse of the canonical
application order, and publishes `uninstalled` only after every operation has
a recorded outcome. A moved generation refuses and preserves the current
receipt.

**Invariant —** Identity drift is a typed refusal, not a partial success: a
file the plan still publishes whose bytes or mode no longer match the receipt
refuses preview, apply, and reversal with `install::drift`, naming the path and
the observed digest. The refusal overwrites nothing, preserves the changed
target and the receipt, and retry resumes the same receipt. A container the
installation created is removed only when still empty.

**Invariant —** An interrupted reversal blocks a new installation until it is
resumed, so two plans never contend for one receipt's first-captured prior
state.

**Invariant —** Both directions are resumable and idempotent: a re-run performs
only operations not yet recorded complete, and a plan already satisfied against
unchanged state performs no receipt-owned effect.

### The canonical receipt

The canonical receipt is the sole installation-status, transaction, and
reversal authority.

**Invariant —** Every receipt generation is canonical and published atomically
through a real non-symbolic file, only while the exact prior generation and
complete image still match; a stale or concurrent writer refuses.

**Invariant —** An unreadable, truncated, noncanonical, linked, or inconsistent
receipt refuses; it is never read as "not installed".

**Invariant —** Every receipt is bound to the repository it describes. A
nonterminal receipt that names another Git-common owner, or a terminal copy
whose original owner still exists, fails as the store fault
`install::state-failed`. A `complete`, `establishing`, or `uninstalled` receipt
may rebase only after the old owner is gone, moving repository, plan, and
activation bindings together.

**Invariant —** The receipt remains after clean terminal directions, retaining
its plan, prior-image references, status, and progress. Its referenced raw
payloads remain available for the receipt's next executable direction. Absence
is never synthesized by deleting provenance.

**Invariant —** Install support is bounded by receipt schema, not product
version. The current schema is the one shape any install command reads. A
receipt of any other schema, older or newer, refuses before receipt-owned
mutation as release-owned state naming its schema; the appliance that wrote
it unapplies it before this build installs. Malformed and inconsistent
receipts are store failures. Only a change to the record layout, the digest
domain, or ownership semantics bumps the schema; adding or removing an
installed asset does not, because retirements derive from the receipt.

### Package installation admits only its target

`spice install --package PACKAGE` admits the canonical `release/package.json`,
then reads only the running target's executable, `payload.json`, `proof.json`,
and declared debug artifact. Their digests must match that target's index entry;
the payload, executable, debug inventory and clean proof retain their evidence
bindings. Selected paths and parent directories must be unlinked regular files
and directories, with bounded member and aggregate sizes. The index must have
mode `644`, and the package's product version must be newer than the running
appliance's; an inadmissible package refuses `install::package-refused`.

A package containing only that target and the index installs identically to
the complete package. Other targets, `release/manifest.json`, and
`release/notes.md` are not installation inputs and are neither enumerated nor
hashed. Release publication constructs and validates the complete three-target
tree through `proof::build_package`, including its closed member inventory.
Installation uses only `proof::admit_package_index` and
`proof::admit_package_target`; there is no separate whole-tree readmission API.

Preview and apply run the verified selected executable. Its temporary staging
directory remains owner-only, while staged files retain their authenticated
modes, including the data mode required by Darwin symbol admission.

### Native per-user lifecycle owner

The keeper has one host-independent policy and command surface. `spice up
--keeper` records an explicit `Configured` owner for the repository;
`spice down --keeper` removes it. The record identity, private worker,
relaunch pacing, registration transitions, replacement behavior, and package
receipt boundary are shared. A host adapter supplies only its native
definition and per-user manager operations. Windows, Linux, and macOS adapters
ship.

Package standing is deliberately platform-sensitive. Windows package apply
selects `Default` ownership when no keeper record exists, preserving the
installed appliance after logon. Linux and macOS package apply select no owner
when the record is absent: package installation alone never makes a resident
persistent. An explicit record remains `Configured` on every host. Preview
binds that standing without publishing bearer material.

The Windows adapter uses a per-user Task Scheduler task under `\Spice`, named
by a digest of the Git common directory. Its principal is the repository
user's own SID with an interactive-token logon and least privilege. No
password is stored; the task runs only while that user is logged on, which is
the earliest boundary after a reboot at which the user's profile and per-user
discovery exist. The Service Control Manager is not used. The one trigger is that
user's logon. The action is the installing executable's private keeper-worker
entry point with the repository worktree as working directory, no execution
time limit, no battery stop, and a scheduler restart only when the keeper
itself exits abnormally.

The Linux adapter writes one repository-digest-named service to
`$XDG_CONFIG_HOME/systemd/user`, or `$HOME/.config/systemd/user` when no
absolute XDG configuration root is present. The unit runs only the installed
executable's private keeper worker from the repository worktree, restarts that
worker on failure, and enables it under the user manager's `default.target`.
All lifecycle operations go directly through `systemctl --user`; no shell,
system service, password, or bearer token enters the unit. Installation proves
that the current user manager and bus are reachable before writing the keeper
record or unit. systemd reads `WorkingDirectory=` verbatim apart from `%`
specifiers, so the worktree path is written as is. A worktree path that one unit
line cannot carry, one with a line break, surrounding whitespace, or a trailing
backslash, refuses with `resident::keeper-path-unrepresentable` before any
record or unit is written. Removal also retires a unit systemd reports as
broken or masked. The owner follows the user's normal login-scoped manager
lifetime. An administrator may enable linger to extend that same owner across
logout and boot, but Spice neither reads nor changes `loginctl` account policy.

The macOS adapter writes one owner-only LaunchAgent property list to the
account's `~/Library/LaunchAgents`, labelled `dev.spice.keeper-` plus the same
repository digest, and bootstraps it into that user's `gui/UID` launchd domain.
It runs the installing executable's private keeper-worker entry point with the
repository worktree as working directory, loads only into the Aqua login
session, runs at load, and is relaunched by launchd only when the keeper exits
unsuccessfully. It names no user, password, environment, or token, and no
LaunchDaemon or root job is involved. Enablement is launchd's disabled-service
override: `spice down` disables it, `spice up` enables it, and a definition left
unchanged is never reloaded, so a running keeper survives a converged `up`.
The agent is login-scoped: logout ends it, the next login loads the retained
property list unless it is disabled, and nothing is served while the user is
logged out. A process without that user's GUI domain, such as an SSH session
while nobody is logged in at the desktop, refuses with
`resident::keeper-login-session-required` before writing a record.

The keeper holds `runtime/locks/keeper` so one runs per repository, appends to
`.spice/runtime/logs/keeper.log` (rotated like the resident log), and relaunches the
resident through the ordinary detached launch after every exit, clean or
forced. It pauses one second after a run lasting at least a minute and backs
off from two seconds to one minute while runs keep failing early. Before each
attempt it asks the native manager whether its registration remains enabled
and exits when it does not. Listen address, bind consent, and bearer token
come from owner-only `.spice/runtime/keeper.json`; the token crosses to each resident
over the private launch handoff, never argv or the native definition. An
install that leaves the port to the system pins the first bound port so
relaunches keep the endpoint.

With an owner installed, bare `spice up` applies it: refreshes its native
registration from the current executable, enables it, starts the keeper if
needed, and answers with the live endpoint. A detached `up` naming a listen or
access flag refuses with `resident::keeper-installed` and points at
`--keeper`. `spice down` disables the registration before draining, so the
keeper observes that state when the resident exits and stops; `spice up`
restores it. `spice down --keeper` disables and removes the registration,
drains a serving resident, and removes the record, log, and lock.

Install and apply are idempotent. A first install drains any ordinary detached
resident before the keeper takes ownership. A converged explicit install keeps
the pinned port and serving resident. A change to the native registration,
executable, worktree, listen address, bind consent, or token writes the new
record, drains the serving resident, retires the previous owner, and starts
the replacement. Uninstall on an absent owner refuses with
`resident::keeper-not-installed`.

Before a keeper, resident, or package lifecycle mutation, the process's
effective user and default creation owner must match the owner of the Git
common directory and any existing runtime root. A mismatch refuses with
`install::lifecycle-owner-mismatch` and names `default-owner`,
`repository-owner`, or `runtime-owner`; state is never repaired or taken over.
On Windows, elevation that defaults new objects to Administrators therefore
refuses, while unelevated Administrators membership alone does not. `spice
app` joins a kept resident and leaves it running when the window closes.
