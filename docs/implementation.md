# Spice implementation profile

[Spice product model](README.md)

Appliance-specific choices and their code owners. The live code is
authoritative.

## Task identity

Task stamps and steering keys share a nine-character, vowel-free base-52
microsecond codec, so ordinary string order matches the encoded moment. Both
allocators advance to the next free later microsecond. A task batch retains
authored order, but unrelated wall-clock and journal events mean stamp order is
not task-creation chronology; journal sequence and durable topic origin are.
Steering-key allocation advances past the last indexed key and never moves
backwards, so key order remains mint order even when the wall clock has not
advanced.

Owners: [`wire/src/witness.rs`](../wire/src/witness.rs),
[`steer/src/ack/keys.rs`](../steer/src/ack/keys.rs),
[`board_command/client.rs`](../spice/src/board_command/client.rs), and
[`resident/steering.rs`](../spice/src/resident/steering.rs).

## Agent sessions

A lane's opening request first negotiates OMP's RPC protocol version; only that
version chunks an oversized frame whole instead of compacting or replacing it,
so a refusal fails the lane. Capture reassembles chunks before they reach the
tape, giving live decoding and replay the same bytes.

The request then states queue and interrupt dispositions, the model OMP's
catalog resolves, its effort, and fast mode, which outranks a resumed session's
recorded tier. The first prompt waits for every exact correlated answer. A
same-ID answer naming another command fails closed, as does any refusal except
the exact no-tier fast-mode response ([refusals](loops.md)). Each remaining
process argument in [`vendor/src/omp.rs`](../vendor/src/omp.rs) explains why no
command replaces it. Each launch pins its configuration overlay and steering
extension beside the occupant's supervisor record without changing
global OMP settings. Unstated settings, including other model families' tiers,
remain OMP-owned. The extension removes OMP's steering wrapper before the
provider reads the lane's message.

The overlay's subagent and eval-tool restrictions, and its publishing gates,
follow the inventory beside [`config_overlay`](../vendor/src/omp.rs). A seat
never hosts a collab room, publishes images, sends tool output to Spotlight,
or files auto-QA reports, and its bash tool denies `omp share`, `omp clip`,
`omp stream`, `omp skill(s) publish`, `omp grievances push`, and
`gh gist create`. These gates are not containment: eval-spawned processes and
hand-written network calls stay outside them.

The first turn is one `prompt` request with follow-up admission. A fresh or
idle session starts it immediately; a resumed session that restored an
in-flight turn queues it once behind that owner, so OMP answers accepted or
queued instead of exposing its transient busy refusal. Other prompt refusals
remain visible and state that no message was accepted. The steering extension
coalesces background notices by purpose at each step boundary. A refused aside
restores every unchanged purpose for the next boundary; a replacement or clear
made while the send was in flight wins.

Owners: [`vendor/src/omp.rs`](../vendor/src/omp.rs),
[`vendor/src/omp/frame.rs`](../vendor/src/omp/frame.rs),
[`vendor/src/omp/steering.mjs`](../vendor/src/omp/steering.mjs), and
[`lane/src/capture.rs`](../lane/src/capture.rs).

The `scripted` vendor seats a real OMP occupant that plays a timed cue script
instead of calling a model. It launches the admitted `vendor.omp.binary` with
the ordinary omp launch contract, the scripted model, and a provider extension
the lane writes beside the seat's steering extension, so every record stays
omp-authored. The lane's model slot names the script as `PATH[@RATE]`: `PATH`
is relative to the worktree, the last `@` separates the rate, and the rate
divides every cue time (`2` plays twice as fast, `0.5` half as fast, `instant`
removes every wait; default `1`). Launch refuses a missing script or a
malformed rate or cue, naming its line. Its occupants share omp's session
variable, resolved by omp's ambient session gate. The child receives only the
admitted environment, so no ambient model credentials reach it.

