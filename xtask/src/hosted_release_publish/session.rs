use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use spice::repository::Repository;
use windows_execution::hosted::release_inputs::ResultManifest;

use super::{PublishError, PublishRequest, plan::PLAN_FILE};

const SCHEMA: u16 = 1;
const PENDING: &str = "pending.json";
/// Present only while a session's persisted plan has reached no publication
/// effect: a stage-only request writes it, publishing withdraws it first.
const UNPUBLISHED: &str = "unpublished";
const COMMIT_HEX_LENGTH: usize = 40;

/// Every publication under one Git-common lock, and the one pinned commit a
/// later invocation must name to resume or abandon it.
pub(super) struct Publications {
    root: PathBuf,
    _possession: sill::Possession,
}

/// One publication's durable state, keyed by the commit its request pinned.
pub(super) struct PublicationSession {
    root: PathBuf,
    record_path: PathBuf,
    source_commit: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema: u16,
    source_commit: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u16,
    source_commit: String,
    inputs_run: u64,
}

impl Publications {
    pub(super) fn lock(worktree: &Path) -> Result<Self, PublishError> {
        let repository = Repository::discover(worktree)
            .map_err(|error| PublishError::Refused(error.to_string()))?;
        let state = repository
            .admit_state()
            .map_err(|error| PublishError::Refused(error.to_string()))?;
        Self::at(
            state.release_operations().join("publication"),
            state.locks(),
        )
    }

    fn at(root: PathBuf, locks: &Path) -> Result<Self, PublishError> {
        for directory in [root.as_path(), locks] {
            fs::create_dir_all(directory).map_err(|source| PublishError::Io {
                path: directory.to_path_buf(),
                source,
            })?;
        }
        let lock_name = sill::LockName::new("publish-release".to_owned())
            .map_err(|error| PublishError::Refused(error.to_string()))?;
        let lock_path = locks.join("publish-release");
        let possession = match sill::LockFile::new(lock_name, &lock_path)
            .try_acquire()
            .map_err(|source| PublishError::Io {
                path: lock_path,
                source,
            })? {
            sill::Acquisition::Acquired(possession) => possession,
            sill::Acquisition::Contended => {
                return Err(PublishError::Refused(
                    "another publisher owns the release request".into(),
                ));
            }
        };
        Ok(Self {
            root,
            _possession: possession,
        })
    }

    /// The commit an unfinished request pinned, if any.
    fn pending(&self) -> Result<Option<String>, PublishError> {
        let Some(bytes) = read_optional(&self.root.join(PENDING))? else {
            return Ok(None);
        };
        let pending: Pending = serde_json::from_slice(&bytes).map_err(PublishError::Decode)?;
        if pending.schema != SCHEMA || !canonical_commit(&pending.source_commit) {
            return Err(PublishError::Refused(
                "pending publication names no canonical source commit".into(),
            ));
        }
        Ok(Some(pending.source_commit))
    }

    /// Admit a local request against the pending pin: the exact pinned commit
    /// a resumption names, or `None` when nothing is pending and a new request
    /// may pin `main`. Any other pairing refuses before a provider call.
    pub(super) fn admit(&self, request: &PublishRequest) -> Result<Option<String>, PublishError> {
        match (self.pending()?, request) {
            (None, PublishRequest::New { .. } | PublishRequest::NewAdmitted { .. }) => Ok(None),
            (
                Some(pinned),
                PublishRequest::Resume { commit, .. }
                | PublishRequest::ResumeAdmitted { commit, .. },
            ) if pinned == *commit => Ok(Some(pinned)),
            (Some(pinned), _) => Err(self.pinned_elsewhere(&pinned)?),
            (
                None,
                PublishRequest::Resume { commit, .. }
                | PublishRequest::ResumeAdmitted { commit, .. }
                | PublishRequest::Abandon(commit),
            ) => Err(PublishError::Refused(format!(
                "no publication request is pending, so {commit} cannot be resumed or \
                 abandoned; run publish-hosted-release with --notes FILE to pin main"
            ))),
        }
    }

