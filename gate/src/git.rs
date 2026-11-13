//! Platform Git executable selection shared by gate and installer boundaries.

use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use wire::{Digest, Domain};

#[cfg(not(windows))]
const SYSTEM_GIT: &str = "/usr/bin/git";
#[cfg(windows)]
const PROGRAM_NAME: &str = "git.exe";
#[cfg(not(windows))]
const PROGRAM_NAME: &str = "git";
#[cfg(any(windows, test))]
const PROGRAM_FILES_GIT: &str = r"Git\cmd\git.exe";

static SECONDARY_WARNING_PRINTED: AtomicBool = AtomicBool::new(false);

/// Where one admitted Git executable was discovered.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitProgramSource {
    /// `/usr/bin/git` selected only after Unix `PATH` had no Git.
    System,
    /// First regular Git executable found on the captured `PATH`.
    SearchPath,
    /// Git for Windows beneath the shell-resolved Program Files folder.
    ProgramFiles,
}

/// One concrete executable selected for every Git launch at a boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitProgram {
    path: PathBuf,
    source: GitProgramSource,
}

impl GitProgram {
    /// Resolved absolute executable path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Discovery branch that selected this executable.
    #[must_use]
    pub const fn source(&self) -> GitProgramSource {
        self.source
    }

    /// Announce a secondary platform selection once per process.
    pub fn announce_secondary(&self) {
        if self.source != GitProgramSource::SearchPath
            && !SECONDARY_WARNING_PRINTED.swap(true, Ordering::Relaxed)
        {
            eprintln!(
                "warning: Git was absent from PATH; using {}",
                self.path.display()
            );
        }
    }
}

/// Why no concrete platform Git executable could be selected.
#[derive(Debug)]
pub struct GitProgramError {
    detail: String,
    source: Option<io::Error>,
}

impl GitProgramError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
            source: None,
        }
    }

    fn io(detail: impl Into<String>, source: io::Error) -> Self {
        Self {
            detail: detail.into(),
            source: Some(source),
        }
    }
}

impl fmt::Display for GitProgramError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)?;
        if let Some(source) = &self.source {
            write!(formatter, ": {source}")?;
        }
        Ok(())
    }
}

impl std::error::Error for GitProgramError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

/// Select the platform Git executable for a repository boundary.
///
/// The captured `PATH` is searched first, anchoring relative entries to
/// `directory`. Only its absence admits the platform default: `/usr/bin/git`
/// on Unix or Git beneath the shell's Program Files folder on Windows. The
/// selected file is canonicalized once and no resolution, launch, or command
/// failure retries another candidate.
pub fn program(
    directory: &Path,
    search_path: Option<&OsStr>,
) -> Result<GitProgram, GitProgramError> {
    if let Some(candidate) = select_search_path(
        directory,
        search_path,
        PROGRAM_NAME,
        cfg!(windows),
        Path::is_file,
    ) {
        return resolved(candidate, GitProgramSource::SearchPath);
    }
    #[cfg(not(windows))]
    {
        let candidate = PathBuf::from(SYSTEM_GIT);
        if !candidate.is_file() {
            return Err(GitProgramError::new(format!(
                "Git is absent from PATH and {} is not a regular file",
                candidate.display()
            )));
        }
        resolved(candidate, GitProgramSource::System)
    }
    #[cfg(windows)]
    {
        let (candidate, source) = select_windows(
            directory,
            None,
            || sill::program_files().map_err(|error| error.to_string()),
            Path::is_file,
        )?;
        resolved(candidate, source)
    }
}

fn resolved(candidate: PathBuf, source: GitProgramSource) -> Result<GitProgram, GitProgramError> {
    let path = fs::canonicalize(&candidate).map_err(|error| {
        GitProgramError::io(
            format!("resolve selected Git executable {}", candidate.display()),
            error,
        )
    })?;
    Ok(GitProgram { path, source })
}

fn select_search_path(
    directory: &Path,
    search_path: Option<&OsStr>,
    program: &str,
    windows_helpers: bool,
    is_file: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    for entry in search_path.into_iter().flat_map(std::env::split_paths) {
        let directory = if entry.is_absolute() {
            entry
        } else {
            directory.join(entry)
        };
        let candidate = directory.join(program);
        if !is_file(&candidate) {
            continue;
        }
        return windows_public_git(&candidate, program, windows_helpers)
            .filter(|public| is_file(public))
            .or(Some(candidate));
    }
    None
}

