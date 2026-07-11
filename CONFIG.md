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
source checkout, import path, or virtualenv for the running code.

## RTK Rewrite Companion

The agent shell requires [RTK](https://github.com/rtk-ai/rtk) `0.42.4` or
newer. Install it before starting agents:

```sh
brew install rtk
rtk --version
rtk rewrite -- git status
```

`spice agent run` passes the command after `--`. Exit `3` with non-empty stdout
rewrites; Exit `1` with empty stdout leaves it unmatched. Every other
exit/stdout combination errors. Upstream RTK uses Exit `0` for an auto-allowed
rewrite; Spice deliberately rejects it to preserve the agent permission
boundary. RTK owns command-selection policy. Spice owns the finite `common`
command-shape layer and the agent-scoped
`.git/spice/agents/<thread>/rtk/history.db` supplied through `RTK_DB_PATH`.
Missing or protocol-invalid RTK stops the agent path. Cargo installation and
the complete protocol live in the
[wrapper contract](docs/cli/wrapper-commands.md#rtk-rewrite-protocol).

## Worktree Speech

Speech is operator-local through `spice config say`; macOS defaults to `say`.
The Linux [`espeak-ng` preset](docs/config/reference.md#linux-speech-with-espeak-ng)
reads text from stdin and returns browser-playable WAV on stdout.

## Maxim Judge Binary

The maxim judge is a worktree-local executable:

```console
spice config judge --bin /path/to/judge
```

Spice launches it without arguments, sends a prompt on stdin, and requires an
exit-`0` plain-text `YES` or `NO` on stdout. The default is `afm-cli`; prompt
schema, retries, exits, and supervisor degradation are specified in the
[judge reference](docs/config/reference.md#maxim-judge-binary).

## `[tool.spice.agent]`

Project-wide agent launch defaults: driver, model, effort, and selected wrapper
groups. Worktree config and explicit launch flags still win. Agent personality
is worktree-local, not a tracked key.

Reference: [agent table](docs/config/reference.md#toolspiceagent).

## `[tool.spice.wrappers.<group>]`

Wrapper groups define shell functions for agent-owned commands. Select groups
with `[tool.spice.agent] wrappers = [...]`. The built-in `common` group contains
the finite RTK command-shape transformations described above; `rtk rewrite`
inside `spice agent run` remains the sole command selector.

Reference: [wrapper groups](docs/config/reference.md#toolspicewrappersgroup).

## `[tool.spice.commands]`

Mounted commands put repo tooling under the `spice` namespace without letting
repo tools shadow built-in or extension-provided actions at any depth. Dotted
mounts may extend built-in verbs with novel action names. Values are command
strings or argv lists, and remaining CLI arguments pass through verbatim.

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
