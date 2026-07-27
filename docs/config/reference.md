# Configuration Reference

Spice configuration has exactly four scopes, in increasing precedence order:

| Scope | Path | Behavior |
| --- | --- | --- |
| `system` | `<installed spice package>/spice.toml` | Installed defaults; writable only when that existing file is writable |
| `pyproject` | `<repository>/pyproject.toml` | Tracked `[tool.spice.*]` tables |
| `repository` | `<repository>/spice.toml` | Tracked plain Spice tables such as `[agent]` |
| `worktree` | `<repository>/.spice/config/spice.toml` | Local plain Spice tables for one worktree |

Managed runtime state has three ownership namespaces. Here,
`<worktree-git-dir>` is the path reported by `git rev-parse --git-dir` for the
current worktree, while `<git-common>` is the path reported by
`git rev-parse --git-common-dir`:

| Namespace | Ownership | Examples |
| --- | --- | --- |
| `<repository>/.spice` | Worktree-visible configuration and generated integration surfaces | Worktree `spice.toml`, Git hook shims, inbox files |
| `<git-common>/.spice` | Managed state shared by every worktree of the repository | Task backend by default, team state, ACKs, maxim metrics, attachments, task artifacts, flex claims |
| `<worktree-git-dir>/.spice` | Managed state owned by one worktree/lane | Agent runtime, sticky constitution state, disabled-maxim state |

An explicit absolute `SPICE_TASK_BACKEND` redirects task configuration,
TaskChampion storage, and team state. It does not redirect repository-owned ACK
or maxim state, nor does it change either canonical Git-internal namespace.

Tables merge recursively from `system` through `worktree`. A scalar or list at
a later scope replaces the earlier leaf completely; lists never concatenate,
and `key = []` explicitly clears an inherited list. A later scalar can replace
an earlier table and a later table can replace an earlier scalar. Named wrapper
groups are the one table-level atomic boundary: defining
`[wrappers.<group>]` in a later scope replaces that whole inherited group.
Every inline `scopes = { ... }` selector is also one atomic leaf: a later
configuration layer replaces the complete selector instead of inheriting
individual axes from earlier layers.

### Universal applicability selectors

Configurable entries express applicability with one inline selector:

```toml
scopes = { paths = ["spice/**", "tests/**"], drivers = ["codex"], models = ["gpt-5.5"] }
```

Values within one axis are alternatives (OR), different axes are simultaneous
requirements (AND), and absent axes are unconstrained. Normalization,
validation, specificity, matching, and explanations come from the shared
selector model; `paths` uses the canonical repository path matcher. Each
consumer declares the axes it can evaluate, and a malformed or unsupported
axis produces the same diagnostic naming that consumer and its supported set.
An empty `scopes = {}` leaf explicitly clears inherited applicability.

The initial admitted axes are grounded in current applicability consumers:

| Axis | Current applicability consumers | Value contract |
| --- | --- | --- |
| `paths` | Policy rules, study providers, pre-commit command steps | Repository-relative PATHPOL glob-or-subtree selectors |
| `drivers` | Wrappers, wrapper routes, maxim bags, pre-commit command steps | Registered agent driver names |
| `models` | Pre-commit command steps | Normalized effective worktree model identifiers |
| `phases` | Pre-commit command steps | `pre-commit` or `pre-commit-success` |
| `extensions` | Policy rules | File suffixes beginning with `.` |

Command heads and flags remain wrapper-routing payload. Language families and
test/generated roles remain classification datasets. Task phases remain live
allocator routing state. `scopes.models` filters an entry against the effective
configured worktree model; it never chooses a launch model. The `agent.model`
and `tasks.phase_models.<driver>.<phase>.model` keys remain launch payload. The
four configuration-layer names remain precedence metadata. None of those
payload, dataset, routing, or layering concepts is accepted as a new `scopes`
axis.

Lane and task routing use similarly named fields, but they are live control
plane predicates rather than configurable entry applicability:

| Runtime field or vocabulary | Classification | Why it is not `scopes` |
| --- | --- | --- |
| Team `members` (`agent_id` / `team_id`) | Live team membership | Membership records which worktree-bound actors currently form a lane; it changes through compose, split, merge, and renewal events. |
| `lifetime = Steer | Drive | Drain` | Lifetime lens | The selected lifetime reinterprets durable route state: manual pins, all stored subscriptions, or every assignable stem. It does not select configuration entries. |
| `task_filter_entries`, route `filter` / `manual`, and `project:<stem>` / `phase:<phase>` / `+tag` terms | Allocator project filters | These are Taskwarrior query predicates derived from current team state and task projects. They control queue visibility, not whether a configuration entry applies. |
| Private `project:agent.<actor>.task` and `origin_thread.is:<actor>` terms | Origin visibility | These terms preserve an actor's private work and provenance visibility across Drive/Drain routing. They are computed per actor and task row. |
| Task-row `phase` | Allocator lifecycle state | This phase advances as work is done; only the separately named pre-commit command-step phase is configuration applicability. |

