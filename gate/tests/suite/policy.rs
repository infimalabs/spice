use gate::{
    BoundName, PACKAGED_POLICY_SOURCE, PolicyContext, PolicyDocument, RulePolicy, RuleSourceKind,
    ScopeAxis, packaged_policy_text, parse_tracked_policy,
};

const OVERLAPPING: &str = r#"
regex_dialect = "rust-regex-1"
magic_examine_threshold = 10

[file_classes]
file_shape = [".rs"]
complexity = [".rs"]
magic = [".rs"]

[limits]
file_loc = 10
file_bytes = 100
routine_ccn = 5
routine_length = 8
commit_message_wrap = 72
repo_truth_doc_chars = 1000

[ratchet]
ratio = 2.0
routine_ccn = 12

[[rules]]
id = "src"
source_order = 0
priority = 1
scopes = { paths = ["src/**"] }
[rules.source]
location = "current.toml"
field = "rules.src"
kind = "current"
[rules.bounds.file_loc]
multiplier = 2.0
ratchet = 2.0
[rules.bounds.file_bytes]
multiplier = 2.0
ratchet = 2.0

[[rules]]
id = "src-vendored"
source_order = 1
priority = 1
scopes = { paths = ["src/vendored/**"] }
magic = { examine_threshold = 100 }
[rules.source]
location = "current.toml"
field = "rules.src-vendored"
kind = "current"
[rules.bounds.file_loc]
multiplier = 3.0
minimum = 35
maximum = 40
ratchet = 1.5

[[rules]]
id = "docs-markdown"
source_order = 2
priority = 1
scopes = { paths = ["Docs"], extensions = [".md"] }
[rules.source]
location = "current.toml"
field = "rules.docs-markdown"
kind = "current"
[rules.bounds.file_loc]
minimum = 25
maximum = 25
ratchet = 1.0
[rules.bounds.routine_length]
unlimited = true
"#;

const EVERY_SCOPE: &str = r#"
regex_dialect = "rust-regex-1"
magic_examine_threshold = 10

[file_classes]
file_shape = [".rs"]
complexity = [".rs"]
magic = [".rs"]

[limits]
file_loc = 10
file_bytes = 100
routine_ccn = 5
routine_length = 8
commit_message_wrap = 72
repo_truth_doc_chars = 1000

[ratchet]
ratio = 1.5

[[rules]]
id = "path-rule"
source_order = 0
priority = 1
scopes = { paths = ["src/**"] }
[rules.source]
location = "current.toml"
field = "rules.path-rule"
kind = "current"
[rules.bounds.file_loc]
multiplier = 2.0

[[rules]]
id = "extension-rule"
source_order = 1
priority = 1
scopes = { extensions = [".RS"] }
[rules.source]
location = "current.toml"
field = "rules.extension-rule"
kind = "current"
[rules.bounds.file_bytes]
multiplier = 2.0

[[rules]]
id = "vendor-rule"
source_order = 2
priority = 1
scopes = { vendors = ["OMP"] }
[rules.source]
location = "current.toml"
field = "rules.vendor-rule"
kind = "current"
[rules.bounds.routine_ccn]
multiplier = 2.0

[[rules]]
id = "model-rule"
source_order = 3
priority = 1
scopes = { models = ["GPT 5.5"] }
[rules.source]
location = "current.toml"
field = "rules.model-rule"
kind = "current"
[rules.bounds.routine_length]
multiplier = 2.0

[[rules]]
id = "phase-rule"
source_order = 4
priority = 1
scopes = { phases = ["pre_commit_success"] }
[rules.source]
location = "current.toml"
field = "rules.phase-rule"
kind = "current"
[rules.bounds.commit_message_wrap]
multiplier = 2.0
"#;

