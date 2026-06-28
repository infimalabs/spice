# Configuration

Tracked project configuration lives under `[tool.spice.*]` in `pyproject.toml`.
Worktree-local operator preferences such as speech voice, judge binary, and
local agent overrides live in `.spice/config/state.json` through `spice config`;
they are not tracked project knobs.

## Runtime Model

Runtime is not a per-repo config surface. The `spice` executable is installed as
a uv tool by default; operators deploying from source use
`uv tool install -e /path/to/spice-main`, making that editable main tree the
server deployment. Worker worktrees are operated trees: config can shape agent
defaults and policy in those trees, but it does not choose a different spice
source checkout, import path, or virtualenv for the running code. The common-dir
layout is opt-in for operators who deliberately set uv's tool directories before
installing.

## `[tool.spice.agent]`

| Key | Default | Meaning |
| --- | --- | --- |
| `model` | driver default | Project-wide desired agent model. Worktree config and explicit launch flags can override it. |
| `effort` | driver default | Project-wide desired reasoning effort. Codex and Claude map this through their driver seams. |
| `driver` | `codex` | Project-wide agent driver, currently `codex` or `claude`. `SPICE_AGENT_DRIVER` and worktree config can override it. |
| `wrappers` | `["common"]` | Ordered wrapper groups loaded into agent shells. Use `[]` to disable configured wrapper functions. |

Agent personality is a worktree-local `spice config personality` setting
(`pragmatic` by default), not a tracked `[tool.spice.agent]` key.

## `[tool.spice.wrappers.<group>]`

Wrapper groups define shell functions for agent-owned commands. Select groups
with `[tool.spice.agent] wrappers = [...]`.

| Entry shape | Meaning |
| --- | --- |
| `wrapper = ["cmd1", "cmd2"]` | Create wrapper function `wrapper` and route each listed command selector through it. |
| `selector = { argv = ["tool", "subcommand"] }` | Create a direct wrapper function named `selector` that runs the configured argv plus caller arguments. |

The built-in `common` group is intentionally empty. RTK rewrite routing happens
inside `spice agent run` — it is not a per-command wrapper. Repo groups should
wrap stable repo-owned tools (see `docs/cli/wrapper-commands.md`).

## `[tool.spice.commands]`

Mounted commands put repo tooling under the `spice` namespace.

```toml
[tool.spice.commands]
release = ["uv", "run", "python", "-m", "spice.release"]
bench = "python -m myproj.bench"
report.inspect = ["project-tool", "report", "inspect"]
```

Keys are dot-separated command paths with lowercase/digit/hyphen segments.
Top-level mounts cannot shadow built-in `spice` commands. Values are command
strings or argv lists; remaining CLI arguments are passed through verbatim.

## `[tool.spice.policy]`

The policy table extends the constitution. Defaults come from `spice/policy.py`.