Configuration may still contain payload that initializes or influences those
runtime models, such as `serve.default_lifetime`. That does not turn the live
membership, lifetime, filter, origin, or task-phase fields into selector axes.

The `pyproject` scope alone uses the `tool.spice` prefix:

```toml
[tool.spice.agent]
model = "gpt-5.5"
```

The same value in `system`, `repository`, or `worktree` TOML uses its plain
shape:

```toml
[agent]
model = "gpt-5.5"
```

`spice config show` prints all four parsed layers, their source paths, the
effective mapping, and the winning source for every key as deterministic JSON.
`spice config system` prints effective agent values and their provenance from
the same layered view. Mutable commands default to `--scope worktree`; agent,
personality, say, and judge settings also accept `system`, `pyproject`, and
`repository`. `--clear` removes only that command's values from the selected
scope, revealing the next earlier layer without changing it.

All mutable commands use one structured TOML editor. It preserves unrelated
tables, comments, ordering, and scalar types, validates the resulting document,
and atomically replaces the selected file. A system write requires the installed
`spice.toml` to exist and be writable; the other three scopes are created on
demand. Invalid or unwritable mutations report `scope=<name> path=<path>` before
changing bytes.

The configuration migration is complete. Runtime code does not read or import
`.spice/config/state.json`; an old file is ignored. Move any values that still
matter into `.spice/config/spice.toml` using the plain tables above, then delete
the JSON file. There is no compatibility scope name or JSON adapter.

## Runtime Model

Runtime is not a per-repo config surface. The `spice` executable is installed as
a uv tool by default; operators deploying from source use
`uv tool install -e /path/to/spice-main`, making that editable main tree the
server deployment. Worker worktrees are operated trees: config can shape agent
defaults and policy in those trees, but it does not choose a different spice
source checkout, import path, or virtualenv for the running code.

