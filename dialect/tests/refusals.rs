use dialect::{Classification, RefusalCode, classify};

fn assert_refusal(source: &str, code: RefusalCode) {
    let Classification::Refused { refusals, .. } = classify(source) else {
        panic!("fixture should be refused");
    };
    assert!(
        refusals.iter().any(|refusal| refusal.code == code),
        "missing {code}; got {refusals:?}"
    );
}

#[test]
fn empty_and_structureless_documents_refuse_exactly() {
    assert_refusal(" \n\t\n", RefusalCode::EmptyDocument);
    assert_refusal("prose only\n", RefusalCode::NoNodes);
}

#[test]
fn malformed_and_unknown_dependencies_are_distinct() {
    assert_refusal(
        "# Goal\nAfter: missing!\n",
        RefusalCode::MalformedDependency,
    );
    assert_refusal("# Goal\nAfter: missing\n", RefusalCode::UnknownDependency);
    assert_refusal("# Goal\nAfter: goal,\n", RefusalCode::MalformedDependency);
}

#[test]
fn duplicate_identity_refuses() {
    assert_refusal(
        "# Goal\n\n- Same task\n- Same task\n",
        RefusalCode::DuplicateIdentity,
    );
}

#[test]
fn unsupported_field_refuses_instead_of_becoming_silent_metadata() {
    assert_refusal("# Goal\nOwner: mei\n", RefusalCode::UnsupportedField);
    assert_refusal("# Goal\nTags: release\n", RefusalCode::UnsupportedField);
}

#[test]
fn priority_and_flow_use_closed_vocabularies() {
    assert_refusal("# Goal\nPriority: urgent\n", RefusalCode::InvalidPriority);
    assert_refusal(
        "# Goal\nFlow: todo, deploy\n",
        RefusalCode::InvalidFlowPhase,
    );
}

#[test]
fn explicit_dependency_cycles_refuse() {
    let Classification::Refused { refusals, .. } =
        classify("# Goal\n\n- Alpha\n  After: beta\n- Beta\n  After: alpha\n")
    else {
        panic!("cycle should be refused");
    };
    assert!(
        refusals
            .iter()
            .any(|refusal| refusal.code == RefusalCode::DependencyCycle)
    );
}

#[test]
fn title_requires_an_ascii_identity_word() {
    assert_refusal("# !!!\n", RefusalCode::TitleWithoutAsciiWords);
}

#[test]
fn bare_reference_to_qualified_duplicates_is_ambiguous() {
    assert_refusal(
        "# Goal\n\n## One\n\n- Shared\n\n## Two\n\n- Shared\n- Consumer\n  After: shared\n",
        RefusalCode::AmbiguousDependency,
    );
}

#[test]
fn every_node_is_checked_for_the_reserved_synthetic_root_identity() {
    assert_refusal(
        "# Alpha\n- Document root\n# Beta\n",
        RefusalCode::SyntheticRootCollision,
    );
}
