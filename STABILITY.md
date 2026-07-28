# Stability

spice is still moving, but not every surface moves at the same speed. Treat this
page as the build-on map: build on the stable rows, wrap or pin the moving rows,
and assume anything unlisted is internal unless another document says otherwise.

| Surface | Status | Build-on guidance |
| --- | --- | --- |
| Inbox file format and ACK protocol | Stable | Durable inbox items, UTC keys, priority/note fields, pending readout, and transcript `ACK <key>: ...` retirement are core protocol. Integrators can build tooling that writes operator steering and watches semantic ACKs. |
| Constitution constants and hook policy | Stable | Policy limits, flex/sticky behavior, repo-shape checks, env-literal inventory, magic-number ratchet, and commit-message rules are executable doctrine. Every disabled built-in is printed by the gate; a tracked disablement also requires current `spice init --apply` approval. Changes should be explicit contract changes, not silent drift. |
| Extension import surface | Stable | Entry points in `spice.drivers`, `spice.studies`, and `spice.wrappers` run in the installed spice process and may use spice modules needed to implement those extension contracts. Underscored names remain private. |
| Command coupling channel | Stable | Repository tools couple to spice through mounted commands, tracked root `spice.toml` config, `spice lock`, JSON study/task/session command output, and the versioned `spice.command-plan` mounted-command protocol instead of importing spice from an operated repository environment. Bare mounted planners preview; `--apply[=<plan-digest>]` is the mutation boundary. |
| Agent bootstrap contract | Stable | Worktree skill invocation, `spice agent activation`, `spice session briefing`, and task-board rehydration are the supported prompt-boundary path. |
| Release commands | Stable enough | `spice release prepare`, `notes`, `publish`, and `github` are operator-facing commands. Bare authored-input release verbs render their ordered plan; only `--apply` mutates or publishes. Minor output changes are possible, but the workflow contract should remain intact. |
| Task allocator CLI | Settling | Handles, phases, claims, review flow, and `spice task next` are real operating surfaces. Allocator mutations remain direct-intent operations that apply immediately and keep driving through phase boundaries. Script against command output cautiously; prefer the CLI over direct Taskwarrior storage. |
| Session forensics | Settling | `spice session briefing`, `phases`, and `messages` are supported for agent rehydration and review. Deeper analytics families may still be renamed or split. |
| Serve lane UI and live bus | In motion | Lane rendering, WebSocket message shapes, browser payload details, and task-drain refresh behavior are active product surfaces, not stable extension APIs. Use them through `spice serve` rather than depending on wire details. |
| Team API and store schema | In motion | Fused teams, lane membership, revisions, metric attribution, and renewal lineage are still being shaped. Expect schema and command changes. |
| Static browser modules | Internal | `spice/serve/static/app.*.js` files are frameworkless implementation modules. Tests document invariants, but module boundaries are not public APIs. |
| Supervisor internals | Internal | Watchdog, side-channel, lifecycle state files, and process supervision details may change as long as the public agent/session/task contracts remain true. |

## Configuration governance

Three built-in constitution gates enforce the mechanically knowable
configuration properties. `config-key-validity` validates every active source
against the structural schema. `config-false-disable` proves every declared
named-entry registry is schema-real, removes literal `false` through the shared
resolver, and has a live production consumer through that resolver.
`config-tracked-trust` proves every declared repository-executable root is
schema-real, included in the approval digest inventory, and represented by a
production approval guard. The approval guard itself refuses any path absent
from that digest inventory.

Semantic classification remains a review responsibility: a new configuration
surface must be identified as a disableable named-entry registry, executable
repository input, both, or neither. Review must also verify the live consumer
checks the shared resolver or approval immediately before it uses the value.
Once those classifications and consumers are declared, the gates prevent
schema, inventory, and call-site drift.

## Mutating command defaults

For this contract, a mutation changes persistent repository or harness
authority, or publishes to an external authority. Rendering standard output or
a caller-selected report, creating a scratch tree, rebuilding a disposable
projection, holding a process lifetime, and delegating a child argv are
operational outputs rather than separate mutation-default decisions.

The default follows from a verb's **effect-driving reads**: inputs whose
contents can change the operations it plans or the payload it applies. Reads
used only to locate a target, check a precondition, preserve unrelated content,
copy bytes or publish a selected Git tree opaquely, or parameterize an
already-named action as approved standing policy do not make that input the
operation's authored operand.

