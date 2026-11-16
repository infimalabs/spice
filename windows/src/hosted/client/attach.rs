//! Re-entry into one hosted stage's provider run after its client stopped
//! observing it.
//!
//! Nothing local records a dispatched run. Attachment re-derives the archive's
//! exact tree and admits a provider run only when its dispatch builds that
//! tree: a `workflow_dispatch` run of the stage's workflow in the configured
//! repository, from a parentless one-use commit carrying the archive's tree,
//! at the tag naming the challenge that commit records. The stage then
//! rebuilds its request from that challenge and admits the signed result as
//! a fresh dispatch would.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::Path;

use serde::Deserialize;
use serde::de::{DeserializeOwned, IgnoredAny};

use super::source::{self, PreparedSource};
use super::{WindowsHostedError, command, successful_text};

/// Which provider run a hosted stage re-enters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attach {
    /// Exactly this run of the stage's workflow.
    Run(u64),
    /// The one unretired run of the stage's workflow that builds the current
    /// source archive.
    Latest,
}

impl Attach {
    /// `latest`, or a positive decimal provider run id.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if value == "latest" {
            return Some(Self::Latest);
        }
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        value.parse().ok().filter(|id| *id != 0).map(Self::Run)
    }
}

/// One provider run proven to build the current archive's one-use source.
pub(super) struct Attached {
    pub(super) run_id: u64,
    pub(super) challenge: String,
    pub(super) source: PreparedSource,
}

#[derive(Deserialize)]
struct ProviderRun {
    id: u64,
    path: String,
    event: String,
    head_branch: Option<String>,
    head_sha: String,
    status: Option<String>,
}

#[derive(Deserialize)]
struct RunPage {
    workflow_runs: Vec<ProviderRun>,
}

#[derive(Deserialize)]
struct ArtifactPage {
    artifacts: Vec<ProviderArtifact>,
}

#[derive(Deserialize)]
struct ProviderArtifact {
    expired: bool,
    workflow_run: Option<ArtifactRun>,
}

#[derive(Deserialize)]
struct ArtifactRun {
    id: u64,
}

#[derive(Deserialize)]
struct ProviderCommit {
    tree: ProviderTree,
    parents: Vec<IgnoredAny>,
    message: String,
}

#[derive(Deserialize)]
struct ProviderTree {
    sha: String,
}

/// Admit the provider run `attach` names for the source archive at `archive`.
///
/// # Errors
///
/// Refuses a run of another repository, workflow, or event, one whose one-use
/// source is not the archive's exact tree, no unretired matching run, or
/// several; returns provider and archive failures.
pub(super) fn attach(
    root: &Path,
    scratch: &Path,
    repository: &str,
    workflow: &str,
    archive: &Path,
    attach: Attach,
) -> Result<Attached, WindowsHostedError> {
    let archive = source::archive_source(root, scratch, archive)?;
    let candidates = match attach {
        Attach::Run(run_id) => vec![api(
            root,
            format!("repos/{repository}/actions/runs/{run_id}"),
        )?],
        Attach::Latest => {
            let file = workflow.rsplit('/').next().unwrap_or(workflow);
            let runs: RunPage = api(
                root,
                format!(
                    "repos/{repository}/actions/workflows/{file}/runs?event=workflow_dispatch&per_page=100"
                ),
            )?;
            let artifacts: ArtifactPage = api(
                root,
                format!("repos/{repository}/actions/artifacts?per_page=100"),
            )?;
            unretired(runs.workflow_runs, &artifacts.artifacts)
        }
    };
    let mut admitted = Vec::new();
    let mut refused = Vec::new();
    for run in candidates {
        let commit = if is_object_id(&run.head_sha) {
            api::<ProviderCommit>(
                root,
                format!("repos/{repository}/git/commits/{}", run.head_sha),
            )
            .map_err(|error| error.to_string())
        } else {
            Err("its source commit is malformed".to_owned())
        };
        match commit.and_then(|commit| admit_run(&run, &commit, workflow, &archive.tree)) {
            Ok(challenge) => admitted.push(Candidate {
                run_id: run.id,
                commit: run.head_sha,
                challenge,
            }),
            Err(reason) => refused.push(format!("run {}: {reason}", run.id)),
        }
    }
    let selected = select(attach, workflow, admitted, &refused)?;
    Ok(Attached {
        run_id: selected.run_id,
        challenge: selected.challenge,
        source: source::attached(root, archive, selected.commit)?,
    })
}

