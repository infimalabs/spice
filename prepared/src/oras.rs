//! Exact ORAS resolution and execution for the prepared-artifact contract.

use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use flate2::read::GzDecoder;
use tar::Archive;

use super::{PreparedArtifactError, Scratch, sha256_file};

const ORAS_VERSION: &str = "1.3.0";

/// Distinguishes concurrent provisions within one process.
static STAGING: AtomicU64 = AtomicU64::new(0);

const ORAS_ASSETS: &[OrasAsset] = &[
    OrasAsset {
        os: "macos",
        arch: "x86_64",
        asset: 290_827_354,
        archive_digest: "sha256:82c33f7da8430ea7fa7e7bdf7721be0a0d0481e5ccb2472ea438490d5e8641a9",
        binary_digest: "sha256:f905ecfec4fbaaee5759a174feb7984004e7d8d1f8007205dc55d60536caf93b",
        archive: OrasArchive::TarGz,
    },
    OrasAsset {
        os: "macos",
        arch: "aarch64",
        asset: 290_827_346,
        archive_digest: "sha256:e10c6552c02d5a7c7eaf7170d3b6f7f094b675a98a1e0edf4d4478a909447245",
        binary_digest: "sha256:48490b6af9ef6929487c34f816637ea9cb56b2b5e303b032c091425b576ffb8d",
        archive: OrasArchive::TarGz,
    },
    OrasAsset {
        os: "linux",
        arch: "x86_64",
        asset: 290_827_355,
        archive_digest: "sha256:6cdc692f929100feb08aa8de584d02f7bcc30ec7d88bc2adc2054d782db57c64",
        binary_digest: "sha256:040e140304b7dbdd9b40dacd798e2303cea44ad84eeb210750afdf15f1dcf8b4",
        archive: OrasArchive::TarGz,
    },
    OrasAsset {
        os: "linux",
        arch: "aarch64",
        asset: 290_827_345,
        archive_digest: "sha256:7649738b48fde10542bcc8b0e9b460ba83936c75fb5be01ee6d4443764a14352",
        binary_digest: "sha256:e2b67c4efb35f20a546aacc680e0f81f907d3cf4cf9f1b2040c34bb471c428f1",
        archive: OrasArchive::TarGz,
    },
    OrasAsset {
        os: "windows",
        arch: "x86_64",
        asset: 290_827_344,
        archive_digest: "sha256:b050e93aa0dc7a79a61fa8e4074dfa302c41d4af01b634fe393c5dd687536aee",
        binary_digest: "sha256:6ed8d5b18a88a2bea32f4a562bf4fb646931590de9152ed55b646a43ed970247",
        archive: OrasArchive::Zip,
    },
];

#[derive(Clone, Copy)]
struct OrasAsset {
    os: &'static str,
    arch: &'static str,
    asset: u64,
    archive_digest: &'static str,
    binary_digest: &'static str,
    archive: OrasArchive,
}

#[derive(Clone, Copy)]
enum OrasArchive {
    TarGz,
    Zip,
}

pub(super) fn run_oras(arguments: &[OsString]) -> Result<(), PreparedArtifactError> {
    let output = oras(arguments)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(classify_oras(arguments, output))
    }
}

pub(super) fn run_oras_in(
    directory: &Path,
    arguments: &[OsString],
) -> Result<(), PreparedArtifactError> {
    let output = oras_command(arguments)?
        .current_dir(directory)
        .output()
        .map_err(PreparedArtifactError::Launch)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(classify_oras(arguments, output))
    }
}

pub(super) fn run_oras_stdout(arguments: &[OsString]) -> Result<Vec<u8>, PreparedArtifactError> {
    let output = oras(arguments)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(classify_oras(arguments, output))
    }
}

fn oras(arguments: &[OsString]) -> Result<Output, PreparedArtifactError> {
    oras_command(arguments)?
        .output()
        .map_err(PreparedArtifactError::Launch)
}

fn oras_command(arguments: &[OsString]) -> Result<Command, PreparedArtifactError> {
    let mut command = exact_oras_command()?;
    command.args(arguments);
    Ok(command)
}

/// Build a command for the exact repository-pinned ORAS executable.
///
/// The immutable host asset is fetched through the already-required GitHub CLI
/// and cached beside the coordinator executable.
///
/// # Errors
///
/// Refuses unsupported hosts, altered binaries, download failures, and
/// filesystem failures.
pub fn exact_oras_command() -> Result<Command, PreparedArtifactError> {
    let asset = current_oras_asset()?;
    let path = oras_cache_path(asset)?;
    let path = if path.is_file() {
        admit_oras_binary(path, asset)?
    } else {
        provision_oras(&path, asset)?;
        admit_oras_binary(path, asset)?
    };
    let mut command = Command::new(path);
    command.env_remove("ORAS_CACHE");
    Ok(command)
}

