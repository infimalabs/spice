# Single-Install Runtime Model

Status: decision, 2026-06-26.

## Decision

Spice runs from a single installed runtime, not from whichever worktree happens
to be active.

- Spice is installed as a **uv tool** by default. The canonical install is an
  editable install — `uv tool install -e <main-tree>` — pointed at one
  representative main tree. That main tree is the server deployment.
- **Worker worktrees are operated trees.** They hold tasks, branches, and work
  in progress, but they do **not** supply their own spice runtime. Editing a
  worker tree changes that tree's files, never the code that is currently
  running.
- The installed tool is the **single coherent running code**. Every `spice`
  invocation in every worktree resolves to that one installation, so the
  allocator, steering socket, and serve process are all the same build
  regardless of which directory a shell sits in.
- **Common-directory install stays supported as an opt-in.** Operators who
  prefer a shared install location rather than the uv tool layout can still use
  it; it is no longer the default, but it is not removed.

This mostly codifies how the operator already runs spice: one main tree deployed
as the server, other trees operated as workers. The bare-repo multi-tree split —
where each worktree carried its own runtime — never paid off, because a
deployment was always needed anyway and editing the deployment tree occasionally
broke the running server.

## Why

The per-tree-runtime model couples *which files an agent edits* to *which code is
running*. In a live system that is a footgun: a routine edit in a worker tree can
shadow or break the running server's steering injection, the supervisor socket,
and the allocator controls — the exact machinery an operator relies on to steer
and recover agents. Stability of the running code must not depend on leaving
every other worktree untouched.

A single installed runtime decouples the two. Worktrees become pure work
surfaces; the runtime is a deliberate, separately-managed deployment. Editing a
worker is always safe. Updating the server is an explicit reinstall/redeploy
step, not an accident of `cd`.

## Per-Tree-Runtime Magic Being Removed

The current code makes the active worktree win the runtime through several
coupled mechanisms. The single-install battery removes all of them:

- **Worktree PYTHONPATH + venv injection** —
  `worktree_spice_environment` (`spice/paths.py:124`) prepends the worktree root
  to `PYTHONPATH` and prepends the worktree `.venv` to `PATH`/`VIRTUAL_ENV`,
  selected by `worktree_spice_source` (`spice/paths.py:93`). This makes a
  worktree's own checkout shadow the installed package.
- **The worktree-spice reexec** —
  `_reexec_worktree_spice_if_needed` (`spice/cli/entry.py:87`) re-execs the
  process into the active worktree checkout
  (`worktree_spice_python_command`, `spice/paths.py:148`) whenever the current
  runtime is not already that checkout. This is the mechanism that makes `spice`
  "run from here" instead of from the install.
- **`python` / `python3` worktree-venv routing** — agent shells route bare
  `python` and `python3` (`PYTHON_ROUTE_COMMANDS`, `spice/agent/wrap.py:68`)
  through `python_route_command_prefix` / `default_venv_python`
  (`spice/agent/wrap.py:364`), pointing them at the worktree `.venv`.
- **The now-dead strippers** — once injection is gone, the compensating code that
  exists only to undo it becomes dead and should be removed too:
  `hermetic_wheel_env` (`spice/release.py:229`) pops the injected `PYTHONPATH`
  so release smokes test the wheel and not the worktree, and the doctor hook
  pops `PYTHONPATH` (`spice/hooks/doctor.py:224`) for the same reason. With no
  injection there is nothing to strip.

## Scope / This Battery

This record is the root of the single-install battery. It states the target
model; the implementing tasks remove the magic above and document/test the
result:

- `lifecycle.install` — make `uv tool` the default install; keep common-dir as
  opt-in.
- `cli.entry` — remove the worktree-spice reexec so spice always runs the
  installed runtime.
- `cli.paths` — remove worktree `PYTHONPATH`/venv injection and the now-dead
  strippers.
- `lifecycle.shellhooks` — stop routing `python`/`python3` to the worktree venv
  in agent shells.
- `serve.deploy` — codify serve as the single main-tree deployment; workers are
  operated trees.
- `tests.hermeticity` — document the single-install model and add
  no-per-tree-runtime tests.

These tasks are not artificial dependencies on each other: they each delete one
strand of the same coupling, and they share this record as the single source of
truth for *what the end state is*. Sequencing matters only where one removal
would leave the runtime unbootable without another (e.g. dropping the reexec
before the default install path exists), not because the tasks are arbitrarily
chained.

## Non-Goals

- Not removing common-directory install support; it stays as an opt-in.
- Not changing how worktrees are created or how tasks/branches are organized.
- Not introducing a build/bundle step; the install remains an editable uv tool
  pointed at source.
