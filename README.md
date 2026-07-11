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

![Live steering and semantic ACK loop](docs/screenshots/spice-live-review-steering.png)

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

See [docs/overview.md](docs/overview.md) for the operating model and
[docs/interface.md](docs/interface.md) for the serve UI.

## Commands

| Surface | Command |
| --- | --- |
| Prepare a full fleet repo | `spice init` / `spice doctor` |
| Install constitution gates only | `spice init --gates` |
| Run through the agent wrapper | `spice agent run -- <cmd>` |
| Maintain a worktree-bound agent | `spice agent ensure` / `spice agent supervise` |
| Pull allocator work | `spice task next` |
| Rehydrate context | `spice session briefing` |
| Open the operator UI | `spice serve` |
| Run studies and gates | `spice study ...` / git pre-commit hook |

Configuration lives in [CONFIG.md](CONFIG.md). The design contract lives in
[DESIGN.md](DESIGN.md). Wrapper command behavior is detailed in
[docs/cli/wrapper-commands.md](docs/cli/wrapper-commands.md). Stability
expectations for extensions and command coupling live in [STABILITY.md](STABILITY.md).

## Install

```sh
uv tool install -e /path/to/spice-main
# or, for the released package:
uv tool install spice-harness

# RTK is required for the agent shell (version 0.42.4 or newer):
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

[RTK](https://github.com/rtk-ai/rtk) is a required companion for the agent
shell: `spice agent run` delegates command selection to `rtk rewrite`, and
`spice doctor` verifies the supported protocol before agents work. The local
judge and speech synthesis are degradable companions; when either is
unavailable, transcript capture, steering, tasks, and the constitution keep
working while maxim feedback or audio narration is skipped. Runtime,
verification, and protocol details are in [CONFIG.md](CONFIG.md).

## Release

Release workflow is documented in [docs/release.md](docs/release.md). Most
users only need to know that releases are cut from clean synchronized worktrees
through the repository's mounted `spice release` command.

## Status

Work in progress toward a standalone, releasable product. The loop described
here is real, exercised daily, and guarded by the same constitution that
`spice init` installs elsewhere.