/// One run whose dispatch builds the current archive.
#[derive(Debug, Eq, PartialEq)]
struct Candidate {
    run_id: u64,
    commit: String,
    challenge: String,
}

/// Runs that still hold provider state: unfinished, or owning an unexpired
/// artifact. A completed run without artifacts was already retired.
fn unretired(runs: Vec<ProviderRun>, artifacts: &[ProviderArtifact]) -> Vec<ProviderRun> {
    let owners = artifacts
        .iter()
        .filter(|artifact| !artifact.expired)
        .filter_map(|artifact| artifact.workflow_run.as_ref().map(|run| run.id))
        .collect::<BTreeSet<_>>();
    runs.into_iter()
        .filter(|run| run.status.as_deref() != Some("completed") || owners.contains(&run.id))
        .collect()
}

/// The challenge `run` was dispatched with, when its dispatch builds `tree`
/// through `workflow`; otherwise why it is unrelated.
fn admit_run(
    run: &ProviderRun,
    commit: &ProviderCommit,
    workflow: &str,
    tree: &str,
) -> Result<String, String> {
    if run.path != workflow {
        return Err(format!("it runs {}, not {workflow}", run.path));
    }
    if run.event != "workflow_dispatch" {
        return Err(format!("it was started by {}, not a dispatch", run.event));
    }
    if !commit.parents.is_empty() || commit.tree.sha != tree {
        return Err("its source is not the current source archive's one-use commit".to_owned());
    }
    let challenge = source::archive_challenge(&commit.message)
        .ok_or_else(|| "its source commit records no hosted challenge".to_owned())?;
    let source_ref = format!("hosted-{}", &challenge[..24]);
    if run.head_branch.as_deref() != Some(source_ref.as_str()) {
        return Err(format!(
            "its source ref is not {source_ref}, the tag its commit's challenge names"
        ));
    }
    Ok(challenge.to_owned())
}

