# Product model — supporting planes — Metrics and hooks

[Product model — supporting planes](../planes.md) · [Spice product model](../README.md)

## Metrics

Transcript accounting is stated in the
[metrics register](../invariants/session.md#metrics).

## Hooks

Git gate hooks (`pre-commit`, `commit-msg`, `reference-transaction`) refuse or
allow a repository mutation. The resident lane terminal separately owns the
agent's shell commands; it is not a Git hook.

Each installed Git hook contains only its shell header and one `exec` of the
repository's installed Spice with the hook name and original arguments. The
launcher owns no environment policy. Before it captures proof authority or
launches a child, `gate pre-commit` removes `GIT_DIR`, `GIT_EXEC_PATH`, and
`GIT_PREFIX`, the leading `PATH` entries that repeat Git's exec path, and the
developer-root variables that still hold the defaults Git injected.

The resident lane terminal receives each complete shell-tool command string
[exactly once](../invariants/identity.md#steering-shell-instrumentation-is-one-shot-at-the-agent-s-own-command-boundary).
That is the last point at which the command still exists as one string rather
than a running process, so the
[command optimizer](../invariants/session.md#extension-the-command-optimizer-is-optional)
rewrites there.

### Git hook activation

Installed Git hooks are activated per process and only for the repository that
owns the appliance. Spice installs one receipt-owned Git config beside the
three hook launchers. It contains only the `core.hooksPath` naming their
directory. Resident lane launches carry two
command-scope `includeIf.gitdir` pairs: one for the canonical owning Git-common
directory and one for its linked-worktree administrative directories. Git
therefore selects the installed config in the main worktree and every linked
worktree, including on a detached HEAD, without writing project, global, or
system configuration.

Another repository receives no Spice hooks setting. Its native hooks and its
local or worktree `core.hooksPath` remain effective; a repository with no
setting remains unset. A selected launcher never turns a missing install
receipt into success: `gate pre-commit` refuses with
`gate::pre-commit-appliance-generation-moved`. The config names the launcher
directory by absolute path, and composition requires that path to be the
current location's launcher directory. After the repository moves or is
copied, the config names another location, so composition fails naming the
install command that refreshes it.

Composition runs no Git. It preserves unrelated inherited command-scope pairs
in place and sets the two conditional includes idempotently, replacing an
inherited pair under the same key. A config selecting another launcher
directory, a `GIT_CONFIG_COUNT` that is not a decimal count, or a counted
`GIT_CONFIG_KEY_n` / `GIT_CONFIG_VALUE_n` pair missing a half fails
composition naming the cause. A lane launch refuses with
`lane::git-hooks-unavailable`. Without the receipt-owned config or launcher
directory nothing composes and Git retains its native hook configuration. An
incomplete `pre-commit`, `commit-msg`, and `reference-transaction` set likewise
composes nothing and leaves the process on its native hook configuration.

Explicit command-line configuration outranks the conditional include, so
internal integration's `-c core.hooksPath=/dev/null` is the intentional bypass
for its reference updates. Gate children keep their stripped command scope.
Terminal and GUI Git started outside a Spice-launched process are not gated by
Spice.

### Supervised host environment

A lane inherits only admitted host variables: `HOME`, `TMPDIR`, `USER`, and
`SSH_AUTH_SOCK`, with `PATH` composed separately. On Windows, its session home
is explicit `HOME`, then `USERPROFILE`, then the complete
`HOMEDRIVE`/`HOMEPATH` pair. Its temporary root is explicit `TMPDIR`, then
`TEMP`, then `TMP`. The supervisor exports the resolved home as both `HOME`
and `USERPROFILE`, and the temporary root as `TMPDIR`, `TEMP`, and `TMP`, so
Spice's pinned session and OMP's native Windows discovery agree. Session
discovery outside the occupant uses the same resolver. A missing home refuses
with `lane::session-home-unavailable` and, on Windows, a missing temporary
root with `lane::session-temp-unavailable`; elsewhere an absent `TMPDIR`
resolves to `/tmp`.

Explicit `SHELL` selects the interpreter used for lane shell-tool commands;
without one the launch refuses with `lane::native-shell-unavailable`. On Windows an
absent or empty `SHELL` instead uses the gate's Git selection (`PATH` first,
then the warned Program Files default) and locates `bin/bash.exe` beside
that exact `cmd/git.exe`; both must be regular files. Discovery inspects files
and the search path but never executes Git or substitutes `ComSpec` or
PowerShell for a POSIX interpreter. Windows also admits `HOMEDRIVE`,
`HOMEPATH`, `APPDATA`, `LOCALAPPDATA`, `SystemRoot`, `PATHEXT`, and `ComSpec`
for the vendor and its tools; unrelated variables remain excluded.

Session publication and discovery share OMP's directory encoder. Temp scope
precedes home scope, so a temporary directory nested beneath the profile, as
on Windows, encodes as temp. Absolute directory names omit Rust's
verbatim-path namespace marker and retain OMP's ordinary drive or UNC
spelling.

Supervisor change notifications prompt a durable-state read; they are not
startup authority. At the start horizon the resident reads `state.json` again,
so an unannounced published failure or proven start wins over a timeout. A
timeout refuses with `lane::supervisor-start-timeout`, naming the supervisor
directory containing `state.json` and `supervisor.log`.

### Shell results

Advice is read-only. A leading `cd` to the literal absolute directory the
command already runs in, and a final `head` or `tail` pipeline stage, each earn
one hint appended to that command's initial result. Advice changes neither the
command nor its exit status, and asks for no acknowledgment.

Shell and job results show a bounded preview. When bytes are omitted the result
names the keeper-owned log: plaintext while the job runs, then a verified
seekable Zstandard `log.zst` once it ended; `read` it with a line range. The
log keeps at most 64 MiB across stdout and stderr, and consecutive exact
repeats of up to 32 lines keep one copy followed by a marker counting the
repeats. A result says how many bytes were collapsed or not retained, and a
pattern wait that found no match says when output past the cap went
unsearched. Spice sends SIGTERM to a job whose output reaches 64 MiB and
SIGKILL once it reaches 80 MiB or stays a second past SIGTERM; the result
records each signal's time and byte count and ends as stopped by Spice.

Fresh human steering published after an OMP step starts records the exact
directive keys that make that step stale. A shell or blocking `job wait`
emitted while any such key remains owed is registered and immediately released
through its ordinary job result. Tape-backed ACK/NAK recognition stops its
keys from preempting before journal catch-up. A successful authoritative inbox
read re-arms a current-step trigger when its stage advances and otherwise
removes canceled, expired, absent, or settled keys; a failed read changes
nothing. The first released boundary replays only the still-owed triggering keys.
Concurrent calls and repeated delivery from the same stale step do not repeat
that replay. A later OMP step clears the prior keys and admits one replay for
its own fresh steering. Peer asides, atomic job operations, and work from a
non-stale step keep their normal foreground behavior.

### Active-work narration

The resident keeps one narration streak per occupant. Assistant prose, a turn
start, and a settled turn reset it; tool results, an outstanding command, and
`job wait` do not. The first reminder is due at the configured tool-call bound
(`lane.narration`, 24 by default; zero disables narration) or three minutes,
and the second at the next call bound or five minutes total. Further reminders
are call-bound only. Each due reminder records `narration::prodded`, interrupts
the active host-tool boundary so healthy work continues as a kept job, and
enters steering immediately. It asks for one short status line and for the
current turn to continue after it, naming no channel. No acknowledgment key is
minted, and a moved or exited occupant never receives a successor's prod.

### Cadence is reachability

[Pending steering](../invariants/identity.md#steering-pending-steering-reaches-each-admitted-agent-boundary)
reaches non-shell work because an unheard key interrupts outstanding tool waits.

An OMP turn that settles with an exact active claim leaves its occupant idle;
without a claim it exits and vacates the lane. A backgrounded command survives
under its detached keeper; a command whose keeper cannot launch fails its call
and leaves no kept job. If its ending, or the match of a command started with
`until`, stays unheard through the delivery grace, the keeper wakes the
matching retained occupant when its claim remains admitted, or requests a lane
wake for a successor. `job wait` is for a finite job whose result blocks
current work. While it blocks, the lane's current activity remains that wait:
the exact job and its latest bounded output replace any earlier tool as the
operator-visible status. That output refreshes within a few seconds of the job
printing, at a bounded rate however fast it prints, and the wait is restated
whenever another tool call settles while it blocks. The job's final result
alone settles the tool call.

A [service](../invariants/lifecycle.md#lifecycle-services-belong-to-one-claim-win)
never ends on its own and never keeps a lane idle; `job wait` answers at once
with its output so far. A task-associated service follows its exact claim win;
an unanchored service stops with the occupant turn that launched it. Every seat
start names the lane's kept commands that still run, marking services, so a
forgotten task service surfaces to the next seat.

A job's ending is told once. An answer that reports how a job ended records it
as heard, so no completion notice follows and no successor repeats it. The
record is claimed atomically before a notice is written, so a racing answer and
notice deliver exactly one; a notice that fails to write gives the claim back.

### Acknowledgment corrections

[Assistant text on the lane tape](../invariants/identity.md#steering-acknowledgment-evidence-is-assistant-prose-on-the-lane-tape)
is the acknowledgment evidence: admitted outbox prose and visible prose.
Reasoning, tool-call intents, tool arguments and quoted examples do not
qualify. `ACK KEY [KEY ...]: response` can retire several keys;
`NAK KEY: reason` declines one. Recognition happens on landing, not at turn end.
A placed header naming a pending key in reasoning or a tool-call intent draws
one hidden-answer correction per key per pending inbox, naming
`spice://outbox`.

Every reminder of an already-delivered directive replaces its body with a
correction, even when the original is shorter. It names the pending key, says
whether replies followed, and supplies the same grammar as hidden-answer,
wrong-key and compaction corrections. First deliveries remain complete.

Once an ordinary, hidden-answer, wrong-key or compaction correction actually
lands, rather than merely entering the aside queue, the first later complete
visible assistant response that leaves its key pending queues an activity
correction. Newly due keys join any older keys still queued. Further ignored
responses repeat it every third complete response. This cadence has no clock:
silence, a blocked or tool-only call, and streamed fragments do not advance it,
and its queued aside does not interrupt a running tool. An ACK in visible
prose, or an outbox receipt reporting it acknowledged, removes its keys from
the queued correction before journal catch-up; partial ACKs leave the other
keys named. Compaction and occupant replacement start a new activity window.

A wrong-key correction names unrecognized tokens and lists pending keys, once
per compaction. Delivered keys stay recognized for that occupant's lifetime,
so repeating a previously acknowledged key causes no false correction.
