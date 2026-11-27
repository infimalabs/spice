//! Coordinator admission for finalized prepared platform bundles.

use std::path::PathBuf;

use crate::hosted::Request;
use crate::hosted::stage_layout::{RESULT_PATH, RUNNER_PATH, StageLayout};

use super::{
    DeclaredSubjects, HostedRun, PreparedBundleTransport, WindowsHostedError,
    download_attested_artifact, verify_attestation,
};

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
    transport
        .fetch(
            run.repository,
            request.prepared_bundle.tag(),
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
