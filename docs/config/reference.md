# Configuration Reference

Tracked project configuration lives under `[tool.spice.*]` in `pyproject.toml`.
Worktree-local operator preferences live in `.spice/config/state.json` through
`spice config`.

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
inside `spice agent run` - it is not a per-command wrapper. Repo groups should
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
| `python_typecheck_interpreter` | Optional interpreter for `python-typecheck`; otherwise repo venv/uv is resolved. |
| `assertion_helpers` | Callable names that count as test assertions. |
| `internal_couplings` | Named private-internals allowlist entries `{ path, test, target }`. |
| `pre_commit` | Extra gate steps that run after built-in pre-commit gates. |
| `pre_commit_success` | Success-only steps that run after the gate passes. |
| `pre_commit_builtins` | Per gate-only pre-commit built-in overrides for `repo-shape`, `staging`, `repo-docs`, `formatters`, `local-paths`, `taste`, `serve-web-typecheck`, `python-typecheck`, `env-policy`, `env-name-ledger`, `file-shape`, `complexity`, `magic-numbers`, `markdown-links`, `reachability`, `symbol-reachability`, `assertion-free-tests`, and `private-internals`. |
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

Policy constants enforced by default: files `1000` LOC / `80000` bytes with
`1.5x` flex, routines CCN `20` / length `80`, commit text wrap `100`,
repo-root markdown `5000` chars plus `5000` per nested directory until
`15000`, magic-number threshold `10`, and magic baselines against `HEAD`.

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

Gate-only prose suggestions for tracked `.md`, `.txt`, and `.rst` files. Keys
are whole-word triggers; values are replacements, and an empty value means
remove or rephrase. Configured words merge over built-ins.

### `[tool.spice.policy.markdown_depth_budget]`

Generated `repo_truth_doc_chars` scopes for tracked markdown: repo root gets
`5000` chars, one nested directory `10000`, two nested directories `15000`, and
deeper docs are unlimited. `extensions` defaults to `[".md"]`; set it to `[]`
to replace generated scopes with explicit `[tool.spice.policy.scopes]`.
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
