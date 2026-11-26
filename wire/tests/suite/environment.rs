//! Environmental requirement shape, canonicality, and compiled semantics.

use serde_json::{Value, json};
use wire::body::TaskAdded;
use wire::{
    Accepted, Actor, AnyFact, CanonicalUuid, Command, CommandKind, CommandRejection, Environment,
    EnvironmentFact, EnvironmentObservation, EnvironmentRequirement, EnvironmentRequirements, Seq,
    TaskHandle, TaskHead, TrustHead, TrustState, WireVersion, Witness,
};

// Principal-authored task fixture.
const TASK_ADDED: &str = r#"{
  "kind": "task::added",
  "task": "1kKRRsDPx",
  "title": "Build the gate",
  "pitch": "The gate needs building.",
  "topic": { "path": "/wire" },
  "depends": [],
  "flow": ["draft", "todo"],
  "phase": "todo",
  "revision": 1,
  "creation": { "kind": "unavailable", "reason": "principal-actor" }
}"#;

const TASK_HEAD: &str = r#"{
  "born": "1kKRRsDPx",
  "topic_origin": 9,
  "title": "Build the gate",
  "pitch": "The gate needs building.",
  "topic": { "path": "/wire" },
  "dependencies": [],
  "provenance": "operator:draft",
  "creation": { "kind": "unavailable", "reason": "principal-actor" },
  "revision": 10
}"#;

fn digest(byte: char) -> wire::Digest {
    std::iter::repeat_n(byte, 64)
        .collect::<String>()
        .parse()
        .expect("fixture digest is canonical")
}

