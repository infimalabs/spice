# Configuration

Tracked project configuration lives under `[tool.spice.*]` in `pyproject.toml`.
Worktree-local operator preferences, such as speech voice, judge binary, and
local agent overrides, live in
`<worktree-git-dir>/.spice/config/spice.toml` through `spice config`, where
`<worktree-git-dir>` is reported by `git rev-parse --git-dir`; they are not
tracked project knobs.

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

The agent shell can use [RTK](https://github.com/rtk-ai/rtk) `0.42.4` or newer
to compact command output. Install it to enable that optimization:

```sh
brew install rtk
rtk --version
rtk rewrite -- git status
```

The `rtk.executable` leaf participates in the standard four-layer precedence.
Tracked pyproject configuration uses:

```toml
[tool.spice.rtk]
executable = "rtk"
```

System, repository, and worktree `spice.toml` files use the plain `[rtk]`
table. The value is one trusted executable basename or absolute path, not a
shell command. Resolution performs no availability lookup; activation, Doctor,
and `agent run` invoke the exact winning identity so missing executables become
observable health or native-fallback outcomes rather than a different resolver
silently winning.

`spice agent run` passes the command after `--`. Exit `0` or Exit `3` with
non-empty stdout rewrites; Exit `1` with empty stdout leaves it unmatched.
Launch failure, malformed output, and every other exit/stdout combination emit
a bounded diagnostic and preserve the original native command. RTK owns
command-selection policy, not command permission. Spice owns the finite
`common` command-shape layer and the agent-scoped
`<worktree-git-dir>/.spice/agents/<thread>/rtk/history.db` supplied through
`RTK_DB_PATH`.
Activation and Doctor report missing, obsolete, or protocol-invalid RTK as
native-command mode without blocking agent setup, and report the same mode when
a rewrite counts a different number of matches than the written search. Cargo installation and the
complete protocol live in the
[wrapper contract](docs/cli/wrapper-commands.md#rtk-rewrite-protocol).

RTK owns rewrite selection and emits the canonical `rtk` frontend in matched
output. Spice remaps only that frontend to the configured executable, then the
built-in `common` wrapper may apply its finite semantic routes; neither layer is
another selector. Spice also chooses the thread-scoped `RTK_DB_PATH` location,
while RTK owns the history database contents. Activation `rtk_status`, the
Doctor row, and bounded repeat-suppressed stderr diagnostics are health
telemetry; they never authorize a rewrite or replace the native command.

## Worktree Speech

Speech is operator-local through `spice config say`; macOS defaults to `say`.
The Linux [`espeak-ng` preset](docs/config/reference.md#linux-speech-with-espeak-ng)
reads text from stdin and returns browser-playable WAV on stdout.

## Maxim Judge Binary

Maxim adjudication is off by default: a matched trigger bag publishes its
`[MAXIM]` reminder judge-free, accepting more false positives and consulting no
judge subprocess. Opt into local YES/NO adjudication per worktree with:

```console
spice config judge --enable
spice config judge --disable
```

When adjudication is enabled, configure the maxim judge executable in the
default worktree scope (or select another configuration layer with `--scope`):

```console
spice config judge --bin /path/to/judge
```

Spice launches it without arguments, sends a prompt on stdin, and requires an
exit-`0` plain-text `YES` or `NO` on stdout. The default is platform-keyed:
`afm-cli` on macOS and the portable `spice-judge` adapter elsewhere. The judge
is consulted only when adjudication is enabled; `--bin` configures which
executable that opt-in path launches, while `--enable` and `--disable` remain
intentionally worktree-local. The prompt schema, portable adapter, retries,
exits, and supervisor degradation are specified in the
[judge reference](docs/config/reference.md#maxim-judge-binary).

## `[tool.spice.agent]`

Project-wide agent launch defaults: driver, model, effort, and selected wrapper
groups. Worktree config and explicit launch flags still win. Agent personality
is worktree-local, not a tracked key.

Reference: [agent table](docs/config/reference.md#toolspiceagent).

## `[tool.spice.wrappers.<group>]`

Wrapper groups define shell functions for agent-owned commands. Select groups
with `[tool.spice.agent] wrappers = [...]`. The built-in `common` group contains
the finite RTK command-shape transformations described above. A selected direct
wrapper whose argv head is not RTK owns its command word regardless of
configuration source, so RTK cannot replace it before the shell function
exists; `rtk rewrite` inside `spice agent run` remains the sole optimization
candidate selector.

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
normalized lowercase alphabetic phrases; a match publishes the bag's message
back to the agent as steering. By default that publish is judge-free; when
adjudication is enabled the judge first decides whether the sampled text
violates the maxim.

Reference: [maxim bags](docs/config/reference.md#toolspicemaximsbag).

## `[tool.spice.tasks]`

Task config adds public project stems, per-stem phase flows, and public project
depth bounds. Built-in priority aliases and SLA due dates are fixed.

Reference: [task config](docs/config/reference.md#toolspicetasks).

## `[tool.spice.serve]`

Serve config controls the browser header/title brand and default lane lifetime.

Reference: [serve config](docs/config/reference.md#toolspiceserve).
