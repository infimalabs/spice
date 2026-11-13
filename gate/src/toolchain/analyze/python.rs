//! Bounded Pyright analysis over one dependency-expanded Python component.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::measure::spawn;
use serde::Deserialize;
use sill::{Argv, Completion};

use super::{Analysis, Captured, complete_output, extension, selected_changes, tool_environment};
use crate::measure::Bounds;
use crate::toolchain::{ArmFinding, ArmOutcome, ArmTiming, FindingDetail, ToolchainDeclaration};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PyrightOutput {
    general_diagnostics: Vec<PyrightDiagnostic>,
    summary: PyrightSummary,
}

#[derive(Deserialize)]
struct PyrightSummary {
    #[serde(rename = "errorCount")]
    error_count: usize,
}

#[derive(Deserialize)]
struct PyrightDiagnostic {
    file: String,
    severity: String,
    message: String,
    range: PyrightRange,
    rule: Option<String>,
}

#[derive(Deserialize)]
struct PyrightRange {
    start: PyrightPosition,
}

#[derive(Deserialize)]
struct PyrightPosition {
    line: u32,
    character: u32,
}

pub(in crate::toolchain) fn python_typecheck(
    declaration: ToolchainDeclaration,
    root: &Path,
    reached_paths: &[String],
    bounds: &Bounds,
    programs: &BTreeMap<&str, &str>,
) -> Analysis {
    let paths = selected_changes(root, reached_paths, |path| {
        extension(path).is_some_and(|extension| matches!(extension.as_str(), "py" | "pyi"))
    });
    if paths.is_empty() {
        return Analysis::not_applicable(declaration);
    }
    let Some(pyright) = programs.get("pyright") else {
        return Analysis::terminal(
            ArmOutcome::ParseFailure,
            "python-typecheck lost its resolved pyright program".to_owned(),
        );
    };
    let environment = match tool_environment("python-typecheck", &[*pyright]) {
        Ok(environment) => environment,
        Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
    };
    let mut arguments = vec!["--level", "error", "--outputjson"];
    arguments.extend(paths.iter().map(String::as_str));
    let argv = match Argv::new(pyright, arguments.iter().map(OsStr::new)) {
        Ok(argv) => argv,
        Err(error) => {
            return Analysis::terminal(
                ArmOutcome::ParseFailure,
                format!("python-typecheck argument vector was refused: {error}"),
            );
        }
    };
    let mut spec = bounds.spec(argv, root);
    spec.environment = environment;
    let running = match spawn(spec) {
        Ok(running) => running,
        Err(error) => {
            return Analysis::terminal(
                ArmOutcome::ParseFailure,
                format!("python-typecheck did not start: {error}"),
            );
        }
    };
    let started = Instant::now();
    let outcome = match running.wait() {
        Ok(outcome) => outcome,
        Err(error) => {
            return Analysis::terminal(
                ArmOutcome::ParseFailure,
                format!("python-typecheck could not be waited on: {error}"),
            )
            .timed(timing(started.elapsed(), bounds));
        }
    };
    let elapsed = started.elapsed();
    let status = match outcome.completion() {
        Completion::Exited(status) => status,
        Completion::TimedOut(_) => {
            return Analysis::terminal(
                ArmOutcome::Timeout,
                "python-typecheck outlived the bounds it was given".to_owned(),
            )
            .timed(timing(elapsed, bounds));
        }
        Completion::Cancelled(_) => {
            return Analysis::terminal(
                ArmOutcome::Cancelled,
                "python-typecheck was cancelled before it reported".to_owned(),
            )
            .timed(timing(elapsed, bounds));
        }
    };
    let printed = match complete_output("python-typecheck", &Captured::of(&outcome)) {
        Ok(printed) => printed,
        Err(reason) => {
            return Analysis::terminal(ArmOutcome::ParseFailure, reason)
                .timed(timing(elapsed, bounds));
        }
    };
    let findings = match pyright_findings(root, &paths, &printed) {
        Ok(findings) => findings,
        Err(reason) => {
            return Analysis::terminal(ArmOutcome::ParseFailure, reason)
                .timed(timing(elapsed, bounds));
        }
    };
    if !findings.is_empty() {
        return Analysis::scanned(declaration.id, paths.len(), findings)
            .timed(timing(elapsed, bounds));
    }
    if status.success() {
        return Analysis::terminal(
            ArmOutcome::Passed,
            format!(
                "{} completed its declared analysis across {} selected paths",
                declaration.id.as_str(),
                paths.len()
            ),
        )
        .timed(timing(elapsed, bounds));
    }
    let Some(code) = status.code() else {
        return Analysis::terminal(
            ArmOutcome::ParseFailure,
            "python-typecheck was signalled without a typed diagnostic".to_owned(),
        )
        .timed(timing(elapsed, bounds));
    };
    Analysis::refused(
        format!("python-typecheck refused with {code} and emitted no typed diagnostic"),
        code,
    )
    .timed(timing(elapsed, bounds))
}

