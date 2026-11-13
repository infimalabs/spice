//! Binary-rim translation for release publication and daemonless notes.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::ArgMatches;
use spice::ambient::Ambient;
use wire::body::{ProofSealed, PublishedTarget, ReleasePublished};
use wire::manifest::ProductVersion;
use wire::release::{ReleaseObservations, ReleasePlan};
use wire::{Digest, Domain, Outcome, Refusal, Reply, ReplyPosition, Seq};

use crate::{environment_fault, protocol_fault};

mod view;

pub(crate) use view::{NotesView, PlanPreview};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AdmittedRelease {
    published: proof::PublishedManifest,
    notes: Digest,
    native: Digest,
    targets: Vec<PublishedTarget>,
}

impl AdmittedRelease {
    pub(crate) const fn manifest(&self) -> &Digest {
        &self.published.digest
    }

    pub(crate) const fn notes(&self) -> &Digest {
        &self.notes
    }

    pub(crate) fn receipts(&self) -> impl Iterator<Item = &Digest> {
        self.targets.iter().map(|target| &target.receipt)
    }

    pub(crate) fn targets(&self) -> &[PublishedTarget] {
        &self.targets
    }

    pub(crate) fn fact(&self) -> ReleasePublished {
        ReleasePublished {
            manifest: self.published.digest.clone(),
            notes: self.notes.clone(),
            targets: self.targets.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublicationState {
    Fresh { proof: Seq },
    Published { fact: Seq },
}

impl PublicationState {
    pub(crate) const fn observation(self) -> Seq {
        match self {
            Self::Fresh { proof } => proof,
            Self::Published { fact } => fact,
        }
    }
}

pub(crate) enum RunResult {
    Plan(Box<Outcome<PlanPreview>>),
    Published(Outcome<wire::intent::RecognitionAnswer>),
}

pub(crate) fn run(
    matches: &ArgMatches,
    contract: &crate::cli::Contract,
    ambient: &Ambient,
) -> RunResult {
    let prepared = match prepare(matches) {
        Ok(prepared) => prepared,
        Err(outcome) => return RunResult::Plan(Box::new(retag(*outcome))),
    };
    let Some(apply) = matches.get_one::<String>("apply") else {
        return RunResult::Plan(Box::new(view::plan_preview(
            prepared.plan,
            &prepared.common,
            &publish_words(matches),
        )));
    };
    let applied = match apply.parse::<Digest>() {
        Ok(applied) => applied,
        Err(error) => {
            return RunResult::Published(Outcome::Refusal(Refusal::bare(
                wire::RefusalDiagnostic::new(
                    "release::apply-digest-refused".to_owned(),
                    Some("publication-plan".to_owned()),
                    Some(prepared.plan.key.to_string()),
                    Some(error.to_string()),
                ),
            )));
        }
    };
    if applied != prepared.plan.key {
        return RunResult::Published(Outcome::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                "release::plan-moved".to_owned(),
                Some("publication-plan".to_owned()),
                Some(prepared.plan.key.to_string()),
                Some(applied.to_string()),
            ),
        )));
    }
    if let Err(outcome) = crate::release_execution::execute(
        &prepared.worktree,
        &prepared.operation_journal,
        &prepared.plan,
        &applied,
        ambient,
    ) {
        return RunResult::Published(outcome);
    }
    let outcome = crate::command_client::execute(
        &prepared.serve,
        contract,
        crate::command_client::SelectedCommand::ReleasePublish {
            release: &prepared.release,
            key: &prepared.plan.key,
            state: prepared.state,
        },
        ambient,
    );
    RunResult::Published(recognition_outcome(outcome))
}

struct Prepared {
    release: AdmittedRelease,
    plan: ReleasePlan,
    state: PublicationState,
    serve: PathBuf,
    worktree: PathBuf,
    operation_journal: PathBuf,
    common: PathBuf,
}

