# Spice Harness

[![PyPI version](https://img.shields.io/pypi/v/spice-harness.svg)](https://pypi.org/project/spice-harness/)
[![Python versions](https://img.shields.io/pypi/pyversions/spice-harness.svg)](https://pypi.org/project/spice-harness/)
[![License](https://img.shields.io/pypi/l/spice-harness.svg)](https://github.com/infimalabs/spice/blob/main/LICENSE)

**Spice Harness is an agent harness / fleet operations console.**

_Simultaneous Production, Integration, and Control Environment._

spice is an installed, repo-native harness for operating coding agents. It
treats the agent transcript as the source of truth and the repository
filesystem as the steering channel; supervision, task routing, git pressure,
live feedback, and hygiene gates are derived from those two surfaces.

It is built for agents moving fast in parallel: every correction is durable,
every task boundary is observable, and the gate catches structural drift before
it lands.

spice is building itself, but it was not created in a vacuum: the loop was born
from a harsher polyglot environment where many languages, conventions, and
agent lanes had to survive contact with one another.

![Live steering and semantic ACK loop](https://raw.githubusercontent.com/infimalabs/spice/main/docs/screenshots/spice-live-review-steering.png)

<sub>Operator steering arrives in the live stream; an assistant ACK retires the
exact inbox key from the durable filesystem queue.</sub>

## What it does

- **Semantic ACKs:** steering is not considered handled until the agent
  acknowledges the durable key in assistant prose.
- **Task allocation:** `spice task next` owns work selection; task boundaries
  own git synchronization and review phases.
- **Conscience:** curated maxims judge assistant prose while work is still in
  flight, then route violations back as ordinary steering.
- **Constitution:** pre-commit and `spice study ...` enforce repository shape,
  file/routine limits, env policy, reachability, assertion density, private
  internals, and commit-message rules.
- **Serve UI:** `spice serve` exposes lanes, teams, live transcripts, steering,
  attachments, task routing, and browser-visible diagnostics.
- **Observer UI:** `spice watch <session-dir>...` follows existing Codex and
  Claude transcripts without initializing or modifying their directories.

See [docs/overview.md](https://github.com/infimalabs/spice/blob/main/docs/overview.md) for the operating model and
[docs/interface.md](https://github.com/infimalabs/spice/blob/main/docs/interface.md) for the serve UI.

## Posture: a single-operator console

`spice serve` is a **single-operator console**, and that is a deliberate
identity — not a limitation to grow out of:

- **SQLite, localhost, one shared token.** The server is a stdlib process backed
  by SQLite, bound to `127.0.0.1` by default; when it is reached beyond loopback
  it is gated by a single shared `--auth-token`. There are no accounts,
  sessions, or per-user identities.
- **The growth vector is remote reach for one operator**, not multi-user auth.
  Reaching your own fleet from elsewhere is a transport choice — an SSH tunnel
  or a tailnet bind over the same one-token surface (see
  [single-operator remote reach](https://github.com/infimalabs/spice/blob/main/docs/design/accepted/single-operator-remote-reach.md)).
- **Multi-user auth is an explicit non-goal.** Do not grow a multi-operator team
  product out of the stdlib server. Many humans may steer one lane, but only
  through the same durable filesystem queue — never privileged per-user channels
  (see [no-privileged-channel](https://github.com/infimalabs/spice/blob/main/docs/design/accepted/no-privileged-channel-multi-human.md)).

## Start Small

Spice Harness is a progressive-disclosure product: **watch**, then **gates**,
then **steer**, then **fleet**. Start by observing existing agent sessions with
no repository changes; add constitution gates when the team wants enforceable
hygiene; bind one agent when direct intervention becomes necessary; move to the
task-backed fleet only when work needs multiple coordinated lanes. The full
prerequisite and graduation path is the [entry ladder](https://github.com/infimalabs/spice/blob/main/docs/overview.md#entry-ladder).

## Commands

| Surface | Command |
| --- | --- |
| Watch existing agent sessions | `spice watch <session-dir>...` |
| Install constitution gates only | `spice init --gates` |
| Prepare steering and fleet surfaces | `spice init` / `spice doctor` |
| Open a manually steered lane | `spice agent ensure` / `spice serve` |
| Run through the agent wrapper | `spice agent run -- <cmd>` |
| Pull allocator work | `spice task next` |
| Rehydrate context | `spice session briefing` |
| Open the operator UI | `spice serve` |
| Observe foreign sessions read-only | `spice watch <session-dir>...` |
| Run studies and gates | `spice study ...` / git pre-commit hook |

Configuration lives in [CONFIG.md](https://github.com/infimalabs/spice/blob/main/CONFIG.md). The design contract lives in
[DESIGN.md](https://github.com/infimalabs/spice/blob/main/DESIGN.md). Wrapper command behavior is detailed in
[docs/cli/wrapper-commands.md](https://github.com/infimalabs/spice/blob/main/docs/cli/wrapper-commands.md). Stability
expectations for extensions and command coupling live in [STABILITY.md](https://github.com/infimalabs/spice/blob/main/STABILITY.md).

## Install

```sh
uv tool install -e /path/to/spice-main
# or, for the released package:
uv tool install spice-harness

# Optional: RTK 0.42.4 or newer compacts agent-shell command output:
brew install rtk
# or: cargo install --git https://github.com/rtk-ai/rtk

cd /path/to/your/repo
spice init
spice doctor
```

For repository hygiene without the task plane, shell wrapper, or agent skill,
install the standalone constitution tier instead:

```sh
spice init --gates
```

This installs the `pre-commit` constitution (including sticky-flex limits,
regression-only magic-number ratchets, taste policy, and configured extensions)
plus commit-message hygiene. It does not install the fleet-specific reference
guard or materialize agent files. Commit normally to run the gates, or invoke
the staged gate directly with `spice dev pre-commit`.

The default install is a uv tool. Operators who deploy from a main tree should
use the editable form so the installed `spice` command resolves to that tree;
that editable main tree is the server deployment. Other worktrees remain
operated trees and do not supply their own runtime.

### Graceful degradation

[RTK](https://github.com/rtk-ai/rtk) is an optional command-output optimizer for
the agent shell. `spice agent activation` and `spice doctor` report whether its
rewrite protocol is active; missing, obsolete, or invalid RTK leaves
`spice agent run` on the original native command path. The local judge and
speech synthesis are also degradable companions; when unavailable, transcript
capture, steering, tasks, and the constitution keep working while optional
feedback or narration is skipped. Runtime, verification, and protocol details
are in [CONFIG.md](https://github.com/infimalabs/spice/blob/main/CONFIG.md).

The layered `rtk.executable` setting accepts one trusted executable basename or
absolute path; activation and Doctor probe that exact identity without a prior
lookup. Exit `0` or Exit `3` with non-empty stdout applies a rewrite, while
Exit `1` with empty stdout is a silent no-match. Other or malformed outcomes
are diagnosed, discarded, and the original native command runs unchanged.
RTK owns selection and its canonical `rtk` frontend; Spice remaps that frontend
to the configured identity, supplies only the built-in `common` wrapper's
finite post-selection routes and thread-scoped `RTK_DB_PATH` at
`<worktree-git-dir>/.spice/agents/<thread>/rtk/history.db`, and reports health
telemetry through activation, Doctor, and bounded stderr diagnostics.

## Release

Release workflow is documented in [docs/release.md](https://github.com/infimalabs/spice/blob/main/docs/release.md). Most
users only need to know that releases are cut from clean synchronized worktrees
through the repository's mounted `spice release` command.

## Status

Work in progress toward a standalone, releasable product. The loop described
here is real, exercised daily, and guarded by the same constitution that
`spice init` installs elsewhere.
