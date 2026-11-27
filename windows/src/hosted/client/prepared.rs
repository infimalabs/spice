//! Coordinator admission for finalized prepared platform bundles.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::hosted::Request;
use crate::hosted::stage_layout::{RESULT_PATH, RUNNER_PATH, StageLayout};

use super::{
    DeclaredSubjects, HostedRun, PreparedBundleTransport, WindowsHostedError,
    download_attested_artifact, verify_attestation,
};

/// The job that publishes a Windows release request's finalized bundle.
const PUBLISH_JOB: &str = "Publish Windows release bundle";

pub(super) fn admitted_artifact(
    run: &HostedRun<'_>,
    request: &Request,
    run_id: u64,
    transport: Option<&dyn PreparedBundleTransport>,
) -> Result<(StageLayout, DeclaredSubjects, PathBuf, PathBuf), WindowsHostedError> {
    if request.release_inputs.is_none() {
        return download_attested_artifact(run, &request.challenge, run_id);
    }
    let transport = transport.ok_or_else(|| {
        WindowsHostedError::ProviderResponse(
            "hosted Windows staging has no prepared-bundle transport".to_owned(),
        )
    })?;
    let artifact = run.scratch.join("artifact");
    let attempt = published_attempt(run, run_id, PUBLISH_JOB)?;
    transport
        .fetch(
            run.repository,
            &request.prepared_bundle.attempt_tag(attempt),
            &request.prepared_bundle.build_key,
            &artifact,
        )
        .map_err(WindowsHostedError::ProviderResponse)?;
    let layout = StageLayout::new(artifact);
    let declared = DeclaredSubjects::load(run, super::WORKFLOW, run_id, &layout)?;
    declared.require(&[RESULT_PATH, RUNNER_PATH])?;
    let runner = layout.runner();
    let result = layout.result();
    for path in [&runner, &result] {
        verify_attestation(run, super::WORKFLOW, run_id, path)?;
    }
    Ok((layout, declared, runner, result))
}

/// Greatest positive attempt in provider output. Every token must be valid:
/// malformed provider output is an admission refusal, not a partial answer.
fn latest_successful_attempt(attempts: &str) -> Option<u32> {
    let mut latest = None;
    for attempt in attempts.split_whitespace() {
        let attempt = attempt.parse::<u32>().ok().filter(|attempt| *attempt > 0)?;
        latest = Some(latest.map_or(attempt, |current: u32| current.max(attempt)));
    }
    latest
}

/// The greatest attempt of run `run_id` whose `job` succeeded. Each attempt
/// signs or records its own bytes and publishes them beneath its own discovery
/// tag. Failed later attempts do not hide a prior successful publication.
pub(super) fn published_attempt(
    run: &HostedRun<'_>,
    run_id: u64,
    job: &str,
) -> Result<u32, WindowsHostedError> {
    let attempts = super::successful_text(
        "publishing job attempt",
        super::command(
            run.root,
            "gh",
            [
                OsString::from("api"),
                OsString::from("--paginate"),
                OsString::from(format!(
                    "repos/{}/actions/runs/{run_id}/jobs?filter=all&per_page=100",
                    run.repository
                )),
                OsString::from("--jq"),
                OsString::from(format!(
                    ".jobs[] | select(.name == \"{job}\" and .conclusion == \"success\") | .run_attempt"
                )),
            ],
        )?,
    )?;
    latest_successful_attempt(&attempts).ok_or_else(|| {
        WindowsHostedError::ProviderResponse(format!(
            "run {run_id} names no successful {job} attempt: {attempts:?}"
        ))
    })
}

#[cfg(test)]
mod tests {
    const WORKFLOW_TEXT: &str = ::std::include_str!("../../../../.github/workflows/windows.yml");
    const CHALLENGE_OUTPUT: &str = "${{ steps.request.outputs.challenge }}";

