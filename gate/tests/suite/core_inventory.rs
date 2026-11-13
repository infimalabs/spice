use gate::{
    ActorCapture, ClaimCapture, ConfigInventoryEvidence, ConfigKeyEvidence, CoreArmReport,
    CoreEvidence, CoreFinding, CoreObservation, CoreSource, CoreStanding, GateId, JudgedLatches,
    Selector, acquire_core,
};

use super::support::{
    Tree, acquired, acquired_arm, captured_baselines, contract_repository, git_commit,
    git_repository, measured, policy_document, rim, selected_contract,
};

#[test]
fn configuration_key_admission_names_the_unknown_active_layer_key() {
    let repository = git_repository("core-acquire-config-key-unknown");
    repository.write(
        "spice.toml",
        &policy_document(concat!(
            "[judge]\n",
            "enabled = true\n",
            "strms = ['misspelled']\n",
        )),
    );
    repository.write("src/app.rs", "pub fn app() {}\n");
    git_commit(&repository, "unknown active configuration key");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let configuration = acquired_arm(&report, GateId::ConfigKeyValidity);
    let expected = ConfigKeyEvidence::new(vec![
        CoreFinding::new(
            "unknown-key",
            None::<String>,
            concat!(
                "tracked layer key judge.strms is absent from the packaged ",
                "configuration registry",
            ),
        )
        .expect("unknown-key evidence"),
    ]);
    assert_eq!(
        configuration.observation(),
        &measured(CoreEvidence::ConfigKeyValidity(expected))
    );
    assert_eq!(configuration.standing(), CoreStanding::Failed);

    repository.remove();
}

#[test]
fn configuration_key_admission_names_a_known_key_forbidden_in_the_active_layer() {
    let repository = git_repository("core-acquire-config-key-layer");
    repository.write(
        "spice.toml",
        &policy_document("[proof]\nrunner = '/usr/bin/runner'\n"),
    );
    repository.write("src/app.rs", "pub fn app() {}\n");
    git_commit(&repository, "forbidden active configuration layer");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let configuration = acquired_arm(&report, GateId::ConfigKeyValidity);
    let expected = ConfigKeyEvidence::new(vec![
        CoreFinding::new(
            "forbidden-layer",
            None::<String>,
            "tracked layer key proof.runner is packaged for the worktree layer, not tracked",
        )
        .expect("layer-policy evidence"),
    ]);
    assert_eq!(
        configuration.observation(),
        &measured(CoreEvidence::ConfigKeyValidity(expected))
    );
    assert_eq!(configuration.standing(), CoreStanding::Failed);

    repository.remove();
}

/// Only the `[policy]` subtables the registry declares are admitted.
///
/// Limits, flex, magic, Markdown depth, and anonymous rule rows are packaged
/// rather than tracked, so the registry refuses each by name; admitting one
/// would tell a repository it may write a key the gate never reads.
///
/// The file classes are the exception, and the document below carries both
/// kinds at once because that is the only way to see the registry tell them
/// apart. No repository loads without them, so reporting them absent from the
/// registry would refuse every document that can be gated at all.
#[test]
fn configuration_key_admission_separates_required_file_classes_from_packaged_subtables() {
    let repository = git_repository("core-acquire-config-key-packaged");
    repository.write(
        "spice.toml",
        &policy_document(concat!(
            "[policy.limits]\n",
            "file_loc = 1234\n",
            "[policy.flex]\n",
            "ratio = 2.25\n",
            "[policy.magic]\n",
            "examine_threshold = 33\n",
            "[policy.markdown_depth]\n",
            "base_chars = 100\n",
            "max_bounded_chars = 300\n",
            "[policy.markdown_depth_budget]\n",
            "extensions = ['.rst']\n",
            "[[policy.rules]]\n",
            "routine_length = { max = 55 }\n",
        )),
    );
    repository.write("src/app.rs", "pub fn app() {}\n");
    git_commit(&repository, "every packaged policy subtable");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let configuration = acquired_arm(&report, GateId::ConfigKeyValidity);
    let expected = ConfigKeyEvidence::new(
        [
            "policy.flex",
            "policy.limits",
            "policy.magic",
            "policy.markdown_depth",
            "policy.markdown_depth_budget",
            "policy.rules",
        ]
        .into_iter()
        .map(|key| {
            CoreFinding::new(
                "unknown-key",
                None::<String>,
                format!(
                    "tracked layer key {key} is absent from the packaged configuration registry"
                ),
            )
            .expect("packaged subtable evidence")
        })
        .collect(),
    );
    assert_eq!(
        configuration.observation(),
        &measured(CoreEvidence::ConfigKeyValidity(expected))
    );
    assert_eq!(configuration.standing(), CoreStanding::Failed);

    repository.remove();
}

