# Configuration Reference

Tracked project configuration lives under `[tool.spice.*]` in `pyproject.toml`.
Worktree-local operator preferences live in `.spice/config/spice.toml` through
`spice config`.

## Runtime Model

Runtime is not a per-repo config surface. The `spice` executable is installed as
a uv tool by default; operators deploying from source use
`uv tool install -e /path/to/spice-main`, making that editable main tree the
server deployment. Worker worktrees are operated trees: config can shape agent
defaults and policy in those trees, but it does not choose a different spice
source checkout, import path, or virtualenv for the running code.

The agent shell also requires
[RTK](https://github.com/rtk-ai/rtk) `0.42.4` or newer. Install and protocol
details live in [CONFIG.md](../../CONFIG.md#rtk-rewrite-companion); this is a
runtime companion requirement, not a tracked project setting.

## Linux Speech with `espeak-ng`

Speech configuration is worktree-local. On Debian or Ubuntu, install the
`espeak-ng` package and verify the executable before configuring spice:

```sh
sudo apt-get update
sudo apt-get install espeak-ng
command -v espeak-ng
espeak-ng --version
```

Other Linux distributions should install the package named `espeak-ng` with
their system package manager. Configure its stdout WAV mode and matching audio
content type exactly as follows:

```sh
spice config say --backend external --command "espeak-ng --stdout" --content-type audio/wav
```

`spice serve` sends prepared speech text to the command on stdin and serves the
WAV bytes returned on stdout as `audio/wav`. Verify the same executable path
independently with:

```sh
printf 'spice speech check' | espeak-ng --stdout > /tmp/spice-speech-check.wav
file /tmp/spice-speech-check.wav
```

## Maxim Judge Binary

Configure the worktree-local judge with:

```console
spice config judge --bin /path/to/judge
```

This stores `[judge].bin` in `.spice/config/spice.toml`. The value is one
executable path or `PATH` name, not a shell command or argv list. When unset,
the default is keyed to the platform: macOS uses the Apple Foundation Models
`afm-cli` binary; every other platform, where `afm-cli` does not exist, uses the
portable `spice-judge` adapter that ships with Spice. An explicit `bin`
overrides this default on every platform. For each verdict Spice launches the
exact argv `[configured_bin]`.

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

Agent personality is a worktree-local `spice config personality` setting
(`pragmatic` by default), not a tracked `[tool.spice.agent]` key.

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

RTK rewrite selection happens inside `spice agent run`. The built-in `common`
group supplies only the finite post-selection command-shape transformations:
rg-only grep flags to `rg`, native find predicates to `find`, diagnostic git
flags to `git`, and a final head-only route that injects `-E` so `rtk grep`
defaults to extended regular expressions (an explicit `-F` or `-G` later in
argv still wins because grep honors the last matcher flag). Naming `common` in
a repo `[tool.spice.wrappers.common]` table replaces the whole group atomically
— routes do not concatenate, so an override must re-list every route it keeps —
while omitting the table inherits this default and `wrappers = []` disables
generation. Repo groups should otherwise wrap stable repo-owned tools (see
[wrapper commands](../cli/wrapper-commands.md)).

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
| `pre_commit` | Extra gate steps that run after built-in pre-commit gates. |
| `pre_commit_success` | Success-only steps that run after the gate passes. |
| `pre_commit_builtins` | Per gate-only pre-commit built-in overrides for `plan-phase`, `repo-shape`, `staging`, `repo-docs`, `formatters`, `local-paths`, `taste`, `serve-web-typecheck`, `python-typecheck`, `env-policy`, `env-name-ledger`, `file-shape`, `complexity`, `magic-numbers`, `markdown-links`, `reachability`, `symbol-reachability`, `assertion-free-tests`, and `private-internals`. |
| `limits`, `flex`, `scopes` | Base bounds, sticky headroom, and per-path overrides. |
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
`[]` to replace generated scopes with explicit `[tool.spice.policy.scopes]`.
`stem_pattern` optionally full-matches file stems; binary files are skipped.

### `[tool.spice.policy.debt]`

Allowed-finding counters, not size limits. Defaults are `0` for
`reachability_test_only` and `assertion_free_tests`; non-zero values are
explicit cleanup debt.

### `[tool.spice.policy.scopes."<matcher>"]`

Per-path numeric overrides. Glob keys match paths; non-glob keys match a path or
subtree. Flat scope keys apply to every numeric bound; named sub-tables target
`file_loc`, `file_bytes`, `routine_ccn`, `routine_length`,
`commit_message_wrap`, or `repo_truth_doc_chars`. Settings accept
`multiplier`, `min`, `max`, `unlimited = true`, and optional `flex`. A nested
`magic.examine_threshold` overrides magic-number scanning. Most-specific
match wins; exact/prefix matchers outrank globs.

### `[tool.spice.policy.magic]`

`examine_threshold` defaults to `10`; `baseline_ref` defaults to `HEAD`.

### `[tool.spice.policy.commit_message]`

`allowed_trailers` optionally limits Git trailer keys. `Co-Authored-By` is
always rejected.

Command-step tables accept:

`label`, `mount`, `run`/`argv`, `when`, `formatter`, and `enabled`.
`pre_commit` steps receive `SPICE_STAGED_PATHS`; mounted steps also receive
`SPICE_MOUNTED_COMMAND=1` and `SPICE_VISIBLE_PROG`.

Reachability provider tables accept:

`name`, `run`, and optional `when`. `name` must not be `python`. Providers
write JSON findings with `kind`, `subject`, `path`, and `imported_by`; `kind`
routes whole-file findings to `reachability` and symbol findings to
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
| `message` | required for new bags; inherited for built-ins | The maxim text sent to the judge and, on violation, back to the agent as steering. |
| `drivers` | all shipped drivers | Driver allowlist; cite `spice maxim report` evidence before narrowing. |

Bag names are case-folded. Trigger phrases are normalized to lowercase words.
Configured bags merge with built-ins, so a repo can tune existing bags or add
new curated near-universal preferences.

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

Priority aliases are fixed: `critical/high -> H`, `medium -> M`, `low -> L`,
and `none` clears priority. SLA due dates are one day, seven days, and thirty
days for H/M/L.

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
`--model`/`--effort` flag, then worktree-local config, then `[tool.spice.agent]`,
then the driver's shipped default.

## `[tool.spice.serve]`

| Key | Default | Meaning |
| --- | --- | --- |
| `brand` | `[project].name` or `spice` | Header and browser-title brand for `spice serve`. |
| `default_lifetime` | `Drive` | Initial serve lane lifetime: `Steer` uses manual filters, `Drive` auto-subscribes to projects the team creates or claims, and `Drain` dissolves the task boundary so all assignable work is visible. |
