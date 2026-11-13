//! The journal-free binary rim for the executable gate leaves.
//!
//! Selection always precedes contract loading. Consequently tracked policy,
//! source bytes, and the root handed to every bounded process all come from one
//! captured Git source rather than from a moving checkout.

use std::path::Path;

use gate::Selector;
use wire::{Outcome, Refusal, Reply, ReplyPosition};

use spice::ambient::Ambient;
use spice::repository::Repository;

use crate::environment_fault;

mod battery;
mod camp;
mod commit_message;
mod latches;
pub(crate) mod output;
mod pre_commit;
mod prove;
mod reference_transaction;
mod selection;
mod verify_claim;

pub(crate) use battery::BatteryRun;
pub(crate) use camp::{CampAnswer, camp};
pub(crate) use latches::{LatchDebtView, LatchesAnswer, catalogue, latches};
pub(crate) use prove::{ProveReport, prove};

/// Whether a gate refusal reports only that no proof covers the staged tree
/// yet. Its detail names each `spice gate prove` command that supplies the
/// proof, so nothing needs repair.
fn proof_absent(reason: &str) -> bool {
    matches!(
        reason,
        "gate::pre-commit-fingerprint-missing"
            | "gate::pre-commit-proof-missing"
            | "gate::prove-fingerprint-missing"
            | "gate::prove-receipt-missing"
    )
}

/// Run the ordinary gate against the captured index or one selected commit.
pub(crate) fn run(
    matches: &clap::ArgMatches,
    live: bool,
    ambient: &Ambient,
) -> Outcome<BatteryRun> {
    let at = matches.get_one::<String>("at");
    let selector = at.map_or_else(Selector::staged, |revision| {
        Selector::revision(revision.clone())
    });
    let invocation = battery::Invocation {
        at: at.map(String::as_str),
        verbose: matches.get_flag("verbose"),
        live,
        ambient,
    };
    let admission = verify_claim::admit_current_worktree(ambient);
    match verify_claim::launch_after_admission(admission, || {
        battery::run(selector, &invocation, &output::current_captures())
    }) {
        Ok(Ok(run)) => direct(run),
        Ok(Err(error)) => battery_failure(error),
        Err(failure) => failure.outcome(),
    }
}

/// Run the release-scoped native staged-change gate without command recursion.
pub(crate) fn pre_commit(
    matches: &clap::ArgMatches,
    configuration: Option<&spice::config::RepositoryCapture>,
    live: bool,
    ambient: &Ambient,
) -> Outcome<pre_commit::Report> {
    pre_commit::run(
        matches
            .get_one::<String>("profile-output")
            .map(std::path::PathBuf::from),
        configuration,
        live,
        ambient,
    )
}

pub(crate) use selection::{CurrentSourceSelectionReport, selection};

pub(crate) use commit_message::Run as CommitMessageRun;

/// Apply the current repository commit-message policy to Git's message file.
pub(crate) fn commit_message(
    matches: &clap::ArgMatches,
    configuration: Option<&spice::config::RepositoryCapture>,
) -> CommitMessageRun {
    commit_message::run(matches, configuration)
}

pub(crate) fn pre_commit_worker_entry(
    arguments: impl Iterator<Item = std::ffi::OsString>,
    ambient: &Ambient,
) -> Option<i32> {
    let arguments = arguments.collect::<Vec<_>>();
    let has_json = arguments
        .iter()
        .skip(1)
        .any(|argument| argument.as_os_str() == std::ffi::OsStr::new("--json"));
    if !has_json
        && let Some(code) = reference_transaction::entry(arguments.iter().cloned(), ambient)
    {
        return Some(code);
    }
    let reference_arguments = parser_admitted_json_reference_arguments(&arguments);
    reference_arguments
        .and_then(|arguments| reference_transaction::entry(arguments.into_iter(), ambient))
        .or_else(|| pre_commit::worker_entry(arguments.into_iter(), ambient))
}