#[test]
fn false_disable_inventory_fails_when_a_declared_path_has_no_consumer() {
    let repository = false_disable_repository("core-false-disable-missing", false);
    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let arm = acquired_arm(&report, GateId::ConfigFalseDisable);
    let evidence = false_disable_evidence(arm);

    assert_eq!(
        evidence.declared(),
        &[config_path(&["vendor", "omp", "fast_mode"])]
    );
    assert!(evidence.observed().is_empty());
    assert!(evidence.violations().is_empty());
    assert_eq!(arm.standing(), CoreStanding::Failed);
    assert!(arm.diagnostic().contains("missing=vendor.omp.fast_mode"));

    repository.remove();
}

#[test]
fn false_disable_inventory_passes_with_canonical_honored_paths() {
    let repository = false_disable_repository("core-false-disable-honored", true);
    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let arm = acquired_arm(&report, GateId::ConfigFalseDisable);
    let evidence = false_disable_evidence(arm);
    let expected = [
        config_path(&["alpha", "feature", "enabled"]),
        config_path(&["vendor", "omp", "fast_mode"]),
    ];

    assert_eq!(evidence.declared(), expected);
    assert_eq!(evidence.observed(), expected);
    assert!(evidence.violations().is_empty());
    assert_eq!(arm.standing(), CoreStanding::Passed);

    repository.remove();
}

#[test]
fn false_disable_inventory_authenticates_the_configuration_root_type() {
    let repository = contract_repository("core-false-disable-root-authority");
    repository.write(
        "spice/src/config.rs",
        &false_disable_registry(&[
            "direct.enabled",
            "renamed.enabled",
            "qualified.enabled",
            "vendor.omp.fast_mode",
        ]),
    );
    repository.write(
        "spice/src/direct.rs",
        concat!(
            "use crate::config::Root;\n",
            "fn consume(root: &Root) -> bool {\n",
            "    let direct = root.direct();\n",
            "    direct.enabled()\n",
            "}\n",
        ),
    );
    repository.write(
        "spice/src/renamed.rs",
        concat!(
            "use crate::config::Root as ConfigRoot;\n",
            "fn consume(root: &ConfigRoot) -> bool {\n",
            "    root.renamed().enabled()\n",
            "}\n",
        ),
    );
    repository.write(
        "spice/src/qualified.rs",
        concat!(
            "fn consume(root: &crate::config::Root) -> bool {\n",
            "    root.qualified().enabled()\n",
            "}\n",
        ),
    );
    repository.write(
        "spice/src/unrelated.rs",
        concat!(
            "use crate::config::Root;\n",
            "mod shadowed {\n",
            "    struct Root;\n",
            "    fn consume(root: &Root) -> bool {\n",
            "        root.vendor().omp().fast_mode()\n",
            "    }\n",
            "}\n",
            "fn generic<Root>(root: &Root) -> bool {\n",
            "    root.vendor().omp().fast_mode()\n",
            "}\n",
            "struct Adapter<Root>(Root);\n",
            "impl<Root> Adapter<Root> {\n",
            "    fn consume(&self, root: &Root) -> bool {\n",
            "        root.vendor().omp().fast_mode()\n",
            "    }\n",
            "}\n",
        ),
    );
    git_commit(&repository, "configuration Root authority fixture");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let arm = acquired_arm(&report, GateId::ConfigFalseDisable);
    let evidence = false_disable_evidence(arm);
    let declared = [
        config_path(&["direct", "enabled"]),
        config_path(&["qualified", "enabled"]),
        config_path(&["renamed", "enabled"]),
        config_path(&["vendor", "omp", "fast_mode"]),
    ];
    let observed = [
        config_path(&["direct", "enabled"]),
        config_path(&["qualified", "enabled"]),
        config_path(&["renamed", "enabled"]),
    ];

    assert_eq!(evidence.declared(), declared);
    assert_eq!(evidence.observed(), observed);
    assert_eq!(arm.standing(), CoreStanding::Failed);
    assert!(arm.diagnostic().contains("missing=vendor.omp.fast_mode"));

    repository.remove();
}

