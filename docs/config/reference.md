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

| Key | Default | Enforced opinion |
| --- | --- | --- |
| `package_roots` | packaging metadata, else `[]` | Python namespace-package roots. When set or derived, `__init__.py` is forbidden under those roots and path names must match `^_*[0-9a-z]+_*$`. |
| `name_cluster_threshold` | `4` | Number of sibling modules sharing a long alphabetic prefix or suffix before the name-cluster guard requires a namespace package. Configured values must be at least `3`. |
| `exclude` | `[]` | Tracked paths or globs excluded from study walkers, useful for committed generated sources. Built-in exclusions already cover `.git`, `.spice`, caches, venvs, and `node_modules`. |
| `generated_paths` | `[]` | Tracked paths or globs (e.g. `**/*_pb2.py`, or a directory like `pkg/proto`) exempt from every `repo-shape` guard: naming law, path shape, generic-split names, and namespace `__init__.py`. Unlike `exclude`, this reaches shape guards, not only study walking. |
| `test_paths` | pytest `testpaths`, else `tests/` | Tracked test-root override for studies that need to distinguish test code from production code. Entries are repo-relative dirs or globs, and may declare multiple roots such as `["tests", "Assets/**/Tests"]`. |
| `repo_truth_docs` | `["AGENTS.md"]` | Explicit doctrine docs checked by the repo-doc guard because they ride in agent context. |
| `env_name_patterns` | `SPICE_*`, `CODEX_THREAD_ID`, `CLAUDE_CODE_SESSION_ID` | Additional environment-variable literal patterns requiring `env-policy: allow` waivers. |
| `env_names` | `[]` | Exact tracked manifest for `spice study env-name-ledger`: every unique literal env-var name referenced by supported env access forms must appear here, and every name here must still be referenced. |
| `env_access_gate` | `true` | Access gate: every env access site, not only watchlisted name literals, must carry an `env-policy: allow` waiver. Existing findings can be seeded into `[tool.spice.policy.env_access] baseline` for ratcheted adoption; set `false` only to opt out wholesale. |
| `reachability_providers` | `[]` | Extra language-aware dead-code providers for `spice study reachability` and `gate:reachability`. |
| `python_typecheck_interpreter` | auto | Optional Python interpreter path for `python-typecheck` in non-standard layouts. Relative paths resolve from the repo root. When omitted, spice resolves repo-local `VIRTUAL_ENV`, `.venv`, then uv project interpreter. |
| `assertion_helpers` | `[]` | Callable names that count as assertions when called inside Python tests. Leaf names match any final attribute; dotted names match exact dotted calls. |
| `internal_couplings` | `[]` | Exact private-internals exceptions as `{ path, test, target }` tables. These are named allowlist entries; stale entries fail the gate until removed. |
| `pre_commit` | `[]` | Extra command steps run after built-ins. Entries are mounted command names or command tables. |
| `pre_commit_success` | `[]` | Command steps run only after the full gate passes. |
| `pre_commit_builtins` | built-ins enabled | Per-built-in overrides for `repo-shape`, `staging`, `repo-docs`, `formatters`, `local-paths`, `taste`, `serve-web-typecheck`, `python-typecheck`, `env-policy`, `env-name-ledger`, `file-shape`, `complexity`, `magic-numbers`, `reachability`, `symbol-reachability`, `assertion-free-tests`, and `private-internals`. |
| `limits` | built-ins | Base numeric bounds for files, routines, commit text, and repo-truth docs. |
| `flex` | `ratio = 1.5` | Temporary headroom before sticky base enforcement for shape-style bounds. |
| `complexity` | built-ins | Routine-complexity study display defaults. |
| `taste` | built-ins | Gate-only prose taste word suggestions used by the `taste` pre-commit built-in. |
| `magic` | built-ins | Magic-number threshold and staged baseline ref. |
| `debt` | `0` counters | Allowed cleanup-debt counts for quality gates. |
| `commit_message` | built-ins | Commit trailer allowlist policy. |
| `languages` | built-ins | Suffix families scanned by grammar-aware studies. |
| `lockfiles` | built-ins | Generated lockfiles exempt from file-shape pressure. |
| `file_shape` | built-ins | Source suffixes scanned by file-shape pressure and generated source patterns exempt from it. |
| `env_access` | built-ins | Env-access matcher families and regex patterns. |
| `markdown_depth_budget` | `[".md"]` | Generated default markdown doc-character scopes. |
| `scopes` | `{}` | Per-path overrides for numeric bounds and scoped magic thresholds. |