fn prepare(matches: &ArgMatches) -> Result<Prepared, Box<Outcome<ReleasePlan>>> {
    let current = std::env::current_dir().map_err(|error| {
        Box::new(environment_fault::<ReleasePlan>(
            "release::working-directory-unavailable",
            error.to_string(),
        ))
    })?;
    let repository = spice::repository::Repository::discover(&current).map_err(|error| {
        Box::new(environment_fault::<ReleasePlan>(
            spice::repository::ResolveError::REASON,
            error.to_string(),
        ))
    })?;
    let state_paths = repository
        .admit_state()
        .map_err(|error| Box::new(crate::state_failure(&error).outcome()))?;
    let reader = journal::JournalReader::open(state_paths.journal()).map_err(|error| {
        Box::new(environment_fault::<ReleasePlan>(
            "release::journal-unavailable",
            error.to_string(),
        ))
    })?;
    let history = crate::release_history::draft(
        repository.worktree_root(),
        &reader,
        matches
            .get_one::<String>("release-commit")
            .map(String::as_str),
        matches.get_one::<String>("version").map(String::as_str),
    )
    .map_err(|error| {
        Box::new(notes_refusal::<ReleasePlan>(
            "release::history-refused",
            &error,
        ))
    })?;
    let external = resolve_external_preconditions(
        repository.worktree_root(),
        matches,
        &history.release_commit,
    )
    .map_err(|error| {
        Box::new(refusal::<ReleasePlan>(
            "release::external-plan-refused",
            &error,
        ))
    })?;
    let canonical = crate::release_history::projected_draft(
        repository.worktree_root(),
        &reader,
        &external.source_commit,
        matches.get_one::<String>("version").map(String::as_str),
        &external.release_commit,
    )
    .map_err(|error| {
        Box::new(notes_refusal::<ReleasePlan>(
            "release::history-refused",
            &error,
        ))
    })?;
    let candidate = match matches.get_one::<String>("notes-file") {
        Some(path) => fs::read_to_string(path).map_err(|error| {
            Box::new(environment_fault::<ReleasePlan>(
                "release::notes-unreadable",
                format!("cannot read release notes {path:?}: {error}"),
            ))
        })?,
        None => canonical.document.clone(),
    };
    let notes = dist::curated_release_notes(&candidate, &canonical).map_err(|error| {
        Box::new(notes_refusal::<ReleasePlan>(
            "release::notes-refused",
            &error,
        ))
    })?;
    let app = app_digest(repository.worktree_root())
        .map_err(|error| Box::new(refusal::<ReleasePlan>("release::app-stage-refused", &error)))?;
    let release = admit_stage(
        &repository.worktree_root().join("dist/stage"),
        &app,
        &notes.identity,
    )
    .map_err(|error| Box::new(refusal::<ReleasePlan>("release::manifest-refused", &error)))?;
    let plan = bind_external_plan(
        repository.worktree_root(),
        notes.document,
        &release,
        external,
    )
    .map_err(|error| {
        Box::new(refusal::<ReleasePlan>(
            "release::external-plan-refused",
            &error,
        ))
    })?;
    let facts = reader.read(observed_facts).map_err(|error| {
        Box::new(environment_fault::<ReleasePlan>(
            "release::journal-unreadable",
            error.to_string(),
        ))
    })?;
    let state = observe(&facts, &release)
        .map_err(|error| Box::new(refusal::<ReleasePlan>("release::evidence-refused", &error)))?;
    Ok(Prepared {
        release,
        plan,
        state,
        serve: state_paths.serve().to_path_buf(),
        worktree: repository.worktree_root().to_path_buf(),
        operation_journal: state_paths.release_operations().to_path_buf(),
        common: repository.common_dir().to_path_buf(),
    })
}

/// Carry a refusal or fault from plan preparation over to the preview.
fn retag<B>(outcome: Outcome<ReleasePlan>) -> Outcome<B> {
    match outcome {
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
        Outcome::Reply(_) => protocol_fault(
            "release::preparation-answered",
            "release preparation answered where it could only refuse",
        ),
    }
}

/// The exact preview invocation, which `--apply` repeats.
fn publish_words(matches: &ArgMatches) -> Vec<String> {
    let mut words = vec!["release".to_owned(), "publish".to_owned()];
    for flag in ["notes-file", "release-commit", "version"] {
        if let Some(value) = matches.get_one::<String>(flag) {
            words.extend([format!("--{flag}"), value.clone()]);
        }
    }
    words
}