#[test]
fn false_disable_inventory_drops_a_receiver_rebound_by_any_pattern_form() {
    let repository = contract_repository("core-false-disable-shadowed");
    let paths = [
        "annotated.enabled",
        "arm.enabled",
        "closure.enabled",
        "iterated.enabled",
        "optional.enabled",
        "tuple.enabled",
    ];
    repository.write("spice/src/config.rs", &false_disable_registry(&paths));
    repository.write(
        "spice/src/shadowed.rs",
        concat!(
            "use crate::config::Root;\n",
            "fn closure_parameter(root: &Root, flag: &Flag) -> bool {\n",
            "    let read = |root: &Flag| root.closure().enabled();\n",
            "    read(flag)\n",
            "}\n",
            "fn match_arm(root: &Root, flag: Option<Flag>) -> bool {\n",
            "    match flag {\n",
            "        Some(root) => root.arm().enabled(),\n",
            "        None => false,\n",
            "    }\n",
            "}\n",
            "fn for_loop(root: &Root, flags: &[Flag]) -> bool {\n",
            "    for root in flags {\n",
            "        if root.iterated().enabled() {\n",
            "            return true;\n",
            "        }\n",
            "    }\n",
            "    false\n",
            "}\n",
            "fn annotated_let(root: &Root, flag: Flag) -> bool {\n",
            "    let root: Flag = flag;\n",
            "    root.annotated().enabled()\n",
            "}\n",
            "fn tuple_let(root: &Root, pair: (Flag, Flag)) -> bool {\n",
            "    let (root, _spare) = pair;\n",
            "    root.tuple().enabled()\n",
            "}\n",
            "fn optional_let(root: &Root, flag: Option<Flag>) -> bool {\n",
            "    if let Some(root) = flag {\n",
            "        return root.optional().enabled();\n",
            "    }\n",
            "    false\n",
            "}\n",
        ),
    );
    git_commit(&repository, "value-shadowed receiver fixture");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let arm = acquired_arm(&report, GateId::ConfigFalseDisable);
    let evidence = false_disable_evidence(arm);
    let declared: Vec<_> = paths
        .iter()
        .map(|path| config_path(&path.split('.').collect::<Vec<_>>()))
        .collect();

    assert_eq!(evidence.declared(), declared);
    assert_eq!(evidence.observed(), Vec::<Vec<String>>::new());
    assert_eq!(arm.standing(), CoreStanding::Failed);
    assert!(
        arm.diagnostic()
            .contains(&format!("missing={}", paths.join(", ")))
    );

    repository.remove();
}

#[test]
fn false_disable_inventory_seeds_an_inherent_method_parameter() {
    let repository = contract_repository("core-false-disable-inherent");
    repository.write(
        "spice/src/config.rs",
        &false_disable_registry(&["vendor.omp.fast_mode"]),
    );
    repository.write(
        "spice/src/launcher.rs",
        concat!(
            "use crate::config::Root;\n",
            "struct Launcher;\n",
            "impl Launcher {\n",
            "    fn compose(&self, root: &Root) -> bool {\n",
            "        let configured = root.vendor().omp();\n",
            "        configured.fast_mode()\n",
            "    }\n",
            "}\n",
        ),
    );
    git_commit(&repository, "inherent method consumer fixture");

    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let arm = acquired_arm(&report, GateId::ConfigFalseDisable);
    let evidence = false_disable_evidence(arm);
    let expected = [config_path(&["vendor", "omp", "fast_mode"])];

    assert_eq!(evidence.declared(), expected);
    assert_eq!(evidence.observed(), expected);
    assert!(evidence.violations().is_empty());
    assert_eq!(arm.standing(), CoreStanding::Passed);

    repository.remove();
}