fn pyright_findings(
    root: &Path,
    reached_paths: &[String],
    printed: &str,
) -> Result<Vec<ArmFinding>, String> {
    let output: PyrightOutput = serde_json::from_str(printed)
        .map_err(|error| format!("python-typecheck emitted malformed JSON: {error}"))?;
    let reached = reached_paths
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let error_diagnostics = output
        .general_diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == "error")
        .count();
    if error_diagnostics != output.summary.error_count {
        return Err(format!(
            "python-typecheck summary declared {} errors but published {error_diagnostics}",
            output.summary.error_count
        ));
    }
    let mut findings = BTreeSet::new();
    for diagnostic in output
        .general_diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.severity == "error")
    {
        let reported = Path::new(&diagnostic.file);
        let relative = if reported.is_absolute() {
            reported.strip_prefix(root).map_err(|_| {
                format!(
                    "python-typecheck diagnostic escaped the selected tree: {}",
                    diagnostic.file
                )
            })?
        } else {
            reported
        };
        let path = relative.to_string_lossy().replace('\\', "/");
        if !reached.contains(path.as_str()) {
            return Err(format!(
                "python-typecheck diagnostic {path:?} escaped the reached component"
            ));
        }
        let line = diagnostic
            .range
            .start
            .line
            .checked_add(1)
            .ok_or_else(|| format!("python-typecheck line overflowed in {path}"))?;
        let column = diagnostic
            .range
            .start
            .character
            .checked_add(1)
            .ok_or_else(|| format!("python-typecheck column overflowed in {path}"))?;
        let code = diagnostic
            .rule
            .filter(|rule| !rule.is_empty())
            .unwrap_or_else(|| "pyright".to_owned());
        if diagnostic.message.is_empty() {
            return Err(format!(
                "python-typecheck emitted an empty diagnostic in {path}"
            ));
        }
        findings.insert(ArmFinding {
            path: Some(path),
            line: Some(line),
            column: Some(column),
            detail: FindingDetail::PythonType {
                code,
                message: diagnostic.message,
            },
        });
    }
    Ok(findings.into_iter().collect())
}

fn timing(elapsed: Duration, bounds: &Bounds) -> ArmTiming {
    let deadlines = bounds.deadlines();
    let observation = deadlines
        .runtime()
        .saturating_add(deadlines.output_drain())
        .saturating_add(deadlines.reap())
        .saturating_add(bounds.grace());
    ArmTiming {
        elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        observation_bound_ms: u64::try_from(observation.as_millis()).unwrap_or(u64::MAX),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::toolchain::exclusive_scratch_root;

    use super::*;

    #[test]
    fn pyright_json_normalizes_only_reached_error_diagnostics() {
        let root = exclusive_scratch_root("gate-pyright-json");
        fs::create_dir_all(root.join("src")).expect("create Pyright fixture");
        let file = root.join("src/subject.py");
        fs::write(&file, "value: str = 1\n").expect("write Pyright fixture source");
        let output = serde_json::json!({
            "generalDiagnostics": [{
                "file": file,
                "severity": "error",
                "message": "Type Literal[1] is not assignable to str",
                "range": {"start": {"line": 0, "character": 7}},
                "rule": "reportAssignmentType"
            }],
            "summary": {"errorCount": 1}
        });

        let findings = pyright_findings(&root, &["src/subject.py".to_owned()], &output.to_string())
            .expect("Pyright diagnostics normalize");

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].path.as_deref(), Some("src/subject.py"));
        assert_eq!(findings[0].line, Some(1));
        assert_eq!(findings[0].column, Some(8));
        assert!(matches!(
            &findings[0].detail,
            FindingDetail::PythonType { code, message }
                if code == "reportAssignmentType" && message.contains("not assignable")
        ));
        fs::remove_dir_all(root).expect("remove Pyright fixture");
    }

    #[test]
    fn pyright_json_refuses_a_diagnostic_outside_reached_component() {
        let root = exclusive_scratch_root("gate-pyright-escape");
        fs::create_dir_all(root.join("other")).expect("create Pyright escape fixture");
        let file = root.join("other/peer.py");
        let output = serde_json::json!({
            "generalDiagnostics": [{
                "file": file,
                "severity": "error",
                "message": "peer diagnostic",
                "range": {"start": {"line": 0, "character": 0}},
                "rule": "reportGeneralTypeIssues"
            }],
            "summary": {"errorCount": 1}
        });

        pyright_findings(&root, &["src/subject.py".to_owned()], &output.to_string())
            .expect_err("foreign diagnostic must refuse");
        fs::remove_dir_all(root).expect("remove Pyright escape fixture");
    }
}
