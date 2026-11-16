//! Binary-rim translation for daemonless release notes and proof source identity.

use std::fmt;
use std::path::Path;
use std::process::Command;

use wire::{Digest, Domain, Outcome, Refusal, Reply, ReplyPosition};

use crate::environment_fault;

mod view;

pub(crate) use view::NotesView;

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

fn git_line(root: &Path, arguments: &[&str]) -> Result<String, String> {
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
    if !output.status.success() {
        return Err(format!("git {:?} refused: {detail}", arguments));
    }
    if detail.contains(['\r', '\n']) {
        return Err(format!("git {:?} returned more than one line", arguments));
    }
    Ok(detail)
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
    let notes =
        match crate::release_history::draft(repository.worktree_root(), &reader, release_commit) {
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
