//! `--split ARM`: run each target in one previewed slow Cargo test arm once
//! under libtest's machine-readable per-test report and attach every target-
//! qualified test wall time to the child that covers the arm.
//!
//! The arm identity `cargo:test:<package>::<kind>:<name>,...` names the exact
//! targets the gate proved; the same selectors run here through the admitted
//! Cargo home. There is no second runner: a toolchain that refuses the report
//! ends the apply with the harness's exact error and nothing is filed.

use std::ffi::OsString;

use serde::Deserialize;
use spice::gate::sweep::SweepDraft;
use wire::intent::{SweepSplit, SweepTestTime};

use super::*;

struct Target {
    identity: String,
    selector: Vec<OsString>,
}

/// Package and exact Cargo targets encoded in one test arm.
fn targets(arm: &str) -> Result<(String, Vec<Target>), String> {
    let Some((package, targets)) = arm
        .strip_prefix("cargo:test:")
        .and_then(|rest| rest.split_once("::"))
    else {
        return Err(format!("{arm} is not a Cargo test arm"));
    };
    if package.is_empty() || targets.is_empty() {
        return Err(format!("{arm} does not name a package and target"));
    }
    let targets = targets
        .split(',')
        .map(|identity| {
            let (kind, name) = identity
                .split_once(':')
                .filter(|(kind, name)| !kind.is_empty() && !name.is_empty())
                .ok_or_else(|| format!("{arm} names the malformed target {identity}"))?;
            let selector = match kind {
                "lib" | "cdylib+rlib" | "proc-macro" => vec!["--lib".into()],
                "bin" | "test" | "example" | "bench" => {
                    vec![format!("--{kind}").into(), name.into()]
                }
                _ => return Err(format!("{arm} names the unsupported target kind {kind}")),
            };
            Ok(Target {
                identity: identity.to_owned(),
                selector,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((package.to_owned(), targets))
}

/// Arguments after `cargo` that run one target under the harness report.
fn arguments(package: &str, target: &Target) -> Vec<OsString> {
    let mut arguments = ["test", "--locked", "--package", package]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    arguments.extend(target.selector.iter().cloned());
    arguments.extend(
        [
            "--",
            "-Z",
            "unstable-options",
            "--format",
            "json",
            "--report-time",
        ]
        .map(OsString::from),
    );
    arguments
}

/// Run every target in `arm` exactly once and collect target-qualified
/// per-test wall times, slowest first.
pub(super) fn run(
    host: Host<'_>,
    draft: &SweepDraft,
    arm: &str,
) -> Result<SweepSplit, Outcome<CommandAnswer>> {
    let child = draft.arm_child(arm).ok_or_else(|| {
        refusal(
            "sweep::split-unselected",
            Some(arm.to_owned()),
            "name a slow Cargo test arm the preview files a child for",
        )
    })?;
    let (package, targets) = targets(arm).map_err(|detail| {
        refusal(
            "sweep::split-arm-invalid",
            Some(detail),
            "name a Cargo test arm",
        )
    })?;
    let mut tests = Vec::new();
    for target in &targets {
        tests.extend(run_target(host, arm, &package, target)?);
    }
    sort_reports(&mut tests);
    Ok(SweepSplit { child, tests })
}

/// Run one target under the one admitted harness report.
fn run_target(
    host: Host<'_>,
    arm: &str,
    package: &str,
    target: &Target,
) -> Result<Vec<SweepTestTime>, Outcome<CommandAnswer>> {
    let arguments = arguments(package, target);
    let mut admitted = crate::cargo_command::admit(
        host.repository,
        host.ambient,
        host.repository.worktree_root(),
        &arguments,
    )
    .map_err(|(outcome, _)| match outcome {
        Outcome::Fault(fault) => Outcome::Fault(fault),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Reply(_) => environment_fault("sweep::split-unadmitted", "Cargo was not admitted"),
    })?;
    let output = admitted
        .command
        .output()
        .map_err(|error| environment_fault("sweep::split-launch-failed", error.to_string()))?;
    let report = report(&target.identity, &String::from_utf8_lossy(&output.stdout));
    if !output.status.success() || !report.observed {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let observed = [stderr.trim(), stdout.trim()]
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let error = harness_error(&stderr).unwrap_or_else(|| output.status.to_string());
        return Err(refusal(
            "sweep::split-report-refused",
            Some(observed),
            format!(
                "{arm} target {} yielded no per-test report: {error}",
                target.identity
            ),
        ));
    }
    Ok(report.tests)
}

/// One libtest JSON line; everything but a finished test is ignored.
#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    event: &'a str,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    exec_time: Option<f64>,
}

struct HarnessReport {
    observed: bool,
    tests: Vec<SweepTestTime>,
}

/// One target's harness report and every finished test with a wall time.
fn report(target: &str, stdout: &str) -> HarnessReport {
    let mut report = HarnessReport {
        observed: false,
        tests: Vec::new(),
    };
    for line in stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Line<'_>>(line).ok())
    {
        if line.kind == "suite" {
            report.observed = true;
        } else if line.kind == "test"
            && matches!(line.event, "ok" | "failed")
            && let (Some(name), Some(seconds)) = (line.name, line.exec_time)
        {
            report.tests.push(SweepTestTime {
                target: target.to_owned(),
                name,
                millis: (seconds * 1000.0).round() as u64,
            });
        }
    }
    report
}

/// Order one arm's reports slowest first, then by their complete identity.
fn sort_reports(tests: &mut [SweepTestTime]) {
    tests.sort_by(|left, right| {
        right
            .millis
            .cmp(&left.millis)
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| left.name.cmp(&right.name))
    });
}

/// The harness's own first error line, before Cargo's rerun hint.
fn harness_error(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("error:") && !line.starts_with("error: test failed"))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arms_name_supported_targets_before_admission() {
        for arm in [
            "cargo:typecheck:xtask::lib:xtask",
            "cargo:test:xtask::dylib:xtask",
            "cargo:test:xtask::lib",
            "cargo:test:::lib:xtask",
        ] {
            assert!(targets(arm).is_err(), "{arm}");
        }
        let (package, targets) = targets("cargo:test:fixture::lib:fixture,test:integration")
            .expect("a package arm may name multiple targets");
        assert_eq!(package, "fixture");
        assert_eq!(
            targets
                .iter()
                .map(|target| target.identity.as_str())
                .collect::<Vec<_>>(),
            ["lib:fixture", "test:integration"]
        );
    }

    #[test]
    fn reports_from_distinct_targets_keep_equal_test_names_distinct() {
        let lib = concat!(
            "{ \"type\": \"suite\", \"event\": \"started\", \"test_count\": 2 }\n",
            "{ \"type\": \"test\", \"name\": \"works\", \"event\": \"ok\", \"exec_time\": 0.0014 }\n",
            "{ \"type\": \"test\", \"name\": \"skipped\", \"event\": \"ignored\" }\n",
        );
        let integration = concat!(
            "{ \"type\": \"suite\", \"event\": \"started\", \"test_count\": 1 }\n",
            "not json\n",
            "{ \"type\": \"test\", \"name\": \"works\", \"event\": \"failed\", \"exec_time\": 2.5 }\n",
        );
        let mut reports = report("lib:fixture", lib).tests;
        reports.extend(report("test:integration", integration).tests);
        sort_reports(&mut reports);

        assert_eq!(
            reports,
            [
                SweepTestTime {
                    target: "test:integration".to_owned(),
                    name: "works".to_owned(),
                    millis: 2500
                },
                SweepTestTime {
                    target: "lib:fixture".to_owned(),
                    name: "works".to_owned(),
                    millis: 1
                },
            ]
        );
        let empty = report(
            "bin:fixture",
            "{ \"type\": \"suite\", \"event\": \"ok\", \"passed\": 0 }\n",
        );
        assert!(empty.observed);
        assert!(empty.tests.is_empty());
        assert!(
            !report(
                "lib:fixture",
                "error: the option `Z` is only accepted on the nightly compiler\n"
            )
            .observed
        );
    }
}