**Default criterion:** A bare mutating verb previews when any effect-driving
read is semantic input the operator authored: a document, a configuration or
manifest surface, or repository content the verb semantically interprets to
decide what it will rewrite or publish. It applies when all of its effects are
specified by the command line, the task or agent board, live runtime state, and
approved standing policy alone. Batch records whose grammar is the argv
equivalent count as command-line intent; a document does not change provenance
merely because standard input transports it. An explicit mutation option is
part of the command line, so a read-only verb with `--fix`, `--write-*`, or
`--create-tasks` has already received its apply instruction; this
classification does not make those existing spellings stable.

Invocation through a Git or agent hook supplies command-line intent for the
backend. It does not change the provenance of a semantic payload the backend
reads or make the backend inherit the parent's mutation-default classification.
A backend that only validates or vetoes a parent-owned mutation has no
mutation-default decision of its own.

Effect count, destructiveness, and reversibility do not choose the default.
There are no per-verb exceptions: if the reads below predict the wrong answer,
the criterion or the read classification must change.

For `init`, `init --unapply`, `dev install-hooks`, `task ingest`,
`task artifact prune`, and the publishing release verbs, bare invocation
renders the human plan, `--json` renders the same ordered plan for machines,
and `--apply` is the only mutation instruction.

The executable authored-input inventory is metadata on the live leaf parsers,
not a second hand-maintained command list. Its regression walks those parsers,
derives every preview/apply, explicit-option, and hook-backend case from that
metadata, and independently checks every live `--fix`, `--write*`, and
`--create-tasks` option against the classification.

Beginning with v0.30.0, the former explicit preview option is withdrawn from
`spice init` and `spice task ingest`. Those commands refuse that old option
with the owning release and direct operators to bare invocation for preview or
`--apply` for execution; they never accept old and current mutation options in
one invocation.

| Mutating verb or invocation | Effect-driving reads | Classification |
| --- | --- | --- |
| `spice init`; `spice dev install-hooks` | The existing repository and Git configuration semantically reconciled with packaged initialization policy through the shared planner | Authored input |
| `spice init --unapply[=<receipt-digest>]` | The current ownership receipt and repository and Git configuration semantically reconciled to determine safe reversal; the optional digest asserts that receipt rather than selecting a caller-supplied path | Authored input |
| `spice task ingest` | The Markdown task document selected by path or standard input | Authored input |
| `spice task artifact prune` | Operator-chosen retention metadata in artifact manifests, combined with task completion state | Authored input |
| `spice release minor`; `patch`; `prepare`; `publish`; `github` | The versioned repository tree and, where supplied, curated release notes that become the published payload | Authored input |
| `spice study env-policy --write-baseline`; `mutations --write-ratchet`; `reachability`, `symbol-reachability`, `assertion-free-tests`, and `private-internals` with `--create-tasks` | Repository content interpreted into a baseline, ratchet, or finding tasks; the explicit option is the apply instruction | Authored input |
| `spice dev serve-web-types --write`; `spice doctor --fix`; `spice dev doctor --fix` | Authored schema or repository state interpreted into generated-state repairs; the explicit option is the apply instruction | Authored input |
| `spice dev commit-msg`; `pre-commit` | The commit message or staged repository tree, plus tracked policy, semantically interpreted to validate and rewrite or restage authored content | Authored input |
| `spice task next`; `add`; `done`; `review`; `oops`; `note`; `reword`; `depends`; `wake`; `claim`; `reclaim`; `unclaim`; `modify`; `delete`; `capture` | Command-line intent and the task board, claim, and dependency graph; a repository tree selected for integration or publication is transferred opaquely | Direct intent |
| `spice task artifact add` | The command line names the task and source path; the selected bytes are copied opaquely rather than interpreted as instructions | Direct intent |
| `spice agent activation`; `requeue-deadletter`; `import`; `reply`; `ensure`; `supervise`; `post-tool-hook` | Command-line or ambient-agent identity, approved launch policy, and live agent, inbox, and supervisor state | Direct intent |
| Mutating forms of `spice config set`; `say`; `judge`; `personality`; `agent` | Exact assignments, clears, and scope named on the command line; existing files are read only to preserve unrelated keys. The system-scope form includes explicit `--apply` after a bare preview names the installed path and reinstall loss. | Direct intent |
| `spice maxim propose`; `file-proposals`; `disable`; `enable` | Command-line intent and durable ACK, maxim, and task-board state | Direct intent |
| Interactive Serve mutations | Native fact stores plus the live operator action that requests the authority change | Direct intent |

`spice agent run` and `spice lock run` only delegate an argv; the child verb
owns the default for its own effects.

When in doubt, prefer commands, tracked config, and extension entry points over internal files.
Stable means compatibility matters; in motion means the idea is real, but the
shape is still allowed to improve.
