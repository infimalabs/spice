# Rust development loop

Run developer commands from the repository's supervised shell, where ambient
`spice` resolves to the Git-common appliance (`spice dev xtask -- help`). Cargo
isolation is described in [spice/CARGO_ISOLATION.md](../spice/CARGO_ISOLATION.md);
commit proof in [Gate evidence](planes/gate-evidence.md).

## Find the owner before editing

The workspace manifest owns crate membership, each crate manifest its
dependency edges, and Cargo metadata the resolved graph.

| Layer | Crates and ownership |
| --- | --- |
| Floors | `wire` owns crossing DTOs; `atomic` owns durable file replacement; `sill` owns process groups and advisory locks; `door` owns bounded one-message local doors between processes of one user and the Windows launch channel from a parent to its exact child. |
| Durable domain | `journal` is the sole SQLite authority; `tape` preserves vendor bytes; `dialect` classifies task documents without I/O; `board` derives task/claim views; `bus` is the resident's sole writer and timer rim. |
| Agent control | `listen` decodes preserved records; `consent` decides pre-action policy; `steer` owns durable direction and acknowledgment; `taste` selects prose reminders; `vendor` declares launch contracts; `lane` owns durable seats and supervised occupants. |
| Product and artifacts | `app` is the WASM browser body and has no workspace dependency except `wire`; `desktop` owns the native app window; `serve` serves the loopback surface and app generations; `rack` builds, links, and reads deterministic resource archives; `artifact` stores task evidence outside the journal; `dist` stages releases; `proof` verifies sealed appliances. |
| Enforcement and composition | `gate` is hermetic repository enforcement and depends on the workspace crates `atomic`, `dist`, `sill`, and `wire`; `windows-execution` (`windows/`) owns hosted and guest Windows execution protocols and runner support; `spice` composes daemonless CLI/resident surfaces; `xtask` owns repository-local build, verification, cache, and release orchestration. |

Authority writes belong to `journal`/`bus`, replayable task views to `board`,
and browser code cannot reach server internals.

## Tracked and generated surfaces

