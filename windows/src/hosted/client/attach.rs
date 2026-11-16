//! Re-entry into one hosted stage's provider run after its client stopped
//! observing it.
//!
//! Nothing local records a dispatched run. Attachment re-derives the archive
//! identities, reads the challenge from the run title, admits the retained
//! immutable archive asset for that challenge, and requires the run to use the
//! exact workflow blob carried by the archive on the configured branch.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::Path;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::source::PreparedSource;
use super::transport::{self, Transport};
use super::{HostedRun, WindowsHostedError, command, successful_text};

/// Which provider run a hosted stage re-enters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attach {
    /// Exactly this run of the stage's workflow.
    Run(u64),
    /// The one unfinished run of the stage's workflow that builds the current
    /// source archive; a bare `--attach` names it.
    Sole,
}

impl Attach {
    /// A positive decimal provider run id.
    #[must_use]
    pub fn run(value: &str) -> Option<Self> {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        value.parse().ok().filter(|id| *id != 0).map(Self::Run)
    }
}

/// One provider run proven to consume the current canonical source archive.
pub(super) struct Attached {
    pub(super) run_id: u64,
    pub(super) challenge: String,
    pub(super) transport: Transport,
}
/// One provider run proven to use the current admitted workflow.
pub(super) struct AttachedRun {
    pub(super) run_id: u64,
    pub(super) challenge: String,
}

/// Admit an attached run whose inputs are provider artifacts rather than a
/// retained source transport.
pub(super) fn attach_run(
    root: &Path,
    repository: &str,
    branch: &str,
    workflow: &str,
    expected_workflow_blob: &str,
    run_title_prefix: &str,
    attach: Attach,
) -> Result<AttachedRun, WindowsHostedError> {
    let candidates = match attach {
        Attach::Run(run_id) => vec![api(
            root,
            format!("repos/{repository}/actions/runs/{run_id}"),
        )?],
        Attach::Sole => {
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
            unretired(runs.workflow_runs, &artifacts.artifacts, &[])
        }
    };
    let mut admitted = Vec::new();
    let mut refused = Vec::new();
    for run in candidates {
        let admission = admit_run(&run, workflow, branch, run_title_prefix).and_then(|challenge| {
            let observed = workflow_blob(root, repository, workflow, &run.head_sha)
                .map_err(|error| error.to_string())?;
            if observed != expected_workflow_blob {
                return Err("its workflow differs from the current source".to_owned());
            }
            Ok(AttachedRun {
                run_id: run.id,
                challenge,
            })
        });
        match admission {
            Ok(candidate) => admitted.push(candidate),
            Err(reason) => refused.push(format!("run {}: {reason}", run.id)),
        }
    }
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
        Attach::Sole if admitted.is_empty() => {
            format!("no unfinished or retained run of {workflow} is adoptable{detail}")
        }
        Attach::Sole => format!(
            "runs {} of {workflow} are adoptable; attach one by its run id",
            admitted
                .iter()
                .map(|candidate| candidate.run_id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }))
}

#[derive(Clone, Deserialize)]
pub(super) struct ProviderRun {
    pub(super) id: u64,
    pub(super) repository: RunRepository,
    pub(super) path: String,
    pub(super) event: String,
    pub(super) display_title: String,
    pub(super) head_branch: Option<String>,
    pub(super) head_sha: String,
    pub(super) status: Option<String>,
}

