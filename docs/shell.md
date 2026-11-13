# Shell boundary inventory

A shell file, tracked or generated, exists only at a boundary Rust cannot
cross without a prior binary artifact or without losing
source-in-the-current-shell behavior. This is the complete inventory.

## Generated Git-hook launchers

`spice install` generates `pre-commit`, `commit-msg`, and
`reference-transaction` under the Git-common `.spice-install/githooks` from
[`spice/src/install/git_hooks.rs`](../spice/src/install/git_hooks.rs), and
selects that directory through a generated `core.hooksPath` configuration. They
are installed artifacts, not tracked files. Each is exactly two lines: a
`#!/bin/sh -eu` shebang and one `exec` of the absolute Git-common appliance's
`gate HOOK` leaf, forwarding Git's argv, standard input, output, and exit
status unchanged. No launcher carries an environment prelude, reads policy, or
routes anywhere else. The appliance path sits in the body rather than the
shebang, so a repository move is repaired by a fresh install.

## Generated Windows MSVC tool wrappers

`spice dev xtask -- windows-msvc` materializes the toolchain pinned by
`dist/windows/msvc.toml` under the bootstrap Cargo home at
`spice/windows-msvc-v1/generations/GENERATION/payload/` from
[`spice/src/cargo_isolation/windows_msvc.rs`](../spice/src/cargo_isolation/windows_msvc.rs).
Its `bin/` holds four generated owner-only `#!/bin/sh` scripts, each one
`exec` of a pinned absolute host tool with the caller's argv: `cl.exe` runs
`clang-cl` in `cl` driver mode for the target with the generation's `sysroot`,
`ml64.exe` runs `llvm-ml --m64`, `lib.exe` runs `llvm-lib`, and `link.exe` runs
`lld-link` with the same sysroot. A `spice dev cargo` command that selects
`x86_64-pc-windows-msvc` prepends `bin/` to `PATH` and names the wrappers
through `CC_x86_64_pc_windows_msvc`, `CXX_x86_64_pc_windows_msvc`,
`AR_x86_64_pc_windows_msvc`, and
`CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER`. The compiler and linker wrappers
locate the sysroot relative to their own paths, so a published generation needs
no further build step.

## `dist/proof-image/cargo-wrapper.sh`

Installed as `/opt/spice/bin/cargo` in the sealed Linux proof image. It creates
`/cargo-target/tmp` on the empty target tmpfs and `exec`s the real Cargo. A Rust
wrapper would need a prebuilt `linux/amd64` binary before the image that
records its toolchain exists.

## `dist/proof-runner/build.sh`

The arm64 macOS release operator's bootstrap. From a clean `HEAD` it runs the
unsealed runner to derive compile-time pins, compiles and exercises the sealed
runner, and binds it as the worktree's `proof.runner` through the previewed,
digest-applied `spice config` mutation. Moving that derivation into the runner
would let the target author its own trust inputs.