/// Git for Windows' injected `mingw64/bin` and `usr/bin` directories route
/// back to the distribution's public `cmd` executable. Every other PATH
/// candidate is selected exactly as found.
fn windows_public_git(candidate: &Path, program: &str, windows_helpers: bool) -> Option<PathBuf> {
    let directory = candidate.parent()?;
    if !windows_helpers || !named(directory, "bin") {
        return None;
    }
    let distribution = directory.parent()?;
    if !named(distribution, "mingw64") && !named(distribution, "usr") {
        return None;
    }
    Some(distribution.parent()?.join("cmd").join(program))
}

/// Whether `path`'s last component is `name`, ignoring ASCII case.
fn named(path: &Path, name: &str) -> bool {
    path.file_name()
        .is_some_and(|component| component.to_string_lossy().eq_ignore_ascii_case(name))
}

#[cfg(any(windows, test))]
fn select_windows(
    directory: &Path,
    search_path: Option<&OsStr>,
    program_files: impl FnOnce() -> Result<PathBuf, String>,
    is_file: impl Fn(&Path) -> bool,
) -> Result<(PathBuf, GitProgramSource), GitProgramError> {
    if let Some(candidate) = select_search_path(directory, search_path, "git.exe", true, &is_file) {
        return Ok((candidate, GitProgramSource::SearchPath));
    }

    let program_files = program_files().map_err(|error| {
        GitProgramError::new(format!(
            "resolve secondary Windows Program Files Git location: {error}"
        ))
    })?;
    let candidate = program_files.join(PROGRAM_FILES_GIT);
    if !is_file(&candidate) {
        return Err(GitProgramError::new(format!(
            "Git is absent from PATH and {} is not a regular file",
            candidate.display()
        )));
    }
    Ok((candidate, GitProgramSource::ProgramFiles))
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitExecutableAuthority {
    path: String,
    digest: Digest,
}

pub(crate) fn executable_authority(
    directory: &Path,
    search_path: Option<&OsStr>,
) -> Result<Option<GitExecutableAuthority>, String> {
    let selected = match program(directory, search_path) {
        Ok(selected) => selected,
        Err(_) => return Ok(None),
    };
    let bytes = fs::read(selected.path()).map_err(|error| {
        format!(
            "read selected Git executable {}: {error}",
            selected.path().display()
        )
    })?;
    Ok(Some(GitExecutableAuthority {
        path: selected.path().to_string_lossy().into_owned(),
        digest: Digest::compute(Domain::GateTool, &bytes),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_first_path_git_precedes_program_files_and_relative_entries_anchor_once() {
        let root = Path::new("/work");
        let path = std::env::join_paths([
            Path::new("git/libexec/git-core"),
            Path::new("empty"),
            Path::new("tools"),
        ])
        .expect("compose fixture PATH");
        let expected = root.join("git/libexec/git-core").join("git.exe");
        let later = root.join("tools").join("git.exe");
        let (selected, source) = select_windows(
            root,
            Some(&path),
            || -> Result<PathBuf, String> { panic!("PATH selection must not inspect fallback") },
            |candidate| candidate == expected || candidate == later,
        )
        .expect("select PATH Git");

        assert_eq!(selected, expected);
        assert_eq!(source, GitProgramSource::SearchPath);
    }

    #[test]
    fn windows_injected_distribution_bins_still_route_to_public_git() {
        let root = Path::new("/work/Git");
        let public = root.join("cmd/git.exe");
        for entry in ["mingw64/bin", "usr/bin"] {
            let helper = root.join(entry).join("git.exe");
            let selected =
                select_search_path(root, Some(OsStr::new(entry)), "git.exe", true, |path| {
                    path == helper || path == public
                });
            assert_eq!(selected, Some(public.clone()), "{entry}");
        }
    }

    #[test]
    fn windows_program_files_is_consulted_only_after_path_has_no_git() {
        let program_files = PathBuf::from("/Program Files");
        let expected = program_files.join(PROGRAM_FILES_GIT);
        let (selected, source) = select_windows(
            Path::new("/work"),
            Some(OsStr::new("empty")),
            || Ok(program_files),
            |candidate| candidate == expected,
        )
        .expect("select Program Files Git");

        assert_eq!(selected, expected);
        assert_eq!(source, GitProgramSource::ProgramFiles);
    }

    #[test]
    fn windows_absence_names_both_search_boundaries() {
        let error = select_windows(
            Path::new("/work"),
            Some(OsStr::new("empty")),
            || Ok(PathBuf::from("/Program Files")),
            |_| false,
        )
        .expect_err("absent Git refuses");

        let detail = error.to_string();
        assert!(detail.contains("absent from PATH"), "{detail}");
        assert!(detail.contains("Git\\cmd\\git.exe"), "{detail}");
    }
}