#[derive(Clone, Deserialize)]
pub(super) struct RunRepository {
    pub(super) full_name: String,
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

/// Admit the provider run `attach` names for `source`.
///
/// # Errors
///
/// Refuses another repository, workflow, branch, event, challenge, workflow
/// blob, or archive transport; returns provider and archive failures.
pub(super) fn attach(
    root: &Path,
    repository: &str,
    branch: &str,
    workflow: &str,
    source: &PreparedSource,
    attach: Attach,
) -> Result<Attached, WindowsHostedError> {
    let candidates = match attach {
        Attach::Run(run_id) => vec![api(
            root,
            format!("repos/{repository}/actions/runs/{run_id}"),
        )?],
        Attach::Sole => {
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
            let releases = transport::releases(root, repository)?;
            unretired(runs.workflow_runs, &artifacts.artifacts, &releases)
        }
    };
    let mut admitted = Vec::new();
    let mut refused = Vec::new();
    for run in candidates {
        let admission = admit_run(&run, workflow, branch, RUN_TITLE_PREFIX).and_then(|challenge| {
            let workflow_blob = workflow_blob(root, repository, workflow, &run.head_sha)
                .map_err(|error| error.to_string())?;
            if workflow_blob != source.workflow_blob {
                return Err("its workflow differs from the current source archive".to_owned());
            }
            let transport = transport::find(root, repository, &challenge, source)
                .map_err(|error| error.to_string())?;
            Ok(Candidate {
                run_id: run.id,
                challenge,
                transport,
            })
        });
        match admission {
            Ok(candidate) => admitted.push(candidate),
            Err(reason) => refused.push(format!("run {}: {reason}", run.id)),
        }
    }
    let selected = select(attach, workflow, admitted, &refused)?;
    Ok(Attached {
        run_id: selected.run_id,
        challenge: selected.challenge,
        transport: selected.transport,
    })
}

/// One admitted run and its retained source transport.
#[derive(Debug, Eq, PartialEq)]
struct Candidate {
    run_id: u64,
    challenge: String,
    transport: Transport,
}

/// Runs that still hold provider state: unfinished, owning an unexpired result
/// artifact, or owning a retained archive transport.
fn unretired(
    runs: Vec<ProviderRun>,
    artifacts: &[ProviderArtifact],
    releases: &[transport::Release],
) -> Vec<ProviderRun> {
    let owners = artifacts
        .iter()
        .filter(|artifact| !artifact.expired)
        .filter_map(|artifact| artifact.workflow_run.as_ref().map(|run| run.id))
        .collect::<BTreeSet<_>>();
    let transports = releases
        .iter()
        .map(|release| release.tag_name.as_str())
        .collect::<BTreeSet<_>>();
    runs.into_iter()
        .filter(|run| {
            run.status.as_deref() != Some("completed")
                || owners.contains(&run.id)
                || challenge_from_title(&run.display_title, RUN_TITLE_PREFIX).is_some_and(
                    |challenge| transports.contains(transport::release_name(challenge).as_str()),
                )
        })
        .collect()
}

/// The challenge `run` was dispatched with when it runs `workflow` from the
/// configured branch.
fn admit_run(
    run: &ProviderRun,
    workflow: &str,
    branch: &str,
    run_title_prefix: &str,
) -> Result<String, String> {
    if run.path != workflow {
        return Err(format!("it runs {}, not {workflow}", run.path));
    }
    if run.event != "workflow_dispatch" {
        return Err(format!("it was started by {}, not a dispatch", run.event));
    }
    if run.head_branch.as_deref() != Some(branch) {
        return Err(format!(
            "its workflow branch is {:?}, not {branch}",
            run.head_branch
        ));
    }
    if !is_object_id(&run.head_sha) {
        return Err("its workflow commit is malformed".to_owned());
    }
    challenge_from_title(&run.display_title, run_title_prefix)
        .map(str::to_owned)
        .ok_or_else(|| "its run title records no challenge".to_owned())
}

pub(super) const RUN_TITLE_PREFIX: &str = "Hosted archive ";
const CHALLENGE_HEX_LENGTH: usize = 64;
fn challenge_from_title<'a>(title: &'a str, prefix: &str) -> Option<&'a str> {
    let challenge = title.strip_prefix(prefix)?;
    (challenge.len() == CHALLENGE_HEX_LENGTH
        && challenge
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(challenge)
}

pub(super) fn workflow_blob(
    root: &Path,
    repository: &str,
    workflow: &str,
    commit: &str,
) -> Result<String, WindowsHostedError> {
    #[derive(Deserialize)]
    struct Content {
        sha: String,
    }
    let content: Content = api(
        root,
        format!("repos/{repository}/contents/{workflow}?ref={commit}"),
    )?;
    Ok(content.sha)
}

/// Require the configured provider branch to carry the workflow blob admitted
/// by the selected source before creating provider state.
pub(super) fn admit_branch_workflow(
    root: &Path,
    repository: &str,
    branch: &str,
    workflow: &str,
    expected_blob: &str,
) -> Result<(), WindowsHostedError> {
    let blob = workflow_blob(root, repository, workflow, branch)?;
    if blob != expected_blob {
        return Err(WindowsHostedError::ProviderResponse(format!(
            "provider branch {branch} carries workflow blob {blob}, not {expected_blob}"
        )));
    }
    Ok(())
}