Shell env-access patterns intentionally cover name-like parameters, not shell
special or positional parameters such as `$?`, `$$`, `$1`, `$@`, `$*`, `$#`,
`$-`, or `$_`.

### `[tool.spice.policy.languages]`

| Key | Default | Enforced opinion |
| --- | --- | --- |
| `complexity` | C-family, Lua, PHP, Python, Ruby | File suffixes scanned by routine complexity. |
| `magic` | Python plus C-family | File suffixes considered by magic-number scans. |
| `env` | Complexity suffixes plus shell | File suffixes scanned by env-policy and env-name-ledger. |
| `c_grammar` | C-family | File suffixes whose magic-number scan uses C-style comments and comparisons. |

### `[tool.spice.policy.lockfiles]`

| Key | Default | Enforced opinion |
| --- | --- | --- |
| `suffixes` | `[".lock"]` | Generated lockfile suffixes exempt from file-shape pressure. |
| `names` | `["bun.lockb", "package-lock.json", "pnpm-lock.yaml"]` | Generated lockfile names exempt from file-shape pressure. |

### `[tool.spice.policy.file_shape]`

| Key | Default | Enforced opinion |
| --- | --- | --- |
| `source_suffixes` | Built-in source/text suffix set | File suffixes eligible for file LOC/byte pressure. Non-text files, generated lockfiles, generated source patterns, excluded paths, and markdown governed by repo-doc budgets are skipped. |
| `generated_patterns` | Built-in generated source globs | Repo-relative globs exempt from file-shape pressure, such as generated protobuf modules, minified web bundles, and build output directories. |

### `[tool.spice.policy.env_access]`

| Key | Default | Enforced opinion |
| --- | --- | --- |
| `family_suffixes` | Built-in Python, C#, Lua, shell, JavaScript/TypeScript suffix families | Table mapping env-access language family names to file suffixes. |
| `default_patterns` | Built-in env-access idioms per family | Table mapping env-access language family names to regexes. A custom family in `default_patterns` must also appear in `family_suffixes`. |
| `baseline` | unset | Repo-relative JSON baseline of existing `env-policy` findings to grandfather while new unwaived findings still fail. Seed a tracked path with `spice study env-policy --write-baseline tools/spice/env-policy-baseline.json`, then set `baseline = "tools/spice/env-policy-baseline.json"`. |

`env-name-ledger` accounts only for literal names it can extract from supported
env access forms, watchlisted env-name patterns, or exact manifest names still
present as literals in scanned sources. Dynamic/non-literal access sites such as
`os.environ[name]` have no extractable exact name; they remain in the access
gate's domain.

The ledger scans tests on the same footing as production. A test that
references an env name must therefore use either a real env name it
meaningfully overwrites or asserts on, or an obviously-fake name that no real
system uses.

Policy constants enforced by default: files `1000` LOC / `80000` bytes with
`1.5x` flex, routines CCN `20` / length `80`, commit text wrap `100`,
repo-root markdown `5000` chars plus `5000` per nested directory until
`15000`, magic-number threshold `10`, and magic baselines against `HEAD`.

### `[tool.spice.policy.limits]`

| Key | Default | Enforced opinion |
| --- | --- | --- |
| `file_loc` | `1000` | Base physical-line cap for file-shape pressure. |
| `file_bytes` | `80000` | Base byte cap for file-shape pressure. |
| `routine_ccn` | `20` | Base cyclomatic-complexity cap per routine. |
| `routine_length` | `80` | Base non-comment routine length cap. |
| `commit_message_wrap` | `100` | Subject/wrapped prose width for commit messages. |
| `repo_truth_doc_chars` | `5000` | Base character cap used by repo-truth docs unless a scope, including generated markdown-depth scopes, replaces it. |