fn recognition_outcome(
    outcome: Outcome<wire::intent::CommandAnswer>,
) -> Outcome<wire::intent::RecognitionAnswer> {
    match outcome {
        Outcome::Reply(reply) => match reply.body {
            wire::intent::CommandAnswer::Recognition(body) => Outcome::Reply(Reply {
                position: reply.position,
                body,
            }),
            _ => protocol_fault(
                "release::answer-shape",
                "resident returned a non-recognition release answer",
            ),
        },
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

#[cfg(test)]
mod tests {
    use wire::intent::{CommandAnswer, StateAnswer};
    use wire::{ResidentState, Seq, SeqRange};

    use super::*;

    #[test]
    fn publication_wrapper_refuses_an_answer_outside_its_command_binding() {
        let outcome = Outcome::Reply(Reply {
            position: ReplyPosition::Committed(SeqRange {
                first: Seq::new(1).expect("fixture position"),
                last: Seq::new(1).expect("fixture position"),
            }),
            body: CommandAnswer::State(StateAnswer {
                from: ResidentState::Seated,
                to: ResidentState::Dark,
            }),
        });

        let Outcome::Fault(fault) = recognition_outcome(outcome) else {
            panic!("a release reply with another command's answer must fault");
        };
        assert_eq!(fault.class, wire::FaultClass::Protocol);
        assert_eq!(fault.reason, "release::answer-shape");
    }
}

pub(crate) fn app_digest(worktree: &Path) -> Result<Digest, String> {
    serve::AppSource::resolve(worktree)
        .and_then(|source| source.current_generation())
        .map(|generation| generation.identity().clone())
        .map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProofSourceIdentity {
    pub commit: String,
    pub tree: String,
    pub digest: Digest,
}

const RELEASE_MANIFEST_STATUS: &str = "?? dist/stage/release/manifest.json";

pub(crate) fn proof_source_identity(root: &Path) -> Result<ProofSourceIdentity, String> {
    let status = git_line(root, &["status", "--porcelain=v1", "--untracked-files=all"])?;
    let unexpected = status
        .lines()
        .filter(|line| *line != RELEASE_MANIFEST_STATUS)
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(format!(
            "proof source is not clean: {}",
            unexpected.join("\n")
        ));
    }
    let commit = git_line(root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let tree = git_line(root, &["rev-parse", "--verify", "HEAD^{tree}"])?;
    let digest = Digest::compute(Domain::DistSourceCommit, commit.as_bytes());
    Ok(ProofSourceIdentity {
        commit,
        tree,
        digest,
    })
}

#[cfg(test)]
mod proof_source_tests {
    use super::*;

    #[test]
    fn only_the_generated_release_manifest_is_absent_from_source_differences() {
        fn differences(status: &str) -> Vec<&str> {
            status
                .lines()
                .filter(|line| *line != RELEASE_MANIFEST_STATUS)
                .collect::<Vec<_>>()
        }

        assert!(differences("").is_empty());
        assert!(differences(RELEASE_MANIFEST_STATUS).is_empty());
        assert_eq!(
            differences("?? dist/stage/release/unexpected.json"),
            ["?? dist/stage/release/unexpected.json"]
        );
        assert_eq!(
            differences(&format!("{RELEASE_MANIFEST_STATUS}\n M source.rs")),
            [" M source.rs"]
        );
    }

    #[test]
    fn sealed_manifest_source_must_name_the_selected_release_commit() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let source = Digest::compute(Domain::DistSourceCommit, commit.as_bytes());
        admit_manifest_source(&source, commit).expect("matching sealed source");

        let moved = Digest::compute(Domain::DistSourceCommit, b"moved");
        assert_eq!(
            admit_manifest_source(&moved, commit),
            Err("sealed release source does not name the selected release commit".to_owned())
        );
    }

    #[test]
    fn sealed_targets_must_come_from_the_release_source_archive() {
        let archive = Digest::compute(Domain::DistSourceArchive, b"release source archive");
        admit_manifest_archive(&archive, &archive).expect("matching source archive");

        let other = Digest::compute(Domain::DistSourceArchive, b"another archive");
        let refused =
            admit_manifest_archive(&other, &archive).expect_err("another archive refuses");
        assert!(refused.contains(other.as_str()) && refused.contains(archive.as_str()));
    }
}

pub(crate) fn admit_stage(
    root: &Path,
    app: &Digest,
    notes: &Digest,
) -> Result<AdmittedRelease, ReleaseError> {
    let executable = std::env::current_exe().map_err(ReleaseError::CurrentExecutable)?;
    let bytes = fs::read(&executable).map_err(|source| ReleaseError::ReadExecutable {
        path: executable,
        source,
    })?;
    let native = Digest::compute(Domain::DistArtifact, &bytes);
    let target = proof::compiled_target().map_err(ReleaseError::UnsupportedHost)?;
    let published =
        proof::admit_manifest(root, app, target, &native).map_err(ReleaseError::Manifest)?;
    let targets = published
        .manifest
        .targets
        .iter()
        .map(|target| PublishedTarget {
            target: target.target.to_string(),
            artifact: target.payload.clone(),
            receipt: target.receipt.clone(),
        })
        .collect();
    Ok(AdmittedRelease {
        published,
        notes: notes.clone(),
        native,
        targets,
    })
}

struct ExternalPreconditions {
    requested_version: ProductVersion,
    source_commit: String,
    release_commit: String,
    repository: Digest,
    head: String,
    clean: bool,
    unrecorded_local_commits: u64,
    source_version: ProductVersion,
    release_version: ProductVersion,
    package_ref: Option<String>,
    tag_ref: Option<String>,
    announcement_ref: Option<String>,
}

struct LocalPreconditions {
    source_commit: String,
    release_commit: String,
    head: String,
    clean: bool,
    release_version: ProductVersion,
}

struct ReleaseVersions {
    requested: ProductVersion,
    source: ProductVersion,
    observed: ProductVersion,
}

struct ExternalRefs {
    package_ref: Option<String>,
    tag_ref: Option<String>,
    announcement_ref: Option<String>,
}

fn resolve_external_preconditions(
    root: &Path,
    matches: &ArgMatches,
    revision_commit: &str,
) -> Result<ExternalPreconditions, String> {
    let versions = release_versions(root, matches)?;
    let local = resolve_local_preconditions(
        root,
        revision_commit,
        &versions.source,
        &versions.requested,
        &versions.observed,
    )?;
    let authority = dist::current_authority(root).map_err(|error| error.to_string())?;
    ensure_source_integrated(root, &authority, &versions.requested, &local.source_commit)?;
    let external_refs =
        resolve_external_refs(root, &authority, &versions.requested, &local.release_commit)?;
    validate_external_refs(&external_refs, &local.release_commit)?;
    Ok(ExternalPreconditions {
        requested_version: versions.requested,
        source_commit: local.source_commit,
        release_commit: local.release_commit,
        repository: repository_identity(root, authority.remote())?,
        head: local.head,
        clean: local.clean,
        unrecorded_local_commits: 0,
        source_version: versions.source,
        release_version: local.release_version,
        package_ref: external_refs.package_ref,
        tag_ref: external_refs.tag_ref,
        announcement_ref: external_refs.announcement_ref,
    })
}

/// The requested version, the version this executable's compiled counters
/// admit for the release commit, and the version the checkout derives. Release
/// never edits version inputs.
fn release_versions(root: &Path, matches: &ArgMatches) -> Result<ReleaseVersions, String> {
    spice::version::source_identity(root)?
        .ok_or_else(|| "release requires an exact source commit identity".to_owned())?;
    let compiled = dist::current_counter_sum().map_err(|error| error.to_string())?;
    let source = dist::source_counter_sum(root)?;
    if compiled != source {
        return Err(format!(
            "release requires compiled and tracked counter sums to agree; compiled={compiled} tracked={source}"
        ));
    }
    let release_version = spice::version::source_version(root, dist::Build::Staged)?;
    let source_version = release_version.clone();
    let requested_version = matches.get_one::<String>("version").map_or_else(
        || Ok(release_version.clone()),
        |requested| ProductVersion::parse(requested).map_err(|error| error.to_string()),
    )?;
    Ok(ReleaseVersions {
        requested: requested_version,
        source: source_version,
        observed: release_version,
    })
}

fn resolve_external_refs(
    root: &Path,
    authority: &dist::ReleaseAuthority,
    requested_version: &ProductVersion,
    release_commit: &str,
) -> Result<ExternalRefs, String> {
    let refs = authority.refs(requested_version);
    git_owned(root, &authority.remote_visibility())?;
    let package_ref = remote_ref(root, authority, refs.package())?;
    let tag_ref = normalize_converged_ref(remote_ref(root, authority, refs.tag())?, release_commit);
    let announcement_ref = normalize_converged_ref(
        remote_ref(root, authority, refs.announcement())?,
        release_commit,
    );
    Ok(ExternalRefs {
        package_ref,
        tag_ref,
        announcement_ref,
    })
}

fn validate_external_refs(refs: &ExternalRefs, release_commit: &str) -> Result<(), String> {
    ensure_release_ref_absent("tag", &refs.tag_ref, release_commit)?;
    ensure_release_ref_absent("announcement", &refs.announcement_ref, release_commit)
}

fn ensure_source_integrated(
    root: &Path,
    authority: &dist::ReleaseAuthority,
    version: &ProductVersion,
    source_commit: &str,
) -> Result<(), String> {
    let source_ref = authority.refs(version).source().to_owned();
    let branch = source_ref
        .strip_prefix("refs/heads/")
        .ok_or_else(|| format!("release source ref is not a branch: {source_ref}"))?;
    let tracking_ref = format!("refs/remotes/{}/{branch}", authority.remote());
    let (integrated, detail) = git_output(
        root,
        &["merge-base", "--is-ancestor", source_commit, &tracking_ref],
    )?;
    if integrated {
        Ok(())
    } else {
        Err(format!(
            "release source {source_commit} is not integrated in {tracking_ref}: {detail}"
        ))
    }
}

fn ensure_release_ref_absent(
    label: &str,
    observed: &Option<String>,
    release_commit: &str,
) -> Result<(), String> {
    if let Some(object) = observed {
        return Err(format!(
            "the {label} ref names {object}, not {release_commit}"
        ));
    }
    Ok(())
}

fn resolve_local_preconditions(
    root: &Path,
    revision_commit: &str,
    source_version: &ProductVersion,
    requested_version: &ProductVersion,
    observed_version: &ProductVersion,
) -> Result<LocalPreconditions, String> {
    let head = git_required(root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let status = tracked_worktree_status(root)?;
    if head != revision_commit || !status.is_empty() {
        return Err("release requires the clean selected release commit at HEAD".to_owned());
    }
    if source_version != requested_version || observed_version != requested_version {
        return Err(format!(
            "release requires requested, compiled, and tracked product version {requested_version} to agree; compiled={source_version} tracked={observed_version}"
        ));
    }
    Ok(LocalPreconditions {
        source_commit: head.clone(),
        release_commit: head.clone(),
        head,
        clean: true,
        release_version: requested_version.clone(),
    })
}

fn bind_external_plan(
    root: &Path,
    notes_document: String,
    release: &AdmittedRelease,
    external: ExternalPreconditions,
) -> Result<ReleasePlan, String> {
    admit_manifest_source(&release.published.manifest.source, &external.release_commit)?;
    let source_archive =
        dist::source_archive_identity(&root.join("dist/stage").join(dist::SOURCE_ARCHIVE_FILE))?;
    admit_manifest_archive(&release.published.manifest.archive, &source_archive)?;
    let package = crate::release_package::ReleaseIndex::admit(
        root,
        &external.requested_version,
        release.notes.clone(),
        notes_document.as_bytes(),
        &release.published.digest,
    )
    .map_err(|error| error.to_string())?;
    let package = package.preview();
    let package_ref = normalize_converged_ref(external.package_ref, &package.git_tree);
    let observations = ReleaseObservations {
        head: external.head,
        clean: external.clean,
        unrecorded_local_commits: external.unrecorded_local_commits,
        source_version: external.source_version,
        release_version: external.release_version,
        credentials_valid: true,
        package_ref,
        tag_ref: external.tag_ref,
        announcement_ref: external.announcement_ref,
    };
    dist::plan_release(dist::PlanInput {
        repository: external.repository,
        requested_version: external.requested_version,
        source_commit: external.source_commit,
        source_archive,
        release_commit: external.release_commit,
        notes_document,
        manifest: release.published.digest.clone(),
        package: package.identity.clone(),
        package_object: package.git_tree.clone(),
        installed_artifact: release.native.clone(),
        targets: release.targets.clone(),
        observations,
    })
    .map_err(|error| error.to_string())
}

fn admit_manifest_source(source: &Digest, release_commit: &str) -> Result<(), String> {
    let expected = Digest::compute(Domain::DistSourceCommit, release_commit.as_bytes());
    if source == &expected {
        Ok(())
    } else {
        Err("sealed release source does not name the selected release commit".to_owned())
    }
}

fn admit_manifest_archive(archive: &Digest, source_archive: &Digest) -> Result<(), String> {
    if archive == source_archive {
        Ok(())
    } else {
        Err(format!(
            "sealed release targets were staged from source archive {archive}, not the release's {source_archive}"
        ))
    }
}

fn tracked_worktree_status(root: &Path) -> Result<String, String> {
    git_required(root, &["status", "--porcelain=v1", "--untracked-files=no"])
}

pub(super) fn repository_identity(root: &Path, remote: &str) -> Result<Digest, String> {
    let key = format!("remote.{remote}.url");
    let repository = git_optional(root, &["config", "--get", &key])?
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    Ok(Digest::compute(
        Domain::DistSourceCommit,
        repository.as_bytes(),
    ))
}

fn remote_ref(
    root: &Path,
    authority: &dist::ReleaseAuthority,
    reference: &str,
) -> Result<Option<String>, String> {
    let command = authority
        .reference_visibility(reference)
        .map_err(|error| error.to_string())?;
    let output = git_owned(root, &command)?;
    let mut fields = output.split_whitespace();
    Ok(fields.next().map(str::to_owned))
}

fn git_owned(root: &Path, command: &dist::GitCommand) -> Result<String, String> {
    let arguments = command
        .arguments()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    git_required(root, &arguments)
}

fn normalize_converged_ref(observed: Option<String>, release: &str) -> Option<String> {
    observed.filter(|object| object != release)
}

fn git_required(root: &Path, arguments: &[&str]) -> Result<String, String> {
    git_output(root, arguments).and_then(|(success, output)| {
        if success {
            Ok(output)
        } else {
            Err(format!("git {} refused: {output}", arguments.join(" ")))
        }
    })
}

fn git_optional(root: &Path, arguments: &[&str]) -> Result<Option<String>, String> {
    git_output(root, arguments).map(|(success, output)| success.then_some(output))
}

fn git_output(root: &Path, arguments: &[&str]) -> Result<(bool, String), String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|error| format!("could not launch git: {error}"))?;
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("git stdout is not UTF-8: {error}"))?;
    let stderr = String::from_utf8(output.stderr)
        .map_err(|error| format!("git stderr is not UTF-8: {error}"))?;
    let detail = [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Ok((output.status.success(), detail))
}

fn git_line(root: &Path, arguments: &[&str]) -> Result<String, String> {
    let (success, output) = git_output(root, arguments)?;
    if !success {
        return Err(format!("git {:?} refused: {output}", arguments));
    }
    if output.contains(['\r', '\n']) {
        return Err(format!("git {:?} returned more than one line", arguments));
    }
    Ok(output)
}

/// The kinds a release observation reads: the proof sealing a manifest and
/// the publication naming it.
pub(crate) const OBSERVED_KINDS: [wire::FactKind; 2] = [
    wire::FactKind::ProofSealed,
    wire::FactKind::ReleasePublished,
];

/// Every observed fact inside one guarded read, rather than the whole
/// journal those few facts sit in.
pub(crate) fn observed_facts(
    read: &journal::JournalRead<'_>,
) -> Result<Vec<wire::Envelope<wire::AnyFact>>, journal::GuardError> {
    let page = read.selected_fact_page(
        &OBSERVED_KINDS,
        None,
        None,
        journal::FactReadBudget::RETAINED,
    )?;
    if let Some(continuation) = page.continuation {
        return Err(journal::GuardError::refused(format!(
            "release observation requires continuation after {continuation}"
        )));
    }
    Ok(page.rows)
}

pub(crate) fn observe<'a>(
    facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
    release: &AdmittedRelease,
) -> Result<PublicationState, ObservationError> {
    let mut proof = None;
    let mut published = None;
    let expected = release.fact();
    for envelope in facts {
        if let Some(body) = envelope.fact.read::<ProofSealed>() {
            if body.manifest == *release.manifest() {
                proof = Some(envelope.seq);
            }
        } else if let Some(body) = envelope.fact.read::<ReleasePublished>() {
            if body.manifest != *release.manifest() {
                continue;
            }
            if body != expected {
                return Err(ObservationError::PublishedMismatch(envelope.seq));
            }
            if published.replace(envelope.seq).is_some() {
                return Err(ObservationError::DuplicatePublication);
            }
        }
    }
    if let Some(fact) = published {
        Ok(PublicationState::Published { fact })
    } else {
        proof
            .map(|proof| PublicationState::Fresh { proof })
            .ok_or(ObservationError::MissingProof)
    }
}