The agent shell can optionally use
[RTK](https://github.com/rtk-ai/rtk) `0.42.4` or newer as a command-output
optimizer. Missing, obsolete, or invalid RTK selects reported native-command
mode without blocking activation, as does an RTK whose rewrite answers a probe
search differently than the command as written. Install and protocol details
live in
[CONFIG.md](../../CONFIG.md#rtk-rewrite-companion).

## `[tool.spice.rtk]`

RTK executable identity is a standard layered setting:

| Key | Default | Meaning |
| --- | --- | --- |
| `executable` | `"rtk"` | One trusted executable basename or absolute path. Relative paths, whitespace-delimited command strings, and argv lists are invalid. |

The tracked form is `[tool.spice.rtk]`; system, repository, and worktree
`spice.toml` files use `[rtk]`. Later scopes replace `rtk.executable` normally,
and `spice config show` reports its winning scope and path. Resolution retains
the exact value and performs no `which`, existence, or executable probe.
Activation, Doctor, and `spice agent run` then invoke that exact identity.

Exit `0` or Exit `3` with non-empty stdout applies a rewrite. Exit `1` with
empty stdout is a silent no-match. Other or malformed outcomes are diagnosed,
discarded, and the original native command runs unchanged. RTK owns rewrite
selection and the canonical `rtk` frontend; Spice remaps that frontend to the
configured identity, owns only the built-in `common` wrapper's finite
post-selection routes and the thread-scoped `RTK_DB_PATH` location at
`<worktree-git-dir>/.spice/agents/<thread>/rtk/history.db`, and emits health
telemetry through activation `rtk_status`, Doctor, and bounded stderr
diagnostics. RTK owns the history database contents.

Task-boundary and worktree-discovery git commands have a 120-second default
deadline; network fetch and push default to 30 seconds. Set
`SPICE_GIT_TIMEOUT_SECONDS` to one positive number of seconds to override both
deadlines for unusually slow repositories. Expiry fails loudly with the exact
git argv instead of retaining the task boundary indefinitely.

## Linux Speech with `espeak-ng`

Speech configuration is worktree-local by default. On Debian or Ubuntu, install
the `espeak-ng` package and verify the executable before configuring spice:

```sh
sudo apt-get update
sudo apt-get install espeak-ng
command -v espeak-ng
espeak-ng --version
```

Other Linux distributions should install the package named `espeak-ng` with
their system package manager. Configure its stdout WAV mode, matching audio
content type, and rate slot exactly as follows:

```sh
spice config say --backend external --command "espeak-ng --stdout -s {words_per_minute}" --content-type audio/wav
```

`spice serve` sends prepared speech text to the command on stdin and serves the
WAV bytes returned on stdout as `audio/wav`.

Spice does not know which flag an arbitrary engine spells its rate with, so an
external command names the spot itself: every `{words_per_minute}` token in the
command is replaced with `say.words_per_minute` scaled by the rate the listener
picked in the UI. Substitution replaces that one token and nothing else, so a
command carrying unrelated braces reaches the engine verbatim. A command naming
no slot renders at whatever rate its own engine defaults to, and the UI rate
control then has nothing to act on. Only the macOS `say` backend receives
`say.voice`, which spice writes into the argv it builds itself.

Verify the same executable path independently with:

```sh
printf 'spice speech check' | espeak-ng --stdout > /tmp/spice-speech-check.wav
file /tmp/spice-speech-check.wav
```

## Maxim Judge Binary

Maxim adjudication is off by default. When a trigger bag matches sampled prose,
Spice publishes its `[MAXIM]` reminder directly, launching no judge subprocess
and taking no verdict as assumed — the trade is more false positives for a
deterministic, portable default that needs no local model. Opt into adjudication
per worktree with:

```console
spice config judge --enable
spice config judge --disable
```

`--enable` stores `[judge].enabled = true` in `.spice/config/spice.toml`;
`--disable` restores the judge-free default. Any value other than a true flag
(`true`, `1`, `yes`, `on`) — including an absent one — resolves to judge-free.
When adjudication is enabled, each matched bag is sampled against its maxim: a
`YES` verdict means the sampled text agrees with the maxim and is therefore not
a violation, so its reminder is suppressed; a `NO` verdict is a violation and
publishes. The judge is consulted only on this opt-in path.

Configure the judge binary in the default worktree scope with:

```console
spice config judge --bin /path/to/judge
```

This stores `[judge].bin` in `.spice/config/spice.toml`. The value is one
executable path or `PATH` name, not a shell command or argv list. When unset,
the default is keyed to the platform: macOS uses the Apple Foundation Models
`afm-cli` binary; every other platform, where `afm-cli` does not exist, uses the
portable `spice-judge` adapter that ships with Spice. An explicit `bin`
overrides this default on every platform. For each verdict Spice launches the
exact argv `[configured_bin]`. `bin` selects which executable the enabled
adjudication path launches; it does not by itself enable adjudication. The
binary participates in the normal four-layer configuration precedence and
accepts `--scope`; the `enabled` flag is intentionally worktree-local, so
`--enable` and `--disable` require the default `--scope worktree`.

### Portable judge with `spice-judge`

`spice-judge` is Spice's own console script and conforms to the contract in this
section: launched as the exact argv `[spice-judge]`, it reads the prompt on
stdin and writes `YES`/`NO` to stdout. It delegates the judgement to a portable
local model command, obtainable off macOS. The default command runs a small
local model through [Ollama](https://ollama.com); install it and pull the model
once with `ollama pull llama3.2`.

`SPICE_JUDGE_MODEL_CMD` overrides the default with any argv that reads a prompt
on stdin and writes an answer to stdout (for example
`SPICE_JUDGE_MODEL_CMD="ollama run mistral"`). `SPICE_JUDGE_TIMEOUT` sets the
per-verdict deadline in seconds (default `60`; a non-positive value disables
it). There is no silent no-op: when the model command is absent, exits non-zero,
or exceeds its deadline, `spice-judge` exits non-zero with an actionable message
on stderr, which Spice surfaces as its judge error detail. Bring your own judge
by setting `[judge].bin` to any conforming executable instead.

The judge receives one prompt on stdin and writes its verdict to stdout. The
default prompt contains these four lines in a random order on every attempt:

```text
IFF "{maxim}" AGREES WITH "{statement}": ANSWER ONLY "YES".
IFF "{maxim}" DISAGREES WITH "{statement}": ANSWER ONLY "NO".
IFF "{statement}" AGREES WITH "{maxim}": ANSWER ONLY "YES".
IFF "{statement}" DISAGREES WITH "{maxim}": ANSWER ONLY "NO".
```

Before interpolation, Spice collapses whitespace in `maxim` and `statement`
and strips trailing punctuation and whitespace. `--prompt-file` on
`spice maxim agree` or `spice maxim disagree` replaces the default template;
only `{maxim}` and `{statement}` fields are accepted.

The output schema is plain text, not JSON. Spice uppercases stdout, removes
characters other than `Y`, `E`, `S`, `N`, `O`, and spaces, and accepts the
result only when its deduplicated token set is exactly `{"YES"}` or `{"NO"}`.
An ambiguous reply is retried, with two attempts by default.

The process must exit `0`. Launch failure or nonzero exit is immediate; stderr
is included for nonzero exits. Spice imposes no subprocess timeout, so wrappers
for models that can hang must enforce one. Direct maxim checks return `0` when
their requested condition is met, `1` when unmet, and `2` for judge or prompt
errors. During supervision, judge errors are logged and skip that maxim
feedback without stopping transcript capture, steering, or tasks. Learning
distillation likewise skips candidates whose judge call fails.

## `[tool.spice.agent]`

| Key | Default | Meaning |
| --- | --- | --- |
| `model` | driver default | Project-wide desired agent model. Worktree config and explicit launch flags can override it. |
| `effort` | driver default | Project-wide desired reasoning effort. Codex and Claude map this through their driver seams. |
| `driver` | `codex` | Project-wide agent driver, currently `codex` or `claude`. `SPICE_AGENT_DRIVER` and worktree config can override it. |
| `wrappers` | `["common"]` | Ordered wrapper groups loaded into agent shells. Use `[]` to disable configured wrapper functions. |

Agent personality defaults to the worktree scope through `spice config
personality`; pass `--scope system`, `--scope pyproject`, or `--scope
repository` to set it in another layer. The effective default is `pragmatic`.

Personality is a launch knob, and unlike `model` and `effort` it does not cross
every driver: Codex carries it into each launch as a config override, while
`claude --print` has no launch-time flag for a personality or for fast mode.
Each driver declares which knobs it can carry, the launch path sends only those,
and `spice config personality` names the active driver's answer as it writes the
value. A launch asked for a knob its driver cannot carry reports the knob on the
`spice agent ensure` output rather than dropping it quietly.

### Supervised Claude tool boundary

Spice is the sole task control plane in supervised Claude lanes. Every
`claude --print` launch therefore places these exact bare names in
`permissions.deny`:

```text
Task, Agent, TaskCreate, TaskGet, TaskList, TaskUpdate, TaskOutput, TaskStop
```

Bare denies remove the tool definitions from Claude's model context before the
launch's `bypassPermissions` mode is evaluated; this is a model-context
boundary, not merely a rejected-call policy. `Task` is retained as the accepted
older name for `Agent`. Spice deliberately inventories every name instead of
making a generic built-in-name wildcard part of its contract, so any new
Task-prefixed Claude built-in requires an explicit inventory and test update.
Future Claude-tool emulation must be built on top of Spice tasks and is outside
this boundary.

The inventory was last checked on 2026-07-12 with installed Claude Code
2.1.201 against Anthropic's [tools
reference](https://code.claude.com/docs/en/tools-reference), [TypeScript SDK
reference](https://code.claude.com/docs/en/agent-sdk/typescript#agent), and
[permission evaluation
reference](https://code.claude.com/docs/en/agent-sdk/permissions#how-permissions-are-evaluated).
`Monitor` has a separate process-lifecycle rationale under the [Lifecycle
Plane](../design/ARCHITECTURE.md#lifecycle-plane), tracked by
`FOUNDAT-1kCyNZT3`.

## `[tool.spice.wrappers.<group>]`

Wrapper groups define shell functions for agent-owned commands. Select groups
with `[tool.spice.agent] wrappers = [...]`.

| Entry shape | Meaning |
| --- | --- |
| `wrapper = ["cmd1", "cmd2"]` | Create wrapper function `wrapper` and route each listed command selector through it. |
| `selector = { argv = ["tool", "subcommand"] }` | Create a direct wrapper function named `selector` that runs the configured argv plus caller arguments. |
| `selector = { scopes = { drivers = ["codex"] }, argv = [...] }` | Render a direct wrapper only for the listed, validated active drivers. Wrapper groups and individual `match` routes accept the same `scopes.drivers` selector. |

RTK rewrite selection happens inside `spice agent run`. The built-in `common`
group supplies only the finite post-selection command-shape transformations:
rg-only grep flags to `rg`, native find predicates to `find`, diagnostic git
flags to `git`, a Codex-scoped head-only route that injects `-E` into
`rtk grep`, and one plain `grep` wrapper shared by both drivers. Every selected
direct wrapper whose argv head is not RTK owns its command word regardless of
configuration source, so a raw `rg` whose RTK candidate ends at `grep` remains
native `rg`. The shared plain wrapper preserves Claude's BASIC-regexp authoring
with `\|`; its Codex-scoped catch-all injects `-E`, with explicit matcher flags
still winning.
Naming `common` in a repo `[tool.spice.wrappers.common]` table replaces the whole
group atomically — routes do not concatenate, so an override must re-list every
route it keeps — while omitting the table inherits this default and
`wrappers = []` disables generation. A `false` group or entry disables that
inherited name explicitly. Repo groups should otherwise wrap stable repo-owned
tools (see [wrapper commands](../cli/wrapper-commands.md)).

## `[tool.spice.commands]`

Mounted commands put repo tooling under the `spice` namespace.

```toml
[tool.spice.commands]
release = ["uv", "run", "python", "-m", "spice.release"]
bench = "python -m myproj.bench"
report.inspect = ["project-tool", "report", "inspect"]
```

Keys are dot-separated command paths with lowercase/digit/hyphen segments.
Mounts cannot shadow built-in or extension-provided `spice` actions at any
depth. Dotted mounts under built-in verbs are allowed when the full command path
is a novel action name. For example, these fail because the full paths already
resolve to registered actions:

```toml
[tool.spice.commands]
"study.csharp-members" = "./scripts/csharp-members.sh"
"dev.pre-commit" = "./scripts/pre-commit.sh"
```

This is allowed when no `spice study repo-tool` action is registered:

```toml
[tool.spice.commands]
"study.repo-tool" = ["project-tool", "study", "repo-tool"]
```

Values are command strings or argv lists; remaining CLI arguments are passed
through verbatim.

## `[tool.spice.locks]`

Resource locks coordinate exclusive local resources such as editor instances,
emulators, databases, license seats, or a fixed pool of sandbox shards. The
tracked table declares the resources; `spice lock run` holds one while a child
command runs and releases it when that child exits.

```toml
[tool.spice.locks]
lock_contention_exit_code = 75
chosen_shard_contention_exit_code = 76
pool_exhaustion_exit_code = 77

[tool.spice.locks.named.editor]
path = ".spice/locks/editor.lock"
contention_exit_code = 75

[tool.spice.locks.pools.android]
directory = ".spice/locks/android"
shards = 3
chosen_shard_contention_exit_code = 76
pool_exhaustion_exit_code = 77
```

`spice lock run editor -- project-tool edit` acquires the single named lock.
`spice lock run android --pool -- project-tool test` acquires the first free
pool shard. `spice lock run android --pool --shard 1 -- project-tool test`
requires a specific zero-based shard.

Default state paths live under `.spice/locks/` when a resource omits `path` or
`directory`. Each held lock writes JSON holder metadata into its lock file with
`pid`, `cwd`, and `started_at`; `spice lock status --json` lists configured
locks and pool shards with that metadata. Per-invocation flags such as
`--path`, `--directory`, `--shards`, `--lock-contention-exit-code`,
`--chosen-shard-contention-exit-code`, and `--pool-exhaustion-exit-code`
override the tracked defaults for that one run.

## `[tool.spice.policy]`

The policy table extends the constitution. Defaults come from `spice/policy.py`.

`exclude`, `generated_paths`, `test_paths`, language suffix families, and
generated-file patterns are domain datasets rather than entry selectors.
Path-bearing datasets retain those names and delegate path evaluation to the
shared PATHPOL matchers.

| Key | Meaning |
| --- | --- |
| `package_roots` | Python namespace-package roots; derived from packaging metadata when unset. |
| `name_cluster_threshold` | Sibling prefix/suffix cluster size before namespace packaging is required. |
| `exclude` | Tracked paths/globs skipped by study walkers. |
| `generated_paths` | Tracked paths/globs exempt from repo-shape guards. |
| `test_paths` | Test roots for production/test classification. |
| `repo_truth_docs` | Doctrine docs checked because they ride in agent context. |
| `env_name_patterns`, `env_names`, `env_access_gate` | Env literal watchlist, exact manifest, and access-waiver gate. |
| `reachability_providers` | Extra language-aware dead-code providers. |
| `csharp_unused_retention` | C# unused-candidate declarations for framework-retained base types, interfaces, and attribute names. |
| `python_typecheck_interpreter` | Optional interpreter for `python-typecheck`; otherwise repo venv/uv is resolved. |
| `assertion_helpers` | Callable names that count as test assertions. |
| `internal_couplings` | Named private-internals allowlist entries `{ path, test, target }`. |
| `suite_seam` | Paths the whole suite depends on, and the suite command that gates a task landing that touches one. |
| `pre_commit` | Extra gate steps that run after built-in pre-commit gates. |
| `pre_commit_success` | Success-only steps that run after the gate passes. |
| `pre_commit_builtins` | Per gate-only pre-commit built-in overrides for `merge-integrity`, `plan-phase`, `repo-shape`, `staging`, `repo-docs`, `formatters`, `local-paths`, `taste`, `serve-web-typecheck`, `javascript-unused`, `python-typecheck`, `env-policy`, `env-name-ledger`, `file-shape`, `complexity`, `magic-numbers`, `markdown-links`, `reachability`, `symbol-reachability`, `python-unused`, `assertion-free-tests`, and `private-internals`. |
| `limits`, `flex`, `rules` | Base bounds, sticky headroom, and applicability-selected overrides. |
| `languages`, `lockfiles`, `file_shape`, `env_access` | Suffix/pattern families for grammar-aware gates. |
| `complexity`, `taste`, `magic`, `debt`, `commit_message` | Gate-specific knobs. |
| `markdown_depth_budget` | Generated repo-doc character scopes for markdown. |

Shell env-access patterns intentionally cover name-like parameters, not shell
special or positional parameters such as `$?`, `$$`, `$1`, `$@`, `$*`, `$#`,
`$-`, or `$_`.

### `[tool.spice.policy.languages]`

Suffix families for `complexity`, `magic`, `env`, and `c_grammar` scans.

### `[tool.spice.policy.lockfiles]`

Generated lockfile `suffixes` and `names` exempt from file-shape pressure.

### `[tool.spice.policy.file_shape]`

`source_suffixes` selects files for LOC/byte pressure. `generated_patterns`
exempts generated sources such as protobuf modules, minified bundles, and build
outputs.

### `[tool.spice.policy.env_access]`

`family_suffixes` maps language families to suffixes. `default_patterns` maps
families to env-access regexes; custom pattern families must have suffixes.
`baseline` points at existing `env-policy` findings. The env-name ledger only
accounts for extractable literal names and scans tests like production.

### `[tool.spice.policy.suite_seam]`

Per-lane verification is a subset twice over. An agent runs the tests that name
the module it changed, and for a widely depended-on module that direct-import
view understates the real reach by an order of magnitude. It then runs that
subset against its own pinned baseline, which the other lanes have moved since.
Both gaps close only on the integrated tree, so this gate runs there: after
`spice task done` merges the task onto the baseline and before it pushes.

`paths` lists the repository paths whose reach is the whole suite. A task whose
footprint touches one runs `run` -- the whole suite -- against the merged tree,
and a red suite refuses the publish with the merge left in the tree to fix. A
task that touches nothing declared matches nothing and runs nothing, so only the
landings that need the coverage pay for it, and no commit pays at all.

`run` is an argv list, kept in order and with repeats intact. Optional `seconds`
is the cost the repository accepts when the gate fires; the gate prints it before
starting and reports the measured wall clock afterwards, so a stale declaration
is visible on every seam landing.

```toml
[tool.spice.policy.suite_seam]
seconds = 200
run = ["spice", "dev", "pytest", "-q", "--ignore=tests/browser"]
paths = ["spice/tasks/tw.py", "spice/policy.py"]
```

Choose `paths` by measurement rather than by feel: a path belongs here when it
is transitively reachable from most of the test suite, which is the condition
that makes a lane's own subset misleading. `spice study suite-seam-reach` is
that measurement, and it fixes the terms the answer turns on -- a test module
is a collected `test_*.py` file under the configured test roots, and reach
follows imports wherever they appear, including inside function bodies. It
ranks every package module by how many test modules reach it, alongside how
many name it directly, so a candidate is compared against the whole ordering
rather than judged alone. The angle-bracketed fields below are filled from the
live graph:

```console
$ spice study suite-seam-reach --limit 30
suite-seam-reach: <declared> declared module(s) of <package-modules>, reached by at least <floor> of <test-modules> test module(s)
suite-seam-reach: <widest-undeclared-path> leads the undeclared rest at <reach>, so the band is <verdict>
  <reach> reached <direct-imports> imported  <package-path> [declared]
  ...
```

The command's two header lines are the decision. The first reports the reach
of the narrowest module in the declared band; the second reports the widest
module left out. When the first is greater than the second, the declaration
names a group the import graph already separates. The command is the source of
these point-in-time figures, which change as tests and imports change, and
exits non-zero when the break closes. `--json` emits the same ranking for a
repository that wants to consume it.

A repository that gates on this should assert the result rather than restate
it. In this one,
`tests/test_suiteseam.py::test_this_repository_declares_exactly_the_widest_reaching_modules`
requires the declared paths to occupy the leading slots of that ranking and the
boundary below them to be a strict break, so a path added by feel, or a module
that grows into the band without being declared, fails the suite with the
ranking in hand.

A red suite here is a refusal to publish, not a lost merge. The integrated tree
stays checked out, so the failures reported are the ones the branch would have
taken; fix them, commit, and run `spice task done` again.

### `[tool.spice.policy.csharp_unused_retention]`

Tracked declarations for C# members that are reached by framework convention
rather than by direct C# references. The table only adds retained findings; the
built-in partial-declaration and attribute-retention defaults still apply when
the table is absent or when no declaration matches.

```toml
[tool.spice.policy.csharp_unused_retention]
base_types = ["HostedServiceBase"]
interfaces = ["IPluginModule"]
attribute_names = ["ServiceEntryPointAttribute"]
```

For example, a dependency-injection container may instantiate every class
derived from `HostedServiceBase`, while a plugin host may discover classes that
implement `IPluginModule`. Private methods and fields inside those types are
reported as retained with reasons such as
`configured_base_type:HostedServiceBase` or
`configured_interface:IPluginModule`. Attribute names match with or without the
`Attribute` suffix, so `[ServiceEntryPoint]` can match
`ServiceEntryPointAttribute` and records
`configured_attribute:ServiceEntryPointAttribute`.

Policy constants enforced by default: files `1000` LOC / `80000` bytes with
`1.5x` flex, routines CCN `20` / length `80`, commit text wrap `100`,
repo-root markdown `10000` chars plus `10000` per nested directory until
`30000`, magic-number threshold `10`, and magic baselines against `HEAD`.

### `[tool.spice.policy.limits]`

Base caps: `file_loc`, `file_bytes`, `routine_ccn`, `routine_length`,
`commit_message_wrap`, and `repo_truth_doc_chars`.

### `[tool.spice.policy.flex]`

Default `ratio` is `1.5`; explicit per-bound flex caps override it. Breaching
flex makes the item sticky until it shrinks under the base cap.

### `[tool.spice.policy.complexity]`

| Key | Default | Meaning |
| --- | --- | --- |
| `hotspot_limit` | `20` | Default number of rows shown by `spice study complexity-hotspots` when `--limit` is omitted. |

### `[tool.spice.policy.taste.words]`

The authoritative built-in map is `policy.TASTE_WORD_SUGGESTIONS`. It feeds
`spice study taste`, the staged pre-commit taste gate, and task-creation wording;
file scans cover tracked `.md`, `.txt`, and `.rst` prose. The defaults include
explicit singular, plural, past-participle, and gerund suggestions for
`allowlist`, `allowlists`, `allowlisted`, and `allowlisting`, plus `blocklist`,
`blocklists`, `blocklisted`, and `blocklisting`.

A bare key matches one whole word case-insensitively. Only a trailing `*` opts
into stem matching and covers every word-character suffix. Values are the exact
suggestions shown to the user; an empty value means remove or rephrase.

The resolver starts from the built-in map, then normalizes repository keys to
lowercase and assigns repository entries in TOML order. A matching normalized
key replaces only that suggestion; new keys extend the map, and every other
built-in entry remains active.

### `[tool.spice.policy.markdown_depth_budget]`

Generated `repo_truth_doc_chars` scopes for tracked markdown: repo root gets
`10000` chars, one nested directory `20000`, two nested directories `30000`,
and deeper docs are unlimited. `extensions` defaults to `[".md"]`; set it to
`[]` to replace generated rules with explicit `[[tool.spice.policy.rules]]`
entries.
`stem_pattern` optionally full-matches file stems; binary files are skipped.

### `[tool.spice.policy.debt]`

Allowed-finding counters, not size limits. Defaults are `0` for
`reachability_test_only` and `assertion_free_tests`; non-zero values are
explicit cleanup debt.

### `[[tool.spice.policy.rules]]`

Each policy rule is one payload with an inline universal selector:

```toml
[[tool.spice.policy.rules]]
scopes = { paths = ["Docs"], extensions = [".md"] }

[tool.spice.policy.rules.repo_truth_doc_chars]
min = 20000
flex = 1.25
```

`paths` uses the canonical PATHPOL glob-or-subtree contract without a
policy-only matcher variant; `extensions` composes with it through the universal
AND-across rule. Flat rule keys apply to every numeric bound; named sub-tables
target `file_loc`, `file_bytes`, `routine_ccn`, `routine_length`,
`commit_message_wrap`, or `repo_truth_doc_chars`. Settings accept `multiplier`,
`min`, `max`, `unlimited = true`, and optional `flex`. A nested
`magic.examine_threshold` overrides magic-number scanning. Universal selector
specificity chooses the winning applicable rule; repository-authored rules
outrank generated markdown-depth rules.

### `[tool.spice.policy.magic]`

`examine_threshold` defaults to `10`; `baseline_ref` defaults to `HEAD`.

### `[tool.spice.policy.commit_message]`

`allowed_trailers` optionally limits Git trailer keys to a finite set;
`blocked_trailers` optionally rejects specific keys. Both are unset by
default, so every trailer -- including `Co-Authored-By` -- rides through.
When a configured policy would reject the attribution trailer
(`Co-Authored-By`), spice also disables the native driver's attribution so it
never emits a trailer the commit-msg gate then rejects.

Command-step tables accept:

`label`, `mount`, `run`/`argv`, `scopes`, `formatter`, and `enabled`.
`pre_commit` steps receive `SPICE_STAGED_PATHS`; mounted steps also receive
`SPICE_MOUNTED_COMMAND=1` and `SPICE_VISIBLE_PROG`. `scopes.paths` narrows the
staged-path set with the universal PATHPOL contract, while `scopes.phases`
selects `pre-commit` or `pre-commit-success`. `scopes.drivers` and
`scopes.models` select the effective configured worktree driver and model. All
four axes compose through the universal AND rule; omitting any axis means all
values on that axis.

Reachability provider tables accept `name`, `run`, and optional
`scopes = { paths = [...] }`. `name` must not be `python`. During staged scans,
the universal selector both decides applicability and narrows
`SPICE_STAGED_PATHS`; full scans run every configured provider. Providers write
JSON findings with `kind`, `subject`, `path`, and `imported_by`; `kind` routes
whole-file findings to `reachability` and symbol findings to
`symbol-reachability`.

`internal_couplings` entries accept `path`, `test`, and `target`; all are
required non-empty strings. `test` is the test function name or `<module>`, and
`target` is the private production symbol the test imports or reaches.

## `[tool.spice.policy.pre_commit_builtins]`

Each built-in key may be:

- `true` to keep the default.
- `false` to disable it.
- A mounted command name to replace it.
- A command-step table using `mount`, `run`, or `argv`.
- `{ enabled = false }` to disable with an explicit table.

## `[tool.spice.maxims.<bag>]`

Maxim bags extend or replace the live prose conscience.

| Key | Default | Meaning |
| --- | --- | --- |
| `words` | required for new bags; inherited for built-ins | Alphabetic trigger words or phrases. |
| `message` | required for new bags; inherited for built-ins | The maxim text published to the agent as steering on a match; when adjudication is enabled it is sent to the judge first and published only on a violation verdict. |
| `scopes` | `{}` (unconstrained) | Universal applicability selector. Maxim bags support the shared `drivers` axis; cite `spice maxim report` evidence before narrowing it. |

```toml
[tool.spice.maxims.routes]
words = ["quiet route"]
message = "Respond to the real event instead."
scopes = { drivers = ["codex"] }
```

Bag names are case-folded. Trigger phrases are normalized to lowercase words.
Configured bags merge with built-ins, so a repo can tune existing bags or add
new curated near-universal preferences. An absent `scopes` leaf applies to all
drivers. The displaced per-bag `drivers` key is unsupported; maxim driver
selection uses the same normalization, validation, matching, and explanation
contract as every other `scopes.drivers` consumer.

Watchdog reminders are deduped by content-derived reminder key within one
compaction epoch. A later compaction can make the same key eligible to publish
again because the agent may have lost the earlier inbox steering, but the
compaction count never changes the configured `message` text.

## `[tool.spice.tasks]`

| Key | Default | Meaning |
| --- | --- | --- |
| `stems` | `[]` plus built-ins `task`, `serve`, `agent` | Additional public project stems. Stems use lowercase letters, digits, and underscores. `agent` is internal and not allocator assignable. |
| `hidden_stems` | `[]` plus built-in `oops` | Additional hidden system project stems. Values omit the leading dot, so `scratch` defines addressable `.scratch` projects. Hidden projects use the private `todo` flow, are reserved for system-created rows, and are excluded from normal boards and lane assignment. |
| `flows` | `{}` | Per-stem phase lists. Approved phases are `design`, `plan`, `todo`, `verify`, and `review`; the default public flow is `todo -> review`. Hidden system projects use the private `todo` flow. |
| `project_min_depth` | `2` | Minimum dotted project depth for public tasks. |
| `project_max_depth` | `3` | Maximum dotted project depth for public tasks. |

Priority aliases are fixed: `critical -> C`, `high -> H`, `medium -> M`,
`low -> L`, and `none` clears priority. The generated Taskwarrior priority UDA
accepts `C,H,M,L,`; its urgency coefficients are 8.1, 6.0, 3.9, and 1.8.
Critical and high SLA due dates are one day, medium is seven days, and low is
thirty days.

## `[tool.spice.tasks.phase_models.<driver>.<phase>]`

Per-driver, per-phase agent launch overrides. Each driver has its own model
space, so the table is keyed by driver name (`claude` or `codex`) and then by
task phase (`design`, `plan`, `todo`, `verify`, `review`, `oops`).

| Key | Default | Meaning |
| --- | --- | --- |
| `model` | unset | Model to launch with while the worktree's claimed task sits in this phase. |
| `effort` | unset | Reasoning effort to launch with for the same phase. |

```toml
[tool.spice.tasks.phase_models.claude.plan]
model = "claude-opus-4-8"
effort = "high"

[tool.spice.tasks.phase_models.claude.todo]
model = "claude-sonnet-5"
```

`spice agent ensure` reads the phase of the worktree's currently claimed task
and looks it up in this table for the active driver. A phase with no entry
(or no claimed task) falls back to the ordinary resolution order: an explicit
`--model`/`--effort` flag, then the effective `agent` table in `worktree`,
`repository`, `pyproject`, and `system` precedence order, then the driver's
shipped default.

These `model` values are launch payload selected by live task-phase routing;
they are not applicability selectors. A `scopes.models` entry filters a
consumer that declares model applicability and does not participate in lane or
task allocation.

## `[tool.spice.serve]`

| Key | Default | Meaning |
| --- | --- | --- |
| `brand` | `[project].name` or `spice` | Header and browser-title brand for `spice serve`. |
| `default_lifetime` | `Drive` | Initial serve lane lifetime: `Steer` uses manual filters, `Drive` auto-subscribes to projects the team creates or claims, and `Drain` dissolves the task boundary so all assignable work is visible. |
