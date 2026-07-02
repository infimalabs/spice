# Transparent Steering Injection

Status: implemented contract.

## Contract

Agent launch owns the shell environment. For an agent-bound worktree, launch
sets `ZDOTDIR` and `BASH_ENV` to the packaged redirector spice shell startup
files, records the original startup values in runtime environment variables, and
precomputes configured wrapper functions into `SPICE_SHELL_HOOK_WRAPPERS`.

For the first non-interactive zsh or bash command shell with an execution
string, the redirector hook clears `ZDOTDIR`/`BASH_ENV` and replaces the shell
with:

```sh
spice agent run -- <shell> -c "<original command>"
```

That gives `spice agent run` ownership of stderr for steering injection,
keep-working guidance, RTK rewrite routing,
source-checkout routing, and wrapper setup before the requested command.
`agent run` repoints `ZDOTDIR`/`BASH_ENV` at the packaged static hook dir for
the shell command it runs, so that shell and its descendants do not reexec and
do not inject steering again. The static stage restores the user's original
`ZDOTDIR`, `BASH_ENV`, and zsh history file, sources the real startup file when
present, rearms the packaged hook environment for later descendants, and evals
`SPICE_SHELL_HOOK_WRAPPERS`. The redirector and static stages are distinct
packaged hook directories, not an environment marker, so there is no reexec
counter to read.

The native harness or shell startup hook must hand the complete top-level shell
command string to `spice agent run` exactly once. `agent run` owns RTK rewrite
because it is the only layer that sees the full shell string before execution.

## Shells

Supported surfaces:

- `zshenv`
- `zprofile`
- `zshrc`
- `zlogin`
- `bash_env`

zsh is covered through `ZDOTDIR` startup files; bash is covered through
`BASH_ENV`. The `.zshrc` surface is static-stage only for interactive shells. Missing
packaged startup files fail at spawn, and static hooks fail loudly when required
environment variables are missing or the command shell cannot be resolved.

## Wrapper Groups

Repos may define wrapper groups under `[tool.spice.wrappers.<group>]` and let
agents select groups with `[tool.spice.agent] wrappers = [...]`. When the agent
does not set `wrappers`, the built-in `common` group is selected. The built-in
`common` group is intentionally empty because RTK coverage comes from the
`rtk rewrite` handoff inside `spice agent run`, not from generated shell
functions. Repos can override `common` for generic selector control, and
repo-specific direct-command wrappers such as code generators belong in their
own selected extension groups. An explicit empty list disables wrapper
generation.

Example:

```toml
[tool.spice.agent]
wrappers = ["common", "repo-tools"]

[tool.spice.wrappers.common]
wrap = ["grep", "find", "git"]

[tool.spice.wrappers.repo-tools]
codegen = { argv = ["uv", "run", "python", "-m", "tools.codegen"] }
```

At spawn, spice renders command functions into `SPICE_SHELL_HOOK_WRAPPERS`; the
static-stage hook evals functions such as:

```sh
grep() {
  wrap grep "$@"
}
```

Path selectors such as `/bin/sh` require a redirector stage and fail loudly
until that resolver exists.

## Invariants

- Do not touch the agent's stdin.
- ACK semantics are transcript-based: items retire only on `ACK <key>`.
- The side-channel repeat policy remains the rate limiter.
- The redirector and static packaged hook dirs are the reexec gate: the
  redirector reexecs an execution-string shell through `agent run` exactly once,
  and `agent run` repoints descendants at the static hook dir so they do not
  reexec again.
- RTK rewrite happens in `spice agent run`, where the complete shell command
  string is still available.
- `SPICE_SHELL_HOOK_WRAPPERS` is generated before shell startup; hooks eval it
  but do not regenerate wrapper functions.
- The direct shell-startup path is the only command-injection contract.
