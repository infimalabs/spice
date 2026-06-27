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
| `repo_truth_docs` | `["AGENTS.md"]` | Doctrine docs capped at `5000` characters because they ride in agent context. |
| `env_name_patterns` | `SPICE_*`, `CODEX_THREAD_ID`, `CLAUDE_CODE_SESSION_ID` | Additional environment-variable literal patterns requiring `env-policy: allow` waivers. |
| `env_presence_gate` | `true` | Presence reverse-gate (on by default): every `os.environ` / `os.getenv` access site (not just watchlisted name literals) must carry an `env-policy: allow` waiver, so the audit covers env reads under any or dynamic names. Set `false` to opt out. |
| `pre_commit` | `[]` | Extra command steps run after built-ins. Entries are mounted command names or command tables. |
| `pre_commit_success` | `[]` | Command steps run only after the full gate passes. |
| `pre_commit_builtins` | built-ins enabled | Per-built-in overrides for `repo-shape`, `staging`, `repo-docs`, `formatters`, `local-paths`, `serve-web-typecheck`, `python-typecheck`, `env-policy`, `file-shape`, `complexity`, and `magic-numbers`. |

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
