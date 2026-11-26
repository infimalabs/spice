# Installing Spice

Installation is a digest-bound authority change. Preview writes nothing and
prints its canonical digest, change counts, affected paths, and exact apply
command; `--json` exposes the complete ordered plan. Apply publishes exactly
that plan and reports durable receipt status and progress. Neither stops,
drains, imports, or deletes foreign processes or state. The receipt and
reversal contract lives
in [Distribution](planes/distribution.md#installing-into-a-repository-the-product-does-not-own).

## Install from source

An installable executable links its app archive. The xtask install builds the
app, links it into a fresh executable, then derives the plan once and applies
exactly that plan, printing its digest. A plain `cargo build -p spice` links
no app, and its preview refuses with `install::canvas-refused`. Run from the
root of the worktree being installed:

```sh
cargo run --locked -p xtask -- install --no-resident || exit
```

The plan publishes the executable, ambient `spice` wrapper, and the three Git
hook launchers with their selecting config under the Git common directory's
`.spice-install`. It leaves the repository's and the user's Git configuration
untouched: Spice-launched processes select the hooks
([hooks.md](planes/hooks.md#git-hook-activation)). Verify the result:

```sh
SPICE_COMMON=$(/usr/bin/git rev-parse --path-format=absolute --git-common-dir) || exit
SPICE_BIN="$SPICE_COMMON/.spice-install/bin/spice"
test -x "$SPICE_BIN" || exit
test -f "$SPICE_COMMON/.spice-install/githooks/config" || exit
```

Give the operator shell the same executable identity:

```sh
export PATH="${SPICE_BIN%/spice}:$PATH"
hash -r
test "$(command -v spice)" = "$SPICE_BIN" || exit
spice --json doctor || exit
test "$(/usr/bin/git config --file "$SPICE_COMMON/.spice-install/githooks/config" --get core.hooksPath)" = "$SPICE_COMMON/.spice-install/githooks" || exit
```

### Windows PowerShell

Use the same xtask install from PowerShell. The verification below names Git
for Windows at its default Program Files location; point `$Git` at another
`git.exe` when yours lives elsewhere:

```powershell
$ErrorActionPreference = 'Stop'
$Git = Join-Path $env:ProgramFiles 'Git\cmd\git.exe'

cargo run --locked -p xtask -- install --no-resident
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$SpiceCommon = & $Git rev-parse --path-format=absolute --git-common-dir
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$SpiceCommon = $SpiceCommon.Trim()
$SpiceBin = Join-Path $SpiceCommon '.spice-install\bin\spice.exe'
if (-not (Test-Path -LiteralPath $SpiceBin -PathType Leaf)) {
    throw "installed Spice executable is missing: $SpiceBin"
}
$HookConfig = Join-Path $SpiceCommon '.spice-install\githooks\config'
if (-not (Test-Path -LiteralPath $HookConfig -PathType Leaf)) {
    throw "installed hook config is missing: $HookConfig"
}

$env:PATH = "$(Split-Path -Parent $SpiceBin);$env:PATH"
if ((Get-Command spice.exe).Source -ne $SpiceBin) {
    throw 'operator shell did not select the installed appliance'
}
spice.exe --json doctor
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$ExpectedHooks = $SpiceCommon.TrimEnd('/', '\') + '/.spice-install/githooks'
$ObservedHooks = spice.exe run -- $Git config --get core.hooksPath
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($ObservedHooks.Trim() -ne $ExpectedHooks) {
    throw "unexpected core.hooksPath: $ObservedHooks"
}
```

## Install into another repository

First build an installable executable with the source flow above or obtain one
from a release. The executable may live outside the repository that will use
Spice. From the root of that target repository, invoke the same absolute
executable for both the read-only preview and the digest-bound apply:

```sh
cd /absolute/path/to/target-repository
SOURCE_SPICE=/absolute/path/to/spice
"$SOURCE_SPICE" install
"$SOURCE_SPICE" install --apply=DIGEST_FROM_PREVIEW
```

Replace `DIGEST_FROM_PREVIEW` with the digest printed by the first command.
The apply copies that appliance, which carries its app, into the target
repository's Git common directory with its hook launchers and shell assets,
and leaves the source installation unchanged.

Select and verify the target repository's installed appliance:

```sh
SPICE_COMMON=$(/usr/bin/git rev-parse --path-format=absolute --git-common-dir) || exit
SPICE_BIN="$SPICE_COMMON/.spice-install/bin/spice"
test -x "$SPICE_BIN" || exit
export PATH="${SPICE_BIN%/spice}:$PATH"
hash -r
test "$(command -v spice)" = "$SPICE_BIN" || exit
spice --json doctor || exit
```

On Windows, set `$SourceSpice = 'C:\absolute\path\to\spice.exe'`, then run
`& $SourceSpice install` followed by
`& $SourceSpice install --apply=DIGEST_FROM_PREVIEW`. Use the PowerShell
verification block above to select the target repository's
`.spice-install\bin\spice.exe`.

## Sign macOS executables

Shared macOS executables must be signed with a Developer ID and notarized. The
operator needs Apple Developer Program membership, a
`Developer ID Application` certificate and its private key in the login
keychain, and a `notarytool` keychain profile created once:

```sh
xcrun notarytool store-credentials PROFILE
security find-identity -v -p codesigning
```

Darwin release staging (`spice dev xtask -- stage-target aarch64-apple-darwin`
and `spice dev xtask -- seal-darwin`) signs and notarizes the executable before
sealing it. Name the identity and profile once in Git-common configuration;
each `spice config` call previews its write and its `--apply=DIGEST` applies it:

```sh
spice config release.darwin.identity "Developer ID Application: NAME (TEAMID)"
spice config release.darwin.notary_profile PROFILE
```

Staging refuses before the release build while either name is missing or
unusable; `--unsigned` stages an executable Gatekeeper blocks on every other
Mac.

To sign an executable built outside the sealed release, sign, submit, wait for
Apple's verdict, and verify it with one repository-owned command:

```sh
spice dev xtask -- share-darwin /absolute/path/to/spice \
  --identity "Developer ID Application: NAME (TEAMID)" \
  --notary-profile PROFILE
```

Both paths change the executable in place. They refuse before signing when the
identity or profile is unavailable, retrieve Apple's log when notarization is
rejected, and finish only after strict `codesign` verification and
Gatekeeper's primary-signature assessment (`spctl --assess --type open
--context context:primary-signature`) accept the file as notarized. A bare
Mach-O cannot carry a stapled ticket, so its first Gatekeeper assessment needs
network access. The hosted finalizer retries that assessment while Apple's
accepted ticket propagates, then refuses if Gatekeeper still cannot observe it.
`share-darwin` does not mutate or replace any sealed release artifact.

The hosted publisher admits every staged payload against two distinct preflight
identities: the selected Git commit and the exact source archive. Either
identity moving refuses publication before a tag or GitHub Release is written.

## Stage the Linux release on macOS

A Mac cannot link or run the Linux release, so
`spice dev xtask -- stage-target x86_64-unknown-linux-musl --unsigned` runs
that staging inside the pinned `linux/amd64` proof image and writes the
payload to this worktree's `dist/stage`. The host needs Docker or Podman
named once in Git-common configuration and the pinned image loaded:

```sh
spice config proof.container /usr/local/bin/docker
spice dev xtask -- proof-image load target/proof-image/spice-proof-image.tar
```

Without an archive, `spice dev xtask -- proof-image build` builds the image
and writes that archive. Staging refuses before any build while either is
missing. It runs emulated on Apple silicon and builds from a cold Cargo
target each time.

## Hook activation

Install leaves Git configuration untouched and inspects no `core.hooksPath`
value. Spice-launched processes select the installed launchers for this
repository only ([hooks.md](planes/hooks.md#git-hook-activation)). A terminal
or GUI Git started outside a Spice-launched process is not gated by Spice.

The installed launchers `exec` the appliance by its absolute Git-common path
([shell.md](shell.md#generated-git-hook-launchers)) and never search `PATH`.

## Reference storage

Spice gates commits by reading Git references through gix, which has only the
files backend. A repository whose Git-common `config` sets
`extensions.refStorage` to `reftable`, as `git init` does under
`feature.experimental` or Git 3.0, would install cleanly and then refuse every
gated commit, so preview refuses first with `install::ref-storage-unsupported`,
naming the backend. Migrate before previewing again, or create the repository
with `git init --ref-format=files`:

```sh
/usr/bin/git -C /absolute/path/to/worktree refs migrate --ref-format=files
```

A repository migrated to reftable after install is refused by the gate itself
with the same remedy.

## Upgrade

An upgrade candidate verifies its own bytes against sealed release evidence
before applying, without Cargo, `xtask`, or a source checkout. It admits the
package index, then its platform's payload and proof receipt, and refuses a
candidate whose identity or product version does not match that evidence.

New install receipts retain that authenticated product version for later
upgrade policy. Receipts from every supported older schema cross forward with
the version explicitly unknown; Spice never infers authority that history did
not record.
Before package preview or apply delegates to the candidate, a known installed
product version refuses a strictly older candidate. The same version may
converge and a newer candidate may upgrade; an unknown migrated version does
not invent a downgrade boundary.

Each hosted release publishes three standalone platform executables, one
deterministic `spice-release-evidence.zip`, and `SHA256SUMS`. Download only the
current platform's executable plus the small shared evidence ZIP, verify both
against `SHA256SUMS`, then extract the evidence before previewing:

```sh
SPICE_CANDIDATE=/absolute/path/to/spice-aarch64-apple-darwin # select this platform
chmod +x "$SPICE_CANDIDATE"
mkdir spice-release-evidence
unzip spice-release-evidence.zip -d spice-release-evidence

"$SPICE_CANDIDATE" install --package /absolute/path/to/spice-release-evidence || exit
"$SPICE_CANDIDATE" install --package /absolute/path/to/spice-release-evidence --apply=DIGEST_PRINTED_BY_PREVIEW || exit
```

`--package` names evidence, not an executable container. The invoking
candidate supplies the executable bytes, so the release does not duplicate
large binaries or publish debug symbols.

### Upgrade from v0.2

Spice v0.3 reads the retained v0.2 install receipt through its forward schema
crossing. Invoke the v0.3 executable directly: either preview and apply it over
the existing install, or unapply the v0.2 install without an intermediate
upgrade. Do not edit the receipt.

```sh
NEW_SPICE=/absolute/path/to/spice-v0.3
"$NEW_SPICE" install
"$NEW_SPICE" install --apply=DIGEST_PRINTED_BY_PREVIEW

# Alternatively, remove the v0.2 install directly:
"$NEW_SPICE" install --unapply
```

Both directions preserve unrelated retained state, including journals and lane
data. Unsupported or incomplete receipts refuse before mutation; Spice never
downgrades an install receipt.

From source, refresh in place through the installed appliance; do not unapply
first. The shortcut builds as above, stops the published resident, applies,
and starts the new executable; `--no-resident` omits that lifecycle and
arguments after `--` go to `spice up`:

```sh
"$SPICE_BIN" dev xtask -- install --port PORT
```

On Linux and macOS, package apply leaves a running resident on the executable
it started with; restart through the installed appliance with
`"$SPICE_BIN" down` and `"$SPICE_BIN" up`. Where `spice up --keeper` configured
a lifecycle owner, an apply that publishes anything drains that owner's
resident first and starts it again under the new executable.

The shortcut refuses with `resident::operator-install-only` when run from a
lane the repository seats, because that lane runs the appliance it would
replace. A lane may refresh a scratch repository that has never seated it.

### Windows PowerShell

The Windows candidate uses the same evidence and digest contract:

```powershell
$Candidate = 'C:\absolute\path\to\spice-x86_64-pc-windows-msvc.exe'
$Evidence = 'C:\absolute\path\to\spice-release-evidence'
Expand-Archive spice-release-evidence.zip $Evidence

& $Candidate install --package $Evidence
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& $Candidate install --package $Evidence --apply=DIGEST_PRINTED_BY_PREVIEW
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
```

For a source refresh, run the xtask install through the installed appliance.
It restarts the resident with arguments after `--` going to `spice up`;
`--no-resident` omits that lifecycle:

```powershell
& $SpiceBin dev xtask -- install --port PORT
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
```

On Windows, an apply that publishes anything registers and starts the
repository's per-user lifecycle owner under the newly installed executable, so
no separate restart is needed. The owner relaunches the resident whenever it
exits, until `spice down` stops it. `spice up --keeper` replaces that default
with an explicit listen and access policy.

After a source refresh run with `--no-resident`, restart explicitly:

```powershell
& $SpiceBin down
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& $SpiceBin up
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
```

## macOS symbols

On macOS, the xtask install keeps the executable's dSYM, and the receipt
records the executable digest, the shared Mach-O UUID, and the source commit.
A release package apply installs the executable alone, since release evidence
carries no symbols. Verify an installed pair, or symbolize a `sample(1)`
report taken from the installed image; both forms are read-only:

```sh
"$SPICE_BIN" --json install symbols
sample "$(pgrep -n -f "$SPICE_BIN")" 1 -file "$TMPDIR/spice-sample.txt"
"$SPICE_BIN" --json install symbols --sample "$TMPDIR/spice-sample.txt"
```

## Reversal

POSIX shell:

```sh
"$SPICE_BIN" install --unapply
```

Windows PowerShell:

```powershell
& $SpiceBin install --unapply
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
```

Reversal removes what the receipt published and restores what it replaced. It
refuses with `install::configured-keeper-preserved` while a lifecycle owner
configured by `spice up --keeper` stands; remove that owner with
`spice down --keeper` first. On Windows, reversal unregisters the default
lifecycle owner and drains its resident before it removes the appliance, then
removes the owner's private artifacts.

Reversal retires the install receipt, its reversal payloads, and every
generated `agent.gitconfig` in the repository's Git worktree administration.
It removes now-empty install and state directories. Any state root carrying
configuration, journal, task, artifact, lane, or other runtime state remains
unchanged.

## State root

The state root is `.spice` under the Git-common directory and under each
worktree's Git directory. No symbolic link stands in for a component of the
root: admission refuses it.

Each top-level member is grouped by how long its contents live, and every
directory segment is one word:

| Member | Lifetime and contents |
| --- | --- |
| `config.toml` | Common-root local configuration. Each worktree root keeps its `worktree.toml` beside it. |
| `authority/` | Durable state a backup copies: `journal/journal.sqlite3` with its WAL and shared-memory sidecars, `gate/` with `state.json`, `sweep.json`, and `arms.json`, `artifacts/tasks/`, `releases/` for effect receipts, and, in every worktree root, `worktree/` with `delivery.json` for steering-delivery idempotency and `recovery/` for task recovery generations. |
| `lanes/LANE/` | Everything one lane owns: `tape/`, `rtk/`, `occupants/OCCUPANT/` for agent supervisor records, `jobs/JOB/` for shell-command jobs, and `media/` for its steering attachments. `spice lane purge` removes exactly this directory and the lane's `runtime/lanes/LANE` discovery entry. |
| `runtime/` | Valid only while a resident generation runs: `resident`, `serve`, `keeper.json`, `degraded`, `capture.sock`, `services/`, `lanes/LANE` discovery entries, `locks/` for every lock file, `logs/` for `resident.log` and `keeper.log` with their rotations, and, per worktree, `meter/` for the session meter. |
| `cache/` | Reconstructible and pruned by its owner: `gate/` for the gate evidence store, `gate/source/` per worktree for the Rust analysis source cache, `proof/`, and `rack/`. |
| `output/` | Bounded human-readable captures under their retention rules: `cargo/`, `gate/`, and `releases/` for complete release preview plans. |
| `appliance/` | What the appliance installs for itself: `rollback/`, `extensions/`, and `install/` with `receipt.json` and `reversals/`. |
| `machine/` | Owner-only host-bound state: `windows/` holds the Windows guest, including its private key. |

The primary worktree's Git directory is the Git-common directory, so its
`authority/worktree/` shares the common root. Deleting `runtime/`, `cache/`, or
`output/` while no resident runs loses no journal, task, tape, artifact,
configuration, task recovery, or steering-delivery state; the next start
recreates what it needs. Names outside this layout are ignored.

## Recovery

**Executable-policy refusal.** The resident binds executable configuration to
one digest over the exact bytes of the tracked and local configuration
documents. When it refuses with `trust::digest-unknown`, run `spice trust
pend` from that worktree; the pend records both documents' text and replies
with their digest. `spice trust show DIGEST` shows those recorded documents as
a line diff against the most recently granted other digest, or their full text
when none is granted; a change to a line ending or the final newline is noted
on its line. Bare `spice trust` is the live task-backed request report: it lists
only pended digests that current pending tasks still require and includes the
same differential without reading a worktree. Once no pending task requires a
digest, its request leaves this live report automatically. This online frontier
has one row per distinct required digest, independent of the number of tasks
sharing it or the amount of retained trust history. `spice trust show DIGEST`
remains the explicit read for any retained digest, including a resolved or
no-longer-needed request, and the app's trust blocker shows the same diff.
Review the diff, then grant only that digest with `spice trust grant DIGEST`, or
reject it with `spice trust revoke DIGEST`. A ruling names the recorded bytes,
so it succeeds whatever any worktree holds by then. Any byte change is a new
identity; withdraw a grant with `spice trust revoke DIGEST`.

**Damaged appliance.** A published file that no longer holds its recorded
bytes refuses preview, apply, and reversal with `install::drift`, naming the
path and the digest observed. Nothing is overwritten; restore the recorded
bytes and retry. Installing a different build, from a rebuilt source tree or
a newer sealed package, replaces a changed executable without that refusal.

**Relocation.** The launchers and the hook config name absolute paths. After
a move or copy the config still names the original location, so a lane launch
refuses with `lane::git-hooks-unavailable`, naming the install command. At the
new location, install from source or apply a sealed package again: the receipt
rebases once the old location no longer exists, and the refresh republishes
the launchers and config. A copy whose original still exists fails with
`install::state-failed`.

**Break glass.** `git commit --no-verify` skips `pre-commit` and `commit-msg`;
`reference-transaction` still runs, so history protection holds while the
commit carries no gate evidence.