A cue script holds one cue per line, `TIME EVENT [ARGUMENTS]`; blank lines and
`#` comments are ignored. `TIME` is a start on the base clock spelled `SS`,
`M:SS`, or `H:MM:SS`, with up to three fractional digits, and never decreases.
Each model request takes the next reply cue no earlier than its scaled time:

- `say TEXT` replies with assistant text.
- `call TOOL {JSON}` replies with one tool call.
- `steering TEXT` acknowledges a directive delivered by that time through
  `spice://outbox` with `TEXT` as its clause: operator steering the scenario
  driver sends with `spice steer`, or a typed peer packet such as a task
  invitation.
- `decline TEXT` answers the same delivery with `NAK` and `TEXT` as its clause.
- `compact SUMMARY` requests a manual compaction at its time as the `say`
  before it ends its turn, with `SUMMARY` as the summary, so the compaction
  asks no model request. That `say` is held until then, because a settled
  occupant may be retired before a later compaction starts. Later requests,
  such as omp's continuation, take the following reply cues.

Native OMP transport proofs use `spice dev xtask -- app-demo --scripted-omp`:
the ordinary disposable app-demo root, with its own agent home, seats one
`scripted` lane playing the built-in
[`scripted_proof.cue`](../xtask/src/app_demo/scripted_proof.cue) at the
`instant` rate. The driver delivers real operator steering through the
resident, wakes the lane, then awaits in order the delivered steering, the
occupant's `spice://outbox` host request, OMP's manual compaction record, the
script's last reply, and the resident's acknowledgement and compaction facts.
A refused cue or missing record fails the run and retains its root. Require it
only when acceptance depends on occupant-authored OMP transport or lifecycle
ordering; Spice-authored contracts stay in-process, and installed behavior
stays the asking lane's direct smoke.

Owners: [`vendor/src/omp/scripted.rs`](../vendor/src/omp/scripted.rs),
[`vendor/src/omp/scripted.mjs`](../vendor/src/omp/scripted.mjs), and
[`xtask/src/app_demo/scripted_omp.rs`](../xtask/src/app_demo/scripted_omp.rs).

## Authority and process life

One resident holds the kernel lock and is the sole writer to one SQLite journal.
A command transaction commits its facts, heads, result, and wake cursor
together. Transcript tape preserves vendor bytes without becoming semantic
authority.

Tape keeps the member accepting appends as plaintext. `Tape::seal` is the one
member-level archival primitive and carries no policy: it streams that member
into a standard seekable Zstandard archive (level 3, frames of at most
256 KiB, each with its content checksum), reopens the staged archive and
compares every decoded byte with the plaintext, renames it into place,
publishes the index naming the member, and only then removes the plaintext,
all under the tape's writer lock. Payload memory stays frame-bounded; seek
metadata grows with the frame count. Failure at any step leaves the plaintext
readable; the next seal removes staged residue, replaces an archive the index
never named, and removes indexed plaintext only once the current archive
reproduces it exactly. An unreadable archive is repaired from that plaintext
or cleanup refuses without removing it; no other storage format authorizes
deletion. A locus keeps its member, offset, and length across sealing.
Readers resolve plaintext, then the current archive, treat only a missing
file as absent, and charge bounded transcript reads for every frame decoded.
Readers and archival share one storage implementation:
[`tape/src/seal.rs`](../tape/src/seal.rs) and
[`tape/src/read.rs`](../tape/src/read.rs).

Lane supervisors opportunistically seal only their own lane/vendor tape at
startup or after clean local capture completion. Retained plaintext, including
interrupted seal cleanup, must total at least **16 MiB**: a minimum for
maintenance, never a trigger to rotate or interrupt active capture. Startup
requires a terminal predecessor with durable capture-drain proof and waits
for its supervisor's kernel exit before opening the next capture; that
handoff and maintenance run outside the successor's launch horizons.
Shutdown requires reaping, fully drained selected output and capture
delivery, and no remaining driver work, even if the agent withdrew the tool
call. Both boundaries require current lane/vendor/worktree/epoch ownership,
no authoritative live claim, and no unresolved or unheard lane jobs; unknown
eligibility skips maintenance, and the tape writer lock refuses contention.
Shutdown publishes the real completion and ending notice before compression,
so storage failure is diagnostic and cannot change the agent result or delay
its ending notification. A retained idle agent, renewal alone, and resident
restart or shutdown with live supervisors do not trigger archival.

