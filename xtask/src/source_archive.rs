//! Source archives carrying one canonical build identity.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use spice::repository::Repository;

/// The directory every archive member sits under.
pub(crate) const ARCHIVE_ROOT: &str = "spice";
const MINIMUM_GIT: (u64, u64) = (2, 40);

/// Write `HEAD` as a tar archive carrying the canonical build record: the
/// one source every release target stages from. `output` defaults to
/// [`dist::SOURCE_ARCHIVE_FILE`] under the stage root, where `stage-target`
/// reads it.
///
/// This is the one point release staging reads Git, so the checkout checks
/// live here: HEAD must be exactly what the checkout holds, with no tracked
/// change, no untracked path outside release output, and no index flag hiding
/// either.
///
/// # Errors
///
/// Refuses Git older than 2.40, a checkout that differs from HEAD, a source
/// that cannot produce a staged identity, a development identity, or a
/// failed archive command.
pub fn source_archive(output: Option<&Path>, ambient: sill::Environment) -> Result<(), String> {
    let repository = Repository::discover(Path::new(".")).map_err(|error| error.to_string())?;
    let root = repository.worktree_root();
    require_git_version(root)?;
    crate::release::verify_release_checkout(root, ambient).map_err(|error| error.to_string())?;
    let build = dist::head_build_info(root, dist::Build::Staged)?;
    if build.product.as_str().contains('-') || build.product.as_str().contains('+') {
        return Err(format!(
            "source archive requires a clean staged product identity, observed {}",
            build.product
        ));
    }
    let bytes = dist::build_record_bytes(&build);
    let record = std::str::from_utf8(&bytes).expect("canonical BuildInfo JSON is UTF-8");
    let virtual_file = format!(
        "--add-virtual-file={ARCHIVE_ROOT}/{}:{record}",
        dist::BUILD_RECORD_FILE
    );
    let default = root.join("dist/stage").join(dist::SOURCE_ARCHIVE_FILE);
    let output = output.unwrap_or(&default);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let status = Command::new("git")
        .current_dir(root)
        .args([
            OsString::from("archive"),
            OsString::from("--format=tar"),
            OsString::from(format!("--prefix={ARCHIVE_ROOT}/")),
            OsString::from(virtual_file),
            OsString::from("--output"),
            output.as_os_str().to_owned(),
            OsString::from("HEAD"),
        ])
        .status()
        .map_err(|error| format!("cannot launch git archive: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git archive failed with {status}"))
    }
}

fn require_git_version(root: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .current_dir(root)
        .arg("--version")
        .output()
        .map_err(|error| format!("cannot launch git --version: {error}"))?;
    if !output.status.success() {
        return Err(format!("git --version failed with {}", output.status));
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| "git --version returned non-UTF-8 output".to_owned())?;
    let version = parse_git_version(text)
        .ok_or_else(|| format!("Git returned an unrecognized version: {text:?}"))?;
    if version < MINIMUM_GIT {
        return Err(format!(
            "source archive requires Git 2.40 or newer, observed {}.{}",
            version.0, version.1
        ));
    }
    Ok(())
}

fn parse_git_version(text: &str) -> Option<(u64, u64)> {
    let version = text
        .strip_prefix("git version ")?
        .split_whitespace()
        .next()?;
    let mut numbers = version.split('.');
    let major = numbers.next()?.parse().ok()?;
    let minor = numbers.next()?.parse().ok()?;
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_version_admission_uses_major_and_minor() {
        assert_eq!(parse_git_version("git version 2.40.0\n"), Some((2, 40)));
        assert_eq!(
            parse_git_version("git version 2.39.3 (Apple Git-145)\n"),
            Some((2, 39))
        );
        assert_eq!(parse_git_version("git version unknown\n"), None);
    }
}
