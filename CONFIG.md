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

## Worktree Speech

Speech playback is an operator-local preference configured through
`spice config say`. macOS uses `say` by default. Linux operators can use the
documented [`espeak-ng` preset](docs/config/reference.md#linux-speech-with-espeak-ng),
which reads speech text from stdin and returns browser-playable WAV audio on
stdout.

## Maxim Judge Binary

The maxim judge is a worktree-local executable configured with:

```console
spice config judge --bin /path/to/judge
```

This stores the logical `[judge].bin` value in `.spice/config/state.json`.
`bin` is one executable path or `PATH` name, not a shell command or argv list.

When `bin` is unset, spice resolves a built-in default keyed to the platform:
macOS uses the Apple Foundation Models `afm-cli` binary; every other platform,
where `afm-cli` does not exist, uses the portable `spice-judge` adapter that
ships with spice. An explicit `bin` overrides this default on every platform,
and `spice doctor` reports the resolved judge for the current platform.

### Portable judge with `spice-judge`

`spice-judge` is spice's own console script and conforms to the contract above:
it is launched as the exact argv `[spice-judge]`, reads the prompt on stdin, and
writes `YES`/`NO` to stdout. It delegates the judgement to a portable local
model command, obtainable off macOS. The default command runs a small local
model through [Ollama](https://ollama.com); install it and pull the model once:

```console
ollama pull llama3.2
```

`SPICE_JUDGE_MODEL_CMD` overrides the default with any argv that reads a prompt
on stdin and writes an answer to stdout (for example
`SPICE_JUDGE_MODEL_CMD="ollama run mistral"`). `SPICE_JUDGE_TIMEOUT` sets the
per-verdict deadline in seconds (default 60; a non-positive value disables it).

There is no silent no-op: when the model command is absent, exits non-zero, or
exceeds its deadline, `spice-judge` exits non-zero with an actionable message on
stderr, which spice surfaces as its judge error detail. Bring your own judge by
setting `[judge].bin` to any conforming executable instead.

For each verdict, spice launches the exact argv `[configured_bin]`: there are no
command-line arguments. The judge receives one prompt on stdin and must write
its verdict to stdout. The default prompt contains these four lines in a random
order on every attempt:

```text
IFF "{maxim}" AGREES WITH "{statement}": ANSWER ONLY "YES".
IFF "{maxim}" DISAGREES WITH "{statement}": ANSWER ONLY "NO".
IFF "{statement}" AGREES WITH "{maxim}": ANSWER ONLY "YES".
IFF "{statement}" DISAGREES WITH "{maxim}": ANSWER ONLY "NO".
```

Before interpolation, spice collapses whitespace in `maxim` and `statement`
and strips trailing punctuation and whitespace. The `--prompt-file` option on
`spice maxim agree` or `spice maxim disagree` replaces the default template;
only `{maxim}` and `{statement}` fields are accepted.

The output schema is plain text, not JSON. Spice uppercases stdout, removes
characters other than `Y`, `E`, `S`, `N`, `O`, and spaces, and accepts the
result only when its deduplicated token set is exactly `{"YES"}` or `{"NO"}`.
`YES` means the statement agrees with the maxim; `NO` means it disagrees. An
ambiguous reply is retried, with two attempts by default. If both replies are
ambiguous, judging fails.

The judge process must exit `0`. A launch failure or nonzero exit is an
immediate error; stderr is included in the error detail for a nonzero exit.
Spice does not currently impose a subprocess timeout, so a conforming wrapper
should enforce its own deadline if its model can hang. Direct
`spice maxim agree` and `spice maxim disagree` calls return `0` when their
requested condition is met, `1` when it is unmet, and `2` for judge or prompt
errors.

During supervised agent operation, judge errors are caught at the conscience
boundary and logged as `spice maxim supervisor error`; transcript capture,
steering, tasks, and other supervision continue, but that maxim feedback is
skipped. Learning distillation likewise records a judge failure as a skipped
candidate instead of stopping the session.

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
