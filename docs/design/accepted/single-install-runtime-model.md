# Single-Install Runtime Model

Status: implemented contract, 2026-07-18.

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
- The installed tool is the **single coherent production/control-plane code**.
  Agent, task, serve, session, and ordinary CLI commands resolve to that one
  installation, so the allocator, steering socket, and serve process are the
  same build regardless of which directory a shell sits in.
- **Development commands are the deliberate exception.** `spice dev ...`
  reexecs through `spice/cli/entry.py` into the operated checkout environment
  so its gates and tests inspect that checkout's candidate code. This scoped
  development seam never changes the runtime used by agent, task, serve, or
  session commands.
- **Common-directory install is removed.** The uv tool layout is the only
  supported install shape; no coherent load-bearing reason for the opt-in
  common-dir variant surfaced, so it was dropped rather than kept as an
  unused option (see `lifecycle.docs.install`, 2026-07-08).

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

## Deployment Evidence Boundary

A change landed on a branch has **no fleet effect** until the installed CLI
carries it. Branch tests establish what the candidate tree contains; they do
not establish what ordinary `spice agent`, `spice task`, `spice serve`, or
`spice session` commands run. Release and task evidence must not claim
fleet-wide behavior from branch state alone.

Two incidents made this distinction load-bearing:

- a team-authority schema version landed while the installed CLI still wrote
  the previous version, so agents repeatedly encountered a shared store whose
  stamp and writer disagreed; and
- generated-skill refresh code landed while the installed editable checkout
  still lacked `_refresh_generated_skill_after_advance`, so no claim or launch
  driven by that runtime could execute the branch's repair.

A reviewer proves the two states independently. `git rev-parse HEAD` describes
the operated candidate. The installed interpreter must be run with the current
directory kept off its import path, and its imported module names the deployed
source:

```sh
/path/to/installed/python -P -c \
  'import spice.tasks.git.boundaries as b; print(b.__file__)'
```

Resolve that module back to its source checkout, then compare that checkout's
commit and tree with the candidate. Importing the candidate from its current
working directory, `PYTHONPATH`, or an interpreter inside the candidate
worktree is not installed-runtime evidence. The repository-mounted release
command carries its parent interpreter across the deliberate candidate-code
reexec and refuses to run release gates unless the independently installed
checkout and candidate have the same committed tree.

Deployment also closes existing generated-copy drift. Once the editable
checkout carries the new packaged source, every `spice agent activation`
materializes the skill before baseline refresh, including when the lane is
already current and no Git advance occurs. Validate convergence with a raw byte
comparison such as `Path(...).read_bytes() == Path(...).read_bytes()`. A
human-oriented diff rendering or its summarized output is not evidence of byte
identity.

## Per-Tree-Runtime Magic Removed

The old code made the active worktree win the runtime through several coupled
mechanisms. The single-install battery removed them:

- **Worktree PYTHONPATH + venv injection** —
  agent, wrapper, and mounted-command environments no longer prepend the
  operated worktree root to `PYTHONPATH` or promote that tree's `.venv`.
- **The production worktree-spice reexec** — ordinary commands no longer
  reexec into an active checkout; only the explicit `spice dev ...` surface
  enters candidate code for development validation.
- **Implicit worktree-venv routing** — agent shells do not select a worktree
  `.venv` directly. Inside a resolved worktree whose root contains
  `pyproject.toml`, bare `python` and `python3` delegate environment selection
  to `uv run python`; outside that project tree they retain native shell
  resolution.
- **The now-dead strippers** — release and doctor no longer compensate for
  worktree-injected `PYTHONPATH`, because the injection path is gone.

## Implementation Evidence

- The packaged console entrypoint owns ordinary runtime resolution.
- `spice/cli/entry.py` limits checkout self-execution to `spice dev ...`.
- Agent, wrapper, and mounted-command environments do not prepend an operated
  worktree to `PYTHONPATH` or promote its virtual environment.
- Bare Python routing is scoped to the marked project root and stays behind
  `uv`; explicit `uv` commands and non-Python commands pass through unchanged.
- Shell-hook, runtime hermeticity, installed-runtime, Doctor, and release tests
  pin the boundary.

The former single-install battery is complete and is not a live work queue.

## Non-Goals

- Not changing how worktrees are created or how tasks/branches are organized.
- Not introducing a build/bundle step; the install remains an editable uv tool
  pointed at source.
