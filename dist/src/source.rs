//! The build identity a source tree derives: its product version and the
//! commit it was built from.
//!
//! An exported tree's untracked [`BUILD_RECORD_FILE`] is that identity
//! verbatim. Otherwise Git HEAD names the commit, and under a tracked major the
//! version is HEAD's counter sum as the minor and HEAD's committer second past
//! the newest first-parent counter-sum raise as the patch. A tree with neither is a
//! development build counting its patch in Unix seconds.

mod git;
mod history;
mod precedence;

use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use wire::manifest::{BuildInfo, ProductVersion};
use wire::{Digest, Domain};

use crate::{BUILD_RECORD_FILE, Build, COUNTERS_FILE, SourceInstant, TrackedVersion, VERSION_FILE};

pub use precedence::build_record_bytes;

use git::GitHistory;
use precedence::{History, derive};

/// Exact source commit identity and its committer second.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceIdentity {
    commit: String,
    instant: SourceInstant,
}

impl SourceIdentity {
    /// Full Git object identity.
    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }

    /// The committer second.
    #[must_use]
    pub(crate) const fn instant(&self) -> SourceInstant {
        self.instant
    }

    #[cfg(test)]
    fn new(commit: String, instant: SourceInstant) -> Self {
        Self { commit, instant }
    }

    /// Admit one full lowercase object identity and canonical committer second.
    fn parse(commit: &str, seconds: &str) -> Result<Self, &'static str> {
        if !matches!(commit.len(), 40 | 64)
            || !commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("commit must be one full lowercase Git object identity");
        }
        if seconds.is_empty()
            || (seconds.len() > 1 && seconds.starts_with('0'))
            || !seconds.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("committer second must be one canonical nonnegative decimal");
        }
        let seconds = seconds
            .parse::<i64>()
            .map_err(|_| "committer second is outside i64")?;
        Ok(Self {
            commit: commit.to_owned(),
            instant: SourceInstant::new(seconds),
        })
    }
}

/// The build identity of the tree at `root`: its record, else Git HEAD, else
/// none. `build` marks a non-record version `-dev` outside release staging.
///
/// # Errors
///
/// Refuses unreadable or malformed tracked inputs, a record disagreeing with
/// the tree, unreadable or unplaceable Git history, and a staged build with no
/// source.
pub fn build_info(root: &Path, build: Build) -> Result<BuildInfo, String> {
    let tracked = tracked_version(root)?;
    let record = build_record(root)?;
    let mut git = git_present(root).then(|| GitHistory::new(root));
    derive(
        &tracked,
        record.as_deref(),
        || source_counter_sum(root),
        git.as_mut().map(|git| git as &mut dyn History),
        build,
        unix_seconds,
    )
    .map_err(|refusal| format!("{}: {refusal}", root.display()))
}

/// The build identity of Git HEAD at `root`, ignoring any record and any
/// worktree change: the record an export of HEAD carries. `build` marks the
/// product `-dev` for a tree that adds uncommitted changes to HEAD.
///
/// # Errors
///
/// Refuses a root without Git and whatever the Git source refuses.
pub fn head_build_info(root: &Path, build: Build) -> Result<BuildInfo, String> {
    if !git_present(root) {
        return Err(format!(
            "{}: HEAD's build record requires Git",
            root.display()
        ));
    }
    let mut git = GitHistory::new(root);
    let tracked = git
        .tracked_version()
        .map_err(|error| format!("{}: {error}", root.display()))?;
    derive(
        &tracked,
        None,
        || source_counter_sum(root),
        Some(&mut git),
        build,
        unix_seconds,
    )
    .map_err(|refusal| format!("{}: {refusal}", root.display()))
}

/// The product version [`build_info`] derives.
///
/// # Errors
///
/// Refuses whatever [`build_info`] refuses.
pub fn source_version(root: &Path, build: Build) -> Result<ProductVersion, String> {
    build_info(root, build).map(|info| info.product)
}

/// The commit the tree at `root` names: its record's source, else Git HEAD,
/// else none. Reads no history.
///
/// # Errors
///
/// Refuses a malformed record and an unreadable Git HEAD.
pub fn source_identity(root: &Path) -> Result<Option<SourceIdentity>, String> {
    if let Some(record) = build_record(root)? {
        let info = serde_json::from_slice::<BuildInfo>(&record)
            .ok()
            .filter(|info| build_record_bytes(info) == record)
            .ok_or_else(|| {
                format!(
                    "{}: {BUILD_RECORD_FILE} is not one canonical build record",
                    root.display()
                )
            })?;
        return info
            .source
            .map(|source| {
                SourceIdentity::parse(&source.commit, &source.committed.to_string())
                    .map_err(|detail| format!("{}: {BUILD_RECORD_FILE}: {detail}", root.display()))
            })
            .transpose();
    }
    if !git_present(root) {
        return Ok(None);
    }
    GitHistory::new(root)
        .head()
        .map(Some)
        .map_err(|error| format!("{}: {error}", root.display()))
}

/// Most bytes one release source archive may carry.
const SOURCE_ARCHIVE_LIMIT: u64 = 1024 * 1024 * 1024;

/// The identity of the source archive at `path`: its exact bytes, which every
/// target payload of the release records.
///
/// # Errors
///
/// Refuses an unreadable archive or one beyond the admitted size.
pub fn source_archive_identity(path: &Path) -> Result<Digest, String> {
    let refused =
        |error: io::Error| format!("cannot read source archive {}: {error}", path.display());
    let file = fs::File::open(path).map_err(refused)?;
    Digest::compute_reader(Domain::DistSourceArchive, file, SOURCE_ARCHIVE_LIMIT)
        .map(|(_, digest)| digest)
        .map_err(refused)
}

/// The sum of the counters in `root`'s tracked registry.
///
/// # Errors
///
/// Refuses an unreadable or malformed registry and overflow.
pub fn source_counter_sum(root: &Path) -> Result<u64, String> {
    let path = root.join(COUNTERS_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    crate::counter_sum(&text).map_err(|error| format!("{}: {error}", root.display()))
}

fn tracked_version(root: &Path) -> Result<TrackedVersion, String> {
    let path = root.join(VERSION_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    crate::admit_tracked_version(&text).map_err(|error| format!("{}: {error}", path.display()))
}

fn build_record(root: &Path) -> Result<Option<Vec<u8>>, String> {
    let path = root.join(BUILD_RECORD_FILE);
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

fn git_present(root: &Path) -> bool {
    root.join(".git").exists()
}

fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}
