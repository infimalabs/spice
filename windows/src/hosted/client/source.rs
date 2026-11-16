//! Canonical source-archive materialization and the identities every hosted
//! run admits before execution.

use std::ffi::OsString;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use sha2::{Digest as _, Sha256};
use wire::Digest;

use super::{HostedSource, WindowsHostedError, command, successful, successful_text};

/// One canonical source archive and the identities a hosted run binds.
pub(super) struct PreparedSource {
    /// The exact archive uploaded for provider delivery.
    pub(super) archive_path: PathBuf,
    /// The canonical release commit recorded by the transported source.
    pub(super) canonical_commit: String,
    /// The domain-separated identity of the source archive.
    pub(super) archive: Digest,
    /// Raw SHA-256 of the transported `source.tar` bytes.
    pub(super) archive_sha256: String,
    /// SHA-256 of the archive's recursive Git listing.
    pub(super) tree: String,
    /// Git blob identity of the workflow this archive admits.
    pub(super) workflow_blob: String,
}

/// Materialize `source` as one canonical archive and derive every identity a
/// provider must admit before using it.
pub(super) fn prepare(
    root: &Path,
    scratch: &Path,
    workflow: &str,
    source: HostedSource<'_>,
) -> Result<PreparedSource, WindowsHostedError> {
    let (archive_path, canonical_commit, archive, tree, workflow_root) = match source {
        HostedSource::Worktree => {
            let canonical_commit = canonical_source_commit(root)?;
            let commit = snapshot(root, scratch)?;
            let archive_path = scratch.join("source.tar");
            successful(
                "snapshot archive",
                command(
                    root,
                    "git",
                    [
                        OsString::from("archive"),
                        OsString::from("--format=tar"),
                        OsString::from("--prefix=spice/"),
                        OsString::from("--output"),
                        archive_path.as_os_str().to_owned(),
                        OsString::from(&commit),
                    ],
                )?,
            )?;
            let archive = archive_identity(&archive_path)?;
            let tree = tree_digest(root, &commit)?;
            (
                archive_path,
                canonical_commit,
                archive,
                tree,
                root.to_path_buf(),
            )
        }
        HostedSource::Archive(archive_path) => {
            let source = archive_source(root, scratch, archive_path)?;
            let tree = tree_digest(&source.worktree, "HEAD")?;
            (
                archive_path.to_path_buf(),
                source.canonical_commit,
                source.archive,
                tree,
                source.worktree,
            )
        }
    };
    let workflow_blob = workflow_blob(root, &workflow_root.join(workflow))?;
    let archive_sha256 = file_sha256(&archive_path)?;
    Ok(PreparedSource {
        archive_path,
        canonical_commit,
        archive,
        archive_sha256,
        tree,
        workflow_blob,
    })
}

fn snapshot(root: &Path, scratch: &Path) -> Result<String, WindowsHostedError> {
    let index = scratch.join("index");
    let git = |arguments: Vec<OsString>| -> Result<Output, WindowsHostedError> {
        Command::new("git")
            .current_dir(root)
            .env("GIT_INDEX_FILE", &index)
            .args(arguments)
            .output()
            .map_err(|source| WindowsHostedError::Launch {
                program: "git".to_owned(),
                source,
            })
    };
    successful(
        "snapshot base",
        git(vec!["read-tree".into(), "HEAD".into()])?,
    )?;
    successful(
        "snapshot worktree",
        git(vec!["add".into(), "-A".into(), "--".into(), ".".into()])?,
    )?;
    let worktree = successful_text("snapshot worktree tree", git(vec!["write-tree".into()])?)?;
    let head = successful_text(
        "snapshot head tree",
        command(
            root,
            "git",
            ["rev-parse", "HEAD^{tree}"].map(OsString::from),
        )?,
    )?;
    let build = if worktree.trim() == head.trim() {
        dist::Build::Staged
    } else {
        dist::Build::Dev
    };
    stage_head_build_record(root, scratch, &git, build)?;
    let tree = successful_text("snapshot tree", git(vec!["write-tree".into()])?)?;
    let parent = successful_text(
        "snapshot parent",
        command(root, "git", ["rev-parse", "HEAD"].map(OsString::from))?,
    )?;
    commit_tree(
        root,
        tree.trim(),
        Some(parent.trim()),
        "spice hosted source snapshot\n",
    )
}

/// The identity of the source archive at `path`.
fn archive_identity(path: &Path) -> Result<Digest, WindowsHostedError> {
    dist::source_archive_identity(path).map_err(WindowsHostedError::ProviderResponse)
}

/// One release source archive's identities and extracted local repository.
pub(super) struct ArchiveSource {
    /// The canonical release commit the archive's record names.
    pub(super) canonical_commit: String,
    /// The identity of the archive's bytes.
    pub(super) archive: Digest,
    /// The exact extracted worktree used for tree and workflow admission.
    pub(super) worktree: PathBuf,
}

pub(super) fn canonical_source_commit(root: &Path) -> Result<String, WindowsHostedError> {
    dist::source_identity(root)
        .map_err(WindowsHostedError::ProviderResponse)?
        .map(|identity| identity.commit().to_owned())
        .ok_or_else(|| {
            WindowsHostedError::ProviderResponse(format!(
                "{} has no canonical source commit",
                root.display()
            ))
        })
}

