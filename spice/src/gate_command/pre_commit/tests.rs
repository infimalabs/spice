//! Focused regressions for the bounded pre-commit worker and supervisor.

use super::*;
use std::cell::Cell;
use wire::TaskHandle;

#[test]
fn commit_dispatch_refuses_only_active_planning_claims() {
    let task: TaskHandle = "PHASE-1kRKw9Qjx".parse().expect("task handle");
    for phase in ["plan", "design", "todo", "review"] {
        let claim = ClaimCapture::Active {
            task: task.clone(),
            phase: phase.parse().expect("phase"),
        };
        let result = dispatch::require_non_planning_claim(&claim);
        if matches!(phase, "plan" | "design") {
            let error = result.expect_err("planning claim must refuse commit dispatch");
            assert_eq!(error.reason, "gate::pre-commit-planning-implementation");
            assert!(!error.fault);
        } else {
            assert!(
                result.is_ok(),
                "non-planning phase {phase} must preserve native permission"
            );
        }
    }
    assert!(dispatch::require_non_planning_claim(&ClaimCapture::Idle).is_ok());
}

#[test]
fn fixed_envelope_reserves_cleanup_inside_ten_minutes() {
    let bounds = ProcessBounds::production();
    let envelope = bounds.runtime + bounds.grace + bounds.reap + bounds.output_drain;
    assert_eq!(envelope, Duration::from_secs(599));
    assert_eq!(HARD_DEADLINE - envelope, Duration::from_secs(1));
    assert!(envelope < HARD_DEADLINE);
}