fn select(
    attach: Attach,
    workflow: &str,
    mut admitted: Vec<Candidate>,
    refused: &[String],
) -> Result<Candidate, WindowsHostedError> {
    if admitted.len() == 1 {
        return Ok(admitted.remove(0));
    }
    let detail = if refused.is_empty() {
        String::new()
    } else {
        format!(": {}", refused.join("; "))
    };
    Err(WindowsHostedError::AttachRefused(match attach {
        Attach::Run(run_id) => format!("provider run {run_id} is not adoptable{detail}"),
        Attach::Latest if admitted.is_empty() => {
            format!("no unretired run of {workflow} builds the current source archive{detail}")
        }
        Attach::Latest => format!(
            "runs {} of {workflow} all build the current source archive; attach one by its run id",
            admitted
                .iter()
                .map(|candidate| candidate.run_id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }))
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn api<T: DeserializeOwned>(root: &Path, path: String) -> Result<T, WindowsHostedError> {
    let response = successful_text(
        "provider run inspection",
        command(root, "gh", [OsString::from("api"), OsString::from(path)])?,
    )?;
    serde_json::from_str(&response).map_err(WindowsHostedError::Decode)
}

#[cfg(test)]
mod tests {
    use super::{
        ArtifactRun, Attach, Candidate, ProviderArtifact, ProviderCommit, ProviderRun,
        ProviderTree, admit_run, select, unretired,
    };
    use crate::hosted::client::WindowsHostedError;

    const WORKFLOW: &str = ".github/workflows/linux.yml";
    const TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    const CHALLENGE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn run(id: u64, status: &str) -> ProviderRun {
        ProviderRun {
            id,
            path: WORKFLOW.to_owned(),
            event: "workflow_dispatch".to_owned(),
            head_branch: Some(format!("hosted-{}", &CHALLENGE[..24])),
            head_sha: "a".repeat(40),
            status: Some(status.to_owned()),
        }
    }

    fn commit() -> ProviderCommit {
        ProviderCommit {
            tree: ProviderTree {
                sha: TREE.to_owned(),
            },
            parents: Vec::new(),
            message: format!("spice hosted source archive {CHALLENGE}\n"),
        }
    }

    fn candidate(run_id: u64) -> Candidate {
        Candidate {
            run_id,
            commit: "a".repeat(40),
            challenge: CHALLENGE.to_owned(),
        }
    }

    #[test]
    fn attach_names_latest_or_one_positive_run_id() {
        assert_eq!(Attach::parse("latest"), Some(Attach::Latest));
        assert_eq!(Attach::parse("19342"), Some(Attach::Run(19342)));
        for refused in ["", "0", "+7", "-7", "7a", "Latest", "18446744073709551616"] {
            assert_eq!(Attach::parse(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn only_the_archive_one_use_dispatch_is_adoptable() {
        assert_eq!(
            admit_run(&run(1, "in_progress"), &commit(), WORKFLOW, TREE).as_deref(),
            Ok(CHALLENGE)
        );

        let other_workflow = ProviderRun {
            path: ".github/workflows/windows.yml".to_owned(),
            ..run(1, "in_progress")
        };
        let pushed = ProviderRun {
            event: "push".to_owned(),
            ..run(1, "in_progress")
        };
        let other_tag = ProviderRun {
            head_branch: Some("hosted-ffffffffffffffffffffffff".to_owned()),
            ..run(1, "in_progress")
        };
        for unrelated in [other_workflow, pushed, other_tag] {
            assert!(admit_run(&unrelated, &commit(), WORKFLOW, TREE).is_err());
        }

        let other_tree = ProviderCommit {
            tree: ProviderTree {
                sha: "b".repeat(40),
            },
            ..commit()
        };
        let parented = ProviderCommit {
            parents: vec![serde::de::IgnoredAny],
            ..commit()
        };
        let snapshot = ProviderCommit {
            message: format!("spice hosted source {CHALLENGE}\n"),
            ..commit()
        };
        for unrelated in [other_tree, parented, snapshot] {
            assert!(admit_run(&run(1, "in_progress"), &unrelated, WORKFLOW, TREE).is_err());
        }
    }

    #[test]
    fn latest_considers_only_runs_still_holding_provider_state() {
        let artifacts = [
            ProviderArtifact {
                expired: false,
                workflow_run: Some(ArtifactRun { id: 2 }),
            },
            ProviderArtifact {
                expired: true,
                workflow_run: Some(ArtifactRun { id: 3 }),
            },
        ];
        let runs = vec![
            run(1, "in_progress"),
            run(2, "completed"),
            run(3, "completed"),
            run(4, "queued"),
            run(5, "completed"),
        ];
        let ids = unretired(runs, &artifacts)
            .iter()
            .map(|run| run.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, [1, 2, 4]);
    }

    #[test]
    fn attachment_admits_exactly_one_matching_run() {
        assert_eq!(
            select(Attach::Latest, WORKFLOW, vec![candidate(7)], &[]).ok(),
            Some(candidate(7))
        );
        for attach in [Attach::Latest, Attach::Run(7)] {
            assert!(matches!(
                select(
                    attach,
                    WORKFLOW,
                    Vec::new(),
                    &["run 7: unrelated".to_owned()]
                ),
                Err(WindowsHostedError::AttachRefused(_))
            ));
        }
        assert!(matches!(
            select(
                Attach::Latest,
                WORKFLOW,
                vec![candidate(7), candidate(8)],
                &[]
            ),
            Err(WindowsHostedError::AttachRefused(_))
        ));
    }
}