`spice tape archive` separately inventories one bounded, sorted snapshot of
historical lane/vendor roots without changing them; `--apply` handles roots
sequentially. Before each seal it takes the lane's maintenance exclusion,
which a new supervisor launch also takes, then re-reads residency, claims,
and retained jobs. A still-admitted vacant lane must also have every retained
supervisor's kernel exit and capture-drain proof and meet the 16 MiB floor,
because it may capture again. Current journal removal is terminal: any
nonempty plaintext of a historical lane is eligible regardless of the floor,
while an empty root with no sealed member reports `empty`. Retained supervisor
documents remain evidence; their schemas never select an execution path. Active,
claimed, job-held, contended,
malformed, unknown, admitted below-floor, empty, and already-archived roots
stay unchanged with a concrete status. Eligible roots call only `Tape::seal`;
progress is flushed after each root, an interrupt finishes the current seal
then stops, and a rerun derives its state from the durable index and files
without recompressing immutable members. Owners:
[`supervisor/archival.rs`](../spice/src/resident/supervisor/archival.rs) and
[`tape_command.rs`](../spice/src/tape_command.rs).

`spice tape check KEY` compares one retained peer message with its author's
source-qualified tape event; `spice tape check --all` counts every outcome in
one authenticated journal snapshot. Both forms read only. They slice the
recorded decoded-text byte range and compare its bytes and digest with the
journal body, without rerunning packet grammar or repairing either copy.
Messages without a recorded range are counted separately. Missing, replaced,
truncated, undecodable and unreadable sources are unavailable, not evidence
that the journal disagrees with itself; `spice journal verify` remains
independent of tape retention. Captured outbox and invitation bodies use their
exact host-write content, not a rendered card.

Occupants cooperate under one operating-system user. Capabilities bind
attribution, lane, occupant, and epoch; they are workflow interlocks, not
isolation between same-user processes. Session authority comes only from an
admitted live vendor identity; transcript prose may select evidence but never
impersonates the caller. The resident runs in the foreground of its owning
process: `spice up` may detach that process, `spice down` owns semantic
shutdown, and an operating-system service manager may own restart policy.
`spice up --open` admits and opens the published endpoint even when another
resident already holds startup authority and the command returns
`resident::already-up`; a plain contending `spice up` has no browser effect.

A task invitation is one typed peer steering head (`task-invite:` control);
there is no separate stored invitation state. Named-task reads and task detail
derive its row from that head and the task's current heads by the currency rule
acceptance applies, so `task show`, the app, agent delivery and peer replies use
`invited`, `joined`, `declined`, `expired` with no bracket-prefix parser.
Invitation facts wrap instead of truncating their state or available replies.
Owner: [`read_guard/invitations.rs`](../spice/src/board_command/read_guard/invitations.rs).

A `steering_heads` row holds its directive's standing without the message text
and attachments; the creating fact at its `created_seq` holds them once, and
every journal head read joins that fact to return a complete head, so a stage
or disposition change rewrites no message content. Reads and verification
reconstruct the complete head from that creating fact. Owner:
[`steering_content.rs`](../journal/src/guard/steering_content.rs).

A session's acceptance names the release the resident runs. A client that
receives an answer its command registry cannot bind refuses it unread with
`command::answer-binding`; when the two releases differ, the refusal names both
and the side that must move: an older resident restarts on the client's
release, and an older client gives way to the resident's installed `spice`.

