//! The one-use source commit one hosted run builds and the archive identity
//! its signed request binds.

use std::ffi::OsString;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use sha2::{Digest as _, Sha256};
use wire::Digest;

use super::{HostedSource, REMOTE, WindowsHostedError, command, successful, successful_text};

/// The transport commit one hosted run checks out and canonical identities
/// bound to its source.
pub(super) struct PreparedSource {
    /// The one-use transport commit pushed under the run's tag.
    pub(super) commit: String,
    /// The canonical release commit recorded by the transported source.
    pub(super) canonical_commit: String,
    /// The identity of the source archive those bytes came from.
    pub(super) archive: Digest,
    /// SHA-256 of the commit's recursive Git listing, which the guest
    /// verifies against its checkout.
    pub(super) tree: String,
}

/// Build the one-use commit for `source` in this repository's object store.
///
/// A worktree snapshot commits the exact worktree on top of HEAD and derives
/// its archive identity from the archive of that commit; a release archive is
/// extracted and committed as a parentless tree equal to the archive's.
pub(super) fn prepare(
    root: &Path,
    scratch: &Path,
    challenge: &str,
    source: HostedSource<'_>,
) -> Result<PreparedSource, WindowsHostedError> {
    let (commit, canonical_commit, archive) = match source {
        HostedSource::Worktree => {
            let canonical_commit = canonical_source_commit(root)?;
            let commit = snapshot(root, scratch, challenge)?;
            let archive = scratch.join("source.tar");
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
                        archive.as_os_str().to_owned(),
                        OsString::from(&commit),
                    ],
                )?,
            )?;
            (commit, canonical_commit, archive_identity(&archive)?)
        }
        HostedSource::Archive(archive) => {
            let source = archive_source(root, scratch, archive)?;
            let commit = commit_tree(root, &source.tree, None, &archive_message(challenge))?;
            (commit, source.canonical_commit, source.archive)
        }
    };
    let tree = tree_digest(root, &commit)?;
    Ok(PreparedSource {
        commit,
        canonical_commit,
        archive,
        tree,
    })
}

fn snapshot(root: &Path, scratch: &Path, challenge: &str) -> Result<String, WindowsHostedError> {
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
        &format!("spice hosted source {challenge}\n"),
    )
}

/// The identity of the source archive at `path`.
fn archive_identity(path: &Path) -> Result<Digest, WindowsHostedError> {
    dist::source_archive_identity(path).map_err(WindowsHostedError::ProviderResponse)
}

/// One release source archive's identities, its exact tree written to this
/// repository's object store.
pub(super) struct ArchiveSource {
    /// The Git tree equal to the archive's tree, `dist/build.json` included.
    pub(super) tree: String,
    /// The canonical release commit the archive's record names.
    pub(super) canonical_commit: String,
    /// The identity of the archive's bytes.
    pub(super) archive: Digest,
}

/// Bind an attached provider run's one-use `commit`, already proven to carry
/// `source.tree`, to the archive it was built from.
pub(super) fn attached(
    root: &Path,
    source: ArchiveSource,
    commit: String,
) -> Result<PreparedSource, WindowsHostedError> {
    let tree = tree_digest(root, &source.tree)?;
    Ok(PreparedSource {
        commit,
        canonical_commit: source.canonical_commit,
        archive: source.archive,
        tree,
    })
}

const ARCHIVE_MESSAGE: &str = "spice hosted source archive ";
/// Hex digits of one hosted challenge's 32 random bytes.
const CHALLENGE_HEX_LENGTH: usize = 64;

/// The message of the one-use archive commit for `challenge`.
fn archive_message(challenge: &str) -> String {
    format!("{ARCHIVE_MESSAGE}{challenge}\n")
}

/// The challenge a one-use archive commit's `message` records, if it is one.
pub(super) fn archive_challenge(message: &str) -> Option<&str> {
    let rest = message.strip_prefix(ARCHIVE_MESSAGE)?;
    let challenge = rest.strip_suffix('\n').unwrap_or(rest);
    (challenge.len() == CHALLENGE_HEX_LENGTH
        && challenge
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(challenge)
}

fn canonical_source_commit(root: &Path) -> Result<String, WindowsHostedError> {
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

/// Extract the archive and write its exact tree, `dist/build.json` included,
/// to this repository's object store.
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
    let git_dir = successful_text(
        "archive commit repository",
        command(
            root,
            "git",
            ["rev-parse", "--absolute-git-dir"].map(OsString::from),
        )?,
    )?;
    let worktree = extracted.join("spice");
    let index = scratch.join("index");
    let git = |arguments: Vec<OsString>| -> Result<Output, WindowsHostedError> {
        Command::new("git")
            .current_dir(&worktree)
            .env("GIT_DIR", git_dir.trim())
            .env("GIT_WORK_TREE", &worktree)
            .env("GIT_INDEX_FILE", &index)
            .args(arguments)
            .output()
            .map_err(|source| WindowsHostedError::Launch {
                program: "git".to_owned(),
                source,
            })
    };
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
    let tree = successful_text("archive tree object", git(vec!["write-tree".into()])?)?;
    let canonical_commit = canonical_source_commit(&worktree)?;
    Ok(ArchiveSource {
        tree: tree.trim().to_owned(),
        canonical_commit,
        archive: identity,
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

pub(super) fn push_source(
    root: &Path,
    commit: &str,
    source_ref: &str,
) -> Result<(), WindowsHostedError> {
    successful(
        "source transfer",
        command(
            root,
            "git",
            [
                OsString::from("push"),
                OsString::from(REMOTE),
                OsString::from(format!("{commit}:refs/tags/{source_ref}")),
            ],
        )?,
    )?;
    Ok(())
}

pub(super) fn delete_source(root: &Path, source_ref: &str) -> Result<(), WindowsHostedError> {
    let deletion = command(
        root,
        "git",
        [
            OsString::from("push"),
            OsString::from(REMOTE),
            OsString::from(format!(":refs/tags/{source_ref}")),
        ],
    )?;
    if deletion.status.success() {
        return Ok(());
    }
    let observed = command(
        root,
        "git",
        [
            OsString::from("ls-remote"),
            OsString::from("--tags"),
            OsString::from(REMOTE),
            OsString::from(format!("refs/tags/{source_ref}")),
        ],
    )?;
    if observed.status.success() && observed.stdout.is_empty() {
        return Ok(());
    }
    successful("source retirement", deletion)?;
    Ok(())
}