### `[tool.spice.policy.flex]`

| Key | Default | Enforced opinion |
| --- | --- | --- |
| `ratio` | `1.5` | Default flex multiplier. A file/routine/doc that breaches flex becomes sticky and must shrink under the base cap. |
| `file_loc` | `1500` | Explicit line flex cap; must be at least `limits.file_loc`. |
| `file_bytes` | `120000` | Explicit byte flex cap; must be at least `limits.file_bytes`. |
| `routine_ccn` | `30` | Explicit CCN flex cap; must be at least `limits.routine_ccn`. |
| `routine_length` | `120` | Explicit routine-length flex cap; must be at least `limits.routine_length`. |

### `[tool.spice.policy.complexity]`

| Key | Default | Meaning |
| --- | --- | --- |
| `hotspot_limit` | `20` | Default number of rows shown by `spice study complexity-hotspots` when `--limit` is omitted. |

### `[tool.spice.policy.taste.words]`

The `taste` guard is a gate-only pre-commit built-in, not a public
`spice study` or `spice dev doctor` surface. It scans tracked prose files
(`.md`, `.txt`, and `.rst`) selected for the gate. Keys are whole-word trigger
phrases; values are preferred replacements. An empty value means remove or
rephrase the word.

Configured words merge over the built-in suggestions:

```toml
[tool.spice.policy.taste.words]
verbose = "terse"
filler = ""
```

### `[tool.spice.policy.markdown_depth_budget]`

Tracked markdown is checked by default through generated
`repo_truth_doc_chars` scopes:

| Depth | Budget |
| --- | --- |
| repo root | `5000` chars |
| one nested directory | `10000` chars |
| two nested directories | `15000` chars |
| deeper than two nested directories | unlimited |

```toml
[tool.spice.policy.markdown_depth_budget]
extensions = [".md"]
stem_pattern = "README|[A-Z_]+"
```

`extensions` defaults to `[".md"]`; set it to `[]` to remove the generated
markdown scopes and replace them with explicit `[tool.spice.policy.scopes]`
entries. `stem_pattern` is optional and full-matches the file stem only after
the suffix is in the doc-extension set. Single-letter stems are never selected
by the generated markdown scopes. Binary files are skipped by the repo-doc
guard.

### `[tool.spice.policy.debt]`

Debt counters are allowed-finding counts, not hard size or sensitivity limits.
The default `0` means the gate is clean and any finding fails. A non-zero value
records explicit, drainable cleanup debt that should be lowered as findings are
removed.

| Key | Default | Meaning |
| --- | --- | --- |
| `reachability_test_only` | `0` | Allowed test-only reachability findings before the reachability gate fails. |
| `assertion_free_tests` | `0` | Allowed assertion-free test findings before the assertion-free-tests gate fails. |

### `[tool.spice.policy.scopes."<matcher>"]`

Scopes adjust numeric policy bounds for matching repo-relative paths. The table
key is the matcher. Glob keys such as `**/*.cs` work as per-language knobs;
non-glob keys match that path or subtree. Generated markdown-depth scopes are
lower priority than tracked scopes, so a matching explicit
`repo_truth_doc_chars` scope replaces the generated markdown budget for that
path.

```toml
[tool.spice.policy.scopes."docs/**"]
multiplier = 2.0
flex = 1.25

[tool.spice.policy.scopes."src/legacy/**".routine_ccn]
multiplier = 1.5
max = 40

[tool.spice.policy.scopes."generated/**"]
unlimited = true
```

Flat scope keys apply to every numeric bound. Named sub-tables target one bound:
`file_loc`, `file_bytes`, `routine_ccn`, `routine_length`,
`commit_message_wrap`, or `repo_truth_doc_chars`. Each scope setting accepts
`multiplier` (default `1.0`), optional `min`/`max` clamps, `unlimited = true`,
and optional `flex` ratio. The effective base is
`clamp(global_base * multiplier, min, max)`; flex is derived from the scope
ratio when present, otherwise from the global flex ratio.