| Key | Default | Enforced opinion |
| --- | --- | --- |
| `package_roots` | `[]` | Python namespace-package roots. When set, `__init__.py` is forbidden under those roots and path names must match `^_*[0-9a-z]+_*$`. |
| `name_cluster_threshold` | `4` | Number of sibling modules sharing a long alphabetic prefix or suffix before the name-cluster guard requires a namespace package. Configured values must be at least `3`. |
| `exclude` | `[]` | Tracked paths or globs excluded from study walkers, useful for committed generated sources. Built-in exclusions already cover `.git`, `.spice`, caches, venvs, and `node_modules`. |
| `generated_paths` | `[]` | Tracked paths or globs (e.g. `**/*_pb2.py`, or a directory like `pkg/proto`) exempt from every `repo-shape` guard — naming law, path shape, generic-split names, and the namespace `__init__.py` rule — so committed generated code inside a package passes without disabling the builtin. Unlike `exclude`, this reaches the shape guards, not just the study walk; the rest of the tree stays enforced. |
| `test_paths` | pytest `testpaths`, else `tests/` | Tracked test-root override for studies that need to distinguish test code from production code. Entries are repo-relative dirs or globs, and may declare multiple roots such as `["tests", "Assets/**/Tests"]`. |
| `repo_truth_docs` | `["AGENTS.md"]` | Doctrine docs capped at `5000` characters because they ride in agent context. |
| `env_name_patterns` | `SPICE_*`, `CODEX_THREAD_ID`, `CLAUDE_CODE_SESSION_ID` | Additional environment-variable literal patterns requiring `env-policy: allow` waivers. |
| `env_names` | `[]` | Exact tracked manifest for `spice study env-name-ledger`: every unique literal env-var name referenced by supported env access forms must appear here, and every name here must still be referenced. |
| `env_presence_gate` | `true` | Presence reverse-gate (on by default): every env *access site* (not just watchlisted name literals) must carry an `env-policy: allow` waiver, so the audit covers env reads under any or dynamic names. Access idioms are matched per language family (built-in Python `os.environ`/`getenv`/`putenv`/`unsetenv`; C# `Environment.GetEnvironmentVariable`/`SetEnvironmentVariable`, with optional `System.`; Lua `os.getenv`; shell `$VAR`, `${VAR}`, and `export VAR=`; JavaScript/TypeScript `process.env`). Set `false` to opt out. |
| `env_access_patterns` | `{}` | Table keyed by language family (`python`, `csharp`, `lua`, `shell`, `javascript`) of extra access-idiom regexes for the presence gate, scoped to that family's suffixes — register a repo's own idioms (e.g. bespoke Lua runtime accessors) without forking the study. |
| `reachability_providers` | `[]` | Extra language-aware dead-code providers for `spice study reachability` and `gate:reachability`. |
| `python_typecheck_interpreter` | auto | Optional Python interpreter path for `python-typecheck` in non-standard layouts. Relative paths resolve from the repo root. When omitted, spice resolves in order: repo-local `VIRTUAL_ENV`, `.venv`, then uv project interpreter. |
| `assertion_helpers` | `[]` | Callable names that count as assertions when called inside Python tests. Use leaf names such as `ensure_contract` or exact dotted calls such as `contracts.require_valid`; they extend the built-in `assert`, `pytest.raises`/`warns`/`fail`, and `assert*` recognition. |
| `internal_couplings` | `[]` | Exact private-internals exceptions as `{ path, test, target }` tables. These are named allowlist entries, never a tolerated count; stale entries fail the gate until removed. |
| `pre_commit` | `[]` | Extra command steps run after built-ins. Entries are mounted command names or command tables. |
| `pre_commit_success` | `[]` | Command steps run only after the full gate passes. |
| `pre_commit_builtins` | built-ins enabled | Per-built-in overrides for `repo-shape`, `staging`, `repo-docs`, `formatters`, `local-paths`, `serve-web-typecheck`, `python-typecheck`, `env-policy`, `env-name-ledger`, `file-shape`, `complexity`, `magic-numbers`, `reachability`, `symbol-reachability`, `assertion-free-tests`, and `private-internals`. |

Shell env-access patterns intentionally cover name-like parameters, not shell
special or positional parameters such as `$?`, `$$`, `$1`, `$@`, `$*`, `$#`,
`$-`, or `$_`.

`env-name-ledger` accounts only for literal names it can extract from supported
env access forms, watchlisted env-name patterns, or exact manifest names still
present as literals in scanned sources. Dynamic/non-literal access sites such as
`os.environ[name]` have no extractable exact name; they remain the
`env_presence_gate` waiver gate's domain.

Policy constants enforced by default: files `1000` LOC / `80000` bytes with
`1.5x` flex, routines CCN `20` / length `80` with the same flex/sticky model,
commit text wrap `100`, magic-number threshold `10`, and magic baselines against
`HEAD`.

Command-step tables accept:

| Key | Meaning |
| --- | --- |
| `label` | Human label for gate output. |
| `mount` | Name from `[tool.spice.commands]`. |
| `run` / `argv` | Command string or argv list. |
| `when` | Non-empty glob list matched against staged paths. |
| `formatter` | `true` means restage matching paths after the command succeeds. |
| `enabled` | For `pre_commit_builtins` only, `false` disables that built-in. |

Every `pre_commit` command step gets `SPICE_STAGED_PATHS` (the `when`-narrowed
staged paths). A step that names a **mount** additionally carries the mount
environment (`SPICE_MOUNTED_COMMAND=1`/`SPICE_VISIBLE_PROG`) — the same as
`spice <mount>` — because a mount run by the gate is still that mount; a raw
`run`/`argv` step is not a mount and gets neither. See
`docs/cli/wrapper-commands.md` (Execution context: mount vs gate step).

Reachability provider tables accept:

| Key | Meaning |
| --- | --- |
| `name` | Provider name shown on the reachability board. Must not be `python`, which is the built-in AST/import-graph provider. |
| `run` | Non-empty argv list executed from the repo root. |
| `when` | Optional non-empty glob list matched against staged paths by the pre-commit gate. If omitted, the provider runs whenever reachability runs. |

The same provider seam feeds both reachability gates; a finding's `kind` routes
it to exactly one gate by granularity. `module` is the coarse whole-file gate
(`gate:reachability`); every other kind (`function`, `class`, `method`, …) is a
symbol and rides the finer `gate:symbol-reachability`. A single provider may
emit both kinds in one run; no finding is counted by both gates.

Assertion helper entries are Python callable names. A leaf entry matches any
call with that final attribute name; a dotted entry matches the full dotted call
as written in the test.

Internal coupling entries use the exact fields printed by the
`private-internals` board: repo-relative test `path`, test function/method name
or `<module>`, and private `target`.

```toml
[tool.spice.policy]
internal_couplings = [
  { path = "tests/test_worker.py", test = "<module>", target = "spice.worker._private_helper" },
]
```

```toml
[tool.spice.policy]
reachability_providers = [
  { name = "csharp", run = ["dotnet", "run", "--project", "tools/reachability-csharp"], when = ["src/**/*.cs", "tests/**/*.cs"] },
  { name = "javascript", run = ["node", "tools/reachability-js.mjs"], when = ["web/**/*.js"] },
  { name = "lua", run = ["lua", "tools/reachability.lua"], when = ["game/**/*.lua"] },
]
```

Provider commands write a JSON list to stdout. Each finding is normalized onto
the matching reachability board by its `kind` (`subject` is the fully-qualified
name; for a symbol it splits into module and leaf):

```json
[
  {
    "kind": "module",
    "subject": "Game.DeadScene",
    "path": "src/Game/DeadScene.cs",
    "imported_by": ["tests/Game/DeadSceneTests.cs"]
  },
  {
    "kind": "method",
    "subject": "Game.Enemy.UnusedTick",
    "path": "src/Game/Enemy.cs",
    "imported_by": ["tests/Game/EnemyTests.cs"]
  }
]
```

During pre-commit, matching staged paths are supplied as newline-delimited
relative paths in `SPICE_STAGED_PATHS`. Outside a staged run, providers should
scan their normal repo scope.

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

Bag names are case-folded. Trigger phrases are normalized to lowercase words.
Configured bags merge with built-ins, so a repo can tune existing bags or add
new curated near-universal preferences.

## `[tool.spice.tasks]`

| Key | Default | Meaning |
| --- | --- | --- |
| `stems` | `[]` plus built-ins `task`, `serve`, `agent`, `oops` | Additional public project stems. Stems use lowercase letters, digits, and underscores. `agent` and `oops` are internal and not allocator assignable. |
| `flows` | `{}` | Per-stem phase lists. Approved phases are `todo`, `verify`, `review`, and `oops`; the default public flow is `todo -> review`. |
| `project_min_depth` | `2` | Minimum dotted project depth for public tasks. |
| `project_max_depth` | `3` | Maximum dotted project depth for public tasks. |

Priority aliases are fixed: `critical/high -> H`, `medium -> M`, `low -> L`,
and `none` clears priority. SLA due dates are one day, seven days, and thirty
days for H/M/L.

## `[tool.spice.serve]`

| Key | Default | Meaning |
| --- | --- | --- |
| `brand` | `[project].name` or `spice` | Header and browser-title brand for `spice serve`. |
| `default_lifetime` | `Drive` | Initial serve lane lifetime: `Steer` uses manual filters, `Drive` auto-subscribes to projects the team creates or claims, and `Drain` dissolves the task boundary so all assignable work is visible. |
