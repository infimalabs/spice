# Wrapper And Mounted Commands

Status: implemented contract.

Spice has two command-extension surfaces with different owners:

- `spice agent run -- <cmd>` is the agent shell wrapper. It is how agent-run
  shell commands receive steering, context pressure, git-shadow routing, source
  checkout routing, and configured wrapper functions before the requested
  command executes.
- `[tool.spice.commands]` mounted commands are repository-owned top-level
  verbs. They let a project expose its own tools under `spice <verb>` without
  making those tools built-ins for every repository.

## Agent Command Wrapper

Agent launch installs static shell startup hooks for zsh and bash and
precomputes configured wrapper functions into `SPICE_SHELL_HOOK_WRAPPERS`. The
first non-interactive command shell with an execution string sees
`SPICE_SHELL_HOOK_REEXEC_STAGE` unset, sets it, and reexecs through:

```sh
spice agent run -- <shell> -c "<original command>"
```

Agents normally run shell commands directly; the startup hooks perform this
reexec. Descendant shells inherit `SPICE_SHELL_HOOK_REEXEC_STAGE=1` and perform
stage-2 startup only: source the user's real startup files, rearm the packaged
hook environment, and eval `SPICE_SHELL_HOOK_WRAPPERS` without a second
`agent run` hop or second steering injection. Use
`spice agent run -- <command>` explicitly only when recovering a command path or
inspecting wrapper behavior.

The wrapper does this before running the requested command:

- prints pending operator steering and context-pressure notices on stderr;
- preserves ACK semantics by leaving inbox retirement to transcript ACK lines;
- routes git through the worktree shadow environment;
- routes `spice` and `python` commands to the correct worktree source checkout
  or target repository virtual environment;
- makes configured shell wrapper functions available.

`spice agent run -- proxy <command>` is only a routing marker. It still goes
through `agent run`; the marker lets configured shell wrapper functions choose
the proxy route before the underlying command is executed.

## Wrapper Groups

Wrapper functions are generated from `[tool.spice.wrappers.<group>]` tables.
The selected groups come from `[tool.spice.agent] wrappers = [...]`. When no
list is configured, spice selects the built-in `common` group. An explicit empty
list disables wrapper generation.

The built-in `common` group maps `rtk` to a broad set of shell-function-safe
command selectors that benefit from RTK routing, including common tools such as
`git`, `grep`, `gh`, `npm`, `pytest`, `ruff`, `docker`, and `kubectl`. Repos
that need exact control can override the group:

```toml
[tool.spice.wrappers.common]
rtk = ["run", "proxy", "grep", "find", "git"]
```

Selectors are command names, not paths. Path selectors such as `/bin/sh` fail
loudly until a redirector stage exists. A wrapper cannot intercept itself, and
duplicate selectors fail during wrapper generation.

Wrapper entries may also be direct command wrappers with a `command = [...]`
argv list; spice shell-quotes each command word while building
`SPICE_SHELL_HOOK_WRAPPERS`. A command word in `$NAME` form is rendered as a
quoted shell variable reference for hook-provided values such as
`$SPICE_SHELL_HOOK_PYTHON`.
For example, a repository can opt into a local pytest wrapper without changing
the global default:

```toml
[tool.spice.wrappers.common]
rtk = ["run", "proxy", "grep", "find", "git"]
pytest = { command = ["$SPICE_SHELL_HOOK_PYTHON", "-m", "pytest"] }
```

## Mounted Commands

Repositories declare mounted commands in tracked `pyproject.toml`:

```toml
[tool.spice.commands]
release = ["uv", "run", "python", "-m", "spice.release"]
```

`spice release notes` runs the mounted command from the repository root with
`notes` passed through verbatim. String mounts are shell-split once; list mounts
pass their argv exactly.

Mounted names are one-level verbs matching `^[a-z][a-z0-9-]*$`. Built-in spice
verbs always win, and a mount that shadows a built-in fails loudly. Large tool
families should mount one namespace owner, then dispatch inside that tool:

```toml
[tool.spice.commands]
toolbox = ["uv", "run", "toolbox"]
```

`spice toolbox lint css --fix` then passes `lint css --fix` to `toolbox`.

Mounted commands can import the public repo-tool seam documented in the README.
They should not rely on private spice modules unless the seam is deliberately
expanded with tests and documentation.

## Choosing A Surface

Use `spice agent run -- <cmd>` for agent-owned execution where steering,
context notices, worktree routing, and wrapper functions must apply.

Use a mounted command for repository-owned tools that operators or hooks should
run as `spice <verb>` in that repository only. Release tooling is mounted in
this repository for that reason: other repositories can mount their own release
implementation without competing with a global spice built-in.