#[derive(Debug)]
pub(crate) enum ReleaseError {
    CurrentExecutable(std::io::Error),
    ReadExecutable {
        path: PathBuf,
        source: std::io::Error,
    },
    UnsupportedHost(proof::UnsupportedHost),
    Manifest(proof::ManifestAdmissionError),
}

impl fmt::Display for ReleaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentExecutable(source) => {
                write!(formatter, "current executable is unresolved: {source}")
            }
            Self::ReadExecutable { path, source } => {
                write!(
                    formatter,
                    "current executable {} is unreadable: {source}",
                    path.display()
                )
            }
            Self::UnsupportedHost(source) => source.fmt(formatter),
            Self::Manifest(source) => source.fmt(formatter),
        }
    }
}

impl Error for ReleaseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::CurrentExecutable(source) => Some(source),
            Self::ReadExecutable { source, .. } => Some(source),
            Self::UnsupportedHost(source) => Some(source),
            Self::Manifest(source) => Some(source),
        }
    }
}

#[derive(Debug)]
pub(crate) enum ObservationError {
    MissingProof,
    PublishedMismatch(Seq),
    DuplicatePublication,
}

impl fmt::Display for ObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProof => {
                formatter.write_str("the current manifest has no durable proof::sealed recognition")
            }
            Self::PublishedMismatch(seq) => write!(
                formatter,
                "release publication at {seq} disagrees with the sealed target identities"
            ),
            Self::DuplicatePublication => {
                formatter.write_str("the sealed manifest was published more than once")
            }
        }
    }
}

