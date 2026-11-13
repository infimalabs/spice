//! One command for a point-in-time Darwin build that disturbs no other
//! worktree.
//!
//! `seal-darwin` seals this worktree's native proof runner at HEAD through
//! `dist/proof-runner/build.sh`, which binds it to this worktree's own
//! `proof.runner`, then cuts the release source archive from HEAD, stages the
//! sealed `aarch64-apple-darwin` target from that archive and, when asked,
//! places the final executable under an output directory. Every
//! write lands in this worktree's `target/` and `dist/stage/`, its own
//! Git-directory configuration, or the named output directory. The
//! repository's installed appliance, its resident, the Git-common
//! configuration, and every other worktree are never touched, so lanes seal
//! independently and never inherit one another's runner.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use sha2::{Digest as _, Sha256};

/// The repository-owned script that seals and binds the native proof runner.
const SEAL_SCRIPT: &str = "dist/proof-runner/build.sh";
/// The name the placed executable takes, the one a cold install expects.
const EXECUTABLE: &str = "spice";
/// Read granularity for digesting the placed copy.
const CHUNK: usize = 1 << 20;

/// One executable placed by `seal-darwin --output`, read back after placement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlacedExecutable {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

/// Seal this worktree's native proof runner at HEAD and bind it here.
///
/// Runs the repository script with the worktree as its working directory and
/// its output passed straight through, so the seal's own refusals (a dirty
/// tree, a foreign host, a failed attestation) reach the operator verbatim.
///
/// # Errors
///
/// The script could not start or did not exit successfully.
pub fn seal_runner(root: &Path, output: &mut impl Write) -> Result<(), SealDarwinError> {
    writeln!(
        output,
        "[seal-darwin] sealing the native proof runner at HEAD"
    )
    .map_err(SealDarwinError::Output)?;
    output.flush().map_err(SealDarwinError::Output)?;
    let script = root.join(SEAL_SCRIPT);
    let status = Command::new("/bin/sh")
        .arg(&script)
        .current_dir(root)
        .status()
        .map_err(|source| SealDarwinError::Io {
            path: script.clone(),
            source,
        })?;
    if !status.success() {
        return Err(SealDarwinError::Seal { script, status });
    }
    Ok(())
}

/// Place the staged executable under `directory` as `spice`, through a staged
/// sibling and one rename, and answer the digest of the bytes read back.
///
/// # Errors
///
/// Any failure creating the directory, copying, renaming, or reading back.
pub fn place_executable(
    executable: &Path,
    directory: &Path,
    output: &mut impl Write,
) -> Result<PlacedExecutable, SealDarwinError> {
    fs::create_dir_all(directory).map_err(|source| SealDarwinError::Io {
        path: directory.to_path_buf(),
        source,
    })?;
    let path = directory.join(EXECUTABLE);
    let staged = directory.join(format!(".{EXECUTABLE}.{}", std::process::id()));
    let placed = fs::copy(executable, &staged)
        .and_then(|_| fs::rename(&staged, &path))
        .map_err(|source| SealDarwinError::Io {
            path: path.clone(),
            source,
        });
    if placed.is_err() {
        let _ = fs::remove_file(&staged);
    }
    placed?;
    let (sha256, bytes) = digest(&path)?;
    writeln!(
        output,
        "[seal-darwin] placed {} sha256={sha256} bytes={bytes}",
        path.display()
    )
    .map_err(SealDarwinError::Output)?;
    Ok(PlacedExecutable {
        path,
        sha256,
        bytes,
    })
}

fn digest(path: &Path) -> Result<(String, u64), SealDarwinError> {
    let io = |source| SealDarwinError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = File::open(path).map_err(io)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; CHUNK];
    let mut bytes = 0;
    loop {
        let read = file.read(&mut buffer).map_err(io)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes += read as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), bytes))
}

#[derive(Debug)]
pub enum SealDarwinError {
    Seal { script: PathBuf, status: ExitStatus },
    Io { path: PathBuf, source: io::Error },
    Output(io::Error),
}

impl fmt::Display for SealDarwinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Seal { script, status } => write!(
                formatter,
                "the native proof runner seal {} refused ({status}); nothing was staged",
                script.display()
            ),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Output(source) => write!(formatter, "seal-darwin output failed: {source}"),
        }
    }
}

impl std::error::Error for SealDarwinError {}