/// Extract the archive and materialize its exact tree, `dist/build.json`
/// included, in one credential-free scratch repository.
pub(super) fn archive_source(
    root: &Path,
    scratch: &Path,
    archive: &Path,
) -> Result<ArchiveSource, WindowsHostedError> {
    let identity = archive_identity(archive)?;
    let extracted = scratch.join("source");
    fs::create_dir(&extracted).map_err(|source| WindowsHostedError::Io {
        path: extracted.clone(),
        source,
    })?;
    successful(
        "archive extraction",
        command(
            root,
            "tar",
            [
                OsString::from("-xf"),
                archive.as_os_str().to_owned(),
                OsString::from("-C"),
                extracted.as_os_str().to_owned(),
            ],
        )?,
    )?;
    let worktree = extracted.join("spice");
    let canonical_commit = canonical_source_commit(&worktree)?;
    let git = |arguments: Vec<OsString>| -> Result<Output, WindowsHostedError> {
        Command::new("git")
            .current_dir(&worktree)
            .args(arguments)
            .output()
            .map_err(|source| WindowsHostedError::Launch {
                program: "git".to_owned(),
                source,
            })
    };
    successful("archive repository", git(vec!["init".into(), "-q".into()])?)?;
    successful(
        "archive checkout policy",
        git(vec![
            "config".into(),
            "core.autocrlf".into(),
            "false".into(),
        ])?,
    )?;
    successful(
        "archive tree",
        git(vec![
            "add".into(),
            "-A".into(),
            "-f".into(),
            "--".into(),
            ".".into(),
        ])?,
    )?;
    successful(
        "archive commit",
        git(vec![
            "-c".into(),
            "user.name=spice-hosted".into(),
            "-c".into(),
            "user.email=hosted@invalid".into(),
            "commit".into(),
            "-q".into(),
            "-m".into(),
            format!("canonical source {canonical_commit}").into(),
        ])?,
    )?;
    Ok(ArchiveSource {
        canonical_commit,
        archive: identity,
        worktree,
    })
}

/// Commit `tree` with the hosted runner's fixed authorship.
fn commit_tree(
    root: &Path,
    tree: &str,
    parent: Option<&str>,
    message: &str,
) -> Result<String, WindowsHostedError> {
    let mut process = Command::new("git");
    process
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Spice Hosted Runner")
        .env("GIT_AUTHOR_EMAIL", "hosted-runner@invalid")
        .env("GIT_COMMITTER_NAME", "Spice Hosted Runner")
        .env("GIT_COMMITTER_EMAIL", "hosted-runner@invalid")
        .arg("commit-tree")
        .arg(tree);
    if let Some(parent) = parent {
        process.args(["-p", parent]);
    }
    let mut child = process
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| WindowsHostedError::Launch {
            program: "git".to_owned(),
            source,
        })?;
    child
        .stdin
        .take()
        .ok_or_else(|| {
            WindowsHostedError::ProviderResponse("git commit-tree stdin is unavailable".to_owned())
        })?
        .write_all(message.as_bytes())
        .map_err(WindowsHostedError::Output)?;
    successful_text(
        "source commit",
        child
            .wait_with_output()
            .map_err(|source| WindowsHostedError::Launch {
                program: "git".to_owned(),
                source,
            })?,
    )
    .map(|value| value.trim().to_owned())
}

/// Add HEAD's canonical record to the snapshot index, derived as `build`: a
/// snapshot carrying uncommitted changes is a development build of HEAD.
fn stage_head_build_record(
    root: &Path,
    scratch: &Path,
    git: &impl Fn(Vec<OsString>) -> Result<Output, WindowsHostedError>,
    build: dist::Build,
) -> Result<(), WindowsHostedError> {
    let build = dist::head_build_info(root, build).map_err(|error| {
        WindowsHostedError::ProviderResponse(format!("snapshot build identity: {error}"))
    })?;
    let record = scratch.join("build.json");
    fs::write(&record, dist::build_record_bytes(&build)).map_err(|source| {
        WindowsHostedError::Io {
            path: record.clone(),
            source,
        }
    })?;
    let blob = successful_text(
        "snapshot build record",
        git(vec![
            "hash-object".into(),
            "-w".into(),
            "--".into(),
            record.as_os_str().to_owned(),
        ])?,
    )?;
    successful(
        "snapshot build record index",
        git(vec![
            "update-index".into(),
            "--add".into(),
            "--cacheinfo".into(),
            format!("100644,{},{}", blob.trim(), dist::BUILD_RECORD_FILE).into(),
        ])?,
    )?;
    Ok(())
}

pub(super) fn workflow_blob(root: &Path, path: &Path) -> Result<String, WindowsHostedError> {
    successful_text(
        "source workflow blob",
        command(
            root,
            "git",
            [
                OsString::from("hash-object"),
                OsString::from("--no-filters"),
                path.as_os_str().to_owned(),
            ],
        )?,
    )
    .map(|identity| identity.trim().to_owned())
}

pub(super) fn file_sha256(path: &Path) -> Result<String, WindowsHostedError> {
    let mut file = fs::File::open(path).map_err(|source| WindowsHostedError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(WindowsHostedError::Output)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn tree_digest(root: &Path, commit: &str) -> Result<String, WindowsHostedError> {
    let mut child = Command::new("git")
        .current_dir(root)
        .args(["ls-tree", "-r", "-z", "--full-tree", commit])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| WindowsHostedError::Launch {
            program: "git".to_owned(),
            source,
        })?;
    let mut listing = child.stdout.take().ok_or_else(|| {
        WindowsHostedError::ProviderResponse("git tree stdout is unavailable".to_owned())
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = listing
            .read(&mut buffer)
            .map_err(WindowsHostedError::Output)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let output = child
        .wait_with_output()
        .map_err(|source| WindowsHostedError::Launch {
            program: "git".to_owned(),
            source,
        })?;
    successful("source tree", output)?;
    Ok(format!("{:x}", digest.finalize()))
}
