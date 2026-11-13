use std::path::PathBuf;

use gate::{ContractError, ParsedContract, PolicyError};
use serde::Deserialize;
use wire::TRACKED_POLICY_FILE;

const TRACKED_POLICY: &str = ::std::include_str!("../../../spice.toml");

#[test]
fn identical_tracked_bytes_yield_identical_execution_identities() {
    let first = fixture_contract(TRACKED_POLICY).expect("first load");
    let second = fixture_contract(TRACKED_POLICY).expect("second load");

    assert_eq!(first.policy().render(), second.policy().render());
    assert_eq!(first.policy().digest(), second.policy().digest());
    assert_eq!(first.gate_set().render(), second.gate_set().render());
    assert_eq!(first.gate_set().digest(), second.gate_set().digest());
    assert_eq!(first.bounds().render(), second.bounds().render());
    assert_eq!(first.bounds().digest(), second.bounds().digest());
}

#[test]
fn admitted_walk_intent_moves_policy_without_selecting_an_executable() {
    let compiled = fixture_contract(TRACKED_POLICY).expect("compiled defaults");
    let narrowed = fixture_contract(&loadable("[gate.walk]\nskip = [\"fixtures/\"]\n"))
        .expect("tracked skip is admitted");

    assert_ne!(compiled.policy().digest(), narrowed.policy().digest());
    assert_eq!(compiled.gate_set().digest(), narrowed.gate_set().digest());
    assert_eq!(compiled.bounds().digest(), narrowed.bounds().digest());
    assert_eq!(
        narrowed.policy().ignore().render(),
        ".git/\ntarget/\nfixtures/\n"
    );

    let refused = fixture_contract("[gate]\nprogram = \"/bin/true\"\n")
        .expect_err("tracked bytes cannot select an executable");
    let ContractError::Policy(PolicyError::Toml { path, source }) = refused else {
        panic!("unknown executable key lost its typed config refusal: {refused:?}");
    };
    assert_eq!(path, policy_path());
    assert!(source.to_string().contains("program"));
}

#[test]
fn malformed_policy_and_unknown_gate_fields_refuse_before_a_contract_exists() {
    let malformed = fixture_contract("gate = [\n").expect_err("malformed shared policy is refused");
    assert!(matches!(
        malformed,
        ContractError::Policy(PolicyError::Toml { ref path, .. }) if path == &policy_path()
    ));

    let unknown = fixture_contract("[gate.walk]\nskip = []\ntimeout = 1\n")
        .expect_err("unknown gate field is refused");
    let ContractError::Policy(PolicyError::Toml { path, source }) = unknown else {
        panic!("unknown gate key lost its typed config refusal: {unknown:?}");
    };
    assert_eq!(path, policy_path());
    assert!(source.to_string().contains("timeout"));

    let source = "[gate.walk]\nskip = [\"fixtures/**\"]\n";
    let selector =
        fixture_contract(&loadable(source)).expect("documented crossing-star skip is admitted");
    let configured = toml::from_str::<GateDocument>(source)
        .expect("the same tracked bytes decode through the configuration tree");
    assert_eq!(
        selector.policy().ignore().render(),
        ".git/\ntarget/\nfixtures/**\n"
    );
    assert_eq!(
        &selector.policy().ignore().rules()[2..],
        configured.gate.walk().skip().rules(),
        "input policy must retain the domain configuration's exact admitted rules"
    );
}

/// The tracked document inventory is the one the contract compiles.
///
/// `AGENTS.md` is packaged, so a contract that never reads the declaration keeps
/// naming it and can produce none of the three other inventories below. The
/// declared documents deliberately carry no markdown suffix, because the
/// packaged markdown-depth rules select every `.md` path on their own and would
/// hide whether the declaration was read at all.
#[test]
fn the_tracked_repository_document_inventory_reaches_the_compiled_rule_policy() {
    let packaged = fixture_contract(TRACKED_POLICY)
        .expect("the tracked workspace declares no repository documents");
    assert_eq!(
        packaged.rule_policy().document().repo_truth_docs(),
        ["AGENTS.md"],
        "a repository that declares nothing measures the packaged inventory"
    );

    let declared = fixture_contract(&declaring(
        "docs = [\"./NOTICE.txt\", \"charter/CHARTER.txt\"]",
    ))
    .expect("a declared repository-document inventory is admitted");
    assert_eq!(
        declared.rule_policy().document().repo_truth_docs(),
        ["NOTICE.txt", "charter/CHARTER.txt"],
        "the declared inventory is the compiled one, normalized and ordered"
    );
    assert!(
        declared
            .rule_policy()
            .selects_repository_document("NOTICE.txt")
            .expect("a declared document resolves"),
        "a declared document no packaged rule can reach was left unselected"
    );

    let none = fixture_contract(&declaring("docs = []"))
        .expect("declaring no repository documents is admitted");
    assert!(
        none.rule_policy().document().repo_truth_docs().is_empty(),
        "declaring an empty inventory is not the same as declaring nothing"
    );

    let refused = fixture_contract(&declaring("docs = [\"../escape.txt\"]"))
        .expect_err("a document outside the repository is refused");
    let ContractError::Policy(PolicyError::Rules {
        path,
        source: refusal,
    }) = refused
    else {
        panic!("an escaping document lost its typed tracked refusal: {refused:?}");
    };
    assert_eq!(path, policy_path());
    assert_eq!(refusal.source(), TRACKED_POLICY_FILE);
    assert_eq!(refusal.field(), "policy.repo_truth.docs");
}

/// The tracked workspace policy plus one `[policy.repo_truth]` declaration.
fn declaring(docs: &str) -> String {
    format!("{TRACKED_POLICY}\n[policy.repo_truth]\n{docs}\n")
}

#[derive(Deserialize)]
struct GateDocument {
    gate: gate::Config,
}

/// One tracked policy document that can load: a fixture body carrying the
/// file-class selections parsing requires of every document.
fn loadable(body: &str) -> String {
    format!("{}{body}", gate::NARROWEST_FILE_CLASSES)
}

fn fixture_contract(policy: &str) -> Result<ParsedContract, ContractError> {
    ParsedContract::from_tracked(&policy_path(), policy)
}

fn policy_path() -> PathBuf {
    PathBuf::from("fixture/spice.toml")
}
