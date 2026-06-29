# Configuration

Tracked project configuration lives under `[tool.spice.*]` in `pyproject.toml`.
Worktree-local operator preferences, such as speech voice, judge binary, and
local agent overrides, live in `.spice/config/state.json` through
`spice config`; they are not tracked project knobs.

For the full key-by-key reference, see
[docs/config/reference.md](docs/config/reference.md).

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

Project-wide agent launch defaults: driver, model, effort, and selected wrapper
groups. Worktree config and explicit launch flags still win. Agent personality
is worktree-local, not a tracked key.

Reference: [agent table](docs/config/reference.md#toolspiceagent).

## `[tool.spice.wrappers.<group>]`

Wrapper groups define shell functions for agent-owned commands. Select groups
with `[tool.spice.agent] wrappers = [...]`. The built-in `common` group is
intentionally empty; RTK rewrite routing happens inside `spice agent run`, not
through a per-command wrapper.

Reference: [wrapper groups](docs/config/reference.md#toolspicewrappersgroup).

## `[tool.spice.commands]`

Mounted commands put repo tooling under the `spice` namespace without letting
repo tools shadow built-in verbs. Values are command strings or argv lists, and
remaining CLI arguments pass through verbatim.

Reference: [mounted commands](docs/config/reference.md#toolspicecommands).

## `[tool.spice.policy]`

The policy table extends the constitution. It names package roots, test roots,
generated/excluded paths, env policy, reachability providers, assertion helpers,
private-internal exceptions, typecheck interpreter selection, and extra
pre-commit steps. Defaults come from `spice/policy.py`; bad config fails
loudly.

Reference: [policy table](docs/config/reference.md#toolspicepolicy).

## `[tool.spice.policy.pre_commit_builtins]`

Per-built-in overrides for hook steps. A key can keep the default, disable the
step, replace it with a mounted command, or replace it with a command-step
table.

Reference:
[pre-commit built-ins](docs/config/reference.md#toolspicepolicypre_commit_builtins).

## `[tool.spice.maxims.<bag>]`

Maxim bags extend or replace the live prose conscience. Trigger words are
normalized lowercase alphabetic phrases; messages are sent to the judge and, on
violation, back to the agent as steering.

Reference: [maxim bags](docs/config/reference.md#toolspicemaximsbag).

## `[tool.spice.tasks]`

Task config adds public project stems, per-stem phase flows, and public project
depth bounds. Built-in priority aliases and SLA due dates are fixed.

Reference: [task config](docs/config/reference.md#toolspicetasks).

## `[tool.spice.serve]`

Serve config controls the browser header/title brand and default lane lifetime.

Reference: [serve config](docs/config/reference.md#toolspiceserve).
