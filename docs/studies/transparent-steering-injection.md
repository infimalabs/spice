# Transparent Steering Injection

Status: implemented contract.

## Contract

Agent launch owns the shell environment. For an agent-bound worktree, launch
sets `ZDOTDIR` and `BASH_ENV` to packaged spice shell startup files and records
the original values in runtime environment variables. Those packaged files ask
`spice agent shell-hook <surface>` to render the current hook body, then `eval`
that body in the shell that is about to run the requested command.

The rendered hook restores the user's original `ZDOTDIR` and `BASH_ENV` before
doing anything else. For non-interactive zsh and bash commands with an execution
string, it reexecs the original shell command through:

```sh
spice agent run -- <shell> -c "<original command>"
```

That makes steering injection, context warnings, git-shadow routing, proxy
routing, and configured wrapper functions run before the requested command.

## Shells

Supported surfaces:

- `zshenv`
- `zprofile`
- `zlogin`
- `bash_env`

zsh is covered through `ZDOTDIR` startup files; bash is covered through
`BASH_ENV`. Unsupported surfaces fail loudly through `spice agent shell-hook`
rather than falling back to an unwrapped command path.

## Wrapper Groups

Repos may define wrapper groups under `[tool.spice.wrappers.<group>]` and let
agents select groups with `[tool.spice.agent] wrappers = [...]`. When the agent
does not set `wrappers`, the built-in `common` group is selected. The built-in
`common` group maps `rtk` to `run`, `proxy`, `grep`, `find`, and `git`; repos
can override it by defining `[tool.spice.wrappers.common]`. An explicit empty
list disables wrapper generation.

Example:

```toml
[tool.spice.agent]
wrappers = ["common"]

[tool.spice.wrappers.common]
rtk = ["run", "proxy", "grep", "find", "git"]
```

The hook renders command functions such as:

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
- The direct shell-startup path is the only command-injection contract.
