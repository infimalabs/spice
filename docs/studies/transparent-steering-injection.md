# Transparent Steering Injection

Status: implemented contract.

## Contract

Agent launch owns the shell environment. For an agent-bound worktree, launch
sets `ZDOTDIR` and `BASH_ENV` to packaged static spice shell startup files,
records the original startup values in runtime environment variables, and
precomputes configured wrapper functions into `SPICE_SHELL_HOOK_WRAPPERS`.

For the first non-interactive zsh or bash command shell with an execution
string, the packaged hook sees `SPICE_SHELL_HOOK_REEXEC_STAGE` unset, sets it to
`1`, and replaces the shell with:

```sh
spice agent run -- <shell> -c "<original command>"
```

That gives `spice agent run` ownership of stderr for steering injection,
keep-working guidance, git-shadow routing, source-checkout routing, and wrapper
setup before the requested command. Descendant shells inherit
`SPICE_SHELL_HOOK_REEXEC_STAGE=1`; they do not reexec and do not inject steering
again. Stage-2 startup restores the user's original `ZDOTDIR`, `BASH_ENV`, and
zsh history file, sources the real startup file when present, rearms the
packaged hook environment for later descendants, and evals
`SPICE_SHELL_HOOK_WRAPPERS`.

## Shells

Supported surfaces:

- `zshenv`
- `zprofile`
- `zshrc`
- `zlogin`
- `bash_env`

zsh is covered through `ZDOTDIR` startup files; bash is covered through
`BASH_ENV`. The `.zshrc` surface is stage-2 only for interactive shells. Missing
packaged startup files fail at spawn, and static hooks fail loudly when required
environment variables are missing or the command shell cannot be resolved.

## Wrapper Groups

Repos may define wrapper groups under `[tool.spice.wrappers.<group>]` and let
agents select groups with `[tool.spice.agent] wrappers = [...]`. When the agent
does not set `wrappers`, the built-in `common` group is selected. The built-in
`common` group maps `rtk` to a broad set of shell-function-safe command
selectors, including tools such as `run`, `proxy`, `grep`, `find`, `git`, `gh`,
`npm`, and `ruff`. Repos can override `common` for generic selector control, and
repo-specific direct-command wrappers such as code generators belong in their
own selected extension groups. An explicit empty list disables wrapper
generation.

Example:

```toml
[tool.spice.agent]
wrappers = ["common", "repo-tools"]

[tool.spice.wrappers.common]
rtk = ["run", "proxy", "grep", "find", "git"]

[tool.spice.wrappers.repo-tools]
codegen = { argv = ["uv", "run", "python", "-m", "tools.codegen"] }
```

At spawn, spice renders command functions into `SPICE_SHELL_HOOK_WRAPPERS`; the
stage-2 hook evals functions such as:

```sh
grep() {
  rtk grep "$@"
}
```

Path selectors such as `/bin/sh` require a redirector stage and fail loudly
until that resolver exists.

## Invariants

- Do not touch the agent's stdin.
- ACK semantics are transcript-based: items retire only on `ACK <key>`.
- The side-channel repeat policy remains the rate limiter.
- `SPICE_SHELL_HOOK_REEXEC_STAGE` is the sole reexec gate.
- `SPICE_SHELL_HOOK_WRAPPERS` is generated before shell startup; hooks eval it
  but do not regenerate wrapper functions.
- The direct shell-startup path is the only command-injection contract.
