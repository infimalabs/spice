use dialect::{
    Classification, ClassificationContext, EdgeKind, RefusalCode, WarningCode, classify,
    classify_with_context, emit,
};

fn document(classification: &Classification) -> &dialect::Document {
    classification
        .document()
        .expect("fixture should produce a document")
}

#[test]
fn canonical_output_is_current_and_carries_caller_identity() {
    let source = "# Cut release\n\n- Build backend\n  Term: binary is reproducible\n  Priority: high\n- Build frontend\n  After: build-backend\n  Flow: todo, verify, review\n";
    let context = ClassificationContext {
        origin: Some("ack:1kExample".to_owned()),
    };
    let first = classify_with_context(source, context.clone());
    let canonical = emit(document(&first));
    let second = classify_with_context(&canonical, context);
    assert!(matches!(second, Classification::Current(_)));
    let graph = document(&second);
    assert_eq!(graph.origin.as_deref(), Some("ack:1kExample"));
    assert_eq!(graph.nodes[graph.root].slug, "cut-release");
    assert!(graph.edges.iter().any(|edge| {
        edge.kind == EdgeKind::Explicit
            && graph.nodes[edge.source].slug == "build-frontend"
            && graph.nodes[edge.target].slug == "build-backend"
    }));
}

#[test]
fn convertible_spelling_reaches_a_byte_stable_fixed_point() {
    let source =
        "Release plan\n============\n\n* [x] Package bits\n  **Acceptance:** archive opens\n";
    let first = classify(source);
    let warnings = first.warnings();
    assert!(
        warnings
            .iter()
            .any(|warning| { warning.code == WarningCode::NonCanonicalHeading })
    );
    assert!(
        warnings
            .iter()
            .any(|warning| { warning.code == WarningCode::CheckedStatusDiscarded })
    );
    let once = emit(document(&first));
    let second = classify(&once);
    assert!(matches!(second, Classification::Current(_)));
    let twice = emit(document(&second));
    assert_eq!(once, twice);
    assert_eq!(
        once,
        "# Release plan\n\n- Package bits\n  Term: archive opens\n"
    );
}

#[test]
fn containment_and_qualified_duplicate_titles_are_unambiguous() {
    let source = "# Rollout\n\n## Phase one\n\n- Update docs\n\n## Phase two\n\n- Update docs\n- Ship fleet\n  After: phase-one--update-docs\n";
    let classification = classify(source);
    let graph = document(&classification);
    assert!(graph.node("phase-one--update-docs").is_some());
    assert!(graph.node("phase-two--update-docs").is_some());
    let canonical = emit(graph);
    assert!(matches!(classify(&canonical), Classification::Current(_)));
}

#[test]
fn several_top_level_items_receive_a_synthetic_root() {
    let classification = classify("- First task\n- Second task\n");
    let graph = document(&classification);
    assert_eq!(graph.nodes[graph.root].slug, "document-root");
    assert_eq!(graph.nodes[graph.root].children.len(), 2);
    assert_eq!(emit(graph), "- First task\n- Second task\n");
}

#[test]
fn every_convertible_result_has_a_reason_code() {
    let classification = classify("# Goal\n");
    match classification {
        Classification::Current(_) => {}
        Classification::Convertible { warnings, .. } => assert!(!warnings.is_empty()),
        Classification::Refused { .. } => panic!("valid goal was refused"),
    }
}

#[test]
fn escaped_field_shape_remains_description_prose() {
    let first = classify("# Goal\n\nOwner\\: operator prose\n");
    let canonical = emit(document(&first));
    assert_eq!(canonical, "# Goal\n\nOwner\\: operator prose\n");
    assert!(matches!(classify(&canonical), Classification::Current(_)));
}

#[test]
fn invalid_context_is_a_named_refusal() {
    let classification = classify_with_context(
        "# Goal\n",
        ClassificationContext {
            origin: Some("memory:old".to_owned()),
        },
    );
    let codes = classification
        .refusals()
        .iter()
        .map(|refusal| refusal.code)
        .collect::<Vec<_>>();
    assert_eq!(codes, vec![RefusalCode::InvalidOrigin]);
}

#[test]
fn accepted_corpus_converges_after_one_normalization() {
    let fixtures = [
        "- A\n- B\n",
        "Goal\n====\n\n1. First\n2. Second\n",
        "# Goal\n\n- [ ] Work\n  Done when: complete\n  Deadline: 2026-08-01\n",
        "# Goal\n\n```text\n# preserved code\n```\n",
        "# Goal\n\n\\- preserved prose\nAfter\\: preserved prose\n",
    ];
    for fixture in fixtures {
        let first = classify(fixture);
        let once = emit(document(&first));
        let second = classify(&once);
        assert!(
            matches!(second, Classification::Current(_)),
            "canonical output was not current:\n{once}"
        );
        assert_eq!(emit(document(&second)), once);
    }
}

#[test]
fn nested_fenced_annotations_reach_the_same_fixed_point() {
    let source = "# Goal\n\n- Task\n\n  ```text\n  preserved\n    nested\n  ```\n";

    let first = classify(source);
    let once = emit(document(&first));
    let second = classify(&once);

    assert!(
        matches!(second, Classification::Current(_)),
        "nested fence did not become current:\n{once}"
    );
    assert_eq!(emit(document(&second)), once);
}
