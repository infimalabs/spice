# spice - design

spice is the Simultaneous Production, Integration, and Control Environment: an
installed agent harness for operating coding agents inside real git worktrees.
This root file states the system contract. Detail lives deeper:

- [Architecture](docs/design/architecture.md)
- [Invariants](docs/design/invariants.md)

## Core Thesis

> The agent's transcript is the single source of truth, the repo filesystem is
> the single channel of steering, and supervision, coordination, conscience,
> and hygiene are derived mechanically from those two surfaces.

That thesis gives spice five planes.

## The Five Planes

1. **Steering fabric.** Durable `.spice/` inbox items are retired only by
   semantic ACKs in the transcript; shell hooks inject pending steering through
   the agent's own commands.
2. **Lifecycle plane.** One supervised agent inhabits one worktree. Startup is
   a neutral skill prompt; activation, briefing, tasks, and inbox recover the
   real ask. Renewal is ordinary steering, not process violence.
3. **Conscience.** Supervisor maxims scan assistant prose, ask a local judge
   whether a hit is real, and return confirmed violations as ordinary steering.
4. **Coordination plane.** Taskwarrior in the git common dir provides phases,
   claims, review separation, oops capture, urgency, and task-boundary git
   sync. `serve` projects that board as lanes, teams, live streams, filters,
   composers, metrics, and speech.
5. **Constitution.** Hook-backed studies guard seams: shape, staging,
   formatters, paths, typechecks, env policy, file pressure, complexity, magic,
   reachability, assertion quality, and private-internal test coupling.

## Design Principles

0. **Standalone product, not a repo organ.** spice is installed once, normally
   as a uv tool (`uv tool install spice-harness`), and operates on any repo from
   outside. Operators deploying from source use
   `uv tool install -e /path/to/spice-main`; that editable main tree is the
   server deployment. A target repo contains only what spice writes into it:
   runtime state under `.spice/`, generated `.spice/hooks` shims, and the
   worktree skill under `.agents/skills/spice`. Worker worktrees are operated
   trees: tasks, branches, and files live there, but the running code remains
   the installed tool. The common-dir layout remains an opt-in install shape
   for operators who deliberately set uv's tool directories before installing.
   The worktree skill ships as package data; every operator- and agent-facing
   command string is `spice ...`.

1. **The driver seam.** Agent CLI specifics live in concrete driver records:
   binary, argv, thread-id env, transcript grammar, and session-id parsing. A
   new supported CLI is another driver value, not a broad mode split.

2. **Defaults have scopes.** Tracked project defaults set clone-wide launch
   policy; `.spice/config/state.json` carries worktree overrides; flags win.

3. **Opinions are configuration with teeth.** Policy defaults are the opinion;
   repo overrides belong in tracked config, and bad config fails loudly.

4. **Studies are any-language by default.** File pressure is suffix-free;
   complexity uses lizard; magic/env families are declared once; Python-package
   guards apply only under configured package roots.

5. **Stdlib-first forensics.** Session analysis runs over JSONL; runtime
   packages install with spice so hooks do not depend on shell PATH.

6. **The task vocabulary opens.** spice ships core stems and lets repos approve
   additional public stems through tracked config.

7. **Inbox is not a public mail app.** ACK and inbox machinery is the internal
   steering fabric used by wrapper, supervisor, and serve UI.

8. **One coherent UI.** Lanes, occupants, fusing, filters, lifetime controls,
   badges, and speech share one protocol and visual language.

9. **Mounted commands unify repo tooling.** `[tool.spice.commands]` mounts
   repo commands under `spice <name> ...`; built-in verbs win and shadowing
   fails loudly.

## What Spice Is Not

- Not an IDE: it is something the operator overhears and steers.
- Not an agent SDK: it operates agent CLIs rather than helping write them.
- Not spec-driven development: the spec is the loop's fixed point, read from
  transcript and steering, not authored ahead of the work.

spice is an operations console for a fleet of agents.

## Dependencies

Runtime: `watchfiles`, `ruff`, and `lizard`; optional binaries degrade loudly.
Development uses `pytest`, `ruff`, and `lizard`.
