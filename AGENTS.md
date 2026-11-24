# Agent guidance

This repository develops and self-hosts the Spice appliance.

## Harness authority

The active harness or an explicitly installed bootstrap skill owns task,
thread, session, and steering state. Never infer a task, lane, session, or claim
merely from the appliance binary's existence. Outside a Spice bootstrap, ignore
allocator commands. No other control-plane convention is authority for this
repository.

## Cargo isolation

Resident lanes receive a private admitted `CARGO_HOME`. When a development
command can create or refresh `Cargo.lock`, or needs dependencies outside the
admitted generation, invoke the appliance's `dev cargo` surface through ambient
`spice`. Repository command-routing policy resolves that word directly to the
validated Git-common appliance. Repository xtasks use the sibling `dev xtask` surface
backed by the same admitted Cargo home:

```sh
spice dev cargo -- test --locked -p spice
spice dev xtask -- verify --package spice
```

Replace the trailing example argument vector with the exact Cargo or xtask
operation required; retain the ambient `spice` command and `dev ... --`
boundary.

Every `dev cargo` command except a lock-writing subcommand is pinned to the
current `Cargo.lock` and runs offline. To add or bump a dependency, edit the
manifest and rewrite the lock through the same surface with `update`,
`generate-lockfile`, `add`, `remove`, or `rm`; those subcommands run under the
bootstrap Cargo home with registry access. The next `dev cargo` command seeds
its generation from the rewritten lock. `update --workspace` admits new crates
without moving the rest of the lock:

```sh
spice dev cargo -- update --workspace
spice dev cargo -- build --locked -p spice
```

The exact appliance behind the wrapper owns repository development surfaces
such as `dev cargo` and `dev xtask`; its existence does not establish harness
authority. A missing, symbolic, non-regular, or non-executable asset is a hard
stop. Do not substitute another Cargo route or point `CARGO_HOME` at a shared
mutable cache to work around a stale generation.

## Persisted schema evolution

Before 1.0, persisted schemas may require forward-only migrations; arbitrary
downgrade operation is not promised. Every publicly released schema remains a
supported source version for upgrades. Keep crossings sequential and retain
each historical crossing step so repositories can roll forward through every
intermediate version.

A migration must preserve all admitted authority and apply deterministically
and atomically. An unsupported version, corrupt state, or failed crossing
refuses clearly without destructive reset, partial writes, or inferred
recovery.

## Permanent test value

Permanent tests are maintained product contracts. Every permanent test must be
fast, deterministic, and in-process. It must not spawn any process, including
Git, the `spice` binary, keepers, `dsymutil`, or Cargo. Slow tests, regression
tests that pin one past incident, and point-in-time assertions of incidental or
retired behavior are not admitted to the permanent suite.

Add a permanent test only when a plausible bug could violate current observable
behavior: a contract, boundary, invariant, state transition, precedence rule, or
real error. Assertions of source text, plumbing and wiring, mock echoes,
incidental defaults, exact wording, implementation shape, bare does-not-throw,
non-empty-output, and length-grew checks do not earn permanent maintenance.

Delete an inadmissible existing test outright when the current contract is
already covered or is not worth permanent coverage. Otherwise exercise the
contract in process through the narrowest real seam.

Prove a change by reproducing the failure and exercising the actual changed
path. When that proof requires a process or another slow external surface, use a
throwaway script or smoke check and do not retain it in the permanent suite.

## Browser verification

Run non-interactive browser checks through OMP eval's Puppeteer-backed
`browser` global, never by launching Chrome, Safari, or Firefox executables
directly, even with a headless flag. Headless mode can hide browser content
without suppressing native operating system dialogs; on macOS, a direct launch
can still show Keychain authorization. Read `xd://eval/browser` before first
use. For app checks, start `spice dev xtask -- app-demo` as a service, wait for
its `[app-demo] endpoint=URL` line, open that URL with
`browser.open({ app: { tern: false }, ... })`, keep managed Chromium's headless
default, inspect the actual surface, and close the tab in `finally`.

## Rust source organization

Use ordinary Rust modules all the way down when organizing handwritten source:
declare modules with `mod name;` and place their contents in `name.rs` or
`name/mod.rs`. Do not use `include!` as a handwritten-source splitting
mechanism; reserve it for generated source or cases where textual inclusion is
the intended contract.
