# Wrapper And Mounted Commands

Status: implemented contract.

Spice has two command-extension surfaces with different owners:

- `spice agent run -- <cmd>` is the agent shell wrapper. It is how agent-run
  shell commands receive steering, keep-working guidance, RTK rewrite routing,
  source checkout routing, and configured wrapper functions
  before the requested command executes.
- `[tool.spice.commands]` mounted commands are repository-owned command paths.
  They let a project expose its own tools under `spice <verb>` or
  `spice <verb> <subcommand> ...` without making those tools built-ins for
  every repository.

## Agent Command Wrapper

Agent launch points `ZDOTDIR`/`BASH_ENV` at the packaged redirector hook dir
and precomputes configured wrapper functions into `SPICE_SHELL_HOOK_WRAPPERS`.
The first non-interactive command shell with an execution string runs the
redirector hook, which clears `ZDOTDIR`/`BASH_ENV` and reexecs through:

```sh
spice agent run -- <shell> -c "<original command>"
```

Agents normally run shell commands directly; the startup hooks perform this
reexec. `agent run` repoints `ZDOTDIR`/`BASH_ENV` at the packaged static hook
dir for the shell command it runs, so that shell and its descendants run the
static stage only: source the user's real startup files, rearm the packaged
hook environment, and eval `SPICE_SHELL_HOOK_WRAPPERS` without a second
`agent run` hop or second steering injection. The redirector and static stages
are distinct packaged hook directories, not an environment marker, so there is
no reexec counter to read. Use `spice agent run -- <command>` explicitly only
when recovering a command path or inspecting wrapper behavior.

The native harness or shell startup hook must hand the complete top-level shell
command string to `spice agent run` exactly once. `agent run` owns RTK rewrite
because it is the only layer that sees the full shell string before execution.

The wrapper does this before running the requested command:

- prints pending operator steering and keep-working guidance on stderr;
- preserves ACK semantics by leaving inbox retirement to transcript ACK lines;
- asks `rtk rewrite` for the rewritten shell command string or direct argv
  replacement when RTK is installed;
- routes git through the worktree shadow environment;
- routes `spice` and `python` commands to the correct worktree source checkout
  or target repository virtual environment;
- makes configured shell wrapper functions available.

## Wrapper Groups

Wrapper functions are generated from `[tool.spice.wrappers.<group>]` tables.
The selected groups come from `[tool.spice.agent] wrappers = [...]`. When no
list is configured, spice selects the built-in `common` group. An explicit empty
list disables wrapper generation.

The built-in `common` group is intentionally empty. RTK command coverage comes
from the `rtk rewrite` handoff inside `spice agent run`, so RTK remains the
single source of truth for which raw commands become `rtk ...` telemetry. Spice
does not carry per-command semantic shims after the rewrite; if RTK maps a
command shape to a non-equivalent `rtk ...` argv, the fix belongs in RTK's
rewrite model.
Repos that need exact shell-function control can override or extend groups:

```toml
[tool.spice.wrappers.common]
wrap = ["grep", "find", "git"]
```

Selectors are command names, not paths. Path selectors such as `/bin/sh` fail
loudly until a redirector stage exists. A wrapper cannot intercept itself, and
duplicate selectors fail during wrapper generation.

Wrapper entries may also be direct argv wrappers with an `argv = [...]` list;
spice shell-quotes each argv word while building
`SPICE_SHELL_HOOK_WRAPPERS`. Prefer stable repository-owned commands over
hook-private environment variables. For example, a repository can opt into a
local code-generation wrapper by selecting its own extension group alongside
`common`, without implying that `codegen` belongs to the generic default:

```toml
[tool.spice.agent]
wrappers = ["common", "repo-tools"]

[tool.spice.wrappers.repo-tools]
codegen = { argv = ["uv", "run", "python", "-m", "tools.codegen"] }
```

The spice checkout itself uses the same local-extension pattern to catch the
common agent habit of running bare `pre-commit`, while leaving the generic
`common` group unchanged:

```toml
[tool.spice.agent]
wrappers = ["common", "spice-dev"]

[tool.spice.wrappers.spice-dev]
pre-commit = { argv = ["spice", "dev", "pre-commit"] }
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

Mounted names are dot-separated segment paths whose segments match
`^[a-z][a-z0-9-]*$`. Top-level mounts that shadow built-in spice verbs fail
loudly. Nested mounts under built-ins are allowed:

```toml
[tool.spice.commands]
toolbox = ["uv", "run", "toolbox"]
report.inspect = ["project-tool", "report", "inspect"]
```

`spice toolbox lint css --fix` then passes `lint css --fix` to `toolbox`.
`spice report inspect --limit 40` then passes `--limit 40` to the mounted nested
path backend.

Mounted commands can import the public repo-tool seam documented in the README.
They should not rely on private spice modules unless the seam is deliberately
expanded with tests and documentation.

### Execution context: mount vs gate step

The same repository command can run two ways, and the two contexts are
deliberately distinct:

- As a **mount** (`spice <name>`), the command runs with the mount environment:
  `SPICE_MOUNTED_COMMAND=1` and `SPICE_VISIBLE_PROG` are exported so the tool can
  present itself as a `spice` verb.
- As a **`pre_commit` gate step**, the command runs argv-only with
  `SPICE_STAGED_PATHS` (newline-delimited staged paths, narrowed by `when`). The
  mount signals (`SPICE_MOUNTED_COMMAND`, `SPICE_VISIBLE_PROG`) are **not** set.

This is intentional, not an oversight: a gate step is a focused check over
staged paths, not a `spice`-fronted invocation. A repo tool therefore must not
branch on detecting spice (e.g. on `SPICE_MOUNTED_COMMAND`) for behavior it also
needs as a gate step — that signal is absent there by design. Keep the tool
context-free and pass what it needs through argv (reading `SPICE_STAGED_PATHS`
when it wants the staged set); a tool written this way behaves identically on
both paths.

## Choosing A Surface

Use `spice agent run -- <cmd>` for agent-owned execution where steering,
keep-working guidance, RTK rewrite routing, worktree routing, and wrapper
functions must apply.

Use a mounted command for repository-owned tools that operators or hooks should
run as `spice <verb>` in that repository only. Release tooling is mounted in
this repository for that reason: other repositories can mount their own release
implementation without competing with a global spice built-in.