    /// Select retained authority for one source-bound admitted notes record.
    ///
    /// Matching authority resumes. A different source replaces the retained
    /// request only while it is safe to abandon; once publication may have
    /// started, [`Self::abandon`] refuses and preserves the original pin.
    pub(super) fn admit_admitted(
        &self,
        request: &PublishRequest,
        source_commit: &str,
    ) -> Result<Option<String>, PublishError> {
        match (self.pending()?, request) {
            (None, PublishRequest::NewAdmitted { .. }) => Ok(None),
            (Some(pinned), PublishRequest::NewAdmitted { .. }) => {
                self.select_admitted(pinned, source_commit)
            }
            (Some(pinned), PublishRequest::ResumeAdmitted { commit, .. }) if pinned == *commit => {
                self.select_admitted(pinned, source_commit)
            }
            (Some(pinned), _) => Err(self.pinned_elsewhere(&pinned)?),
            (None, PublishRequest::ResumeAdmitted { commit, .. }) => Err(PublishError::Refused(
                format!("no publication request is pending, so {commit} cannot be resumed"),
            )),
            (None, _) => Err(PublishError::Refused(
                "an admitted source requires an admitted release-notes request".into(),
            )),
        }
    }

    fn select_admitted(
        &self,
        pinned: String,
        source_commit: &str,
    ) -> Result<Option<String>, PublishError> {
        if pinned == source_commit {
            return Ok(Some(pinned));
        }
        self.abandon(&pinned)?;
        Ok(None)
    }

    /// Drop the pending request pinned to exactly `commit`, with its recorded
    /// preflight, while it cannot have reached a tag or release mutation: it
    /// has no publication plan, or only a stage-only request persisted one.
    /// Otherwise publication converges only by resumption.
    pub(super) fn abandon(&self, commit: &str) -> Result<(), PublishError> {
        match self.pending()? {
            Some(pinned) if pinned == commit => {}
            Some(pinned) => return Err(self.pinned_elsewhere(&pinned)?),
            None => {
                return Err(PublishError::Refused(format!(
                    "no publication request is pending, so {commit} cannot be abandoned"
                )));
            }
        }
        let session = self.session(commit);
        if session.publication_may_have_started()? {
            return Err(PublishError::Refused(format!(
                "the request for {commit} already admitted its publication plan and may have \
                 mutated the release; resume it with `spice dev xtask -- publish-hosted-release \
                 {commit} --notes FILE`"
            )));
        }
        match fs::remove_dir_all(&session.root) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(PublishError::Io {
                    path: session.root,
                    source,
                });
            }
        }
        self.finish()
    }

    fn pinned_elsewhere(&self, pinned: &str) -> Result<PublishError, PublishError> {
        let inputs = match self.session(pinned).inputs_run()? {
            Some(run) => format!("inputs run {run}"),
            None => "no inputs run yet".to_owned(),
        };
        Ok(PublishError::Refused(format!(
            "a publication request is pending for {pinned} ({inputs}); resume it with \
             `spice dev xtask -- publish-hosted-release {pinned} --notes FILE`, or drop it before \
             any release mutation with `spice dev xtask -- publish-hosted-release --abandon {pinned}`"
        )))
    }

    fn session(&self, source_commit: &str) -> PublicationSession {
        let root = self.root.join(source_commit);
        PublicationSession {
            record_path: root.join("inputs.json"),
            root,
            source_commit: source_commit.to_owned(),
        }
    }

    /// Pin `source_commit` as the request every later invocation resumes
    /// until it is published.
    pub(super) fn begin(&self, source_commit: &str) -> Result<PublicationSession, PublishError> {
        if !canonical_commit(source_commit) {
            return Err(PublishError::Refused(format!(
                "{source_commit} is not one canonical source commit"
            )));
        }
        let session = self.session(source_commit);
        fs::create_dir_all(&session.root).map_err(|source| PublishError::Io {
            path: session.root.clone(),
            source,
        })?;
        let pending = Pending {
            schema: SCHEMA,
            source_commit: source_commit.to_owned(),
        };
        publish(
            &self.root.join(PENDING),
            &serde_json::to_vec(&pending).map_err(PublishError::Encode)?,
        )?;
        Ok(session)
    }

    /// Release the pinned request once its publication verified.
    pub(super) fn finish(&self) -> Result<(), PublishError> {
        let path = self.root.join(PENDING);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(PublishError::Io { path, source }),
        }
    }
}