#[test]
fn journal_schema_is_one_typed_fact_and_one_typed_value() {
    let requirement = EnvironmentRequirement::journal_schema(15).expect("positive schema");
    let encoded: Value = serde_json::from_slice(&wire::encode(&requirement).expect("encodes"))
        .expect("requirement is JSON");

    assert_eq!(encoded, json!({"fact": "journal-schema", "value": 15}));
    assert_eq!(
        wire::decode::<EnvironmentRequirement>(br#"{"fact":"journal-schema","value":15}"#)
            .expect("canonical requirement"),
        requirement
    );
}

#[test]
fn journal_schema_comparison_is_compiled_as_observed_at_least_required() {
    let requirements = EnvironmentRequirements::one(
        EnvironmentRequirement::journal_schema(15).expect("positive schema"),
    );
    let below = Environment::journal_schema(14).expect("positive schema");
    let equal = Environment::journal_schema(15).expect("positive schema");
    let above = Environment::journal_schema(16).expect("positive schema");

    assert!(!requirements.are_satisfied_by(&Environment::default()));
    assert!(!requirements.are_satisfied_by(&below));
    assert!(requirements.are_satisfied_by(&equal));
    assert!(requirements.are_satisfied_by(&above));
    assert!(EnvironmentRequirements::default().are_satisfied_by(&Environment::default()));
}

#[test]
fn trust_grant_is_an_exact_digest_fact_with_bound_producer_provenance() {
    let required = digest('a');
    let requirement = EnvironmentRequirement::trust_grant(required.clone());
    assert_eq!(requirement.fact(), EnvironmentFact::TrustGrant);
    assert_eq!(
        serde_json::from_slice::<Value>(&wire::encode(&requirement).expect("encodes"))
            .expect("requirement is JSON"),
        json!({"fact": "trust-grant", "value": required})
    );

    let requirements = EnvironmentRequirements::one(requirement);
    let observed = |digest: wire::Digest, state, worktree: Option<&str>| TrustHead {
        digest,
        state,
        worktree: worktree.map(str::to_owned),
    };
    for unsatisfied in [
        Environment::default(),
        Environment::default().with_trust_heads([observed(
            required.clone(),
            TrustState::Pended,
            Some("/producer/one"),
        )]),
        Environment::default().with_trust_heads([observed(
            required.clone(),
            TrustState::Revoked,
            Some("/producer/one"),
        )]),
        Environment::default().with_trust_heads([observed(
            required.clone(),
            TrustState::Granted,
            None,
        )]),
        Environment::default().with_trust_heads([observed(
            required.clone(),
            TrustState::Granted,
            Some(""),
        )]),
        Environment::default().with_trust_heads([observed(
            digest('b'),
            TrustState::Granted,
            Some("/producer/one"),
        )]),
    ] {
        assert!(!requirements.are_satisfied_by(&unsatisfied));
        assert!(requirements.has_unsatisfied_trust_grant(&unsatisfied));
    }

    let granted_elsewhere = Environment::default().with_trust_heads([observed(
        required,
        TrustState::Granted,
        Some("/another/worktree"),
    )]);
    assert!(
        requirements.are_satisfied_by(&granted_elsewhere),
        "producer worktree is provenance, not an eligibility scope"
    );
    assert!(!requirements.has_unsatisfied_trust_grant(&granted_elsewhere));
    assert!(
        !EnvironmentRequirements::one(
            EnvironmentRequirement::journal_schema(15).expect("positive schema")
        )
        .has_unsatisfied_trust_grant(&Environment::default()),
        "journal-schema blockers do not ice claims"
    );
}

#[test]
fn journal_and_trust_requirements_form_one_canonical_conjunction() {
    let schema = EnvironmentRequirement::journal_schema(15).expect("positive schema");
    let trust = EnvironmentRequirement::trust_grant(digest('c'));
    let requirements = EnvironmentRequirements::new(vec![schema.clone(), trust.clone()])
        .expect("fact order is canonical");
    assert_eq!(requirements.as_slice(), &[schema.clone(), trust.clone()]);
    assert!(EnvironmentRequirements::new(vec![trust.clone(), schema]).is_err());
    assert!(EnvironmentRequirements::new(vec![trust.clone(), trust]).is_err());

    let environment = Environment::journal_schema(15)
        .expect("positive schema")
        .with_trust_heads([TrustHead {
            digest: digest('c'),
            state: TrustState::Granted,
            worktree: Some("/producer".to_owned()),
        }]);
    assert!(requirements.are_satisfied_by(&environment));
}

#[test]
fn appliance_version_is_satisfied_only_by_a_strictly_later_running_appliance() {
    let version = |text| wire::manifest::ProductVersion::parse(text).expect("canonical version");
    let requirement =
        EnvironmentRequirement::appliance_after(version("0.2.0-dev.0.dTYp0bz3B+bin.old"));
    let encoded: Value = serde_json::from_slice(&wire::encode(&requirement).expect("encodes"))
        .expect("requirement is JSON");
    assert_eq!(
        encoded,
        json!({"fact": "appliance-version", "value": "0.2.0-dev.0.dTYp0bz3B+bin.old"})
    );
    assert!(
        wire::decode::<EnvironmentRequirement>(br#"{"fact":"appliance-version","value":"v1.2.3"}"#)
            .is_err()
    );

    let requirements = EnvironmentRequirements::new(vec![
        EnvironmentRequirement::journal_schema(15).expect("positive schema"),
        requirement,
    ])
    .expect("appliance version follows journal schema in fact order");
    let running = |text| {
        Environment::journal_schema(15)
            .expect("positive schema")
            .with_appliance(version(text))
    };
    assert!(!requirements.are_satisfied_by(&Environment::journal_schema(15).expect("schema")));
    for unsatisfied in [
        "0.2.0-dev.0.dTYp0bz3B+bin.old",
        "0.2.0-dev.0.dTYp0bz3B+bin.rebuilt",
        "0.2.0-dev.0",
        "0.2.0-dev.0.dTYnzzzzz+bin.new",
    ] {
        assert!(
            !requirements.are_satisfied_by(&running(unsatisfied)),
            "{unsatisfied} is not later"
        );
    }
    for satisfied in ["0.2.0-dev.0.dTYpFZV7w+bin.new", "0.2.0-dev.1", "0.2.0"] {
        assert!(
            requirements.are_satisfied_by(&running(satisfied)),
            "{satisfied} is later"
        );
    }
}

#[test]
fn resource_pool_requires_its_declared_available_shard_count() {
    let name = "windows-guest"
        .parse::<wire::ResourcePoolName>()
        .expect("normal pool name");
    let requirement =
        EnvironmentRequirement::resource_pool(name.clone(), 2).expect("positive shard count");
    let encoded: Value = serde_json::from_slice(&wire::encode(&requirement).expect("encodes"))
        .expect("requirement is JSON");
    assert_eq!(
        encoded,
        json!({"fact": "resource-pool", "name": "windows-guest", "shards": 2})
    );
    let requirements = EnvironmentRequirements::one(requirement);
    assert!(!requirements.are_satisfied_by(&Environment::default()));
    assert!(
        !requirements.are_satisfied_by(&Environment::default().with_resource_pool(name.clone(), 1))
    );
    assert!(requirements.are_satisfied_by(&Environment::default().with_resource_pool(name, 2)));
    assert!(
        EnvironmentRequirement::resource_pool(
            "windows-guest".parse().expect("normal pool name"),
            0,
        )
        .is_err()
    );
    for malformed in [
        br#"{"fact":"resource-pool","name":"../guest","shards":1}"#.as_slice(),
        br#"{"fact":"resource-pool","name":"windows-guest","shards":0}"#,
    ] {
        assert!(wire::decode::<EnvironmentRequirement>(malformed).is_err());
    }
}

#[test]
fn repeated_resource_entries_compile_into_one_sorted_task_map() {
    let emulator = "emulator"
        .parse::<wire::ResourcePoolName>()
        .expect("resource name");
    let windows = "windows-guest"
        .parse::<wire::ResourcePoolName>()
        .expect("resource name");
    let authored = EnvironmentRequirements::new(vec![
        EnvironmentRequirement::resource_pool(emulator.clone(), 2).expect("positive count"),
        EnvironmentRequirement::resource_pool(windows.clone(), 1).expect("positive count"),
    ])
    .expect("resource entries are in pool-name order");
    let resources = authored.compiled_resources();
    assert_eq!(
        serde_json::from_slice::<Value>(&wire::encode(&resources).expect("map encodes"))
            .expect("map is JSON"),
        json!({"emulator": 2, "windows-guest": 1})
    );
    assert!(authored.without_resources().is_empty());
    assert!(!resources.are_satisfied_by(&Environment::default().with_resource_pool(emulator, 2)));
    assert!(
        resources.are_satisfied_by(
            &Environment::default()
                .with_resource_pool("emulator".parse().expect("resource name"), 2,)
                .with_resource_pool(windows, 1)
        )
    );
    assert!(
        EnvironmentRequirements::new(vec![
            EnvironmentRequirement::resource_pool(
                "windows-guest".parse().expect("resource name"),
                1,
            )
            .expect("positive count"),
            EnvironmentRequirement::resource_pool("emulator".parse().expect("resource name"), 2,)
                .expect("positive count"),
        ])
        .is_err(),
        "authored resource entries must already be in canonical name order"
    );
}

#[test]
fn authored_evaluators_and_non_values_have_no_wire_shape() {
    for malformed in [
        br#"{"fact":"journal-schema","value":0}"#.as_slice(),
        br#"{"fact":"journal-schema","value":-1}"#,
        br#"{"fact":"journal-schema","value":1.0}"#,
        br#"{"fact":"journal-schema","value":4294967296}"#,
        br#"{"fact":"journal-schema","value":"15"}"#,
        br#"{"fact":"journal-schema"}"#,
        br#"{"fact":"journal-schema","value":15,"operator":">="}"#,
        br#"{"fact":"journal-schema","value":15,"evaluator":"shell"}"#,
        br#"{"fact":"journal-schema","value":15,"not":true}"#,
        br#"{"fact":"journal-schema","value":15,"or":[]}"#,
        br#"{"fact":"expression","value":"journal_schema >= required"}"#,
        br#"{"fact":"trust-grant","value":"ABC"}"#,
        br#"{"fact":"trust-grant","value":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","operator":"="}"#,
    ] {
        assert!(
            wire::decode::<EnvironmentRequirement>(malformed).is_err(),
            "malformed requirement was admitted: {}",
            String::from_utf8_lossy(malformed)
        );
    }
    assert!(EnvironmentRequirement::journal_schema(0).is_err());
    assert!(EnvironmentObservation::journal_schema(0).is_err());
    assert!(Environment::journal_schema(0).is_err());
}

#[test]
fn requirement_collections_refuse_duplicates_without_normalizing() {
    let duplicate = br#"[
      {"fact":"journal-schema","value":14},
      {"fact":"journal-schema","value":15}
    ]"#;

    assert!(wire::decode::<EnvironmentRequirements>(duplicate).is_err());
    assert!(
        EnvironmentRequirements::new(vec![
            EnvironmentRequirement::journal_schema(14).expect("positive schema"),
            EnvironmentRequirement::journal_schema(15).expect("positive schema"),
        ])
        .is_err()
    );
}

#[test]
fn task_carriers_omit_empty_requirements_and_refuse_an_explicit_empty_array() {
    let fact: AnyFact = wire::decode(TASK_ADDED.as_bytes()).expect("task fact decodes");
    let added: TaskAdded = fact.read().expect("task-added body");
    assert!(added.requirements.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&wire::encode(&fact).expect("retained fact re-encodes"))
            .expect("fact is JSON"),
        serde_json::from_str::<Value>(TASK_ADDED).expect("fixture is JSON")
    );

    let head: TaskHead = wire::decode(TASK_HEAD.as_bytes()).expect("task head decodes");
    assert!(head.requirements.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&wire::encode(&head).expect("retained head re-encodes"))
            .expect("head is JSON"),
        serde_json::from_str::<Value>(TASK_HEAD).expect("fixture is JSON")
    );

    let mut fact = serde_json::from_str::<Value>(TASK_ADDED).expect("fixture is JSON");
    fact["requirements"] = json!([]);
    assert!(wire::decode::<AnyFact>(&serde_json::to_vec(&fact).expect("fact encodes")).is_err());

    let mut head = serde_json::from_str::<Value>(TASK_HEAD).expect("fixture is JSON");
    head["requirements"] = json!([]);
    assert!(wire::decode::<TaskHead>(&serde_json::to_vec(&head).expect("head encodes")).is_err());
}

