//! Linux release staging inside the sealed image, for hosts that cannot link
//! or execute the Linux release themselves.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use super::{
    CARGO_HOME, CARGO_TARGET, Container, ContainerError, ExecutionCompletion, RUN_PLATFORM,
    TEMPORARY, WORKSPACE_MOUNT,
};

/// Where the release pipeline publishes target payloads, relative to the
/// workspace. The only workspace path the staging container may write.
const STAGE: &str = "dist/stage";
const WORKSPACE_STAGE: &str = "/workspace/dist/stage";

/// The one Linux release target the sealed image links and executes.
const LINUX_TARGET: &str = "x86_64-unknown-linux-musl";

/// The environment name that hands `stage-target` the identity of the source
/// archive whose exact tree is the workspace.
const SOURCE_ARCHIVE: &str = "SPICE_SOURCE_ARCHIVE"; // env-policy: allow
/// The environment name that redirects the staged payload outside the
/// read-only workspace.
const STAGE_ROOT: &str = "SPICE_HOSTED_STAGE"; // env-policy: allow

/// The environment names that hand `stage-target` the provider-built canvas
/// every native release target links unchanged, and its app generation.
const RELEASE_CANVAS: &str = "SPICE_RELEASE_CANVAS"; // env-policy: allow
const RELEASE_APP: &str = "SPICE_RELEASE_APP"; // env-policy: allow

/// Where the provider-built canvas is mounted read-only inside the image.
const CANVAS_MOUNT: &str = "/release-inputs/canvas.zip";

/// The provider-built canvas a delivered stage links, and the app generation
/// it carries.
#[derive(Clone, Copy, Debug)]
pub struct ProviderCanvas<'a> {
    pub path: &'a Path,
    pub app: &'a str,
}

/// Where the Windows cross build's one writable host directory is mounted,
/// outside the read-only workspace.
const CROSS_OUT: &str = "/windows-cross";
/// Writable private stage used only while reconstructing detached symbols.
const SYMBOL_STAGE: &str = "/symbol-stage";
const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";

/// Label that marks a persistent sealed Cargo target with the purpose it
/// serves, so the first run under a new image finds the volumes it supersedes.
const TARGET_PURPOSE_LABEL: &str = "org.spice.release-target";

/// Purposes of the persistent sealed Cargo targets, one volume each per image.
const LINUX_STAGE_PURPOSE: &str = "linux-stage";
const WINDOWS_CROSS_PURPOSE: &str = "windows-cross";

/// The Cargo target one sealed release run compiles into.
#[derive(Debug)]
enum SealedTarget {
    /// A caller-owned host directory the run leaves behind.
    Host(PathBuf),
    /// A runtime volume kept for one purpose and image across runs, so Cargo
    /// freshness rather than a cold build decides what compiles.
    Volume(String),
    /// An anonymous volume `--rm` destroys with this exact container, rather
    /// than tmpfs: a release link and the `xtask` it runs outgrow a default
    /// Docker VM's memory.
    Scratch,
}

impl SealedTarget {
    fn mount(&self) -> String {
        match self {
            Self::Host(path) => format!(
                "type=bind,source={},destination={CARGO_TARGET}",
                path.display()
            ),
            Self::Volume(name) => format!("type=volume,source={name},destination={CARGO_TARGET}"),
            Self::Scratch => format!("type=volume,destination={CARGO_TARGET}"),
        }
    }
}

/// One Windows cross build: the cross image layered over this vendor set's
/// image and the MSVC generation it carries, the repository's Git common
/// directory, the host output directory, and the source and canvas
/// [`Container::stage_release`] also takes.
#[derive(Clone, Copy, Debug)]
pub struct WindowsCross<'a> {
    pub cross_image: &'a str,
    pub msvc_generation: &'a str,
    pub git_common: &'a Path,
    pub out: &'a Path,
    pub delivered: Option<&'a str>,
    pub canvas: Option<ProviderCanvas<'a>>,
}

