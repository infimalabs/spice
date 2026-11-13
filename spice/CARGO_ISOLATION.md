# Cargo isolation

Resident lanes receive a private `CARGO_HOME` over one immutable,
content-addressed directory-source store keyed by the exact `Cargo.lock` bytes.
`spice dev cargo -- CARGO_ARGS...` and `spice dev xtask -- XTASK_ARGS...` are the
only supported Cargo routes; [AGENTS.md](../AGENTS.md#cargo-isolation) states
when to use them. A `dev cargo` run prepares the generation of the current
`Cargo.lock` and runs offline inside it, except that the lock-writing
subcommands `update`, `generate-lockfile`, `add`, `remove`, and `rm` run under
the bootstrap Cargo home with registry access; the next run seeds its
generation from the rewritten lock.

## Homes and toolchain

The bootstrap Cargo home is `SPICE_BOOTSTRAP_CARGO_HOME`, else `CARGO_HOME`,
else `HOME/.cargo`, and descendants inherit it as `SPICE_BOOTSTRAP_CARGO_HOME`
while their private home is `CARGO_HOME`. Cargo, rustc, and rustdoc are
admitted as one native toolchain before `CARGO_HOME` changes: either a complete
`SPICE_NATIVE_CARGO`, `SPICE_NATIVE_RUSTC`, `SPICE_NATIVE_RUSTDOC` trio, or the
channel `rust-toolchain.toml` declares, resolved by the bootstrap home's
`bin/rustup` under `RUSTUP_HOME`, else `HOME/.rustup`. A partial trio refuses
`cargo-isolation::native-toolchain-incomplete` and tools from different
toolchain directories `cargo-isolation::native-toolchain-mixed`; a missing
executable also refuses before Cargo starts, and `PATH` never selects the
compiler. Every target `rust-toolchain.toml` lists must have a complete
`rust-std` in the admitted compiler's own sysroot; a missing one is installed
through the bootstrap rustup for that same toolchain, and a target rustup
cannot supply there refuses with a `cargo-isolation::tracked-target-*` code.

`spice dev xtask -- install` and the detached `spice up` launcher drop
`CARGO_HOME`, `SPICE_BOOTSTRAP_CARGO_HOME`, and the three `SPICE_NATIVE_*`
variables from the resident's environment and set
`SPICE_BOOTSTRAP_CARGO_HOME` alone, derived from the launcher's own host: an
explicit bootstrap home, else `CARGO_HOME` lifted to the native root when it is
an isolated store home, else `HOME/.cargo`. A store home offered as the
bootstrap home refuses `cargo-isolation::bootstrap-home-isolated` by name. An
install that restarts the resident refuses before building when the derived
host does not resolve.

Package trees are sealed after atomic publication: directories are read-only,
and regular source files keep owner-write permission. Cargo's directory-source
checksums reject changed bytes, and `spice dev cargo-store` verifies every
package digest and reports drift without repairing it.
`spice dev cargo-store --cleanup` removes temporary state, generations no
home references, and packages no remaining generation references, each only
once it is seven days old, and drops pins whose process has exited. When any
home's generation cannot be read, every generation is retained.

Each `dev cargo` process holds a kernel-backed reader pin on its generation
until its Cargo child exits, and cleanup never removes a pinned generation. A
resident lane's home takes no pin, and an occupied lane's home is never
advanced. Proof planning binds the staged lock bytes to an already-admitted
home and never advances a lane home or seeds dependencies; unadmitted
generations refuse.

## Output

Both surfaces forward the argument vector and exit status exactly. Cargo's
diagnostics are captured whole in
`<git-common>/.spice/output/cargo/DIGEST.log`, with a periodic heartbeat
naming Cargo's current step. Success prints one summary line with the capture
path; failure adds a bounded tail and an inspection command. Output that is the
command's product (`dev xtask` tasks, `cargo run`, `metadata`, `tree`,
`--help`, `--version`, test `-- --list`, JSON message formats, and similar)
reaches standard output byte for byte and is also retained. `--verbose`
before `--` streams up to 200 captured lines live. Under a
pre-commit worker, which captures its configured leaf itself, the streams stay
verbatim.

## Target budget

On Unix, each worktree's `target/` has a 24 GiB bound. `dev cargo`, `dev xtask`,
`spice gate prove`, and the pre-commit gate share the worktree's Cargo-output
lease for their whole run. Least recently used units are trimmed until
`target/` fits at two points, at most once per ten minutes: when a run finds
the lease free on entry, and when an outer `dev cargo`, `dev xtask`, or
`spice gate prove` has finished its children and released the lease while no
other run holds it. A run nested in another reclaims at neither point.
Retention has two levels: each `<profile>/incremental/<crate>-<hash>` rustc
cache is a unit with its own last use, emptied before the profile's compiled
dependencies and executables, and a profile directory is a unit whose last use
is the newest write below it, caches included. A unit used within the last
hour is kept. A profile whose Cargo lock a live Cargo or rust-analyzer holds is
left whole with its caches, and a cache whose rustc session lock is held is
left whole. A run whose lease file cannot be created, opened, or locked
refuses `cargo-output::lease-unavailable` before touching Cargo output.
`spice dev cargo-footprint` reports usage per unit and the bound.