#[test]
fn task_fact_and_head_carry_the_same_canonical_requirements() {
    let requirement = json!([{"fact": "journal-schema", "value": 15}]);
    let mut fact = serde_json::from_str::<Value>(TASK_ADDED).expect("fixture is JSON");
    fact["requirements"] = requirement.clone();
    let fact_bytes = serde_json::to_vec(&fact).expect("fact encodes");
    let added: TaskAdded = wire::decode::<AnyFact>(&fact_bytes)
        .expect("task fact admits requirements")
        .read()
        .expect("task-added body");

    let mut head = serde_json::from_str::<Value>(TASK_HEAD).expect("fixture is JSON");
    head["requirements"] = requirement;
    let task_head: TaskHead =
        wire::decode(&serde_json::to_vec(&head).expect("head encodes")).expect("task head decodes");

    assert_eq!(added.requirements, task_head.requirements);
    assert_eq!(
        added.requirements.as_slice(),
        &[EnvironmentRequirement::journal_schema(15).expect("positive schema")]
    );
}

#[test]
fn requirement_edit_has_distinct_set_clear_and_unchanged_spellings() {
    let set = wire::intent::TaskRequirementsEdit::one(
        EnvironmentRequirement::journal_schema(15).expect("positive schema"),
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&wire::encode(&set).expect("set encodes"))
            .expect("set is JSON"),
        json!({
            "action": "set",
            "requirements": [{"fact": "journal-schema", "value": 15}]
        })
    );
    assert_eq!(
        wire::decode::<wire::intent::TaskRequirementsEdit>(br#"{"action":"clear"}"#)
            .expect("clear decodes")
            .requirements(),
        EnvironmentRequirements::default()
    );
    for malformed in [
        br#"{"action":"set","requirements":[]}"#.as_slice(),
        br#"{"action":"clear","requirements":[]}"#,
        br#"{"action":"replace","requirements":[]}"#,
    ] {
        assert!(wire::decode::<wire::intent::TaskRequirementsEdit>(malformed).is_err());
    }
}

