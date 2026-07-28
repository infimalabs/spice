# Configuration Layers and Routing

This companion to the [configuration reference](reference.md) documents
applicability selectors, mutation behavior, migrations, supervised Claude tool
boundaries, and wrapper routing.

## Universal applicability selectors

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
three configuration-layer names remain precedence metadata. None of those
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

## Configuration mutation and migration

Every source uses the same bare table shape:

```toml
[agent]
model = "gpt-5.5"
```

`spice config show` prints all three parsed layers, their source paths, the
effective mapping, and the winning source for every key as deterministic JSON.
`spice config system` prints effective agent values and their provenance from
the same layered view. Mutable commands default to `--scope worktree`; agent,
personality, say, and judge settings also accept `system` and `repository`.
`--clear` removes only that command's values from the selected
scope, revealing the next earlier layer without changing it. A mutation with
`--scope system` previews the exact installed package path and change; pass
`--apply` to write it. The preview warns that reinstalling Spice replaces this
installed file and loses the change.

`spice config set <dotted-key> <value> [--scope <scope>]` exposes every leaf in
the loader schema, including settings without a specialized convenience flag.
Bare text is a string; `true`, `false`, numbers, TOML arrays, and inline tables
retain their types. Quote the whole shell argument when a typed value contains
spaces, and quote an individual TOML key segment when its name contains a dot:

```console
spice config set say.timeout_seconds 180
spice config set policy.repo_truth.docs '["AGENTS.md", "CONTRIBUTING.md"]' --scope repository
spice config set 'tasks.taskwarrior_urgency."age.coefficient"' 2.5 --scope repository
spice config set commands.audit false
```

The last form exercises the registry-removal contract in the main reference.
The setter prints the selected value, the effective value, and its winning scope
and path; `spice config show` exposes the stored leaf and provenance even though
the registry consumer omits that entry.

All mutable commands use one structured TOML editor. It preserves unrelated
tables, comments, ordering, and value types, validates the complete prospective
layer through the same key schema as the loader, and atomically replaces the
selected file. A system write requires the installed `spice.toml` to exist and
be writable as well as explicit `--apply`; the other two scopes are created on
demand and continue to apply directly. Invalid or unwritable mutations report
`scope=<name> path=<path>` before changing bytes.

Spice v0.30 dropped `[tool.spice]` repository configuration. A repository that
still contains that table refuses configuration loading and names root
`spice.toml` as its replacement. Migrate once by moving the contents of
`[tool.spice]` into root `spice.toml` and removing the `tool.spice` prefix; for
example, `[tool.spice.policy]` becomes `[policy]`. Runtime code never merges,
unwraps, or assigns precedence to both shapes.

Runtime code also does not read or import `.spice/config/state.json`; an old
file is ignored. Move any values that still matter into
the worktree scope using `spice config`, then delete the JSON file. There is no
compatibility scope name or JSON adapter. v0.30 also moved the plain worktree
file from `<repository>/.spice/config/spice.toml` to
`<worktree-git-dir>/.spice/config/spice.toml`: an untracked predecessor is
migrated once, while a tracked, repeated, or competing predecessor is refused
and never honored.

## Supervised Claude tool boundary

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

## `[wrappers.<group>]`

Wrapper groups define shell functions for agent-owned commands. Select groups
with `[agent] wrappers = [...]`.

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
Naming `common` in a repo `[wrappers.common]` table replaces the whole
group atomically — routes do not concatenate, so an override must re-list every
route it keeps — while omitting the table inherits this default and
`wrappers = []` disables generation. Repo groups should otherwise wrap stable
repo-owned tools (see [wrapper commands](../cli/wrapper-commands.md)).