#[test]
fn appliance_generation_fence_is_canonical_bound_or_explicitly_unbound() {
    let uncached = ApplianceGenerationFence::unbound();
    let uncached_document = uncached.encode().expect("encode uncached fence");
    assert_eq!(uncached_document, OsStr::new(r#"{"version":1}"#));
    assert_eq!(
        ApplianceGenerationFence::decode(&uncached_document).expect("decode uncached fence"),
        uncached,
    );

    let appliance = Digest::compute(wire::Domain::InstallPlan, b"appliance");
    let bound = ApplianceGenerationFence::bound(appliance.clone());
    let encoded = bound.encode().expect("encode bound fence");
    assert_eq!(
        ApplianceGenerationFence::decode(&encoded).expect("decode bound fence"),
        bound,
    );
    assert_eq!(bound.expected(), Some(&appliance));

    let invalid = ApplianceGenerationFence {
        version: APPLIANCE_GENERATION_FENCE_VERSION + 1,
        ..bound
    };
    assert!(invalid.encode().is_err());
}

#[test]
fn board_claim_capture_round_trips_through_the_worker_argument() {
    let captures = [
        ClaimCapture::Idle,
        ClaimCapture::Absent {
            reason: "resident has not answered".to_owned(),
        },
        ClaimCapture::Active {
            task: "PHASE-1kRKw9Qjx".parse().expect("fixture task handle"),
            phase: "plan".parse().expect("fixture phase"),
        },
    ];
    for capture in captures {
        let encoded = encode_claim_capture(&capture).expect("encode claim capture");
        assert_eq!(
            decode_claim_capture(&encoded).expect("decode claim capture"),
            capture,
        );
    }
}

#[test]
fn run_enters_supervision_with_only_private_worker_artifacts() {
    let phase = Cell::new(0);
    let answer = run_with(|files| {
        assert_eq!(phase.get(), 0);
        for path in [&files.progress, &files.terminal] {
            assert_eq!(
                sill::authority::owner_mode(path).expect("worker artifact mode"),
                0o600
            );
        }
        assert!(!files.report.exists());
        phase.set(1);
        fault("gate::supervisor-fixture", "bounded supervisor entered")
    });

    assert_eq!(phase.get(), 1);
    let Outcome::Fault(fault) = answer else {
        panic!("fixture supervisor returns its typed marker fault");
    };
    assert_eq!(fault.reason, "gate::supervisor-fixture");
}

#[test]
fn selected_test_refusal_renders_its_stage_and_output_evidence() {
    let evidence = concat!(
        "package \"spice\" ended as exit code=101 at binary \"gate_cli\", ",
        "last test \"change selection fixture\""
    );
    let outcome = worker_failure(WorkerFailure {
        reason: "gate::change-gate-tests-refused".to_owned(),
        detail: evidence.to_owned(),
        fault: false,
    });

    let Outcome::Refusal(refusal) = &outcome else {
        panic!("failed selected test execution must refuse");
    };
    assert_eq!(refusal.reason(), "gate::change-gate-tests-refused");
    let rendering = crate::render::render_commit_gate(&outcome, crate::render::Form::Text)
        .expect("selected-test refusal renders")
        .expect("selected-test refusal is visible");
    let text = std::str::from_utf8(rendering.document()).expect("selected-test refusal is UTF-8");
    assert!(text.contains("package \"spice\""), "{text}");
    assert!(text.contains("exit code=101"), "{text}");
    assert!(text.contains("binary \"gate_cli\""), "{text}");
    assert!(text.contains("last test"), "{text}");
}

#[test]
fn worker_artifact_directory_is_private() {
    let files = WorkerFiles::create().expect("create private worker files");
    assert_eq!(
        sill::authority::owner_mode(&files.root).expect("worker root mode"),
        0o700
    );
    for path in [&files.progress, &files.terminal] {
        assert_eq!(
            sill::authority::owner_mode(path).expect("worker artifact mode"),
            0o600
        );
    }
    assert!(!files.report.exists());
    files.cleanup().expect("remove private worker files");
}

#[test]
fn last_phase_prefers_later_selected_test_progress() {
    let files = WorkerFiles::create().expect("create progress worker files");
    fs::write(
        &files.progress,
        concat!(
            "[change-gate-policy] gate=repo-docs disposition=executed\n",
            "[rust-test] stage=progress shard=2/4 package=\"spice\" last_test=\"fixture\"\n",
        ),
    )
    .expect("write ordered progress evidence");
    assert_eq!(
        last_phase(&files.progress),
        "selected-tests stage=progress shard=2/4 package=\"spice\" last_test=\"fixture\""
    );
    files.cleanup().expect("remove progress worker files");
}

#[test]
fn last_phase_reports_in_process_change_gate_selection() {
    let files = WorkerFiles::create().expect("create progress worker files");
    fs::write(
        &files.progress,
        concat!(
            "{\"phase\":\"test-selection\"}\n",
            "[change-gate-phase] stage=capture\n",
            "[change-gate-phase] stage=selection changes=2\n",
        ),
    )
    .expect("write change-gate phase evidence");
    assert_eq!(
        last_phase(&files.progress),
        "change-gate selection changes=2"
    );
    files.cleanup().expect("remove progress worker files");
}

#[test]
fn last_phase_accepts_policy_disposition_evidence_after_the_gate_token() {
    let files = WorkerFiles::create().expect("create progress worker files");
    fs::write(
        &files.progress,
        "[change-gate-policy] gate=rust-typecheck disposition=executed\n",
    )
    .expect("write policy progress evidence");
    assert_eq!(last_phase(&files.progress), "arm gate=rust-typecheck");
    files.cleanup().expect("remove progress worker files");
}

#[test]
fn configured_only_report_is_validated_and_silent_when_every_step_admits() {
    let source = Digest::compute(wire::Domain::GateTree, b"configured-only source");
    let document = serde_json::json!({
        "version": PRE_COMMIT_REPORT_VERSION,
        "source": source,
        "configured": {
            "source": source,
            "steps": [{
                "label": "configured fixture",
                "argv": ["/bin/true"],
                "selected_paths": ["src/lib.rs"],
                "enabled": true,
                "findings": [],
            }],
        },
    });
    let report = Report::decode(&serde_json::to_vec(&document).expect("encode fixture report"))
        .expect("configured-only report is valid");

    assert!(report.packaged().is_none());
    let configured = report.configured().expect("configured report retained");
    assert_eq!(configured.steps().len(), 1);
    assert_eq!(configured.steps()[0].label(), "configured fixture");
    assert_eq!(configured.steps()[0].selected_paths(), ["src/lib.rs"]);
    assert!(configured.steps()[0].enabled());
    assert!(configured.steps()[0].findings().is_empty());
    assert!(report.admits());
    let outcome = report_outcome(report, Path::new("unused-on-admission"));
    assert!(matches!(outcome, Outcome::Reply(_)));
    assert!(
        crate::render::render_commit_gate(&outcome, crate::render::Form::Text)
            .expect("configured admission renders")
            .is_none(),
        "clean configured-only admission must remain silent",
    );
}

#[test]
fn composite_report_refuses_an_empty_battery_or_mismatched_source() {
    let source = Digest::compute(wire::Domain::GateTree, b"empty battery");
    let empty = serde_json::json!({
        "version": PRE_COMMIT_REPORT_VERSION,
        "source": source,
    });
    assert!(
        Report::decode(&serde_json::to_vec(&empty).expect("encode empty report"))
            .expect_err("empty battery must refuse")
            .contains("empty effective battery")
    );

    let other = Digest::compute(wire::Domain::GateTree, b"other configured source");
    let mismatch = serde_json::json!({
        "version": PRE_COMMIT_REPORT_VERSION,
        "source": source,
        "configured": {
            "source": other,
            "steps": [{
                "label": "configured fixture",
                "argv": ["/bin/true"],
                "selected_paths": [],
                "enabled": true,
                "findings": [],
            }],
        },
    });
    assert!(
        Report::decode(&serde_json::to_vec(&mismatch).expect("encode mismatched report"))
            .expect_err("mismatched configured source must refuse")
            .contains("different staged source")
    );
}
