//! Repository-owned Darwin implementation of the native proof-runner protocol.

#![forbid(unsafe_code)]

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};

use proof::Battery;
use proof::challenge::{Challenge, ChallengeError};
use proof::native::{ATTESTATION_PREFIX, ISOLATION_CLAIMS, PROTOCOL};
use serde::Deserialize;
use wire::{Digest, Domain, IdentError};

const VENDOR_RELATIVE: &str = "target/proof-native-darwin/vendor";

/// Every pin this runner was sealed with, as `dist/proof-runner/build.sh` wrote
/// it.
///
/// The pins arrive as one compiled document rather than one variable each so
/// the runner's own bytes carry them: an executable built without the document
/// refuses instead of running unpinned, and a document edited after the build
/// is a different executable with a different identity. A workspace check-out
/// that never ran the build script still compiles, which is what keeps the
/// binary inside the ordinary workspace battery.
const PINS: Option<&str> = option_env!("SPICE_NATIVE_PINS"); // env-policy: allow

fn main() -> ExitCode {
    match dispatch(env::args_os().skip(1).collect()) {
        Ok(code) => u8::try_from(code)
            .ok()
            .map_or(ExitCode::FAILURE, ExitCode::from),
        Err(error) => {
            eprintln!("native Darwin proof runner refused: {error}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(arguments: Vec<OsString>) -> Result<i32, String> {
    match arguments.as_slice() {
        [operation, path] if operation == OsStr::new("closure-digest") => {
            println!(
                "{}",
                closure_digest(Path::new(path)).map_err(|error| error.to_string())?
            );
            Ok(0)
        }
        [operation, path] if operation == OsStr::new("tool-digest") => {
            println!(
                "{}",
                file_digest(Domain::GateTool, Path::new(path)).map_err(|error| error.to_string())?
            );
            Ok(0)
        }
        [operation, path] if operation == OsStr::new("runner-identity") => {
            println!(
                "{}",
                file_digest(Domain::ProofNativeRunner, Path::new(path))
                    .map_err(|error| error.to_string())?
            );
            Ok(0)
        }
        [operation] if operation == OsStr::new("cargo-home") => {
            println!("{}", ambient_cargo_home()?.display());
            Ok(0)
        }
        [protocol, identity, challenge, program, command @ ..]
            if protocol == OsStr::new(PROTOCOL) =>
        {
            execute_protocol(identity, challenge, program, command)
        }
        _ => Err(format!(
            "usage: {PROTOCOL} IDENTITY CHALLENGE cargo ARGUMENT..."
        )),
    }
}

fn execute_protocol(
    identity: &OsStr,
    challenge: &OsStr,
    program: &OsStr,
    arguments: &[OsString],
) -> Result<i32, String> {
    let identity: Digest = text(identity, "runner identity")?
        .parse()
        .map_err(|error: IdentError| error.to_string())?;
    let challenge: Challenge = text(challenge, "challenge")?
        .parse()
        .map_err(|error: ChallengeError| error.to_string())?;
    if program != OsStr::new("cargo") || !allowed_cargo_arguments(arguments) {
        return Err(format!(
            "command is outside the compiled proof battery: {:?} {:?}",
            program, arguments
        ));
    }

    validate_self(&identity)?;
    let pins = Pins::admit()?;
    pins.validate()?;
    let workspace = env::current_dir()
        .and_then(fs::canonicalize)
        .map_err(|error| format!("cannot resolve proof workspace: {error}"))?;
    validate_source(&workspace, &pins)?;
    validate_closure(&workspace, &pins)?;

    // Everything past this point is joined with the removal below, so a refusal
    // between creating the fresh directories and running the member takes them
    // with it: a leaked root would make the next run under the same challenge
    // refuse for a reason that has nothing to do with what it was asked.
    let temporary = Temporary::fresh(&challenge)?;
    let execution = prepared(
        arguments, &workspace, &pins, &temporary, &identity, &challenge,
    );
    let cleanup = temporary.remove();
    match (execution, cleanup) {
        (Ok(status), Ok(())) => Ok(status),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(execution), Err(cleanup)) => Err(format!(
            "{execution}; native proof cleanup also failed: {cleanup}"
        )),
    }
}

/// Establish the boundary, attest it, and run the member inside it.
fn prepared(
    arguments: &[OsString],
    workspace: &Path,
    pins: &Pins,
    temporary: &Temporary,
    identity: &Digest,
    challenge: &Challenge,
) -> Result<i32, String> {
    let profile = sandbox_profile(workspace, pins, temporary)?;
    temporary.write_cargo_config(workspace)?;

    eprintln!("{ATTESTATION_PREFIX}:runner={identity}:challenge={challenge}:{ISOLATION_CLAIMS}");
    io::stderr()
        .flush()
        .map_err(|error| format!("cannot flush runner attestation: {error}"))?;

    run_sandboxed(arguments, workspace, pins, temporary, &profile)
        .map(|status| status.code().unwrap_or(1))
}

/// Admit the toolchain probe and whatever the compiled battery names, only.
///
/// The members are read back from [`Battery::current`] rather than restated
/// here. The refusal above already says "outside the compiled proof battery",
/// and a second copy of the member list is the one thing that could make that
/// sentence false: a scope changed in the battery would keep passing the
/// unchanged copy, and the receipt would publish an identity for commands this
/// boundary never actually admitted.
///
/// An argument that is not UTF-8 is refused rather than compared. Every member
/// argument is a `String`, so no such argument can be one.
fn allowed_cargo_arguments(arguments: &[OsString]) -> bool {
    let Some(observed) = arguments
        .iter()
        .map(|argument| argument.to_str())
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    // Not a member: the sealed build probes the toolchain with it before any
    // member runs, so it is admitted beside them rather than declared as one.
    if observed == ["--version"] {
        return true;
    }
    Battery::current()
        .checks()
        .iter()
        .any(|check| check.program() == "cargo" && observed == check.arguments())
}

fn validate_self(expected: &Digest) -> Result<(), String> {
    let executable = env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|error| format!("cannot resolve runner executable: {error}"))?;
    let observed = file_digest(Domain::ProofNativeRunner, &executable)
        .map_err(|error| format!("cannot identify runner executable: {error}"))?;
    if &observed != expected {
        return Err(format!(
            "runner identity changed: expected {expected}, observed {observed}"
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pins {
    source_commit: String,
    vendor_digest: Digest,
    sdk_path: PathBuf,
    developer_dir: PathBuf,
    /// The warm Cargo caches the sealed build is denied, so they cannot stand
    /// in for the vendored closure.
    ///
    /// This is sealed rather than read back from the environment because the
    /// host launches the runner with no environment to read: naming the caches
    /// is the sealing host's statement about itself, and the runner refuses
    /// below if that statement stopped describing this machine.
    cargo_home: PathBuf,
    cargo: Tool,
    rustc: Tool,
    git: Tool,
    sandbox_exec: Tool,
}

impl Pins {
    fn admit() -> Result<Self, String> {
        let document = PINS.ok_or("runner was built without its pin document")?;
        toml::from_str(document)
            .map_err(|error| format!("runner pin document is malformed: {error}"))
    }

    fn tools(&self) -> [(&'static str, &Tool); 4] {
        [
            ("cargo", &self.cargo),
            ("rustc", &self.rustc),
            ("git", &self.git),
            ("sandbox-exec", &self.sandbox_exec),
        ]
    }

    fn directories(&self) -> [(&'static str, &Path); 3] {
        [
            ("SDK", &self.sdk_path),
            ("developer directory", &self.developer_dir),
            ("Cargo home", &self.cargo_home),
        ]
    }

    /// Every path outside the workspace this run was sealed to reach.
    ///
    /// The two halves are the ones already declared above rather than a third
    /// list, so a pin added to the document arrives here without being
    /// remembered twice. What reads this is [`denied_parent`], which needs to
    /// know what a denial of the workspace's neighbourhood would take away.
    fn sealed_paths(&self) -> Vec<(&'static str, &Path)> {
        let mut paths = self.directories().to_vec();
        paths.extend(self.tools().map(|(name, tool)| (name, tool.path.as_path())));
        paths
    }

    fn validate(&self) -> Result<(), String> {
        if env::consts::OS != "macos" || env::consts::ARCH != "aarch64" {
            return Err(format!(
                "runner requires native aarch64 macOS, observed {} {}",
                env::consts::OS,
                env::consts::ARCH
            ));
        }
        for (name, tool) in self.tools() {
            tool.validate(name)?;
        }
        for (label, path) in self.directories() {
            if !path.is_dir() {
                return Err(format!(
                    "pinned {label} is not a directory: {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }
}

/// One executable this runner is sealed against.
///
/// The pin is the exact bytes at an exact absolute path. A tool cannot report
/// a version its bytes do not already determine, so there is nothing to ask it:
/// every pinned tool is exercised for real on the path that uses it -- `git`
/// reading the source, `sandbox-exec` establishing the boundary, and `cargo`
/// driving `rustc` inside it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tool {
    path: PathBuf,
    digest: Digest,
}

impl Tool {
    fn validate(&self, name: &str) -> Result<(), String> {
        if !self.path.is_absolute() {
            return Err(format!("pinned {name} path is not absolute"));
        }
        let canonical = fs::canonicalize(&self.path)
            .map_err(|error| format!("cannot resolve pinned {name}: {error}"))?;
        if canonical != self.path {
            return Err(format!(
                "pinned {name} path moved: expected {}, observed {}",
                self.path.display(),
                canonical.display()
            ));
        }
        let observed = file_digest(Domain::GateTool, &self.path)
            .map_err(|error| format!("cannot identify pinned {name}: {error}"))?;
        if observed != self.digest {
            return Err(format!(
                "pinned {name} changed: expected {}, observed {observed}",
                self.digest
            ));
        }
        Ok(())
    }
}

fn validate_source(workspace: &Path, pins: &Pins) -> Result<(), String> {
    let commit = git_one_line(
        &pins.git.path,
        workspace,
        ["rev-parse", "--verify", "HEAD^{commit}"],
    )?;
    if commit != pins.source_commit {
        return Err(format!(
            "this runner is sealed for source commit {} but the worktree HEAD is {}; \
             reseal and rebind it for this worktree with dist/proof-runner/build.sh",
            pins.source_commit, commit
        ));
    }
    let tracked = git_status(
        &pins.git.path,
        workspace,
        ["diff-index", "--quiet", "HEAD", "--"],
    )?;
    if !tracked.success() {
        return Err("tracked source differs from the pinned commit".to_owned());
    }
    let untracked = git_text(
        &pins.git.path,
        workspace,
        ["ls-files", "--others", "--exclude-standard"],
    )?;
    if let Some(path) = untracked.lines().find(|path| !release_output_path(path)) {
        return Err(format!(
            "untracked source is outside release output: {path}"
        ));
    }
    Ok(())
}

fn release_output_path(path: &str) -> bool {
    path.starts_with("dist/stage/objects/")
        || path.starts_with("dist/stage/targets/")
        || path == "dist/stage/release/manifest.json"
}

fn validate_closure(workspace: &Path, pins: &Pins) -> Result<(), String> {
    let vendor = workspace.join(VENDOR_RELATIVE);
    let observed = closure_digest(&vendor).map_err(|error| error.to_string())?;
    if observed != pins.vendor_digest {
        return Err(format!(
            "vendor closure changed: expected {}, observed {}",
            pins.vendor_digest, observed
        ));
    }
    Ok(())
}

fn git_one_line<const N: usize>(
    git: &Path,
    workspace: &Path,
    arguments: [&str; N],
) -> Result<String, String> {
    let bytes = git_bytes(git, workspace, arguments)?;
    one_line(&bytes, "Git source observation")
}

fn git_text<const N: usize>(
    git: &Path,
    workspace: &Path,
    arguments: [&str; N],
) -> Result<String, String> {
    let bytes = git_bytes(git, workspace, arguments)?;
    std::str::from_utf8(&bytes)
        .map(|value| value.trim_end_matches(['\r', '\n']).to_owned())
        .map_err(|_| "Git source observation is not UTF-8".to_owned())
}

fn git_bytes<const N: usize>(
    git: &Path,
    workspace: &Path,
    arguments: [&str; N],
) -> Result<Vec<u8>, String> {
    let output = Command::new(git)
        .args(arguments)
        .env_clear()
        .current_dir(workspace)
        .output()
        .map_err(|error| format!("cannot inspect pinned source: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "pinned source inspection failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim_end()
        ));
    }
    Ok(output.stdout)
}

fn git_status<const N: usize>(
    git: &Path,
    workspace: &Path,
    arguments: [&str; N],
) -> Result<ExitStatus, String> {
    Command::new(git)
        .args(arguments)
        .env_clear()
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status()
        .map_err(|error| format!("cannot inspect pinned source: {error}"))
}

struct Temporary {
    root: PathBuf,
    cargo_home: PathBuf,
    cargo_target: PathBuf,
    home: PathBuf,
    temporary: PathBuf,
}

/// Restore this process's access to every real directory in its private tree.
///
/// `symlink_metadata` and `DirEntry::file_type` both inspect links themselves,
/// so a member cannot make cleanup chmod or descend into a target outside the
/// runner-owned root.
fn restore_owner_directory_access(root: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let metadata = fs::symlink_metadata(root)?;
        if !metadata.file_type().is_dir() {
            return Ok(());
        }
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o700);
        fs::set_permissions(root, permissions)?;
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                restore_owner_directory_access(&entry.path())?;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
}

impl Temporary {
    fn fresh(challenge: &Challenge) -> Result<Self, String> {
        let root = Path::new("/private/tmp").join(format!("spice-proof-native-{challenge}"));
        fs::create_dir(&root).map_err(|error| {
            format!(
                "cannot create fresh native proof root {}: {error}",
                root.display()
            )
        })?;
        let instance = Self {
            cargo_home: root.join("cargo-home"),
            cargo_target: root.join("cargo-target"),
            home: root.join("home"),
            temporary: root.join("tmp"),
            root,
        };
        for path in [
            &instance.cargo_home,
            &instance.cargo_target,
            &instance.home,
            &instance.temporary,
        ] {
            fs::create_dir(path).map_err(|error| {
                format!("cannot create fresh proof path {}: {error}", path.display())
            })?;
        }
        Ok(instance)
    }

    fn write_cargo_config(&self, workspace: &Path) -> Result<(), String> {
        let vendor = toml_string(&workspace.join(VENDOR_RELATIVE))?;
        let config = format!(
            concat!(
                "[source.crates-io]\n",
                "replace-with = \"vendored-sources\"\n\n",
                "[source.vendored-sources]\n",
                "directory = \"{}\"\n\n",
                "[net]\n",
                "offline = true\n"
            ),
            vendor
        );
        fs::write(self.cargo_home.join("config.toml"), config)
            .map_err(|error| format!("cannot write fresh Cargo configuration: {error}"))
    }

    fn remove(self) -> Result<(), String> {
        restore_owner_directory_access(&self.root)
            .and_then(|()| fs::remove_dir_all(&self.root))
            .map_err(|error| {
                format!(
                    "cannot remove native proof root {}: {error}",
                    self.root.display()
                )
            })
    }
}

/// Where the sealing host keeps the warm Cargo caches the sealed build is denied.
///
/// Only sealing asks this, and it asks the runner rather than the shell so
/// every pin the sealed build enforces was computed by the one implementation
/// that enforces it. The sealed runner reads the answer back from its pins,
/// because the host launches it with no environment left to read.
#[expect(
    clippy::disallowed_methods,
    reason = "the runner rim owns ambient acquisition of the caches it denies"
)]
fn ambient_cargo_home() -> Result<PathBuf, String> {
    if let Some(home) = env::var_os("CARGO_HOME") {
        return Ok(PathBuf::from(home));
    }
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".cargo"))
        .ok_or_else(|| "the sealing environment names no Cargo home".to_owned())
}

/// The host dependency roots a sealed build is denied.
///
/// Every entry is a package manager prefix or the third-party framework
/// directory, which is where a `-sys` crate's build script goes looking when it
/// probes for a library to link. The container half cannot see any of them
/// because it runs where the host filesystem is absent, so a build that found
/// one here would link a dependency that half could never link, and both
/// receipts would still read `host-dependencies=denied`. The pinned SDK,
/// developer directory and toolchain are deliberately absent from this list:
/// the build is meant to read those, and `/Library/Developer` stays readable
/// precisely because the denial names `/Library/Frameworks` and not `/Library`.
const HOST_DEPENDENCY_ROOTS: [&str; 6] = [
    "/Library/Frameworks",
    "/nix",
    "/opt/homebrew",
    "/opt/local",
    "/sw",
    "/usr/local",
];

/// The roots above as one denial in the sandbox dialect.
///
/// Spelled here rather than inside the profile literal so the fixture proving
/// the denial bites can run the bytes this runner ships instead of a restated
/// copy of them, which is the one difference a restatement could not show.
fn host_dependency_denials() -> String {
    let mut denial = "(deny file-read*".to_owned();
    for root in HOST_DEPENDENCY_ROOTS {
        let _ = write!(denial, " (subpath \"{root}\")");
    }
    denial.push_str(")\n");
    denial
}

/// The directory whose contents the boundary takes away, or why it cannot.
///
/// `(allow default)` leaves everything no rule names both readable and
/// writable, and on a machine that keeps its checkouts together that is every
/// other checkout: ten peer worktrees sit beside this one here. A sealed build
/// reading a neighbour's tree would be reading source no receipt names and no
/// pin covers, and one writing to it would be destroying a lane it was never
/// told about while still attesting a clean boundary. Neither is a difference
/// the receipt could show afterward, so the directory holding the workspace is
/// denied for both and only the paths this profile itself names are given back.
///
/// A pinned path under that parent refuses the run instead, because the denial
/// would take it with nothing to allow it back from and the build would then
/// fail somewhere far from the cause. The rustup toolchain root is not pinned by
/// name and does not need to be: `dist/proof-runner/build.sh` seals the compiler
/// `rustup which rustc` resolves, and that binary lives inside the root -- here
/// `rustc --print sysroot` answers exactly the grandparent of the pinned path --
/// so a toolchain under the parent brings the `rustc` pin under it and refuses.
///
/// A pin below the workspace is exempt, because the workspace is read back
/// whole and reading is the whole of what a pin is reached through -- the
/// sealed run is handed the workspace read-only either way. Nothing else is
/// exempt: a path is compared as the pin document spells it, which for a tool
/// is the canonical path [`Tool::validate`] already insisted on.
fn denied_parent<'a>(workspace: &'a Path, pins: &Pins) -> Result<&'a Path, String> {
    let parent = workspace.parent().ok_or_else(|| {
        format!(
            "proof workspace {} has no parent, so the boundary has no neighbourhood to take",
            workspace.display()
        )
    })?;
    for (label, path) in pins.sealed_paths() {
        if path.starts_with(parent) && !path.starts_with(workspace) {
            return Err(format!(
                "pinned {label} {} sits beside the proof workspace, under {}, which the \
                 boundary denies so no neighbouring checkout is readable or writable; the \
                 sealed build would lose it with nothing to allow it back from",
                path.display(),
                parent.display()
            ));
        }
    }
    Ok(parent)
}

/// Where this workspace's repository actually lives.
///
/// A linked worktree keeps it beside the workspace rather than inside it: every
/// lane of this repository is one, and answers here with a sibling directory.
/// That is why the denial above cannot be the whole rule.
fn git_common_dir(workspace: &Path, git: &Path) -> Result<PathBuf, String> {
    let common = git_one_line(
        git,
        workspace,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    fs::canonicalize(common)
        .map_err(|error| format!("cannot resolve Git common directory: {error}"))
}

/// The boundary the attestation above claims, written in the sandbox dialect.
///
/// Loopback is admitted because the battery's own corpus binds it: the members
/// stand up servers on `127.0.0.1` and talk to them, so a boundary that denied
/// loopback would not be isolating the proof, it would be deleting it. What the
/// proof needs denied is a route off this host, which is the same boundary the
/// container half establishes with a network namespace that has nothing but a
/// loopback interface in it.
///
/// `(allow default)` opens the profile, so every path this build reads is
/// readable unless a later rule takes it away -- the dialect resolves a path by
/// its last matching rule. That is why [`HOST_DEPENDENCY_ROOTS`] is denied
/// below rather than an allow-list of everything a Rust toolchain touches being
/// enumerated here, which would be a second and weaker copy of the toolchain's
/// own knowledge of itself. It also means this profile still denies less than
/// the container half does, which has no host filesystem to deny.
///
/// The one allow-list here is the pair [`denied_parent`] forces: the
/// workspace's neighbourhood is denied for reading and writing alike, then the
/// the parent's own metadata and the workspace, its repository and the run's
/// scratch root are read back. Resolving one of those admitted descendants asks
/// the kernel for metadata on its parent first; Cargo does exactly that while it
/// translates compiler dep-info. The literal metadata allowance reveals neither
/// the parent's entries nor any peer's metadata or bytes. What the run is allowed
/// to write is one path and it is named by the last rule in the profile, so a
/// scratch root that happens to sit inside the neighbourhood is still writable
/// while the workspace and its repository stay read-only, which they already
/// were two rules above.
///
/// That pair sits above every later denial on purpose. Read downward, nothing
/// below it is weakened -- the warm Cargo caches and the host dependency roots
/// still resolve to their denials even for a workspace that happens to contain
/// one -- so what the pair adds is subtraction only, which is the whole of what
/// it is for.
fn sandbox_profile(workspace: &Path, pins: &Pins, temporary: &Temporary) -> Result<String, String> {
    let parent = denied_parent(workspace, pins)?;
    let git_common = git_common_dir(workspace, &pins.git.path)?;
    compose_sandbox_profile(workspace, parent, &git_common, pins, temporary)
}

#[cfg(test)]
fn sandbox_profile_with_common_dir(
    workspace: &Path,
    git_common: &Path,
    pins: &Pins,
    temporary: &Temporary,
) -> Result<String, String> {
    let parent = denied_parent(workspace, pins)?;
    compose_sandbox_profile(workspace, parent, git_common, pins, temporary)
}

fn compose_sandbox_profile(
    workspace: &Path,
    parent: &Path,
    git_common: &Path,
    pins: &Pins,
    temporary: &Temporary,
) -> Result<String, String> {
    let host_registry = pins.cargo_home.join("registry");
    let host_git = pins.cargo_home.join("git");
    Ok(format!(
        concat!(
            "(version 1)\n",
            "(allow default)\n",
            "(deny network*)\n",
            "(allow network-bind (local ip \"localhost:*\"))\n",
            "(allow network-inbound (local ip \"localhost:*\"))\n",
            "(allow network-outbound (remote ip \"localhost:*\"))\n",
            "(deny file-write* (subpath \"{}\") (subpath \"{}\"))\n",
            "(deny file-read* file-write* (subpath \"{}\"))\n",
            "(allow file-read-metadata (literal \"{}\"))\n",
            "(allow file-read* (subpath \"{}\") (subpath \"{}\") (subpath \"{}\"))\n",
            "(deny file-read* (subpath \"{}\") (subpath \"{}\"))\n",
            "{}",
            "(allow file-write* (subpath \"{}\"))\n"
        ),
        sandbox_string(workspace)?,
        sandbox_string(git_common)?,
        sandbox_string(parent)?,
        sandbox_string(parent)?,
        sandbox_string(workspace)?,
        sandbox_string(git_common)?,
        sandbox_string(&temporary.root)?,
        sandbox_string(&host_registry)?,
        sandbox_string(&host_git)?,
        host_dependency_denials(),
        sandbox_string(&temporary.root)?,
    ))
}

fn run_sandboxed(
    arguments: &[OsString],
    workspace: &Path,
    pins: &Pins,
    temporary: &Temporary,
    profile: &str,
) -> Result<ExitStatus, String> {
    let cargo_dir = pins
        .cargo
        .path
        .parent()
        .ok_or_else(|| "pinned Cargo has no parent directory".to_owned())?;
    let path = env::join_paths([cargo_dir, Path::new("/usr/bin"), Path::new("/bin")])
        .map_err(|error| format!("cannot construct sealed PATH: {error}"))?;
    Command::new(&pins.sandbox_exec.path)
        .arg("-p")
        .arg(profile)
        .arg(&pins.cargo.path)
        .args(arguments)
        .env_clear()
        .env("CARGO", &pins.cargo.path)
        .env("CARGO_HOME", &temporary.cargo_home)
        .env("CARGO_TARGET_DIR", &temporary.cargo_target)
        .env("CARGO_BUILD_BUILD_DIR", temporary.root.join("build"))
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_NET_OFFLINE", "true")
        .env("DEVELOPER_DIR", &pins.developer_dir)
        .env("HOME", &temporary.home)
        .env("NO_COLOR", "true")
        .env("PATH", path)
        .env("RUSTC", &pins.rustc.path)
        .env("RUSTC_WRAPPER", "")
        .env("RUSTC_WORKSPACE_WRAPPER", "")
        .env("SDKROOT", &pins.sdk_path)
        .env("TMPDIR", &temporary.temporary)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .status()
        .map_err(|error| format!("cannot execute pinned sandbox boundary: {error}"))
}

/// Digest of what the vendored closure contains.
///
/// Each file is digested under its own domain and the listing of those digests
/// is digested under another, the same framing a gate jurisdiction identifies
/// its tree with, so a file whose bytes happen to spell a listing cannot be
/// mistaken for one.
fn closure_digest(root: &Path) -> io::Result<Digest> {
    let root = fs::canonicalize(root)?;
    let mut files = Vec::new();
    collect_files(&root, &root, &mut files)?;
    files.sort();
    let mut listing = String::new();
    for relative in files {
        let spelling = relative.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "vendor path is not UTF-8")
        })?;
        let bytes = fs::read(root.join(&relative))?;
        let _ = writeln!(
            listing,
            "{} {spelling}",
            Digest::compute(Domain::ProofNativeClosureFile, &bytes)
        );
    }
    Ok(Digest::compute(
        Domain::ProofNativeClosure,
        listing.as_bytes(),
    ))
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("vendor closure contains symlink {}", path.display()),
            ));
        }
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else if metadata.is_file() {
            files.push(
                path.strip_prefix(root)
                    .expect("recursive vendor path remains below root")
                    .to_path_buf(),
            );
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("vendor closure contains special file {}", path.display()),
            ));
        }
    }
    Ok(())
}

