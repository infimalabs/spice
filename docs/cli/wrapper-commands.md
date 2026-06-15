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

Agent launch installs shell startup hooks for zsh and bash. Non-interactive
shell commands are reexecuted through:

```sh
spice agent run -- <shell> -c "<original command>"
```

Agents normally run shell commands directly; the startup hooks perform this
reexec. Use `spice agent run -- <command>` explicitly only when recovering a
command path or inspecting wrapper behavior.

The wrapper does this before running the requested command:

- prints pending operator steering and context-pressure notices on stderr;
- preserves ACK semantics by leaving inbox retirement to transcript ACK lines;
- routes git through the worktree shadow environment;
- routes `spice` and `python` commands to the correct worktree source checkout
  or target repository virtual environment;
- injects configured shell wrapper functions.

`spice agent run -- proxy <command>` is only a routing marker. It still goes
through `agent run`; the marker lets configured shell wrapper functions choose
the proxy route before the underlying command is executed.

## Wrapper Groups

Wrapper functions are generated from `[tool.spice.wrappers.<group>]` tables.
The selected groups come from `[tool.spice.agent] wrappers = [...]`. When no
list is configured, spice selects the built-in `common` group. An explicit empty
list disables wrapper generation.

The built-in `common` group maps `rtk` to these selectors:

```toml
[tool.spice.wrappers.common]
rtk = ["run", "proxy", "grep", "find", "git"]
```

Selectors are command names, not paths. Path selectors such as `/bin/sh` fail
loudly until a redirector stage exists. A wrapper cannot intercept itself, and
duplicate selectors fail during hook rendering.

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