#[test]
fn requirement_edits_bind_statement_and_claim_witnesses_to_the_same_task() {
    let task: TaskHandle = "WIRE-1kKRRsDPx".parse().expect("task handle");
    let born = task.born().expect("task stamp");
    let seq = Seq::new(9).expect("head sequence");
    let epoch: CanonicalUuid = "018f82d4-6f59-7d06-92e3-bce913116302"
        .parse()
        .expect("epoch");
    let accepted = Accepted {
        wire: WireVersion::CURRENT,
        actor: Actor::TEST_PRINCIPAL,
        epoch,
        release: "1.2.3".parse().expect("fixture release"),
    };
    let roles = wire::principal::PrincipalRoles::default();
    let edit = wire::intent::TaskEdit {
        intent: None,
        task,
        title: None,
        pitch: None,
        description: None,
        term: None,
        requirements: Some(wire::intent::TaskRequirementsEdit::Clear {}),
        priority: None,
        topic: None,
        flow: None,
        related: Vec::new(),
    };
    let command = |expected| Command {
        v: WireVersion::CURRENT,
        id: "018f82d4-6f59-7d06-92e3-bce913116303"
            .parse()
            .expect("command identity"),
        epoch,
        kind: CommandKind::TaskEdit,
        expected,
        body: serde_json::to_value(&edit).expect("edit encodes"),
    };
    let task_witness = Witness::Task {
        task: born.clone(),
        head: seq,
    };
    let claim_witness = Witness::Claim {
        task: born.clone(),
        claimed: None,
        head: None,
    };

    assert!(matches!(
        command(vec![task_witness.clone()]).admit(&accepted, &roles),
        Err(CommandRejection::Refusal(ref refusal)) if refusal.reason() == "command::witness-law"
    ));
    let foreign: wire::TaskStamp = "1kKRRsDQx".parse().expect("foreign task stamp");
    assert!(matches!(
        command(vec![
            task_witness.clone(),
            Witness::Claim {
                task: foreign.clone(),
                claimed: None,
                head: None,
            },
        ])
        .admit(&accepted, &roles),
        Err(CommandRejection::Refusal(ref refusal)) if refusal.reason() == "command::witness-law"
    ));
    assert!(
        command(vec![
            task_witness.clone(),
            claim_witness.clone(),
            Witness::Lease {
                task: born,
                claimed: seq,
                head: seq,
            },
        ])
        .admit(&accepted, &roles)
        .is_ok()
    );
    assert!(
        command(vec![task_witness, claim_witness])
            .admit(&accepted, &roles)
            .is_ok()
    );
}