fn file_digest(domain: Domain, path: &Path) -> io::Result<Digest> {
    fs::read(path).map(|bytes| Digest::compute(domain, &bytes))
}

fn text<'a>(value: &'a OsStr, label: &str) -> Result<&'a str, String> {
    value
        .to_str()
        .ok_or_else(|| format!("{label} is not UTF-8"))
}

fn one_line(bytes: &[u8], label: &str) -> Result<String, String> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| format!("{label} output is not UTF-8"))?
        .trim_end_matches(['\r', '\n']);
    if value.is_empty() || value.contains(['\r', '\n']) {
        return Err(format!("{label} output is not one nonempty line"));
    }
    Ok(value.to_owned())
}

fn toml_string(path: &Path) -> Result<String, String> {
    escaped_path(path, "TOML")
}

fn sandbox_string(path: &Path) -> Result<String, String> {
    escaped_path(path, "sandbox profile")
}

fn escaped_path(path: &Path, label: &str) -> Result<String, String> {
    let spelling = path
        .to_str()
        .ok_or_else(|| format!("{label} path is not UTF-8: {}", path.display()))?;
    if spelling.contains(['"', '\\', '\n', '\r']) {
        return Err(format!(
            "{label} path contains an unsupported character: {}",
            path.display()
        ));
    }
    Ok(spelling.to_owned())
}

#[cfg(test)]
mod tests;