/// Admit one run's workflow identity before trusting its attestations.
pub(super) fn workflow_commit(
    run: &HostedRun<'_>,
    workflow: &str,
    run_id: u64,
) -> Result<String, WindowsHostedError> {
    let provider: ProviderRun = api(
        run.root,
        format!("repos/{}/actions/runs/{run_id}", run.repository),
    )?;
    let observed = admit_run(&provider, workflow, run.branch, run.run_title_prefix)
        .map_err(WindowsHostedError::ProviderResponse)?;
    if observed != run.challenge {
        return Err(WindowsHostedError::ProviderResponse(format!(
            "provider run {run_id} records challenge {observed}, not {}",
            run.challenge
        )));
    }
    let blob = workflow_blob(run.root, run.repository, workflow, &provider.head_sha)?;
    if blob != run.workflow_blob {
        return Err(WindowsHostedError::ProviderResponse(format!(
            "provider run {run_id} used workflow blob {blob}, not {}",
            run.workflow_blob
        )));
    }
    Ok(provider.head_sha)
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
        Attach::Sole if admitted.is_empty() => {
            format!("no unfinished run of {workflow} builds the current source archive{detail}")
        }
        Attach::Sole => format!(
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

pub(super) fn api<T: DeserializeOwned>(root: &Path, path: String) -> Result<T, WindowsHostedError> {
    let response = successful_text(
        "provider run inspection",
        command(root, "gh", [OsString::from("api"), OsString::from(path)])?,
    )?;
    serde_json::from_str(&response).map_err(WindowsHostedError::Decode)
}

#[cfg(test)]
mod tests {
    use super::{
        ArtifactRun, Attach, Candidate, ProviderArtifact, ProviderRun, RUN_TITLE_PREFIX,
        RunRepository, admit_run, challenge_from_title, select, unretired,
    };
    use crate::hosted::client::WindowsHostedError;
    use crate::hosted::client::transport::Transport;

    const WORKFLOW: &str = ".github/workflows/linux.yml";
    const BRANCH: &str = "main";
    const CHALLENGE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn run(id: u64, status: &str) -> ProviderRun {
        ProviderRun {
            id,
            repository: RunRepository {
                full_name: "infimalabs/spice".to_owned(),
            },
            path: WORKFLOW.to_owned(),
            event: "workflow_dispatch".to_owned(),
            display_title: format!("Hosted archive {CHALLENGE}"),
            head_branch: Some(BRANCH.to_owned()),
            head_sha: "a".repeat(40),
            status: Some(status.to_owned()),
        }
    }

    fn candidate(run_id: u64) -> Candidate {
        Candidate {
            run_id,
            challenge: CHALLENGE.to_owned(),
            transport: Transport {
                release_id: 11,
                asset_id: 12,
            },
        }
    }

    #[test]
    fn attach_names_one_positive_run_id() {
        assert_eq!(Attach::run("19342"), Some(Attach::Run(19342)));
        for refused in ["", "0", "+7", "-7", "7a", "latest", "18446744073709551616"] {
            assert_eq!(Attach::run(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn only_the_policy_workflow_dispatch_is_adoptable() {
        assert_eq!(
            admit_run(&run(1, "in_progress"), WORKFLOW, BRANCH, RUN_TITLE_PREFIX).as_deref(),
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
        let other_branch = ProviderRun {
            head_branch: Some("release".to_owned()),
            ..run(1, "in_progress")
        };
        let other_title = ProviderRun {
            display_title: "Hosted archive not-a-challenge".to_owned(),
            ..run(1, "in_progress")
        };
        for unrelated in [other_workflow, pushed, other_branch, other_title] {
            assert!(admit_run(&unrelated, WORKFLOW, BRANCH, RUN_TITLE_PREFIX).is_err());
        }
        assert_eq!(
            challenge_from_title(&format!("{RUN_TITLE_PREFIX}{CHALLENGE}"), RUN_TITLE_PREFIX),
            Some(CHALLENGE)
        );
        assert_eq!(
            challenge_from_title(&format!("Release inputs {CHALLENGE}"), "Release inputs "),
            Some(CHALLENGE)
        );
    }
    #[test]
    fn workflow_title_prefix_is_admitted_at_the_run_identity_boundary() {
        let release_workflow = crate::hosted::release_inputs::WORKFLOW;
        let release_prefix = crate::hosted::release_inputs::RUN_TITLE_PREFIX;
        let release = ProviderRun {
            path: release_workflow.to_owned(),
            display_title: format!("{release_prefix}{CHALLENGE}"),
            ..run(1, "completed")
        };
        assert_eq!(
            admit_run(&release, release_workflow, BRANCH, release_prefix).as_deref(),
            Ok(CHALLENGE)
        );
        assert!(admit_run(&release, release_workflow, BRANCH, RUN_TITLE_PREFIX).is_err());
        let cross_workflow = ProviderRun {
            path: WORKFLOW.to_owned(),
            ..release
        };
        assert!(admit_run(&cross_workflow, release_workflow, BRANCH, release_prefix).is_err());
    }

    #[test]
    fn sole_considers_unfinished_runs_and_retained_results() {
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
        let ids = unretired(runs, &artifacts, &[])
            .iter()
            .map(|run| run.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, [1, 2, 4]);
    }

    #[test]
    fn attachment_admits_exactly_one_matching_run() {
        assert_eq!(
            select(Attach::Sole, WORKFLOW, vec![candidate(7)], &[]).ok(),
            Some(candidate(7))
        );
        for attach in [Attach::Sole, Attach::Run(7)] {
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
                Attach::Sole,
                WORKFLOW,
                vec![candidate(7), candidate(8)],
                &[]
            ),
            Err(WindowsHostedError::AttachRefused(_))
        ));
    }
}