#[test]
fn resolution_prefers_the_most_specific_rule_and_otherwise_takes_global_policy() {
    let policy = current_policy(OVERLAPPING);

    let vendored = policy
        .resolve_bound(
            BoundName::FileLoc,
            &PolicyContext::new().with_path("src/vendored/app.rs"),
        )
        .expect("specific bound resolves");
    assert_eq!(vendored.limit(), 35);
    assert_eq!(vendored.ratchet_limit(), 52);
    assert!(!vendored.unlimited());
    let evidence = vendored.selection().expect("specific rule selected");
    assert_eq!(evidence.rule_id().as_str(), "src-vendored");
    assert_eq!(evidence.metric(), "file_loc");
    assert_eq!(evidence.priority(), 1);
    assert_eq!(evidence.source_order(), 1);
    assert_eq!(evidence.source().field(), "rules.src-vendored");
    assert!(evidence.match_basis().contains("src/vendored/**"));
    assert_eq!(evidence.specificity().constrained_axes(), 1);

    let broad = policy
        .resolve_bound(
            BoundName::FileBytes,
            &PolicyContext::new().with_path("src/app.rs"),
        )
        .expect("broad rule resolves");
    assert_eq!((broad.limit(), broad.ratchet_limit()), (200, 400));
    assert_eq!(
        broad.selection().expect("broad selection").source_order(),
        0
    );

    let global = policy
        .resolve_bound(
            BoundName::FileBytes,
            &PolicyContext::new().with_path("other/app.rs"),
        )
        .expect("global bound resolves");
    assert_eq!((global.limit(), global.ratchet_limit()), (100, 200));
    assert!(global.selection().is_none());

    let global_override = policy
        .resolve_bound(
            BoundName::RoutineCcn,
            &PolicyContext::new().with_path("other/app.rs"),
        )
        .expect("global per-bound ratchet resolves");
    assert_eq!(
        (global_override.limit(), global_override.ratchet_limit()),
        (5, 12)
    );
    assert!(global_override.selection().is_none());

    let unlimited = policy
        .resolve_bound(
            BoundName::RoutineLength,
            &PolicyContext::new().with_path("Docs/guide.md"),
        )
        .expect("per-bound unlimited resolves");
    assert!(unlimited.unlimited());
    assert_eq!(
        unlimited
            .selection()
            .expect("unlimited selection")
            .source_order(),
        2
    );

    let magic = policy
        .resolve_magic(&PolicyContext::new().with_path("src/vendored/app.rs"))
        .expect("magic resolves");
    assert_eq!(magic.metric(), "magic.examine_threshold");
    assert_eq!(magic.examine_threshold(), 100);
    assert_eq!(magic.selection().expect("magic rule").source_order(), 1);

    let global_magic = policy
        .resolve_magic(&PolicyContext::new().with_path("other/app.rs"))
        .expect("global magic resolves");
    assert_eq!(global_magic.examine_threshold(), 10);
    assert!(global_magic.selection().is_none());
}

#[test]
fn packaged_document_values_are_the_compiled_globals() {
    let declared: toml::Value =
        toml::from_str(packaged_policy_text()).expect("packaged policy is TOML");
    let compiled = parse_tracked_policy(PACKAGED_POLICY_SOURCE, packaged_policy_text())
        .expect("packaged policy compiles through the tracked parser");
    let document = compiled.document();

    for bound in BoundName::ALL {
        let declared = declared["policy"]["limits"][bound.as_str()]
            .as_integer()
            .expect("packaged limit is an integer");
        assert_eq!(
            document.limits().get(bound),
            u64::try_from(declared).expect("packaged limit is nonnegative"),
            "{bound}"
        );
    }
    assert_eq!(
        document.ratchet().ratio(),
        declared["policy"]["flex"]["ratio"]
            .as_float()
            .expect("packaged flex ratio is a float")
    );
    assert_eq!(
        document.magic_examine_threshold(),
        u64::try_from(
            declared["policy"]["magic"]["examine_threshold"]
                .as_integer()
                .expect("packaged magic threshold is an integer")
        )
        .expect("packaged magic threshold is nonnegative")
    );

    let declared_docs = declared["policy"]["repo_truth"]["docs"]
        .as_array()
        .expect("packaged repository documents are a list")
        .iter()
        .map(|value| value.as_str().expect("packaged document is text"))
        .collect::<Vec<_>>();
    assert_eq!(document.repo_truth_docs(), declared_docs);
}