fn false_disable_repository(label: &str, honored: bool) -> Tree {
    let repository = contract_repository(label);
    let mut paths = vec!["vendor.omp.fast_mode"];
    if honored {
        paths.push("alpha.feature.enabled");
    }
    repository.write("spice/src/config.rs", &false_disable_registry(&paths));
    if honored {
        repository.write(
            "spice/src/consumer.rs",
            concat!(
                "use crate::config::Root;\n",
                "fn consume(root: &Root) -> bool {\n",
                "    let configured = root.vendor().omp();\n",
                "    let fast = configured.fast_mode();\n",
                "    let feature = root.alpha().feature();\n",
                "    fast || feature.enabled()\n",
                "}\n",
            ),
        );
    }
    git_commit(&repository, "false-disable inventory fixture");
    repository
}

/// One registry declaring every named path with a tracked false builtin.
fn false_disable_registry(paths: &[&str]) -> String {
    let rows: String = paths
        .iter()
        .map(|path| {
            format!(
                concat!(
                    "    fixed \"{path}\" {{\n",
                    "        kind: Boolean,\n",
                    "        builtin: Some(Builtin::Boolean(false)),\n",
                    "        layers: TRACKED,\n",
                    "        class: Policy,\n",
                    "        flag: None,\n",
                    "    }}\n",
                ),
                path = path,
            )
        })
        .collect();
    ["configuration_registry! {\n", &rows, "}\n"].concat()
}

fn false_disable_evidence(arm: &CoreArmReport) -> &ConfigInventoryEvidence {
    let CoreObservation::Measured {
        evidence: CoreEvidence::ConfigFalseDisable(evidence),
    } = arm.observation()
    else {
        panic!("false-disable acquisition must produce inventory evidence")
    };
    evidence
}