fn parser_admitted_json_reference_arguments(
    arguments: &[std::ffi::OsString],
) -> Option<Vec<std::ffi::OsString>> {
    let (program, operands) = arguments.split_first()?;
    let normalized = std::iter::once(program.clone())
        .chain(
            operands
                .iter()
                .filter(|argument| argument.as_os_str() != std::ffi::OsStr::new("--json"))
                .cloned(),
        )
        .collect::<Vec<_>>();
    if normalized.get(1).map(std::ffi::OsString::as_os_str) != Some(std::ffi::OsStr::new("gate"))
        || normalized.get(2).map(std::ffi::OsString::as_os_str)
            != Some(std::ffi::OsStr::new("reference-transaction"))
    {
        return None;
    }
    crate::cli::command()
        .try_get_matches_from(arguments.iter().cloned())
        .ok()?;
    Some(normalized)
}

fn battery_failure<B>(error: spice::gate::RunError) -> Outcome<B> {
    if error.is_fault() {
        return environment_fault(error.reason(), error.detail());
    }
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            error.reason().to_owned(),
            None,
            Some("one complete canonical Rust battery capture".to_owned()),
            Some(error.detail().to_owned()),
        )
        .with_detail("repair the named capture input; the Rust battery is the sole execution path"),
    ))
}

fn direct<B>(body: B) -> Outcome<B> {
    Outcome::Reply(Reply {
        position: ReplyPosition::Direct {},
        body,
    })
}

/// One immutable selected tree, its current contract, and evidence destination.
struct SelectedContract {
    tree: gate::SelectedTree,
    contract: gate::Contract,
    captures: output::Captures,
}

/// The repository the invoking directory belongs to.
fn current_repository() -> Result<Repository, Failure> {
    let current = std::env::current_dir().map_err(|error| Failure::Fault {
        reason: "gate::working-directory-unavailable",
        detail: error.to_string(),
    })?;
    Repository::discover(&current).map_err(|error| Failure::Fault {
        reason: spice::repository::ResolveError::REASON,
        detail: error.to_string(),
    })
}

impl SelectedContract {
    fn capture(selector: Selector) -> Result<Self, Failure> {
        let repository = current_repository()?;
        let tree = selector
            .materialize(repository.worktree_root())
            .map_err(|error| Failure::Refusal {
                reason: "gate::selection-refused",
                expected: "one stable captured index or one immutable commit".to_owned(),
                observed: error.to_string(),
                detail: "resolve the named Git source before running the gate".to_owned(),
            })?;
        let captures = output::captures(repository.common_dir());
        let contract = gate::Contract::load(tree.root()).map_err(|error| Failure::Refusal {
            reason: "gate::contract-refused",
            expected: "one complete current tracked gate contract".to_owned(),
            observed: error.to_string(),
            detail: format!(
                "repair the selected {} or compiled tool",
                wire::TRACKED_POLICY_FILE
            ),
        })?;
        Ok(Self {
            tree,
            contract,
            captures,
        })
    }

    fn root(&self) -> &Path {
        self.tree.root()
    }

    fn captures(&self) -> &output::Captures {
        &self.captures
    }
}

enum Failure {
    Refusal {
        reason: &'static str,
        expected: String,
        observed: String,
        detail: String,
    },
    Fault {
        reason: &'static str,
        detail: String,
    },
}

impl Failure {
    fn outcome<B>(self) -> Outcome<B> {
        match self {
            Self::Refusal {
                reason,
                expected,
                observed,
                detail,
            } => Outcome::Refusal(Refusal::bare(
                wire::RefusalDiagnostic::new(
                    reason.to_owned(),
                    None,
                    Some(expected),
                    Some(observed),
                )
                .with_detail(detail),
            )),
            Self::Fault { reason, detail } => environment_fault(reason, detail),
        }
    }
}
