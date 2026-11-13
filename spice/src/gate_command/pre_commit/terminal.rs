//! Terminal worker-record admission and refusal projection.

use super::*;

pub(super) fn classify(files: &WorkerFiles, waited: &WaitOutcome) -> Outcome<Report> {
    if !waited.outputs_drained() {
        return worker_lost(
            &files.progress,
            "worker output did not drain inside its reserved deadline".to_owned(),
        );
    }
    match waited.completion() {
        Completion::TimedOut(_) => deadline(&files.progress),
        Completion::Cancelled(_) => worker_lost(
            &files.progress,
            "worker was cancelled outside the commit deadline".to_owned(),
        ),
        Completion::Exited(status) if !status.success() => worker_lost(
            &files.progress,
            format!(
                "worker exited {:?}; stderr={}",
                status.code(),
                captured(waited.stderr()),
            ),
        ),
        Completion::Exited(_) => terminal(&files.terminal, &files.progress, &files.report),
    }
}

fn terminal(path: &Path, progress: &Path, report_artifact: &Path) -> Outcome<Report> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return worker_lost(
                progress,
                format!("terminal record could not be read: {error}"),
            );
        }
    };
    let Some((kind, document)) = bytes.split_first() else {
        return worker_lost(progress, "worker published no terminal record".to_owned());
    };
    match *kind {
        REPORT_RECORD => match Report::decode(document) {
            Ok(report) => report_outcome(report, report_artifact),
            Err(error) => worker_lost(
                progress,
                format!("worker published an invalid pre-commit report: {error}"),
            ),
        },
        FAILURE_RECORD => match serde_json::from_slice::<WorkerFailure>(document) {
            Ok(failure) => worker_failure(failure),
            Err(error) => worker_lost(
                progress,
                format!("worker published an invalid failure record: {error}"),
            ),
        },
        other => worker_lost(
            progress,
            format!("worker published unknown terminal record {other}"),
        ),
    }
}

pub(super) fn report_outcome(report: Report, artifact: &Path) -> Outcome<Report> {
    if report.admits() {
        return Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body: report,
        });
    }
    let evidence = match refusal_evidence(&report, artifact) {
        Ok(evidence) => evidence,
        Err(fault) => return Outcome::Fault(fault),
    };
    Outcome::Refusal(Refusal::evidence(
        "gate::pre-commit-regressed",
        evidence,
        "repair the selected packaged or configured finding, then retry the same plain git commit; file unrelated failures as separate work instead of widening this run",
    ))
}

fn refusal_evidence(report: &Report, artifact: &Path) -> Result<String, Fault> {
    let mut evidence = String::new();
    if let Some(packaged) = report.packaged().filter(|packaged| !packaged.admits()) {
        evidence.push_str(packaged.refusal_fold().as_str());
    }
    if let Some(configured) = report
        .configured()
        .filter(|configured| !configured.admits())
    {
        let findings = configured.refusal_evidence();
        let findings =
            super::super::output::bounded(&super::super::output::current_captures(), &findings);
        evidence.push_str(&findings);
        if !findings.ends_with('\n') {
            evidence.push('\n');
        }
    }
    let bytes = report.encode().map_err(|error| Fault {
        class: FaultClass::Protocol,
        reason: "gate::pre-commit-report-unencodable".to_owned(),
        detail: error.to_string(),
    })?;
    let artifact_text = artifact.to_str().ok_or_else(|| Fault {
        class: FaultClass::Environment,
        reason: "gate::pre-commit-report-artifact-unavailable".to_owned(),
        detail: format!("report artifact path is not UTF-8: {artifact:?}"),
    })?;
    let mut file = sill::authority::create_private_file(artifact)
        .map_err(|error| report_artifact_fault(artifact_text, error))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| report_artifact_fault(artifact_text, error))?;
    let artifact = serde_json::to_string(artifact_text).map_err(|error| Fault {
        class: FaultClass::Protocol,
        reason: "gate::pre-commit-report-artifact-unencodable".to_owned(),
        detail: error.to_string(),
    })?;
    evidence.push_str(&format!("full-report: {artifact}\n"));
    Ok(evidence)
}

fn report_artifact_fault(artifact: &str, error: io::Error) -> Fault {
    Fault {
        class: FaultClass::Environment,
        reason: "gate::pre-commit-report-artifact-unavailable".to_owned(),
        detail: format!("cannot publish canonical report at {artifact}: {error}"),
    }
}