impl Error for ObservationError {}

fn refusal<B>(reason: &str, error: &impl fmt::Display) -> Outcome<B> {
    Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
        reason.to_owned(),
        Some("sealed-release".to_owned()),
        Some("one current complete all-target proof and manifest chain".to_owned()),
        Some(error.to_string()),
    )))
}

pub(crate) fn notes(matches: &clap::ArgMatches) -> Outcome<NotesView> {
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(error) => {
            return environment_fault("release::working-directory-unavailable", error.to_string());
        }
    };
    let repository = match spice::repository::Repository::discover(&current) {
        Ok(repository) => repository,
        Err(error) => {
            return environment_fault(spice::repository::ResolveError::REASON, error.to_string());
        }
    };
    let state_paths = match repository.admit_state() {
        Ok(paths) => paths,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    let reader = match journal::JournalReader::open(state_paths.journal()) {
        Ok(reader) => reader,
        Err(error) => {
            return environment_fault("release::journal-unavailable", error.to_string());
        }
    };
    let release_commit = matches
        .get_one::<String>("release-commit")
        .map(String::as_str);
    let notes = match crate::release_history::draft(
        repository.worktree_root(),
        &reader,
        release_commit,
        None,
    ) {
        Ok(notes) => notes,
        Err(error) => return notes_refusal("release::history-refused", &error),
    };
    let output = matches.get_one::<String>("output").map(String::as_str);
    match view::notes_view(&notes, output, release_commit) {
        Ok(body) => Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body,
        }),
        Err(refusal) => Outcome::Refusal(refusal),
    }
}

fn notes_refusal<B>(reason: &str, error: &impl fmt::Display) -> Outcome<B> {
    Outcome::Refusal(Refusal::bare(wire::RefusalDiagnostic::new(
        reason.to_owned(),
        Some("first-parent-release-notes".to_owned()),
        Some(
            "reachable release history and curated highlights above the generated inventory"
                .to_owned(),
        ),
        Some(error.to_string()),
    )))
}
