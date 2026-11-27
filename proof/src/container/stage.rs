//! Linux release staging inside the sealed image, for hosts that cannot link
//! or execute the Linux release themselves.

use std::fs;
use std::path::{Path, PathBuf};

use super::{
    CARGO_HOME, CARGO_TARGET, Container, ContainerError, ExecutionCompletion, RUN_PLATFORM,
    TEMPORARY, WORKSPACE_MOUNT,
};

/// Where the release pipeline publishes target payloads, relative to the
/// workspace. The only workspace path the staging container may write.
const STAGE: &str = "dist/stage";

/// The one Linux release target the sealed image links and executes.
const LINUX_TARGET: &str = "x86_64-unknown-linux-musl";

/// The environment name that hands `stage-target` the identity of the source
/// archive whose exact tree is the workspace.
const SOURCE_ARCHIVE: &str = "SPICE_SOURCE_ARCHIVE"; // env-policy: allow

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
    /// configuration nor its tools reach the build. The Cargo target is fresh
    /// scratch too unless `target` names a host directory, which the run
    /// mounts writable as its target and leaves behind: a caller restores a
    /// prepared third-party layer into it beforehand and may publish it after.
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
    ) -> Result<ReleaseStage, ContainerError> {
        let stage = self.workspace.join(STAGE);
        fs::create_dir_all(&stage).map_err(|source| ContainerError::InspectWorkspace {
            path: stage.clone(),
            source,
        })?;
        let mut runtime_arguments = self.sealed_release_arguments(
            git_common,
            &stage,
            &format!("{WORKSPACE_MOUNT}/{STAGE}"),
            delivered,
            canvas,
            target,
        )?;
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
        self.sealed_release_run(
            runtime_arguments,
            "sealed Linux release staging",
            "complete Linux release staging",
        )
    }

    /// Cross-build the Windows MSVC release set in `cross_image`, the Windows
    /// cross layer over this vendor set's image, writing its artifact root,
    /// build key, and command records under `out`.
    ///
    /// The workspace, Git, Cargo, network, and canvas boundary is exactly
    /// [`Container::stage_release`]'s; `out` replaces `dist/stage` as the one
    /// writable host path, mounted outside the workspace.
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
        let out = representable(out)?;
        let mut runtime_arguments =
            self.sealed_release_arguments(git_common, &out, CROSS_OUT, delivered, canvas, None)?;
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
            "sealed Windows cross build",
            "complete Windows cross build",
        )
    }

    /// Runtime arguments every sealed release run shares, up to its image.
    ///
    /// The workspace and its Git common directory are read-only, `writable`
    /// is the one host path mounted writable at `destination`, the Cargo home
    /// is fresh scratch owned by this run, the Cargo target is `target` or
    /// fresh scratch, and the network is disabled.
    fn sealed_release_arguments(
        &self,
        git_common: &Path,
        writable: &Path,
        destination: &str,
        delivered: Option<&str>,
        canvas: Option<ProviderCanvas<'_>>,
        target: Option<&Path>,
    ) -> Result<Vec<String>, ContainerError> {
        let git_common = representable(git_common)?;
        let target = target.map(representable).transpose()?;
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
            // Without a named host target, an anonymous volume rather than
            // tmpfs: a release link and the `xtask` it runs outgrow a default
            // Docker VM's memory, and `--rm` destroys the volume with this
            // exact container.
            target.map_or_else(
                || format!("type=volume,destination={CARGO_TARGET}"),
                |target| {
                    format!(
                        "type=bind,source={},destination={CARGO_TARGET}",
                        target.display()
                    )
                },
            ),
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

    fn sealed_release_run(
        &self,
        runtime_arguments: Vec<String>,
        label: &'static str,
        operation: &'static str,
    ) -> Result<ReleaseStage, ContainerError> {
        let execution = self.run(self.runtime_spec(runtime_arguments, label)?)?;
        let ExecutionCompletion::Exited { success, code } = execution.completion else {
            return Err(Self::control_refusal(operation, &execution));
        };
        Ok(ReleaseStage {
            success,
            code,
            stdout: execution.stdout.bytes,
            stderr: execution.stderr.bytes,
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