fn config_path(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

#[test]
fn tracked_trust_inventory_fails_with_one_untracked_policy_source() {
    let repository = untracked_trust_repository("core-tracked-trust-untracked");
    let report = acquired(&repository, ClaimCapture::Idle, ActorCapture::Absent);
    let arm = acquired_arm(&report, GateId::ConfigTrackedTrust);
    let evidence = tracked_trust_evidence(arm);
    let expected = [
        config_path(&["lane", "vendor"]),
        config_path(&["vendor", "omp", "binary"]),
    ];

    assert_eq!(evidence.declared(), expected);
    assert_eq!(evidence.observed(), &expected[..1]);
    let [violation] = evidence.violations() else {
        panic!("one untracked policy source must yield exactly one violation")
    };
    assert_eq!(violation.code(), "config-tracked-trust::untracked-source");
    assert_eq!(arm.standing(), CoreStanding::Failed);

    repository.remove();
}

#[test]
fn inventories_refuse_a_missing_authoritative_registry_despite_a_decoy() {
    let repository = contract_repository("core-configuration-registry-missing");
    write_registry_decoy(&repository);
    git_commit(&repository, "missing authoritative configuration registry");

    let refusal = inventory_refusal(&repository);
    assert!(
        refusal.contains("authoritative root spice/src/config.rs is absent"),
        "{refusal}"
    );

    repository.remove();
}

#[test]
fn inventories_use_the_declared_split_registry_and_ignore_a_decoy() {
    let repository = contract_repository("core-configuration-registry-split");
    repository.write("spice/src/config.rs", "mod registry;\npub struct Root;\n");
    repository.write(
        "spice/src/config/registry.rs",
        &false_disable_registry(&["vendor.omp.fast_mode"]),
    );
    repository.write(
        "spice/src/consumer.rs",
        concat!(
            "use crate::config::Root;\n",
            "fn consume(root: &Root) -> bool {\n",
            "    root.vendor().omp().fast_mode()\n",
            "}\n",
        ),
    );
    write_registry_decoy(&repository);
    git_commit(&repository, "split authoritative registry with decoy");

    for gate_id in [GateId::ConfigFalseDisable, GateId::ConfigTrackedTrust] {
        let result = inventory_result(&repository, gate_id);
        assert_eq!(
            result.standing(),
            CoreStanding::Passed,
            "{gate_id} did not use only the declared split registry: {}",
            result.diagnostic()
        );
    }

    repository.remove();
}

#[test]
fn inventories_refuse_a_declared_but_missing_split_registry_module() {
    let repository = contract_repository("core-configuration-registry-split-missing");
    repository.write("spice/src/config.rs", "mod registry;\npub struct Root;\n");
    git_commit(&repository, "missing declared split registry module");

    let refusal = inventory_refusal(&repository);
    assert!(
        refusal.contains("authoritative split module spice/src/config/registry.rs is absent"),
        "{refusal}"
    );

    repository.remove();
}

fn inventory_result(repository: &Tree, gate_id: GateId) -> CoreArmReport {
    let report = acquired(repository, ClaimCapture::Idle, ActorCapture::Absent);
    acquired_arm(&report, gate_id).clone()
}

fn inventory_refusal(repository: &Tree) -> String {
    let tree = Selector::staged()
        .materialize(&repository.root)
        .expect("materialize the captured index");
    let contract = selected_contract(tree.root());
    let baselines = captured_baselines(repository, &contract);
    let rim = rim(repository);
    let source = CoreSource::capture(
        &tree,
        &contract,
        ClaimCapture::Idle,
        ActorCapture::Absent,
        &JudgedLatches::default(),
        &rim,
    )
    .expect("capture one immutable core source");
    acquire_core(&source, &contract, &baselines)
        .expect_err("an unusable configuration inventory must refuse")
        .to_string()
}

fn write_registry_decoy(repository: &Tree) {
    repository.write(
        "spice/src/registry_decoy.rs",
        concat!(
            "macro_rules! configuration_registry { ($($tokens:tt)*) => {}; }\n",
            "configuration_registry! {\n",
            "    fixed \"vendor.omp.fast_mode\" {\n",
            "        kind: Boolean,\n",
            "        builtin: Some(Builtin::Boolean(false)),\n",
            "        layers: LOCAL,\n",
            "        class: Policy,\n",
            "        flag: None,\n",
            "    }\n",
            "    fixed \"decoy.enabled\" {\n",
            "        kind: Boolean,\n",
            "        builtin: Some(Builtin::Boolean(false)),\n",
            "        layers: TRACKED,\n",
            "        class: Policy,\n",
            "        flag: None,\n",
            "    }\n",
            "}\n",
        ),
    );
}

fn untracked_trust_repository(label: &str) -> Tree {
    let repository = contract_repository(label);
    repository.write(
        "spice/src/config.rs",
        concat!(
            "configuration_registry! {\n",
            "    fixed \"lane.vendor\" {\n",
            "        kind: String,\n",
            "        builtin: Some(Builtin::String(\"omp\")),\n",
            "        layers: TRACKED_FLAG,\n",
            "        class: Policy,\n",
            "        flag: None,\n",
            "    }\n",
            "    fixed \"vendor.omp.binary\" {\n",
            "        kind: String,\n",
            "        builtin: Some(Builtin::String(\"omp\")),\n",
            "        layers: LOCAL,\n",
            "        class: Policy,\n",
            "        flag: None,\n",
            "    }\n",
            "    fixed \"proof.runner\" {\n",
            "        kind: String,\n",
            "        builtin: Some(Builtin::Absent),\n",
            "        layers: LOCAL,\n",
            "        class: MachineLocal,\n",
            "        flag: None,\n",
            "    }\n",
            "}\n",
        ),
    );
    git_commit(&repository, "tracked-trust inventory fixture");
    repository
}

fn tracked_trust_evidence(arm: &CoreArmReport) -> &ConfigInventoryEvidence {
    let CoreObservation::Measured {
        evidence: CoreEvidence::ConfigTrackedTrust(evidence),
    } = arm.observation()
    else {
        panic!("tracked-trust acquisition must produce inventory evidence")
    };
    evidence
}
