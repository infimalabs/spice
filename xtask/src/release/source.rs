//! The one source archive a release stages every target from.

use super::*;

/// The directory under the build root one staging run extracts its source
/// archive into.
const SOURCE_SCRATCH: &str = "release-source";

/// The release source archive and the identity its bytes carry.
///
/// A local staging run reads the archive at the stage root and extracts it
/// into a fresh scratch root; a hosted guest builds the tree the provider
/// checked out from that same archive and is handed its identity instead.
pub(super) struct SourceArchive {
    path: Option<PathBuf>,
    pub(super) digest: Digest,
}

/// What one extracted archive names: the commit it exported and the digests
/// every target payload records.
pub(super) struct ExtractedSource {
    pub(super) commit: String,
    pub(super) identity: dist::TargetSource,
}

impl SourceArchive {
    /// The archive at `path`, identified by its exact bytes.
    pub(super) fn read(path: &Path) -> Result<Self, TargetBuildError> {
        let digest =
            dist::source_archive_identity(path).map_err(TargetBuildError::SourceArchive)?;
        Ok(Self {
            path: Some(path.to_path_buf()),
            digest,
        })
    }

    /// An archive whose tree the hosted provider delivered as the checkout,
    /// known only by the identity the signed request bound.
    pub(super) fn delivered(digest: &OsStr) -> Result<Self, TargetBuildError> {
        let digest = digest
            .to_str()
            .ok_or_else(|| "hosted source archive identity is not UTF-8".to_owned())
            .and_then(|text| {
                text.parse::<Digest>()
                    .map_err(|error| format!("hosted source archive identity {text:?}: {error}"))
            })
            .map_err(TargetBuildError::SourceArchive)?;
        Ok(Self { path: None, digest })
    }

    /// Extract the archive into a fresh scratch root under `build_root` and
    /// return the exported tree, which carries no Git and derives its version
    /// from the record the archive added.
    pub(super) fn extract(
        &self,
        tar: &Path,
        build_root: &Path,
    ) -> Result<PathBuf, TargetBuildError> {
        let path = self
            .path
            .as_deref()
            .expect("a delivered archive is never extracted");
        let scratch = build_root.join(SOURCE_SCRATCH);
        match fs::remove_dir_all(&scratch) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(TargetBuildError::Io {
                    path: scratch,
                    source,
                });
            }
        }
        fs::create_dir_all(&scratch).map_err(|source| TargetBuildError::Io {
            path: scratch.clone(),
            source,
        })?;
        let arguments = vec![
            OsString::from("-xf"),
            path.as_os_str().to_owned(),
            OsString::from("-C"),
            scratch.as_os_str().to_owned(),
        ];
        let output = std::process::Command::new(tar)
            .args(&arguments)
            .output()
            .map_err(|source| TargetBuildError::Launch {
                label: "source-extract",
                source,
            })?;
        if !output.status.success() {
            return Err(TargetBuildError::CommandFailed(CommandRecord {
                label: "source-extract",
                program: tar.to_path_buf(),
                arguments,
                code: output.status.code(),
                stdout: output.stdout,
                stderr: output.stderr,
            }));
        }
        Ok(scratch.join(crate::source_archive::ARCHIVE_ROOT))
    }

    /// The commit the tree at `root` records, paired with this archive.
    pub(super) fn source(&self, root: &Path) -> Result<ExtractedSource, TargetBuildError> {
        let commit = dist::source_identity(root)
            .map_err(TargetBuildError::SourceArchive)?
            .ok_or_else(|| {
                TargetBuildError::SourceArchive(format!(
                    "{} names no source commit",
                    root.join(dist::BUILD_RECORD_FILE).display()
                ))
            })?
            .commit()
            .to_owned();
        let lock_path = root.join("Cargo.lock");
        let lock = fs::read(&lock_path).map_err(|error| {
            TargetBuildError::SourceArchive(format!(
                "cannot read selected {}: {error}",
                lock_path.display()
            ))
        })?;
        Ok(ExtractedSource {
            identity: dist::TargetSource {
                commit: Digest::compute(Domain::DistSourceCommit, commit.as_bytes()),
                archive: self.digest.clone(),
                source_commit: commit.clone(),
                cargo_lock: Digest::compute(Domain::DistArtifact, &lock),
            },
            commit,
        })
    }

    /// The archive bytes are still the ones this run extracted.
    pub(super) fn verify(&self) -> Result<(), TargetBuildError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let observed =
            dist::source_archive_identity(path).map_err(TargetBuildError::SourceArchive)?;
        if observed == self.digest {
            Ok(())
        } else {
            Err(TargetBuildError::SourceMoved {
                expected: self.digest.to_string(),
                observed: observed.to_string(),
            })
        }
    }
}

