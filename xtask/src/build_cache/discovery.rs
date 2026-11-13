//! One Cargo metadata observation for each linked worktree.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use super::BuildCacheError;

pub(super) struct CargoDirectoryInventory {
    pub(super) current: CargoDirectories,
    pub(super) linked: Vec<LinkedCargoDirectories>,
}

pub(super) struct LinkedCargoDirectories {
    pub(super) worktree: PathBuf,
    pub(super) directories: CargoDirectories,
}

#[derive(Deserialize)]
pub(super) struct CargoDirectories {
    pub(super) build_directory: PathBuf,
    pub(super) target_directory: PathBuf,
}

pub(super) fn discover_cargo_directories(
    workspace: &Path,
) -> Result<CargoDirectoryInventory, BuildCacheError> {
    let current = metadata_directories(workspace)?;
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(workspace)
        .output()
        .map_err(|source| BuildCacheError::Launch {
            program: "git",
            source,
        })?;
    if !output.status.success() {
        return Err(BuildCacheError::CommandFailed {
            program: "git",
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| BuildCacheError::Observation {
        label: "git worktree inventory",
        observed: "non-UTF-8 output".to_owned(),
    })?;
    let canonical_workspace =
        fs::canonicalize(workspace).map_err(|source| BuildCacheError::Io {
            path: workspace.to_path_buf(),
            source,
        })?;
    let mut linked = Vec::new();
    for line in text.lines() {
        let Some(path) = line.strip_prefix("worktree ") else {
            continue;
        };
        let worktree = PathBuf::from(path);
        let canonical = fs::canonicalize(&worktree).map_err(|source| BuildCacheError::Io {
            path: worktree.clone(),
            source,
        })?;
        if canonical == canonical_workspace || !canonical.join("Cargo.toml").is_file() {
            continue;
        }
        linked.push(LinkedCargoDirectories {
            directories: metadata_directories(&canonical)?,
            worktree: canonical,
        });
    }
    linked.sort_by(|left, right| left.worktree.cmp(&right.worktree));
    Ok(CargoDirectoryInventory { current, linked })
}

fn metadata_directories(workspace: &Path) -> Result<CargoDirectories, BuildCacheError> {
    let output = Command::new(env!("CARGO"))
        .arg("metadata")
        .arg("--no-deps")
        .arg("--format-version")
        .arg("1")
        .arg("--offline")
        .current_dir(workspace)
        .output()
        .map_err(|source| BuildCacheError::Launch {
            program: env!("CARGO"),
            source,
        })?;
    if !output.status.success() {
        return Err(BuildCacheError::CommandFailed {
            program: env!("CARGO"),
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|source| BuildCacheError::Json {
        label: "cargo metadata",
        source,
    })
}