Scopes may also include `[tool.spice.policy.scopes."<matcher>".magic]` with
`examine_threshold` to override the magic-number threshold for matching paths.

Overlapping scopes are resolved by most-specific match, not TOML table order.
Exact or prefix matchers outrank globs, then the matcher with more literal path
text wins; ties use the matcher text for deterministic results.

### `[tool.spice.policy.magic]`

| Key | Default | Meaning |
| --- | --- | --- |
| `examine_threshold` | `10` | Absolute numeric magnitude at or above which an unnamed comparison/default/slice literal is a magic-number candidate. |
| `baseline_ref` | `HEAD` | Git ref used by the staged gate to decide whether a candidate literal is new debt. |

### `[tool.spice.policy.commit_message]`

| Key | Default | Meaning |
| --- | --- | --- |
| `allowed_trailers` | any trailer except `Co-Authored-By` | Optional finite set of allowed Git trailer keys. Values are normalized to lowercase; `Co-Authored-By` is always rejected. |

Command-step tables accept:

| Key | Default | Meaning |
| --- | --- | --- |
| `label` | mounted command name or built-in label | Human label for gate output. |
| `mount` | none | Name from `[tool.spice.commands]`. |
| `run` / `argv` | required for raw command tables | Command string or argv list. |
| `when` | all staged paths | Non-empty glob list matched against staged paths. |
| `formatter` | `false` | `true` means restage matching paths after the command succeeds. |
| `enabled` | `true` | For `pre_commit_builtins` only, `false` disables that built-in. |

Every `pre_commit` command step gets `SPICE_STAGED_PATHS` with the
`when`-narrowed staged paths. A step that names a mount additionally carries
`SPICE_MOUNTED_COMMAND=1` and `SPICE_VISIBLE_PROG`.

Reachability provider tables accept:

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | required | Provider name shown on the reachability board. Must not be `python`, which is the built-in AST/import-graph provider. |
| `run` | required | Non-empty argv list executed from the repo root. |
| `when` | always | Optional non-empty glob list matched against staged paths by the pre-commit gate. |

`internal_couplings` entries accept `path`, `test`, and `target`; all are
required non-empty strings. `test` is the test function name or `<module>`, and
`target` is the private production symbol the test imports or reaches.

The same provider seam feeds both reachability gates; a finding's `kind` routes
it to exactly one gate by granularity. `module` is the coarse whole-file gate;
every other kind (`function`, `class`, `method`, ...) is a symbol and rides the
finer `gate:symbol-reachability`.

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

Provider commands write a JSON list to stdout:

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
| `stems` | `[]` plus built-ins `task`, `serve`, `agent` | Additional public project stems. Stems use lowercase letters, digits, and underscores. `agent` is internal and not allocator assignable. |
| `hidden_stems` | `[]` plus built-in `oops` | Additional hidden system project stems. Values omit the leading dot, so `scratch` defines addressable `.scratch` projects. Hidden projects use the private `todo` flow, are reserved for system-created rows, and are excluded from normal boards and lane assignment. |
| `flows` | `{}` | Per-stem phase lists. Approved phases are `study`, `plan`, `todo`, `verify`, and `review`; the default public flow is `todo -> review`. Hidden system projects use the private `todo` flow. |
| `project_min_depth` | `2` | Minimum dotted project depth for public tasks. |
| `project_max_depth` | `3` | Maximum dotted project depth for public tasks. |

Priority aliases are fixed: `critical/high -> H`, `medium -> M`, `low -> L`,
and `none` clears priority. SLA due dates are one day, seven days, and thirty
days for H/M/L.

## `[tool.spice.tasks.phase_models.<driver>.<phase>]`

Per-driver, per-phase agent launch overrides. Each driver has its own model
space, so the table is keyed by driver name (`claude` or `codex`) and then by
task phase (`study`, `plan`, `todo`, `verify`, `review`, `oops`).

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