#[test]
fn packaged_markdown_budgets_grow_with_depth_and_skip_one_letter_stems() {
    let policy = parse_tracked_policy(wire::TRACKED_POLICY_FILE, gate::NARROWEST_FILE_CLASSES)
        .expect("tracked policy parses");

    assert_eq!(policy.document().rules().len(), 6);
    assert_eq!(
        policy
            .document()
            .rules()
            .iter()
            .filter(|rule| rule.source().kind() == RuleSourceKind::GeneratedMarkdownDepth)
            .count(),
        4
    );
    assert_eq!(
        policy
            .document()
            .rules()
            .iter()
            .filter(|rule| { rule.source().kind() == RuleSourceKind::GeneratedDocumentationFamily })
            .count(),
        2
    );
    assert!(
        policy
            .document()
            .rules()
            .iter()
            .all(|rule| rule.priority() == 0)
    );

    let root = resolve_doc(&policy, "README.md");
    assert_eq!((root.limit(), root.ratchet_limit()), (10_000, 15_000));
    assert!(root.selection().is_some());
    assert_eq!(resolve_doc(&policy, "guides/guide.md").limit(), 20_000);
    assert_eq!(
        resolve_doc(&policy, "guides/reference/guide.md").limit(),
        30_000
    );
    let deepest = resolve_doc(&policy, "guides/reference/generated/guide.md");
    assert!(!deepest.unlimited());
    assert_eq!(deepest.limit(), 120_000);
    assert_eq!(
        resolve_doc(&policy, "guides/reference/generated/more/guide.md").limit(),
        120_000
    );

    for path in [
        "guides/reference/generated/A.md",
        "guides/reference/generated/guide.txt",
    ] {
        let global = resolve_doc(&policy, path);
        assert_eq!(global.limit(), 10_000, "{path}");
        assert!(global.selection().is_none(), "{path}");
    }
}

#[test]
fn documentation_components_contribute_one_implicit_markdown_depth() {
    let policy = parse_tracked_policy(wire::TRACKED_POLICY_FILE, gate::NARROWEST_FILE_CLASSES)
        .expect("tracked policy parses");

    for directory in [
        "doc",
        "docs",
        "document",
        "documents",
        "documentation",
        "documentations",
        "Doc",
        "Docs",
        "Document",
        "Documents",
        "Documentation",
        "Documentations",
    ] {
        let resolved = resolve_doc(&policy, &format!("{directory}/guide.md"));
        assert_eq!(resolved.limit(), 30_000, "{directory}");
        assert_eq!(
            resolved
                .selection()
                .map(|selection| selection.source().kind()),
            Some(RuleSourceKind::GeneratedDocumentationFamily),
            "{directory}"
        );
    }

    assert_eq!(
        resolve_doc(&policy, "docs/guide.md").limit(),
        resolve_doc(&policy, "guides/product/guide.md").limit()
    );
    for path in [
        "docs/reference/guide.md",
        "guides/product/reference/guide.md",
    ] {
        let deepest = resolve_doc(&policy, path);
        assert!(!deepest.unlimited(), "{path}");
        assert_eq!(deepest.limit(), 120_000, "{path}");
    }

    for path in [
        "adocs/guide.md",
        "docs-old/guide.md",
        "DOCS/guide.md",
        "documentation.md",
    ] {
        let resolved = resolve_doc(&policy, path);
        assert_eq!(
            resolved.limit(),
            if path == "documentation.md" {
                10_000
            } else {
                20_000
            },
            "{path}"
        );
        assert_ne!(
            resolved
                .selection()
                .map(|selection| selection.source().kind()),
            Some(RuleSourceKind::GeneratedDocumentationFamily),
            "{path}"
        );
    }
}

#[test]
fn authored_document_prefix_overrides_the_packaged_family_grant() {
    let packaged = parse_tracked_policy(wire::TRACKED_POLICY_FILE, gate::NARROWEST_FILE_CLASSES)
        .expect("tracked policy parses");
    let mut document =
        serde_json::to_value(packaged.document()).expect("packaged policy serializes");
    document["rules"]
        .as_array_mut()
        .expect("policy rules serialize as an array")
        .push(serde_json::json!({
            "id": "authored-docs",
            "source": {
                "location": "current.toml",
                "field": "rules.authored-docs",
                "kind": "current"
            },
            "source_order": 6,
            "priority": 1,
            "scopes": {
                "paths": ["docs"],
                "extensions": [".md"]
            },
            "bounds": {
                "repo_truth_doc_chars": {
                    "minimum": 25_000,
                    "maximum": 25_000
                }
            }
        }));
    let document: PolicyDocument =
        serde_json::from_value(document).expect("authored policy decodes");
    let policy = RulePolicy::compile(document).expect("authored policy compiles");

    let resolved = resolve_doc(&policy, "docs/guide.md");
    assert_eq!(resolved.limit(), 25_000);
    let selection = resolved.selection().expect("authored prefix selected");
    assert_eq!(selection.rule_id().as_str(), "authored-docs");
    assert_eq!(selection.source().kind(), RuleSourceKind::Current);
}