| Surface | Owner and rule |
| --- | --- |
| `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` | Tracked compiler and dependency contract; repair and commit, never regenerate blindly. |
| `spice.toml`, `gate/src/battery/manifest.toml` | Tracked configuration and gate inventory; change with the owning compiled registry. |
| `dist/stage-app/` | Ignored atomic app generations owned by `spice dev xtask -- app`; rebuild, never edit a generation. |
| `dist/stage/` | Generated content-addressed release staging owned by `spice dev xtask -- stage-target TARGET`; rebuild, never edit staged output. |
| `target/` | Rebuildable compiler and test caches; see cleanup below. |
| `<git-common>/.spice/` | Appliance state grouped by lifetime as laid out in [install](install.md#state-root): `authority/` is durable and removed only by an authorized lifecycle operation; `runtime/`, `cache/`, and `output/` are deletable while no resident runs; `lanes/LANE/` dies with its lane. |
| `<git-dir>/.spice/` | Per-worktree `worktree.toml`, `authority/worktree/`, `runtime/`, and `cache/` in the same layout. Lifecycle and configuration commands own this root. A `.spice` inside worktree content is ordinary content, never runtime authority. |
| `<git-common>/.spice-install/` | Installed appliance and hooks; republish through `spice install`, never edit. |

## Schema changes

`dist/version/base` is the human-selected major or exact product version. Under
a major, `dist/version/counters` is the single sorted registry of independently
owned monotonic contributors, written as `name=decimal` records. The product
minor sums every record automatically, and owner crates select their record
from the same file at compile time. Add a contributor by adding one sorted
line; there is no Rust registry or build-generation step. The `gap` record
absorbs a retired contributor's last value so the sum never moves backwards
within that major.

A journal shape change is one atomic cutover:

1. Increment the `journal` record in `dist/version/counters`.
2. Add `journal/src/migrate/crossing/vN.rs`, where `N` is the schema being left,
   and register `mod vN;` plus `(N, vN::cross)` in
   `journal/src/migrate/crossing.rs`.
3. Exercise the transformed stored shape through the narrowest journal seam.

Schema zero is an empty database. Its registered `v0` crossing constructs
schema 1, so every later shape extends one executable chain rather than
inventing the first migration. The schema contract test fails when the first
step does not leave zero, the final step does not reach the current schema, or
retained steps have a gap.

An install receipt shape change follows the same forward-only rule: increment
`install`, add one `spice/src/install/record/migrate/vN.rs` crossing for the
schema being left, register it in `migrate.rs`, and exercise the oldest retained
schema through current apply. Every crossing in the current product major stays
registered as one uninterrupted chain; install never downgrades a receipt.

## Feedback

Permanent tests follow [AGENTS.md](../AGENTS.md#permanent-test-value). Retained
data belongs behind a one-time forward migration or an owner-naming refusal;
never preserve an earlier release's DTO, command path, or process fixture as
another executable test arm.

| Surface | Command and boundary |
| --- | --- |
| Exact test | `spice dev cargo -- test --locked -p PACKAGE` with a target selector or test filter. |
| Package | `spice dev xtask -- verify --package PACKAGE` runs package Clippy, all targets, and doctests as serial one-job Cargo children, and prints every repository-wide action it omits. Add downstream packages when a public seam changes. |
| App WASM typecheck | `spice dev cargo -- app-wasm-typecheck` checks only `app` for `wasm32`. |
| Browser | `spice dev xtask -- app-demo --open` stages current worktree app sources and serves them with the current native binary in an isolated disposable root; see [`app/BROWSER.md`](../app/BROWSER.md). Plain `spice demo` exercises the installed appliance and retains its recovery commands. Browser evidence is disposable. |
| Commit | Stage the change, then run `spice gate prove ACTION` for one selected action, `spice gate prove --missing` for every missing proof, or `spice gate prove` for the full selected set; then `git commit`. |
| Whole tree | `spice gate prove --all` proves the staged tree as if every path changed: every applicable reusable action and every Cargo typecheck and test shard, Windows typechecks included, reusing current receipts and listing each one. On a clean worktree it proves the checked-out commit. |
| Gate change | `spice gate prove` and the commit hooks run the installed appliance's gate code, not current source. When a change edits gate logic, run each changed arm from current source against the staged tree with `spice dev xtask -- gate ARM` before relying on the installed gate. |

The unscoped `spice dev xtask -- verify` refuses: repository-wide verification
is never routine feedback.

## Subsystem guides

- Browser checks: [`app/BROWSER.md`](../app/BROWSER.md); the always-chat team
  console: [`app/CONSOLE.md`](../app/CONSOLE.md) and its activity rail:
  [`app/TRACE.md`](../app/TRACE.md); rooms and the operator inbox:
  [`app/ROOMS.md`](../app/ROOMS.md); Design rooms:
  [`app/DESIGNS.md`](../app/DESIGNS.md); the task overview:
  [`app/OVERVIEW.md`](../app/OVERVIEW.md); menus:
  [`app/MENU.md`](../app/MENU.md); image and media handling:
  [`app/IMAGES.md`](../app/IMAGES.md); speech and narration:
  [`app/SPEECH.md`](../app/SPEECH.md).
- Firefox latency profiling: [`app/PROFILE.md`](../app/PROFILE.md).

## Safe cleanup

Preview the bounded build-cache collector, then replace `--dry-run` with
`--apply` after review:

```sh
spice dev xtask -- build-cache --dry-run --max-bytes BYTES --retain-live-worktrees COUNT
```

It collects only real Cargo-tagged target directories below their owning
worktrees. It retains the current roots and the requested number of hottest
linked-worktree roots, and skips roots whose Cargo locks are held.
`--free-space-floor BYTES` adds a free-space target. Remove unneeded worktrees
without `--force` after preserving wanted changes. Never use `git clean -fdX`:
appliance state, artifacts, generations, dependencies, and compiler output have
distinct lifecycles.