/// The only untracked paths a release build is allowed to have produced.
const RELEASE_OUTPUT_ROOTS: [&str; 2] = ["dist/stage/objects/", "dist/stage/targets/"];
const RELEASE_OUTPUTS: [&str; 2] = ["dist/stage/release/manifest.json", "dist/stage/source.tar"];

/// The porcelain code and separator an untracked entry is reported under.
const UNTRACKED: &str = "?? ";

/// The checkout at the executor's root is exactly HEAD: no tracked change,
/// no untracked path outside release output, no index flag hiding either.
pub(super) fn verify_checkout<E: Executor>(
    executor: &E,
    git: &Path,
) -> Result<(), TargetBuildError> {
    // One porcelain status answers both questions, and it is the only spelling
    // that answers them truthfully. `git diff-index` trusts the index stat
    // cache, so a checkout whose files are byte-identical to HEAD still reads
    // as modified whenever that cache is stale -- exactly what a fresh clone
    // observed through another kernel's view of the same bytes produces, which
    // is the sealed proof image reading a mounted host checkout. `git status`
    // refreshes the index before it compares, so it reports content.
    let status = successful(executor.execute(CommandSpec::new(
        "source-status",
        git,
        ["status", "--porcelain=v1", "--untracked-files=all"],
    ))?)?;
    let status = text(&status.stdout, "source status inventory")?;
    for entry in status.lines() {
        let Some(path) = entry.strip_prefix(UNTRACKED) else {
            return Err(TargetBuildError::SourceDirty(format!(
                "tracked source differs from HEAD: {entry}"
            )));
        };
        if !release_output(path) {
            return Err(TargetBuildError::SourceDirty(format!(
                "untracked source path is outside release output: {path}"
            )));
        }
    }
    // Status trusts the assume-unchanged and skip-worktree index flags, so a
    // tracked file under either can differ from HEAD while status stays
    // silent. No release flow sets them, so an entry carrying one is local
    // state whose bytes status cannot vouch for, and it refuses. Reading the
    // flags writes nothing, leaving the caller's index and configuration as
    // found.
    let flags = successful(executor.execute(CommandSpec::new(
        "source-index-flags",
        git,
        ["ls-files", "-v", "-z"],
    ))?)?;
    let flags = text(&flags.stdout, "source index flags")?;
    for entry in flags.split_terminator('\0') {
        let (tag, path) = entry.split_at_checked(2).unwrap_or((entry, ""));
        let hidden = match tag.as_bytes().first() {
            Some(b'S' | b's') => "skip-worktree",
            Some(byte) if byte.is_ascii_lowercase() => "assume-unchanged",
            _ => continue,
        };
        return Err(TargetBuildError::SourceDirty(format!(
            "index flag {hidden} hides tracked source from status: {path}"
        )));
    }
    Ok(())
}

fn release_output(path: &str) -> bool {
    RELEASE_OUTPUT_ROOTS
        .iter()
        .any(|root| path.starts_with(root))
        || RELEASE_OUTPUTS.contains(&path)
}

pub(super) struct GuardedPublisher<'a, E> {
    pub(super) inner: RootedFiles,
    pub(super) archive: &'a SourceArchive,
    pub(super) required: &'a RequiredPins,
    pub(super) programs: &'a Programs,
    pub(super) target: ReleaseTarget,
    pub(super) executor: &'a E,
    pub(super) proof_container: &'a str,
}

impl<E: Executor> GuardedPublisher<'_, E> {
    /// The source archive and toolchain pins still match those the build began with.
    fn verify_unmoved(&self) -> Result<(), String> {
        self.archive.verify().map_err(|error| error.to_string())?;
        let observed = observe_pins(
            self.executor,
            self.programs,
            self.target,
            self.proof_container,
        )
        .map_err(|error| error.to_string())?;
        self.required
            .verify(&observed)
            .map_err(|error| error.to_string())
    }
}

impl<E: Executor> RootedPublish for GuardedPublisher<'_, E> {
    fn publish(&mut self, path: &Path, mode: u32, bytes: &[u8]) -> Result<(), atomic::Error> {
        if path.starts_with("targets") && path.file_name() == Some(OsStr::new("payload.json")) {
            self.verify_unmoved().map_err(|reason| {
                atomic::Error::Refused(atomic::Refusal::Withheld {
                    path: path.to_path_buf(),
                    reason,
                })
            })?;
        }
        self.inner.publish(path, mode, bytes)
    }
}
