use super::*;

use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

fn current_source_rules() -> gate::RulePolicy {
    gate::repository_rule_policy(None).expect("packaged rule policy")
}

#[test]
fn report_rejects_missing_and_source_mismatched_policy_evidence() {
    let source = wire::Digest::compute(wire::Domain::GateTree, b"change gate report");
    let selection = gate::select_current_source_change_gates(
        source.clone(),
        &[],
        &[],
        &[],
        &current_source_rules(),
    )
    .expect("empty selection");
    let tests = gate::TestSelectionReport::new(
        source,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        gate::CoverageCompleteness::Complete,
    )
    .expect("empty test selection");

    let policy = ChangePolicyReport::Focused {
        source: selection.source().clone(),
        results: Vec::new(),
    };
    let error = ChangeGateReport::new(selection, tests, policy)
        .expect_err("selected always-on arms require focused evidence");
    assert_eq!(error.reason(), "gate::change-gate-policy-incomplete");
}

#[test]
fn proof_image_definition_selects_every_current_source_gate() {
    let source = wire::Digest::compute(wire::Domain::GateTree, b"proof image definition");
    let selection = gate::select_current_source_change_gates(
        source,
        &[],
        &["dist/proof-image/Dockerfile".to_owned()],
        &[],
        &current_source_rules(),
    )
    .expect("proof image definition selects");

    assert_eq!(selection.gate_ids(), GateId::BATTERY);
}

#[test]
fn malformed_staged_policy_refuses_before_any_compiled_inventory_is_selected() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "spice-change-policy-malformed-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir(&root).expect("create malformed-policy fixture");
    fs::write(
        root.join(wire::TRACKED_POLICY_FILE),
        "[policy\ninvalid = true\n",
    )
    .expect("write malformed staged policy");
    let paths = vec![wire::TRACKED_POLICY_FILE.to_owned()];
    let environment = crate::ambient::capture()
        .environment()
        .expect("capture test environment");

    let error = packaged_selection_policy(&root, &paths, &environment)
        .expect_err("malformed staged policy cannot use a compiled inventory");

    assert_eq!(error.reason(), "gate::change-selection-policy-refused");
    fs::remove_dir_all(root).expect("remove malformed-policy fixture");
}

#[test]
fn zero_config_selection_keeps_unclassified_sources_local_and_known_sources_specialized() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "spice-change-policy-zero-config-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    fs::create_dir(&root).expect("create zero-config fixture");
    let paths = ["index.qmd", "render.lua", "verify.pl", "README.md"].map(str::to_owned);
    let environment = crate::ambient::capture()
        .environment()
        .expect("capture test environment");
    let policy = packaged_selection_policy(&root, &paths, &environment)
        .expect("compile zero-config packaged policy");

    let generic = gate::select_change_gates(
        wire::Digest::compute(wire::Domain::GateTree, b"unclassified external sources"),
        &paths[..3],
        &policy,
    )
    .expect("unclassified ordinary sources retain generic checks");
    assert_eq!(
        generic.gate_ids(),
        [
            GateId::MergeIntegrity,
            GateId::PlanningPhase,
            GateId::Staging,
            GateId::LocalPaths,
        ]
    );

    let markdown = gate::select_change_gates(
        wire::Digest::compute(wire::Domain::GateTree, b"known markdown source"),
        &["README.md".to_owned()],
        &policy,
    )
    .expect("known Markdown source retains specialized checks");
    assert!(markdown.gate_ids().contains(&GateId::Taste));
    assert!(markdown.gate_ids().contains(&GateId::MarkdownLinks));

    fs::write(
        root.join(wire::TRACKED_POLICY_FILE),
        gate::NARROWEST_FILE_CLASSES,
    )
    .expect("write configured policy");
    let configured_paths = paths
        .iter()
        .cloned()
        .chain([wire::TRACKED_POLICY_FILE.to_owned()])
        .collect::<Vec<_>>();
    let configured = packaged_selection_policy(&root, &configured_paths, &environment)
        .expect("compile configured packaged policy");
    gate::select_change_gates(
        wire::Digest::compute(wire::Domain::GateTree, b"configured unknown source"),
        &["index.qmd".to_owned()],
        &configured,
    )
    .expect_err("configured policy still refuses an undeclared source class");

    fs::remove_dir_all(root).expect("remove zero-config fixture");
}