impl PublicationSession {
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// Durable generated stage authority for this exact pinned publication.
    pub(super) fn stage_root(&self) -> PathBuf {
        self.root.join("stage")
    }

    pub(super) fn source_commit(&self) -> &str {
        &self.source_commit
    }

    /// Whether this session holds a plan that a publishing request may have
    /// carried into tag or release effects.
    pub(super) fn publication_may_have_started(&self) -> Result<bool, PublishError> {
        Ok(exists(&self.root.join(PLAN_FILE))? && !exists(&self.root.join(UNPUBLISHED))?)
    }

    pub(super) fn mark_unpublished(&self) -> Result<(), PublishError> {
        publish(&self.root.join(UNPUBLISHED), &[])
    }

    pub(super) fn withdraw_unpublished(&self) -> Result<(), PublishError> {
        let path = self.root.join(UNPUBLISHED);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(PublishError::Io { path, source }),
        }
    }

    pub(super) fn inputs_run(&self) -> Result<Option<u64>, PublishError> {
        let Some(bytes) = read_optional(&self.record_path)? else {
            return Ok(None);
        };
        let record: Record = serde_json::from_slice(&bytes).map_err(PublishError::Decode)?;
        if record.schema != SCHEMA
            || record.source_commit != self.source_commit
            || record.inputs_run == 0
        {
            return Err(PublishError::Refused(
                "publication input record moved from this exact source".into(),
            ));
        }
        Ok(Some(record.inputs_run))
    }

    pub(super) fn record_inputs(&self, inputs: &ResultManifest) -> Result<(), PublishError> {
        let record = Record {
            schema: SCHEMA,
            source_commit: inputs.source_commit.clone(),
            inputs_run: inputs.run_id,
        };
        publish(
            &self.record_path,
            &serde_json::to_vec(&record).map_err(PublishError::Encode)?,
        )
    }
}