fn current_oras_asset() -> Result<&'static OrasAsset, PreparedArtifactError> {
    ORAS_ASSETS
        .iter()
        .find(|asset| asset.os == std::env::consts::OS && asset.arch == std::env::consts::ARCH)
        .ok_or_else(|| {
            PreparedArtifactError::Invalid(format!(
                "ORAS {ORAS_VERSION} has no admitted asset for {} {}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
        })
}

fn oras_cache_path(asset: &OrasAsset) -> Result<PathBuf, PreparedArtifactError> {
    let executable = std::env::current_exe().map_err(|error| {
        PreparedArtifactError::Provision(format!(
            "could not locate the coordinator executable for ORAS provisioning: {error}"
        ))
    })?;
    let root = executable.parent().ok_or_else(|| {
        PreparedArtifactError::Invalid(
            "coordinator executable has no parent for ORAS provisioning".to_owned(),
        )
    })?;
    Ok(root
        .join("prepared-tools")
        .join(format!("oras-{ORAS_VERSION}-{}-{}", asset.os, asset.arch))
        .join(if cfg!(windows) { "oras.exe" } else { "oras" }))
}

fn admit_oras_binary(path: PathBuf, asset: &OrasAsset) -> Result<PathBuf, PreparedArtifactError> {
    let observed = sha256_file(&path)?;
    if observed != asset.binary_digest {
        return Err(PreparedArtifactError::Invalid(format!(
            "ORAS binary {} differs from the admitted {} {} asset: expected {}, observed {observed}",
            path.display(),
            asset.os,
            asset.arch,
            asset.binary_digest
        )));
    }
    Ok(path)
}

fn provision_oras(path: &Path, asset: &OrasAsset) -> Result<(), PreparedArtifactError> {
    let scratch = Scratch::new()?;
    let archive = scratch.path.join("oras-archive");
    download_oras_asset(&archive, asset)?;
    let observed = sha256_file(&archive)?;
    if observed != asset.archive_digest {
        return Err(PreparedArtifactError::Invalid(format!(
            "downloaded ORAS archive differs from the admitted {} {} asset: expected {}, observed {observed}",
            asset.os, asset.arch, asset.archive_digest
        )));
    }
    let extracted = scratch
        .path
        .join(if cfg!(windows) { "oras.exe" } else { "oras" });
    extract_oras(&archive, &extracted, asset.archive)?;
    admit_oras_binary(extracted.clone(), asset)?;
    install_oras(&extracted, path, asset)
}

fn download_oras_asset(path: &Path, asset: &OrasAsset) -> Result<(), PreparedArtifactError> {
    let output = File::create(path).map_err(|source| PreparedArtifactError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let endpoint = format!("repos/oras-project/oras/releases/assets/{}", asset.asset);
    let result = Command::new("gh")
        .args(["api", "-H", "Accept: application/octet-stream", &endpoint])
        .stdout(Stdio::from(output))
        .output()
        .map_err(|error| {
            PreparedArtifactError::Provision(format!(
                "could not launch gh to fetch ORAS {ORAS_VERSION}: {error}"
            ))
        })?;
    if !result.status.success() {
        return Err(PreparedArtifactError::Provision(format!(
            "gh refused ORAS {ORAS_VERSION} asset {} with {}: {}",
            asset.asset,
            result.status,
            String::from_utf8_lossy(&result.stderr).trim()
        )));
    }
    Ok(())
}

fn extract_oras(
    archive: &Path,
    destination: &Path,
    format: OrasArchive,
) -> Result<(), PreparedArtifactError> {
    match format {
        OrasArchive::TarGz => extract_oras_tar(archive, destination),
        #[cfg(windows)]
        OrasArchive::Zip => extract_oras_zip(archive, destination),
        #[cfg(not(windows))]
        OrasArchive::Zip => Err(PreparedArtifactError::Invalid(
            "the zipped ORAS asset unpacks only on Windows hosts".to_owned(),
        )),
    }
}

fn extract_oras_tar(archive: &Path, destination: &Path) -> Result<(), PreparedArtifactError> {
    let file = File::open(archive).map_err(|source| PreparedArtifactError::Io {
        path: archive.to_path_buf(),
        source,
    })?;
    let mut tar = Archive::new(GzDecoder::new(file));
    let entries = tar.entries().map_err(|source| PreparedArtifactError::Io {
        path: archive.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let mut entry = entry.map_err(|source| PreparedArtifactError::Io {
            path: archive.to_path_buf(),
            source,
        })?;
        if entry.path().is_ok_and(|path| path == Path::new("oras")) {
            entry
                .unpack(destination)
                .map_err(|source| PreparedArtifactError::Io {
                    path: destination.to_path_buf(),
                    source,
                })?;
            return Ok(());
        }
    }
    Err(PreparedArtifactError::Invalid(
        "admitted ORAS archive has no executable".to_owned(),
    ))
}

// Only the Windows ORAS asset is a zip. Keeping the inflating `zip` features
// off every other host leaves the release tool resolving `zip` and `flate2`
// exactly as the product binary does, so both share their compiled units.
#[cfg(windows)]
fn extract_oras_zip(archive: &Path, destination: &Path) -> Result<(), PreparedArtifactError> {
    let file = File::open(archive).map_err(|source| PreparedArtifactError::Io {
        path: archive.to_path_buf(),
        source,
    })?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| PreparedArtifactError::Invalid(format!("ORAS zip is invalid: {error}")))?;
    let mut binary = archive.by_name("oras.exe").map_err(|error| {
        PreparedArtifactError::Invalid(format!("ORAS zip has no executable: {error}"))
    })?;
    let mut output = File::create(destination).map_err(|source| PreparedArtifactError::Io {
        path: destination.to_path_buf(),
        source,
    })?;
    std::io::copy(&mut binary, &mut output).map_err(|source| PreparedArtifactError::Io {
        path: destination.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn install_oras(
    source: &Path,
    destination: &Path,
    asset: &OrasAsset,
) -> Result<(), PreparedArtifactError> {
    let parent = destination.parent().ok_or_else(|| {
        PreparedArtifactError::Invalid("ORAS cache path has no parent".to_owned())
    })?;
    fs::create_dir_all(parent).map_err(|source| PreparedArtifactError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let staged = parent.join(format!(
        ".oras-{}-{}",
        std::process::id(),
        STAGING.fetch_add(1, Ordering::Relaxed)
    ));
    fs::copy(source, &staged).map_err(|source| PreparedArtifactError::Io {
        path: staged.clone(),
        source,
    })?;
    make_executable(&staged)?;
    match fs::rename(&staged, destination) {
        Ok(()) => Ok(()),
        Err(_) if destination.is_file() => {
            let _ = fs::remove_file(&staged);
            admit_oras_binary(destination.to_path_buf(), asset).map(|_| ())
        }
        Err(source) => Err(PreparedArtifactError::Io {
            path: destination.to_path_buf(),
            source,
        }),
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), PreparedArtifactError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).map_err(|source| {
        PreparedArtifactError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), PreparedArtifactError> {
    Ok(())
}

pub(super) fn admit_oras_version() -> Result<(), PreparedArtifactError> {
    let output = exact_oras_command()?
        .arg("version")
        .output()
        .map_err(PreparedArtifactError::Launch)?;
    if !output.status.success() {
        return Err(PreparedArtifactError::Oras {
            command: "oras version".to_owned(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let admitted = stdout.lines().any(|line| {
        line.trim() == ORAS_VERSION
            || line
                .trim()
                .strip_prefix("Version:")
                .is_some_and(|version| version.trim() == ORAS_VERSION)
    });
    if !admitted {
        return Err(PreparedArtifactError::Invalid(format!(
            "oras must be exactly {ORAS_VERSION}, observed {}",
            stdout.trim()
        )));
    }
    Ok(())
}

fn classify_oras(arguments: &[OsString], output: Output) -> PreparedArtifactError {
    let command = format!(
        "oras {}",
        arguments
            .iter()
            .map(|value| value.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.to_ascii_lowercase().contains("not found") {
        PreparedArtifactError::OrasMissing(command)
    } else {
        PreparedArtifactError::Oras {
            command,
            status: output.status.code(),
            stderr,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_altered_oras_binary_is_refused_before_execution() {
        let scratch = Scratch::new().unwrap();
        let binary = scratch.path.join("oras");
        fs::write(&binary, b"not the admitted executable").unwrap();
        let error = admit_oras_binary(binary, current_oras_asset().unwrap()).unwrap_err();
        assert!(error.to_string().contains("differs from the admitted"));
    }
}