    /// One top-level workflow section or job: its key and indented body.
    fn blocks<'a>(text: &'a str, indent: &str) -> Vec<(&'a str, Vec<&'a str>)> {
        let mut blocks: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in text.lines() {
            let key = line
                .strip_prefix(indent)
                .filter(|rest| !rest.starts_with(' ') && !rest.starts_with('#'))
                .and_then(|rest| rest.strip_suffix(':'));
            match key {
                Some(key) => blocks.push((key, Vec::new())),
                None => {
                    if let Some((_, body)) = blocks.last_mut() {
                        body.push(line);
                    }
                }
            }
        }
        blocks
    }

    fn jobs() -> Vec<(&'static str, Vec<&'static str>)> {
        let (_, after) = WORKFLOW_TEXT
            .split_once("\njobs:\n")
            .expect("the workflow declares jobs");
        blocks(after, "  ")
    }

    /// Each `- ` step of one job, as its lines.
    fn steps<'a>(job: &[&'a str]) -> Vec<Vec<&'a str>> {
        let mut steps: Vec<Vec<&str>> = Vec::new();
        for line in job {
            if line.starts_with("      - ") {
                steps.push(vec![line]);
            } else if let Some(step) = steps.last_mut() {
                step.push(line);
            }
        }
        steps
    }

    fn value<'a>(lines: &[&'a str], key: &str) -> Option<&'a str> {
        lines.iter().find_map(|line| {
            line.trim_start()
                .trim_start_matches("- ")
                .strip_prefix(key)
                .map(str::trim)
        })
    }

    #[test]
    fn latest_successful_publication_survives_later_failed_attempts() {
        assert_eq!(super::latest_successful_attempt("1\n3\n2\n"), Some(3));
        assert_eq!(super::latest_successful_attempt("1\n"), Some(1));
        assert_eq!(super::latest_successful_attempt(""), None);
        assert_eq!(super::latest_successful_attempt("1\ninvalid\n2\n"), None);
    }

    #[test]
    fn each_request_kind_reads_the_transport_its_workflow_publishes() {
        let top = blocks(WORKFLOW_TEXT, "");
        let (_, workflow_permissions) = top
            .iter()
            .find(|(key, _)| *key == "permissions")
            .expect("workflow permissions");
        assert!(
            workflow_permissions
                .iter()
                .all(|line| !line.contains("packages:")),
            "ad hoc batches must not inherit package-write authority"
        );

        let jobs = jobs();
        let publishers: Vec<_> = jobs
            .iter()
            .filter(|(_, body)| body.iter().any(|line| line.contains("packages: write")))
            .collect();
        assert_eq!(publishers.len(), 1, "one job holds package-write authority");
        let (_, publisher) = publishers[0];
        assert_eq!(
            value(publisher, "if:"),
            Some("needs.stage.outputs.release_inputs_run != ''"),
            "only release requests reach the package-writing job"
        );
        for (key, body) in &jobs {
            let publishes = body
                .iter()
                .any(|line| line.contains("prepared-artifact publish"));
            assert_eq!(
                publishes,
                std::ptr::eq(body, publisher),
                "job {key} publication must match its package authority"
            );
        }

        let (_, stage) = jobs
            .iter()
            .find(|(key, _)| *key == "stage")
            .expect("the stage job");
        let challenge = "a".repeat(64);
        let returned = steps(stage)
            .into_iter()
            .filter(|step| {
                value(step, "uses:")
                    .is_some_and(|uses| uses.starts_with("actions/upload-artifact@"))
                    && value(step, "if:").is_none()
            })
            .filter_map(|step| value(&step[1..], "name:").map(str::to_owned))
            .map(|name| name.replace(CHALLENGE_OUTPUT, &challenge))
            .collect::<Vec<_>>();
        assert!(
            returned.contains(&super::super::result_artifact(&challenge)),
            "every ad hoc batch uploads the artifact its coordinator downloads: {returned:?}"
        );
    }
}
