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
    /// writable workspace path. The Cargo home and target are fresh scratch
    /// owned by this run, and the network is disabled, so neither the host's
    /// Cargo configuration nor its tools reach the build. The app generation
    /// is read from the workspace as already staged, never compiled here.
    ///
    /// # Errors
    ///
    /// Refuses a Git common directory or stage directory the OCI mount grammar
    /// cannot represent, and returns runtime launch, wait, and capture failures
    /// or a run the deadline or a cancellation ended.
    pub fn stage_release(&self, git_common: &Path) -> Result<ReleaseStage, ContainerError> {
        let stage = self.workspace.join(STAGE);
        fs::create_dir_all(&stage).map_err(|source| ContainerError::InspectWorkspace {
            path: stage.clone(),
            source,
        })?;
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
                "type=bind,source={},destination={WORKSPACE_MOUNT}/{STAGE}",
                stage.display()
            ),
            "--tmpfs".to_owned(),
            format!("{CARGO_HOME}:rw,noexec,mode=0700"),
            // An anonymous volume rather than tmpfs: a release link and the
            // `xtask` it runs outgrow a default Docker VM's memory, and `--rm`
            // destroys the volume with this exact container.
            "--mount".to_owned(),
            format!("type=volume,destination={CARGO_TARGET}"),
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
            "--staged-app".to_owned(),
        ]);
        let execution =
            self.run(self.runtime_spec(runtime_arguments, "sealed Linux release staging")?)?;
        let ExecutionCompletion::Exited { success, code } = execution.completion else {
            return Err(Self::control_refusal(
                "complete Linux release staging",
                &execution,
            ));
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