/// One completed staging run: the in-image command's exit and both streams.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseStage {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Container {
    /// Stage the unsigned Linux release target with the image's own toolchain.
    ///
    /// The image compiles the workspace's `xtask` and runs its native
    /// `stage-target`, so the musl link uses the linker the image's root Cargo
    /// config pins and the version report executes the Linux executable on
    /// Linux. The workspace and its Git common directory are mounted
    /// read-only at the paths Git records for them, with `dist/stage` the one
    /// writable workspace path. The Cargo home is fresh scratch owned by this
    /// run and the network is disabled, so neither the host's Cargo
    /// configuration nor its tools reach the build. When `target` names a host
    /// directory, the run mounts it writable as its Cargo target and leaves it
    /// behind: a caller restores a prepared third-party layer into it
    /// beforehand and may publish it after. Otherwise the target is this
    /// image's persistent Linux staging volume, so a later stage under the
    /// same image compiles only what Cargo finds stale; when the runtime
    /// cannot provide that volume, the run compiles in fresh scratch and says
    /// so first on its standard error.
    ///
    /// Without `delivered`, the app generation is read from the workspace as
    /// already staged, never compiled here. With `delivered`, the workspace is
    /// the exact tree of that source archive identity. A `canvas` is mounted
    /// read-only and linked unchanged; without one the image builds the app
    /// itself, offline, into its own scratch, so no host toolchain touches any
    /// part of the release.
    ///
    /// # Errors
    ///
    /// Refuses a Git common directory, stage directory, canvas path, or target
    /// directory the OCI mount grammar cannot represent, and returns runtime
    /// launch, wait, and capture failures or a run the deadline or a
    /// cancellation ended.
    pub fn stage_release(
        &self,
        git_common: &Path,
        delivered: Option<&str>,
        canvas: Option<ProviderCanvas<'_>>,
        target: Option<&Path>,
        stage: Option<&Path>,
        capture_symbols: bool,
    ) -> Result<ReleaseStage, ContainerError> {
        let (stage, destination, redirected) = match stage {
            Some(stage) => (stage.to_path_buf(), SYMBOL_STAGE, true),
            None => (self.workspace.join(STAGE), WORKSPACE_STAGE, false),
        };
        fs::create_dir_all(&stage).map_err(|source| ContainerError::InspectWorkspace {
            path: stage.clone(),
            source,
        })?;
        if redirected {
            Self::make_bind_source_writable(&stage)?;
        }
        let (target, notice) = match target {
            Some(path) => {
                Self::make_bind_source_writable(path)?;
                (SealedTarget::Host(representable(path)?), None)
            }
            None => self.persistent_target(LINUX_STAGE_PURPOSE, self.image.as_str()),
        };
        let mut runtime_arguments = self.sealed_release_arguments(
            git_common,
            &stage,
            destination,
            delivered,
            canvas,
            &target,
        )?;
        if redirected {
            runtime_arguments.extend(["--env".to_owned(), format!("{STAGE_ROOT}={SYMBOL_STAGE}")]);
        }
        runtime_arguments.extend([
            self.image.to_string(),
            "cargo".to_owned(),
            "run".to_owned(),
            "--locked".to_owned(),
            "-p".to_owned(),
            "xtask".to_owned(),
            "--".to_owned(),
            "stage-target".to_owned(),
            LINUX_TARGET.to_owned(),
            "--unsigned".to_owned(),
            "--proof-container".to_owned(),
            self.image.to_string(),
        ]);
        if delivered.is_none() {
            runtime_arguments.push("--staged-app".to_owned());
        }
        if capture_symbols {
            runtime_arguments.push("--symbols".to_owned());
        }
        let run = self.sealed_release_run(
            runtime_arguments,
            notice,
            "sealed Linux release staging",
            "complete Linux release staging",
        )?;
        if redirected {
            if let SealedTarget::Host(path) = &target {
                self.make_bind_descendants_removable(
                    self.image.as_str(),
                    &[
                        (stage.as_path(), SYMBOL_STAGE),
                        (path.as_path(), CARGO_TARGET),
                    ],
                )?;
            } else {
                self.make_bind_descendants_removable(
                    self.image.as_str(),
                    &[(stage.as_path(), SYMBOL_STAGE)],
                )?;
            }
        }
        Ok(run)
    }

    /// Rebuild a Windows candidate and its PDB inside the exact cross image.
    ///
    /// The source, provider canvas, Cargo target, and writable stage use the
    /// same sealed boundary as the canonical cross build. The cross image
    /// carries the admitted MSVC closure; the proof image identity remains the
    /// one recorded in the generated release recipe.
    pub fn stage_windows_symbols(
        &self,
        cross_image: &str,
        git_common: &Path,
        stage: &Path,
        delivered: &str,
        canvas: ProviderCanvas<'_>,
    ) -> Result<ReleaseStage, ContainerError> {
        fs::create_dir_all(stage).map_err(|source| ContainerError::InspectWorkspace {
            path: stage.to_path_buf(),
            source,
        })?;
        Self::make_bind_source_writable(stage)?;
        let (target, notice) = self.persistent_target(WINDOWS_CROSS_PURPOSE, cross_image);
        let mut runtime_arguments = self.sealed_release_arguments(
            git_common,
            stage,
            SYMBOL_STAGE,
            Some(delivered),
            Some(canvas),
            &target,
        )?;
        runtime_arguments.extend([
            "--env".to_owned(),
            format!("{STAGE_ROOT}={SYMBOL_STAGE}"),
            cross_image.to_owned(),
            "cargo".to_owned(),
            "run".to_owned(),
            "--locked".to_owned(),
            "-p".to_owned(),
            "xtask".to_owned(),
            "--".to_owned(),
            "stage-target".to_owned(),
            WINDOWS_TARGET.to_owned(),
            "--unsigned".to_owned(),
            "--proof-container".to_owned(),
            self.image.to_string(),
            "--symbols".to_owned(),
        ]);
        let run = self.sealed_release_run(
            runtime_arguments,
            notice,
            "sealed Windows symbol staging",
            "complete Windows symbol staging",
        )?;
        self.make_bind_descendants_removable(cross_image, &[(stage, SYMBOL_STAGE)])?;
        Ok(run)
    }

    /// Cross-build the Windows MSVC release set in `cross_image`, the Windows
    /// cross layer over this vendor set's image, writing its artifact root,
    /// build key, and command records under `out`.
    ///
    /// The workspace, Git, Cargo home, network, and canvas boundary is exactly
    /// [`Container::stage_release`]'s; `out` replaces `dist/stage` as the one
    /// writable host path, mounted outside the workspace. The Cargo target is
    /// the cross image's persistent volume, so a later build under the same
    /// cross image compiles only what Cargo finds stale; when the runtime
    /// cannot provide that volume, the run compiles in fresh scratch and says
    /// so first on its standard error.
    ///
    /// # Errors
    ///
    /// Refuses a Git common directory, output directory, or canvas path the
    /// OCI mount grammar cannot represent, and returns runtime launch, wait,
    /// and capture failures or a run the deadline or a cancellation ended.
    pub fn cross_build_windows(
        &self,
        request: WindowsCross<'_>,
    ) -> Result<ReleaseStage, ContainerError> {
        let WindowsCross {
            cross_image,
            msvc_generation,
            git_common,
            out,
            delivered,
            canvas,
        } = request;
        fs::create_dir_all(out).map_err(|source| ContainerError::InspectWorkspace {
            path: out.to_path_buf(),
            source,
        })?;
        Self::make_bind_source_writable(out)?;
        let out = representable(out)?;
        let (target, notice) = self.persistent_target(WINDOWS_CROSS_PURPOSE, cross_image);
        let mut runtime_arguments =
            self.sealed_release_arguments(git_common, &out, CROSS_OUT, delivered, canvas, &target)?;
        runtime_arguments.extend([
            cross_image.to_owned(),
            "cargo".to_owned(),
            "run".to_owned(),
            "--locked".to_owned(),
            "-p".to_owned(),
            "xtask".to_owned(),
            "--".to_owned(),
            "windows-cross-build".to_owned(),
            CROSS_OUT.to_owned(),
            "--cross-image".to_owned(),
            cross_image.to_owned(),
            "--msvc-generation".to_owned(),
            msvc_generation.to_owned(),
        ]);
        if delivered.is_none() {
            runtime_arguments.push("--staged-app".to_owned());
        }
        self.sealed_release_run(
            runtime_arguments,
            notice,
            "sealed Windows cross build",
            "complete Windows cross build",
        )
    }

    /// Restore host removability after a remapped container user populated
    /// private bind directories. Only descendant directories need write
    /// permission for the host owner to unlink the scratch tree.
    fn make_bind_descendants_removable(
        &self,
        image: &str,
        bindings: &[(&Path, &str)],
    ) -> Result<(), ContainerError> {
        let mut runtime_arguments = vec![
            "run".to_owned(),
            "--rm".to_owned(),
            "--platform".to_owned(),
            RUN_PLATFORM.to_owned(),
            "--network=none".to_owned(),
            "--read-only".to_owned(),
            "--cap-drop=ALL".to_owned(),
            "--security-opt=no-new-privileges".to_owned(),
        ];
        for (source, destination) in bindings {
            let source = representable(source)?;
            runtime_arguments.extend([
                "--mount".to_owned(),
                format!(
                    "type=bind,source={},destination={destination}",
                    source.display()
                ),
            ]);
        }
        runtime_arguments.extend([
            image.to_owned(),
            "find".to_owned(),
            bindings[0].1.to_owned(),
        ]);
        runtime_arguments.extend(
            bindings
                .iter()
                .skip(1)
                .map(|(_, destination)| (*destination).to_owned()),
        );
        runtime_arguments.extend([
            "-mindepth".to_owned(),
            "1".to_owned(),
            "-type".to_owned(),
            "d".to_owned(),
            "-exec".to_owned(),
            "chmod".to_owned(),
            "a+rwx".to_owned(),
            "{}".to_owned(),
            "+".to_owned(),
        ]);
        let execution = self.run(self.runtime_spec(
            runtime_arguments,
            "restore sealed symbol scratch removability",
        )?)?;
        if matches!(
            execution.completion,
            ExecutionCompletion::Exited { success: true, .. }
        ) {
            Ok(())
        } else {
            Err(Self::control_refusal(
                "restore sealed symbol scratch removability",
                &execution,
            ))
        }
    }

    #[cfg(unix)]
    fn make_bind_source_writable(out: &Path) -> Result<(), ContainerError> {
        fn make_directory_writable(path: &Path) -> Result<(), ContainerError> {
            let mut permissions = fs::metadata(path)
                .map_err(|source| ContainerError::InspectWorkspace {
                    path: path.to_path_buf(),
                    source,
                })?
                .permissions();
            // Docker may map its root user away from the host user that created
            // this private bind source. The sealed run has no other writable
            // host path.
            permissions.set_mode(permissions.mode() | 0o777);
            fs::set_permissions(path, permissions).map_err(|source| {
                ContainerError::InspectWorkspace {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
            for entry in fs::read_dir(path).map_err(|source| ContainerError::InspectWorkspace {
                path: path.to_path_buf(),
                source,
            })? {
                let entry = entry.map_err(|source| ContainerError::InspectWorkspace {
                    path: path.to_path_buf(),
                    source,
                })?;
                let child = entry.path();
                if entry
                    .file_type()
                    .map_err(|source| ContainerError::InspectWorkspace {
                        path: child.clone(),
                        source,
                    })?
                    .is_dir()
                {
                    make_directory_writable(&child)?;
                }
            }
            Ok(())
        }

        make_directory_writable(out)
    }

    #[cfg(not(unix))]
    fn make_bind_source_writable(_out: &Path) -> Result<(), ContainerError> {
        Ok(())
    }

    /// Runtime arguments every sealed release run shares, up to its image.
    ///
    /// The workspace and its Git common directory are read-only, `writable`
    /// is the one host path mounted writable at `destination`, the Cargo home
    /// is fresh scratch owned by this run, the Cargo target is `target`, and
    /// the network is disabled.
    fn sealed_release_arguments(
        &self,
        git_common: &Path,
        writable: &Path,
        destination: &str,
        delivered: Option<&str>,
        canvas: Option<ProviderCanvas<'_>>,
        target: &SealedTarget,
    ) -> Result<Vec<String>, ContainerError> {
        let git_common = representable(git_common)?;
        let mut runtime_arguments = vec![
            "run".to_owned(),
            "--rm".to_owned(),
            "--platform".to_owned(),
            RUN_PLATFORM.to_owned(),
            "--network=none".to_owned(),
            "--read-only".to_owned(),
            "--cap-drop=ALL".to_owned(),
            "--security-opt=no-new-privileges".to_owned(),
            "--mount".to_owned(),
            format!(
                "type=bind,source={},destination={WORKSPACE_MOUNT},readonly",
                self.workspace.display()
            ),
        ];
        // A linked worktree's `.git` names its administrative directory by
        // absolute host path, so the common directory is mounted where that
        // pointer leads. A repository whose common directory is inside the
        // workspace already has it.
        if !git_common.starts_with(&self.workspace) {
            runtime_arguments.extend([
                "--mount".to_owned(),
                format!(
                    "type=bind,source={0},destination={0},readonly",
                    git_common.display()
                ),
            ]);
        }
        runtime_arguments.extend([
            "--mount".to_owned(),
            format!(
                "type=bind,source={},destination={destination}",
                writable.display()
            ),
            "--tmpfs".to_owned(),
            format!("{CARGO_HOME}:rw,noexec,mode=0700"),
            "--mount".to_owned(),
            target.mount(),
            // Rosetta's thread-context temporary file; see `battery_spec`.
            "--tmpfs".to_owned(),
            format!("{TEMPORARY}:rw,nosuid,nodev,noexec,mode=1777"),
            "--workdir".to_owned(),
            WORKSPACE_MOUNT.to_owned(),
            "--env".to_owned(),
            format!("CARGO_HOME={CARGO_HOME}"),
            "--env".to_owned(),
            format!("CARGO_TARGET_DIR={CARGO_TARGET}"),
            "--env".to_owned(),
            "CARGO_NET_OFFLINE=true".to_owned(),
        ]);
        if let Some(delivered) = delivered {
            runtime_arguments.extend(["--env".to_owned(), format!("{SOURCE_ARCHIVE}={delivered}")]);
        }
        if let Some(canvas) = canvas {
            let path = representable(canvas.path)?;
            runtime_arguments.extend([
                "--mount".to_owned(),
                format!(
                    "type=bind,source={},destination={CANVAS_MOUNT},readonly",
                    path.display()
                ),
                "--env".to_owned(),
                format!("{RELEASE_CANVAS}={CANVAS_MOUNT}"),
                "--env".to_owned(),
                format!("{RELEASE_APP}={}", canvas.app),
            ]);
        }
        Ok(runtime_arguments)
    }

    /// The persistent Cargo target volume for `purpose` under the image
    /// `identity`, created on first use. Volumes this purpose kept for other
    /// images are removed, so disk use stays bounded to the current image; a
    /// volume another run still mounts stays until a later run. When the
    /// runtime cannot list or create the volume, the run compiles in fresh
    /// scratch and the returned notice says why.
    fn persistent_target(&self, purpose: &str, identity: &str) -> (SealedTarget, Option<String>) {
        match self.prepare_volume(purpose, identity) {
            Ok(name) => (SealedTarget::Volume(name), None),
            Err(reason) => (
                SealedTarget::Scratch,
                Some(format!(
                    "persistent {purpose} Cargo target unavailable ({reason}); compiling in fresh scratch\n"
                )),
            ),
        }
    }

    fn prepare_volume(&self, purpose: &str, identity: &str) -> Result<String, String> {
        let digest = identity.strip_prefix("sha256:").unwrap_or(identity);
        if digest.is_empty() || !digest.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(format!("image identity {identity:?} cannot name a volume"));
        }
        let name = format!("spice-release-target-{purpose}-{digest}");
        let label = format!("{TARGET_PURPOSE_LABEL}={purpose}");
        let listed = self.volume_control(
            &[
                "volume",
                "ls",
                "--quiet",
                "--filter",
                &format!("label={label}"),
            ],
            "list sealed Cargo target volumes",
        )?;
        let mut present = false;
        for existing in listed.lines().filter(|line| !line.is_empty()) {
            if existing == name {
                present = true;
            } else {
                // A refusal here is a volume another run still mounts; the
                // first run after it ends removes it.
                let _ = self.volume_control(
                    &["volume", "rm", existing],
                    "remove superseded sealed Cargo target",
                );
            }
        }
        if !present {
            self.volume_control(
                &["volume", "create", "--label", &label, &name],
                "create sealed Cargo target",
            )?;
        }
        Ok(name)
    }

    fn volume_control(
        &self,
        arguments: &[&str],
        operation: &'static str,
    ) -> Result<String, String> {
        let execution = self
            .run(
                self.runtime_spec(
                    arguments
                        .iter()
                        .map(|argument| (*argument).to_owned())
                        .collect(),
                    operation,
                )
                .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        Self::require_control(operation, &execution).map_err(|error| error.to_string())?;
        String::from_utf8(execution.stdout.bytes).map_err(|error| error.to_string())
    }

    fn sealed_release_run(
        &self,
        runtime_arguments: Vec<String>,
        notice: Option<String>,
        label: &'static str,
        operation: &'static str,
    ) -> Result<ReleaseStage, ContainerError> {
        let execution = self.run(self.runtime_spec(runtime_arguments, label)?)?;
        let ExecutionCompletion::Exited { success, code } = execution.completion else {
            return Err(Self::control_refusal(operation, &execution));
        };
        let mut stderr = notice.map(String::into_bytes).unwrap_or_default();
        stderr.extend(execution.stderr.bytes);
        Ok(ReleaseStage {
            success,
            code,
            stdout: execution.stdout.bytes,
            stderr,
        })
    }
}

/// The canonical spelling of one host path the OCI mount grammar can carry.
fn representable(path: &Path) -> Result<PathBuf, ContainerError> {
    let canonical = fs::canonicalize(path).map_err(|source| ContainerError::InspectWorkspace {
        path: path.to_path_buf(),
        source,
    })?;
    match canonical.to_str() {
        Some(spelling) if !spelling.contains([',', '\n', '\r']) => Ok(canonical),
        _ => Err(ContainerError::UnrepresentableWorkspace(canonical)),
    }
}