#[test]
fn current_named_rules_cover_every_scope_dimension_with_provenance() {
    let document: PolicyDocument = toml::from_str(EVERY_SCOPE).expect("current policy decodes");
    let encoded = serde_json::to_value(&document).expect("current policy serializes");
    let decoded = serde_json::from_value(encoded).expect("current policy round trips");
    let policy = RulePolicy::compile(decoded).expect("current policy compiles");

    let cases = [
        (
            BoundName::FileLoc,
            PolicyContext::new().with_path("src/lib.rs"),
            "path-rule",
            "paths=",
        ),
        (
            BoundName::FileBytes,
            PolicyContext::new().with_path("src/lib.rs"),
            "extension-rule",
            "extension=",
        ),
        (
            BoundName::RoutineCcn,
            PolicyContext::new().with_vendor("Omp"),
            "vendor-rule",
            "vendor=",
        ),
        (
            BoundName::RoutineLength,
            PolicyContext::new().with_model("gpt 5.5"),
            "model-rule",
            "model=",
        ),
    ];
    for (metric, context, id, basis) in cases {
        let resolved = policy
            .resolve_bound(metric, &context)
            .expect("scope resolves");
        let evidence = resolved.selection().expect("scope selects a rule");
        assert_eq!(evidence.rule_id().as_str(), id);
        assert_eq!(evidence.metric(), metric.as_str());
        assert!(evidence.match_basis().contains(basis));
        assert_eq!(evidence.source().kind(), RuleSourceKind::Current);
    }

    let explicit_extension = policy
        .resolve_bound(
            BoundName::FileBytes,
            &PolicyContext::new().with_extension(" .RS "),
        )
        .expect("explicit extension normalizes");
    assert_eq!(
        explicit_extension
            .selection()
            .expect("extension rule")
            .rule_id()
            .as_str(),
        "extension-rule"
    );
}

#[test]
fn missing_candidate_context_refuses_instead_of_falling_back() {
    let policy = current_policy(EVERY_SCOPE);
    let error = policy
        .resolve_bound(BoundName::RoutineCcn, &PolicyContext::new())
        .expect_err("missing vendor must refuse");

    assert_eq!(error.code(), "policy::context-missing");
    assert_eq!(error.metric(), "routine_ccn");
    assert_eq!(
        error.rule().map(ToString::to_string).as_deref(),
        Some("vendor-rule")
    );
    assert_eq!(error.dimension(), Some(ScopeAxis::Vendors));
    assert_eq!(
        error.recovery(),
        "supply PolicyContext::with_vendor before resolving routine_ccn"
    );
}

const RESOLVER_ORDER_POLICY: &str = r#"
        regex_dialect = "rust-regex-1"
        magic_examine_threshold = 10
        [file_classes]
        file_shape = [".rs"]
        complexity = [".rs"]
        magic = [".rs"]
        [limits]
        file_loc = 10
        file_bytes = 100
        routine_ccn = 5
        routine_length = 8
        commit_message_wrap = 72
        repo_truth_doc_chars = 1000
        [ratchet]
        ratio = 1.0

        [[rules]]
        id = "priority-broad"
        source_order = 0
        priority = 2
        scopes = { paths = ["src/**"] }
        [rules.source]
        location = "current.toml"
        field = "rules.priority-broad"
        kind = "current"
        [rules.bounds.file_loc]
        multiplier = 2.0

        [[rules]]
        id = "specific-low"
        source_order = 1
        priority = 1
        scopes = { paths = ["src/legacy/app.rs"] }
        [rules.source]
        location = "current.toml"
        field = "rules.specific-low"
        kind = "current"
        [rules.bounds.file_loc]
        multiplier = 3.0

        [[rules]]
        id = "specific-first"
        source_order = 2
        priority = 1
        scopes = { paths = ["src/**"] }
        [rules.source]
        location = "generated"
        field = "generated.first"
        kind = "generated-markdown-depth"
        [rules.bounds.file_bytes]
        multiplier = 2.0

        [[rules]]
        id = "specific-second"
        source_order = 3
        priority = 1
        scopes = { paths = ["src/**"] }
        [rules.source]
        location = "generated"
        field = "generated.second"
        kind = "generated-markdown-depth"
        [rules.bounds.file_bytes]
        multiplier = 3.0

        [[rules]]
        id = "specific-exact"
        source_order = 4
        priority = 1
        scopes = { paths = ["docs/guide.rs"] }
        [rules.source]
        location = "current.toml"
        field = "rules.specific-exact"
        kind = "current"
        [rules.bounds.routine_length]
        multiplier = 3.0

        [[rules]]
        id = "specific-glob"
        source_order = 5
        priority = 1
        scopes = { paths = ["docs/**"] }
        [rules.source]
        location = "current.toml"
        field = "rules.specific-glob"
        kind = "current"
        [rules.bounds.routine_length]
        multiplier = 2.0
        "#;