Owners: [`journal/src/guard.rs`](../journal/src/guard.rs),
[`spice/src/session_command/authority.rs`](../spice/src/session_command/authority.rs),
and [`spice/src/resident/lifecycle.rs`](../spice/src/resident/lifecycle.rs).

## Command vocabulary

Fixed compiled tables own the launch vendors, appliance routes, and gates,
while a closed command-contract registry owns the built-in verbs. Repository command
mounts join the CLI only after collision and configuration admission; a mount
that collides with a built-in path refuses before dispatch. Those tables, the
contract registry, and the admitted mounts are the entire command vocabulary.

Owners: [`spice/src/extension.rs`](../spice/src/extension.rs),
[`spice/src/cli/contracts/registry.rs`](../spice/src/cli/contracts/registry.rs),
and [`spice/src/cli/mounts.rs`](../spice/src/cli/mounts.rs).

## Windows development C toolchain

The tracked Rust toolchain includes `x86_64-pc-windows-msvc`, but Windows
development checks also require native C and assembly producers. The
repository declares one Apple-silicon host toolchain in
[`dist/windows/msvc.toml`](../dist/windows/msvc.toml): exact xwin and LLVM
releases and executable hashes; one closure digest over every Homebrew dylib
those LLVM tools load and every clang builtin header; the Visual Studio
channel and package manifest hashes; and exact CRT and SDK selectors.

Microsoft licence acceptance belongs to each worktree's operator authority, not
the tracked toolchain declaration. After recording an operator-authored
directive that accepts the Microsoft Software License Terms for both the CRT
and Windows SDK, write its steering key to the ignored local declaration:

```toml
# dist/windows/msvc.local.toml
operator_acceptance = "KEY"
```

After installing the declared Homebrew bottles (`xwin`, `llvm@20`, and
`lld@20`), materialize the CRT and SDK with:

```sh
spice dev xtask -- windows-msvc
```

The command resolves the local key against the repository journal and requires
its original record to be a principal holding the operator role whose text
accepts both named components; an absent, malformed, non-operator, or unrelated
record refuses. It then verifies xwin before downloading Microsoft payloads,
verifies both manifests before splatting, and publishes a read-only,
content-addressed generation under the bootstrap Cargo home.

A Cargo invocation selecting `x86_64-pc-windows-msvc` rehashes that
generation, each pinned launcher, every non-platform dylib in its transitive
load closure, and clang's complete builtin include tree before launch. macOS
platform libraries stay covered by the signed system volume and dyld shared
cache rather than mutable Homebrew paths. Only that target gets the `cl.exe`,
`ml64.exe`, `lib.exe`, and `link.exe` wrappers: they resolve to the admitted
clang-cl, llvm-ml, llvm-lib, and lld-link bytes, and both clang-cl and
lld-link receive the admitted xwin `/winsysroot`, so final x64 executables
link against the admitted CRT and SDK import libraries. Missing or changed
bytes refuse rather than use Apple's compiler, ambient headers, or another
toolchain.