fn canonical_commit(commit: &str) -> bool {
    commit.len() == COMMIT_HEX_LENGTH
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn exists(path: &Path) -> Result<bool, PublishError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(PublishError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, PublishError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PublishError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn publish(path: &Path, bytes: &[u8]) -> Result<(), PublishError> {
    let atomic_error = |source| PublishError::Atomic {
        path: path.to_path_buf(),
        source,
    };
    let mut staged = atomic::Destination::new(path)
        .map_err(atomic_error)?
        .stage()
        .map_err(atomic_error)?;
    staged.write_all(bytes).map_err(|source| PublishError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    staged.commit().map_err(atomic_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "spice-publication-session-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("scratch");
            Self(root)
        }

        fn publications(&self) -> Result<Publications, PublishError> {
            Publications::at(self.0.join("publication"), &self.0.join("locks"))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn refused(result: Result<impl std::fmt::Debug, PublishError>, naming: &str) {
        match result {
            Err(PublishError::Refused(detail)) => assert!(
                detail.contains(naming),
                "the refusal does not name {naming}: {detail}"
            ),
            other => panic!("expected a refusal naming {naming}, got {other:?}"),
        }
    }

    fn new_request() -> PublishRequest {
        PublishRequest::New {
            notes: PathBuf::from("notes.md"),
        }
    }

    fn resume(commit: String) -> PublishRequest {
        PublishRequest::Resume {
            commit,
            notes: PathBuf::from("notes.md"),
        }
    }

    fn admitted_request() -> PublishRequest {
        PublishRequest::NewAdmitted {
            notes: PathBuf::from("notes.json"),
        }
    }

    fn admitted_resume(commit: String) -> PublishRequest {
        PublishRequest::ResumeAdmitted {
            commit,
            notes: PathBuf::from("notes.json"),
        }
    }

    #[test]
    fn a_pending_request_resumes_only_when_its_commit_is_named() {
        let scratch = Scratch::new();
        let pinned = "a".repeat(COMMIT_HEX_LENGTH);
        let other = "b".repeat(COMMIT_HEX_LENGTH);
        {
            let publications = scratch.publications().expect("first request");
            assert_eq!(publications.admit(&new_request()).expect("new"), None);
            let session = publications.begin(&pinned).expect("pin");
            assert_eq!(session.inputs_run().expect("no preflight yet"), None);
        }
        let publications = scratch.publications().expect("later invocation");
        refused(publications.admit(&new_request()), &pinned);
        refused(publications.admit(&resume(other.clone())), &pinned);
        assert_eq!(
            publications
                .admit(&resume(pinned.clone()))
                .expect("the named pin resumes"),
            Some(pinned.clone())
        );
        publications.finish().expect("finish");
        assert_eq!(publications.admit(&new_request()).expect("released"), None);
        refused(
            publications.admit(&resume(pinned)),
            "no publication request",
        );
    }

    #[test]
    fn admitted_source_replaces_only_safe_retained_authority() {
        let scratch = Scratch::new();
        let first = "a".repeat(COMMIT_HEX_LENGTH);
        let second = "b".repeat(COMMIT_HEX_LENGTH);
        let third = "c".repeat(COMMIT_HEX_LENGTH);
        let publications = scratch.publications().expect("publication authority");
        let first_session = publications.begin(&first).expect("first pin");
        fs::write(first_session.root.join(PLAN_FILE), b"admitted plan").expect("persist plan");
        first_session.mark_unpublished().expect("stage-only marker");

        assert_eq!(
            publications
                .admit_admitted(&admitted_resume(first.clone()), &second)
                .expect("another admitted source replaces safe retained authority"),
            None
        );
        assert!(
            !first_session.root.exists(),
            "superseded source authority survived"
        );

        let second_session = publications.begin(&second).expect("second pin");
        assert_eq!(
            publications
                .admit_admitted(&admitted_resume(second.clone()), &second)
                .expect("exact admitted source resumes"),
            Some(second.clone())
        );

        fs::write(second_session.root.join(PLAN_FILE), b"crossed plan").expect("persist plan");
        refused(
            publications.admit_admitted(&admitted_request(), &third),
            "resume it",
        );
        assert_eq!(
            publications.pending().expect("retained crossed pin"),
            Some(second)
        );
    }

    #[test]
    fn only_a_matching_pin_without_a_plan_is_abandoned() {
        let scratch = Scratch::new();
        let pinned = "a".repeat(COMMIT_HEX_LENGTH);
        let publications = scratch.publications().expect("request");
        let session = publications.begin(&pinned).expect("pin");
        let record = Record {
            schema: SCHEMA,
            source_commit: pinned.clone(),
            inputs_run: 7,
        };
        fs::write(
            &session.record_path,
            serde_json::to_vec(&record).expect("encode preflight record"),
        )
        .expect("record preflight");

        refused(
            publications.abandon(&"b".repeat(COMMIT_HEX_LENGTH)),
            &format!("{pinned} (inputs run 7)"),
        );
        publications
            .abandon(&pinned)
            .expect("an unplanned pin abandons");
        assert_eq!(publications.admit(&new_request()).expect("cleared"), None);
        assert!(
            !session.root.exists(),
            "the abandoned preflight record survived"
        );

        let session = publications.begin(&pinned).expect("pin again");
        fs::write(session.root.join(PLAN_FILE), b"admitted plan").expect("persist plan");
        refused(publications.abandon(&pinned), "resume it");
        assert_eq!(
            publications
                .admit(&resume(pinned.clone()))
                .expect("a planned pin still resumes"),
            Some(pinned)
        );
    }

    #[test]
    fn a_stage_only_plan_abandons_until_a_publishing_request_crosses_it() {
        use crate::hosted_release_publish::{PublishEffect, conclude};

        let scratch = Scratch::new();
        let pinned = "a".repeat(COMMIT_HEX_LENGTH);
        let publications = scratch.publications().expect("request");
        let stage_only = |session: &PublicationSession| {
            let unpublished = !session.publication_may_have_started().expect("observe");
            fs::write(session.root.join(PLAN_FILE), b"admitted plan").expect("persist plan");
            conclude(PublishEffect::StageOnly, session, unpublished, || {
                panic!("stage-only reached a publication effect")
            })
            .expect("stage only");
        };

        let session = publications.begin(&pinned).expect("pin");
        stage_only(&session);
        publications
            .abandon(&pinned)
            .expect("a stage-only plan abandons");
        assert!(!session.root.exists(), "the abandoned stage survived");

        let session = publications.begin(&pinned).expect("pin again");
        stage_only(&session);
        let unpublished = !session.publication_may_have_started().expect("observe");
        let mut published = false;
        conclude(PublishEffect::Publish, &session, unpublished, || {
            published = true;
            refused(publications.abandon(&pinned), "resume it");
            Err(PublishError::Refused("provider interrupted".into()))
        })
        .expect_err("the interrupted publication surfaces");
        assert!(published, "publishing never reached its effect");

        stage_only(&session);
        refused(publications.abandon(&pinned), "resume it");
        assert_eq!(
            publications
                .admit(&resume(pinned.clone()))
                .expect("a crossed plan still resumes"),
            Some(pinned)
        );
    }

    #[test]
    fn generated_stage_authority_resumes_outside_the_operator_worktree() {
        let scratch = Scratch::new();
        let worktree = scratch.0.join("worktree");
        fs::create_dir(&worktree).expect("operator worktree");
        let operator_file = worktree.join("notes.md");
        fs::write(&operator_file, b"operator bytes").expect("operator file");
        let pinned = "a".repeat(COMMIT_HEX_LENGTH);
        let stage_root;

        {
            let publications = scratch.publications().expect("first invocation");
            let session = publications.begin(&pinned).expect("pin");
            stage_root = session.stage_root();
            let receipt = stage_root.join("proofs/source/target/receipt.json");
            let manifest = stage_root.join("release/manifest.json");
            fs::create_dir_all(receipt.parent().expect("receipt parent")).expect("proof tree");
            fs::create_dir_all(manifest.parent().expect("manifest parent")).expect("release tree");
            fs::write(&receipt, b"receipt bytes").expect("receipt");
            fs::write(&manifest, b"manifest bytes").expect("manifest");
        }

        {
            let publications = scratch.publications().expect("resumed invocation");
            assert_eq!(
                publications
                    .admit(&resume(pinned.clone()))
                    .expect("exact pin resumes"),
                Some(pinned.clone())
            );
            let session = publications.begin(&pinned).expect("resume pin");
            assert_eq!(session.stage_root(), stage_root);
            assert_eq!(
                fs::read(stage_root.join("proofs/source/target/receipt.json"))
                    .expect("receipt resumes"),
                b"receipt bytes"
            );
            assert_eq!(
                fs::read(stage_root.join("release/manifest.json")).expect("manifest resumes"),
                b"manifest bytes"
            );
        }

        assert_eq!(
            fs::read(operator_file).expect("operator file survives"),
            b"operator bytes"
        );
        assert!(
            !worktree.join("dist/stage").exists(),
            "generated authority never enters the worktree"
        );
    }

    #[test]
    fn only_a_canonical_commit_is_pinned() {
        let scratch = Scratch::new();
        let publications = scratch.publications().expect("request");
        for commit in ["HEAD", "main", &"A".repeat(COMMIT_HEX_LENGTH)] {
            assert!(matches!(
                publications.begin(commit),
                Err(PublishError::Refused(_))
            ));
        }
        assert_eq!(publications.pending().expect("nothing pinned"), None);
    }
}