#[test]
fn resolver_order_is_priority_then_specificity_then_earlier_source() {
    let policy = current_policy(RESOLVER_ORDER_POLICY);

    let priority = policy
        .resolve_bound(
            BoundName::FileLoc,
            &PolicyContext::new().with_path("src/legacy/app.rs"),
        )
        .expect("priority resolves");
    assert_eq!(
        priority
            .selection()
            .expect("priority selection")
            .rule_id()
            .as_str(),
        "priority-broad"
    );

    let source = policy
        .resolve_bound(
            BoundName::FileBytes,
            &PolicyContext::new().with_path("src/app.rs"),
        )
        .expect("source order resolves");
    assert_eq!(source.limit(), 200);
    assert_eq!(
        source
            .selection()
            .expect("source selection")
            .rule_id()
            .as_str(),
        "specific-first"
    );

    let specific = policy
        .resolve_bound(
            BoundName::RoutineLength,
            &PolicyContext::new().with_path("docs/guide.rs"),
        )
        .expect("specificity resolves");
    assert_eq!(
        specific
            .selection()
            .expect("specific selection")
            .rule_id()
            .as_str(),
        "specific-exact"
    );
}

/// A current document is held to the same file-class law as the tracked one.
///
/// A current [`PolicyDocument`] is deserialized directly rather than parsed
/// from tracked fields, so without this an unusable selection would compile —
/// and the whole point of requiring the field is that no door admits a
/// document selecting nothing. The refusals name the current field, because
/// that is the field the author of this document actually wrote.
#[test]
fn compiling_a_current_document_refuses_the_same_unusable_file_classes() {
    for (broken, field, reason) in [
        (
            "file_shape = [\".rs\"]",
            "file_classes.file_shape",
            "must select at least one suffix",
        ),
        (
            "complexity = [\".rs\"]",
            "file_classes.complexity",
            "must select at least one suffix",
        ),
        (
            "magic = [\".rs\"]",
            "file_classes.magic",
            "must select at least one suffix",
        ),
    ] {
        let text = EVERY_SCOPE.replacen(broken, &broken.replace("[\".rs\"]", "[]"), 1);
        let document = toml::from_str(&text).expect("an empty file class still decodes");
        let error = RulePolicy::compile(document).expect_err("an empty file class must refuse");
        assert_eq!(error.field(), field, "{error}");
        assert!(error.reason().contains(reason), "{error}");
    }

    let text = EVERY_SCOPE.replacen("magic = [\".rs\"]", "magic = [\"rs\"]", 1);
    let document = toml::from_str(&text).expect("a period-less suffix still decodes");
    let error = RulePolicy::compile(document).expect_err("a period-less suffix must refuse");
    assert_eq!(error.field(), "file_classes.magic", "{error}");
    assert!(
        error
            .reason()
            .contains("\"rs\": must be a file suffix beginning with a period"),
        "{error}"
    );
}

fn current_policy(text: &str) -> RulePolicy {
    let document = toml::from_str(text).expect("current policy decodes");
    RulePolicy::compile(document).expect("current policy compiles")
}

fn resolve_doc(policy: &RulePolicy, path: &str) -> gate::ResolvedBound {
    policy
        .resolve_bound(
            BoundName::RepoTruthDocChars,
            &PolicyContext::new().with_path(path),
        )
        .expect("document bound resolves")
}