Those x64 executables run on the development host only inside the persistent
Windows 11 ARM developer guest, as non-authoritative developer evidence; see
[Persistent Windows 11 ARM developer guest](planes/distribution.md#persistent-windows-11-arm-developer-guest).

Owners: [`spice/src/cargo_isolation/windows_msvc.rs`](../spice/src/cargo_isolation/windows_msvc.rs)
for admission, materialization, verification, and Cargo binding, and
[`xtask/src/windows_msvc.rs`](../xtask/src/windows_msvc.rs) for the command.

## App and renderers

Rust/WASM owns the app; JavaScript assets only render, and `wire` owns the DTOs.
Each app generation is immutable and binds its renderer members by exact digest.
Diagram and math renderers run in a sandboxed frame pinned to the loaded page's
generation and theme; the parent admits only the decoded image, never renderer
markup in its own DOM. Renderer modules are checked-in bytes, so rendering makes
no runtime network or package request.

The renderer provenance index maps every upstream package's notices to
source-owned assets. Byte-identical notices share one existing asset; distinct
copyright and legal text remain verbatim.

Owners: [`wire/src/manifest/renderer.rs`](../wire/src/manifest/renderer.rs),
[`app/src/browser/mermaid.rs`](../app/src/browser/mermaid.rs), and
[`app/renderer/renderer-000.txt`](../app/renderer/renderer-000.txt). Which
activity becomes a Mosaic card lives in [Mosaic card lineage](mosaic.md).

Each staged generation G also has one app ZIP, `generations/G.zip`, beside its
unzipped directory, the development hot-swap source. Its members are the
generation's files under `app/`. G is the identity the shell names in every
qualified route, not the archive digest. Every appliance executable links one
[rack](rack.md#format-and-linking) carrying those members; that document owns
its format, admission, and linking.

App compilation remaps the workspace, Cargo and Rustup homes, and every
configured Cargo directory source beneath stable `/spice/` paths before Rust
emits Wasm. The generation cache binds both the selected app sources and the
xtask code that compiles and finalizes them, so a build-recipe change cannot
reuse an older generation carrying host paths or obsolete normalization.

The Windows development build enables the same `spice` binary and modules as
macOS and Linux; only filesystem, process, and local-IPC adapters vary by
target.

Materialize the pinned xwin/LLVM toolchain, cross-check the ordinary
executable, create the release archive, then build the appliance:

```sh
spice dev xtask -- windows-msvc
spice dev cargo -- build --locked --release --target x86_64-pc-windows-msvc -p spice --bin spice
spice dev xtask -- source-archive
spice dev xtask -- windows-appliance --unsigned
```

From a clean checkout, `windows-appliance` uses native `stage-target`'s source
path: read `dist/stage/source.tar`, require its record to name HEAD, extract it
beneath the build root, and build the Git-free tree. It reports the archive
digest and record version, and writes
`target/release-appliance/x86_64-pc-windows-msvc/release/spice.exe`. A dirty
checkout instead builds its worktree's `-dev` product beneath `target/`,
without reading a release archive.

Every Gate selection reached by Rust source, a Cargo manifest, `Cargo.lock`,
or `rust-toolchain.toml` plans these cacheable cross-target checks:

```sh
cargo check --locked --package spice --bin spice --target x86_64-pc-windows-msvc
cargo check --locked --package spice --tests --target x86_64-pc-windows-msvc
cargo check --locked --package sill --tests --target x86_64-pc-windows-msvc
```

Gate sets `CARGO_ENCODED_RUSTFLAGS` to the inherited value when present, else
to `RUSTFLAGS`, and appends `-D warnings`. Cargo prefers that variable over
`RUSTFLAGS` and configured rustflags, so inherited flags extend but cannot
displace the deny. With `--target`, the flags bind only the Windows units.
Checks use Gate's admitted Cargo home and pinned toolchain without linking or
guest execution; proof inputs are `dist/windows/msvc.toml`, the package closure,
and the changed paths that selected each package.

Development and release builds place the staged canvas object at one
profile-stable path and pass it only to the final `cargo rustc` unit. A native
release stage also requires the adjacent `spice.pdb`. Stable
`--print link-args` records the real linker command beside that object. Cargo
rebuilds it after Rust, dependency, profile, or target-setting changes; a
rack-only `build.json` change replays the command and copies its executable
and PDB to Cargo's public paths without running rustc. Final-image admission
locates the canvas through Mach-O, ELF, or PE metadata and refuses absent,
repeated, truncated, or wrong bytes. Windows release policy also refuses an
embedded Authenticode certificate; its Sigstore proof is detached after all
executable bytes are fixed. No persisted build-time offset participates.

[`spice/src/version.rs`](../spice/src/version.rs) admits the linked archive
once. An installed resident without one refuses with
`serve::canvas-archive-absent`, or `serve::canvas-archive-invalid` when it does
not admit. The executable is the only app carrier and binds the generation it
names.

The release stage is:

```sh
spice dev xtask -- stage-target x86_64-pc-windows-msvc --unsigned
```

It packages `bin/spice.exe`, `symbols/spice.pdb`, the target payload, and the
detached proof receipt. Windows proof requires the configured repository to be
public: against a private repository it refuses at the typed public
hosted-attestation prerequisite before dispatch. The Windows 11 ARM VM, Wine,
and the macOS cross-build are development evidence only; no Enterprise plan or
self-hosted signing key substitutes.

Owners: [`rack/src/linked.rs`](../rack/src/linked.rs) for the symbols,
[`rack/src/link.rs`](../rack/src/link.rs) and
[`rack/src/link/coff.rs`](../rack/src/link/coff.rs) for object writing
and final-image section discovery, [`dist/src/canvas.rs`](../dist/src/canvas.rs)
for final-image admission and the Authenticode policy,
[`xtask/src/app.rs`](../xtask/src/app.rs) for the link arguments, and
[`spice/src/install/canvas.rs`](../spice/src/install/canvas.rs) for install
admission.

The resident serves app routes through one boundary, `CanvasAssets`, with
exactly two sources. Only a staged `spice up` serves `DevelopmentDirectory`:
the staged generation directories, hot-swapped as generations publish, behind
a two-entry generation cache. Every installed resident serves
`EmbeddedArchive`: the archive linked into its executable, admitted once at
startup into an index from logical path to the stored gzip slice, the table's
content type, and a strong ETag naming generation and member; it reads no app
file from disk and inflates only HTML members, once, for their content
security policies. Every embedded body is the stored slice, sent with
`Content-Encoding: gzip`, its exact `Content-Length`, and
`Vary: Accept-Encoding`; a request whose `Accept-Encoding` refuses gzip gets
406. An installed resident holds one generation for its lifetime; a route
qualified with another generation answers 410 Gone, with
`x-spice-app-generation` naming the served one. `/build.json` answers the
linked rack's build record the same way, its ETag naming the record's own
bytes, so a rebuilt record under an unchanged app never revalidates as
unmodified. Owners:
[`serve/src/canvas/assets.rs`](../serve/src/canvas/assets.rs) and
[`serve/src/axum.rs`](../serve/src/axum.rs).

A resident restart keeps the app's last good surface on screen until the new
session's scope is fully hydrated. Every app route answers `no-cache` with a
content ETag, so an ordinary reload loads the served generation. A page whose
generation the resident no longer serves reloads once into the served
generation, whether or not the app protocol changed: when its next
connection's bootstrap names another generation, before any session opens, or
when a qualified fetch answers 410 for the shell's module or wasm (caught by
the shell's inline loader guard) or for a renderer document or member (probed
when the renderer frame opens and after any renderer failure). A staged
`spice up` hot swap reaches an open page only at its next connection. Unsent
composer and observation drafts travel to the reloaded page through
tab-scoped storage, with attachment annotations flattened into their image.
Only a submission whose result is still unknown keeps the stale page in place
behind the update offer and its reload control; an incompatible app protocol
then still requires that explicit update. Owners:
[`app/index.html`](../app/index.html) (inline full-viewport boot),
[`app/src/runtime/publication.rs`](../app/src/runtime/publication.rs),
[`app/src/browser/connection.rs`](../app/src/browser/connection.rs),
[`app/src/browser/mermaid/service.rs`](../app/src/browser/mermaid/service.rs),
and [`app/src/browser/draft_handoff.rs`](../app/src/browser/draft_handoff.rs).

One inventory of unsent work arms the page's leave guard: every handed-off
composer and observation draft (live text included, mounted or not), the
submissions the resident has not yet accepted, and annotation marks not yet
applied to an image; an empty inventory leaves it disarmed. The browser's
leave dialog carries no page text, so a leave the operator cancels lists each
item under the command bar, naming its kind and owner: the lane and its team,
a Design's handle, a task's handle, or an image. **Show** reveals and focuses
a draft's composer; a draft whose lane no console shows says so, and
**Discard** removes any draft after a confirm naming its owner. Owners:
[`app/src/unsent_work.rs`](../app/src/unsent_work.rs) and
[`app/src/browser/unsent.rs`](../app/src/browser/unsent.rs).

## Version identity

`spice --version`, `spice version`, the app menu, and the Spice header report
one product version. A build derives one canonical `BuildInfo` record by the
first applicable source:

1. An ignored `dist/build.json` supplied by an export is authoritative. A full
   semantic version in `dist/version/base` must equal the record's product. A
   major-only base must equal its major, and the sum of
   `dist/version/counters` must equal its minor. This is the only build route
   that needs no Git repository.
2. Otherwise Git supplies HEAD's commit, committer second, and version. A full
   semantic base pins the product verbatim. A major-only base gives the major;
   the minor is HEAD's product-counter sum, and the patch is the seconds from
   the newest first-parent commit that raised that sum. The walk streams
   first-parent commits that changed `dist/version/counters` newest-first and
   stops at that raise. An equal
   registry sum carries the earlier floor, including a retired counter
   absorbed by `gap`; a decrease refuses. A staged shallow history that ends
   before the raise also refuses.
3. Without an export record or Git, staged builds refuse. Development builds
   use the current Unix second as the patch and append `-dev`.

`dist/version/base` therefore contains either one canonical major or a complete
`MAJOR.MINOR.PATCH`; `dist/version/counters` contains one sorted
`name=decimal` record per additive contributor plus `gap`. The product minor
parses and sums that registry, while each owner selects its record as a
compile-time constant without a dependency cycle. Adding a contributor
therefore changes one line and no Rust registry. Git ratchets the total and
preserves it when a retired contributor moves into `gap`. A dirty Git build
appends `-dev`. No landing rewrites the version inputs: the landed tree is
exactly the proved tree.

Every route serializes the same compact canonical `BuildInfo` into the rack's
root `build.json`, naming the product and, when available, its source commit
and committer second. The executable reads its version from that member; a
linked rack whose record is absent or invalid refuses admission. A seat reads
it as `spice://build.json` and a browser as `/build.json`. The version never
carries the executable digest; that digest is the native-binary identity.
`spice --version` ends with `app G` when the executable links generation G's
archive, and `spice version` reports that same G without reading the worktree.

`spice dev xtask -- source-archive [OUT]` writes a Git archive rooted at
`spice/` and injects HEAD's canonical record as `spice/dist/build.json`; it
requires Git 2.40 or newer and a checkout exactly at HEAD. `OUT` defaults to
`dist/stage/source.tar`, the one archive every release target stages from:
a local `stage-target` extracts it into a fresh scratch root with no `.git`
and builds there, so its rack identity is the archive's record, byte for byte.
The archive's identity is the digest of its bytes under the source-archive
domain; every target payload and the release manifest record it. The hosted
Windows route commits that same tree for staging; an ad hoc hosted command
still snapshots the worktree, injecting HEAD's record into its temporary index
before creating its one-use commit, so a fetch-depth-one guest uses the
exported identity rather than deriving one from the snapshot commit. A snapshot
whose tree adds uncommitted changes to HEAD injects HEAD's commit and committer
second with a `-dev` product, as any dirty Git build does.

Owners: [`dist/src/version.rs`](../dist/src/version.rs),
[`dist/src/source.rs`](../dist/src/source.rs),
[`spice/src/version.rs`](../spice/src/version.rs),
[`xtask/src/source_archive.rs`](../xtask/src/source_archive.rs), and
[`windows/src/hosted/client.rs`](../windows/src/hosted/client.rs).