#[test]
fn pretest_screen_covers_reusable_and_cheap_static_policy() {
    let screened = GateId::BATTERY
        .into_iter()
        .filter(|gate_id| pretest_screen_gate(*gate_id))
        .collect::<Vec<_>>();

    assert_eq!(
        screened,
        vec![
            GateId::RustTypecheck,
            GateId::AppWebTypecheck,
            GateId::Complexity,
        ],
    );
}

#[test]
fn policy_schedule_starts_known_long_arms_before_short_remainder_work() {
    assert!(policy_schedule_rank(GateId::EnvNameLedger) < policy_schedule_rank(GateId::RepoDocs));
    assert!(policy_schedule_rank(GateId::MarkdownLinks) < policy_schedule_rank(GateId::RepoShape));
    assert!(policy_schedule_rank(GateId::Formatters) < policy_schedule_rank(GateId::LocalPaths));
    assert!(policy_schedule_rank(GateId::MagicNumbers) < policy_schedule_rank(GateId::Taste));

    let source = wire::Digest::compute(wire::Domain::GateTree, b"policy schedule order");
    let changed = ["spice/src/change_gate.rs".to_owned()];
    let selection = gate::select_current_source_change_gates(
        source,
        &changed,
        &changed,
        &[],
        &current_source_rules(),
    )
    .expect("select the benchmark policy inventory");
    let mut scheduled = selection
        .reach()
        .iter()
        .filter(|reach| !pretest_screen_gate(reach.gate_id()) && reach.gate_id() != GateId::Staging)
        .enumerate()
        .collect::<Vec<_>>();
    scheduled.sort_by_key(|(index, reach)| (policy_schedule_rank(reach.gate_id()), *index));
    assert_eq!(
        scheduled
            .into_iter()
            .map(|(_, reach)| reach.gate_id())
            .take(3)
            .collect::<Vec<_>>(),
        [
            GateId::Formatters,
            GateId::EnvNameLedger,
            GateId::MarkdownLinks,
        ],
    );
}

#[test]
fn budget_publication_warns_only_above_the_exact_advisory_share() {
    let below = published_budget(CargoBudget::from_allocations(12, 1, 1, 1));
    assert!(below.contains("advisory_share=3"));
    assert!(!below.contains("change-gate-budget-warning"));

    let equal = published_budget(CargoBudget::from_allocations(12, 2, 3, 1));
    assert!(!equal.contains("change-gate-budget-warning"));

    let above = published_budget(CargoBudget::from_allocations(12, 3, 4, 1));
    assert_eq!(above.matches("[change-gate-budget-warning]").count(), 1);
    assert!(above.contains("[change-gate-budget-warning] reason=advisory-lane-share-exceeded"));
    assert!(above.contains("host_capacity=12 ordinary_lanes=4 advisory_share=3"));
    assert!(above.contains("requested_slots=4"));
    assert!(above.contains("allocations=policy_peak:4 test_peak:4x1=4 action=continue-unchanged"));
}

#[test]
fn measured_twelve_cpu_budget_caps_each_ordinary_gate_at_three_direct_workers() {
    let budget = CargoBudget::from_capacity(12, 4);
    let published = published_budget(budget);

    assert_eq!(budget.worker_processes(), 3);
    assert_eq!(budget.policy_jobs(), 1);
    assert_eq!(budget.native_policy_jobs(), 2);
    assert_eq!(budget.policy_slots(), 3);
    assert_eq!(budget.test_concurrency(), 3);
    assert_eq!(budget.shard_jobs(), 1);
    assert!(published.contains(
        "cpus=12 ordinary_lanes=4 advisory_share=3 worker_process_cap=3 policy_jobs=1 native_policy_jobs=2 policy_peak=3 test_concurrency=3 shard_jobs=1"
    ));
    assert!(published.contains("worker_scope=phase-peaks policy_and_tests=nonoverlapping"));
    assert!(!published.contains("change-gate-budget-warning"));
}

fn published_budget(budget: CargoBudget) -> String {
    let mut output = Vec::new();
    publish_budget(&mut output, budget, "policy-then-tests").expect("publish gate budget");
    String::from_utf8(output).expect("budget publication is text")
}
