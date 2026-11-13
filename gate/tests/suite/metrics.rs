use gate::battery::{ArmEvidence, ArmExecution, BatteryState};
use gate::metrics::{
    EvidenceState, LatchBinding, LatchLimits, LatchOutcome, MagicFinding, MarkdownAnchorInventory,
    MetricEvidence, MetricFinding, MetricId, MetricKey, MetricMeasurement, MetricReport,
    MetricStateBinding, MetricUnit, ParseError, ParserEvidence, ParserId,
    RUST_DOC_MARKDOWN_CITATION_ALLOWANCES, ResolvedMetricRule, RuleBinding,
    RustDocMarkdownCitationScan, STRUCTURAL_ARMS, STRUCTURAL_METRICS, StructuralArmEvidence,
    measure_complexity, measure_file_shape, measure_repository_document, resolve_magic_metric,
    resolve_metric_rule, scan_magic_regressions, scan_markdown_links,
    scan_rust_doc_markdown_citations,
};
use gate::{GateId, PolicyContext, RulePolicy};

const EXISTING_MAGIC_VALUE: u64 = 75;
pub(super) const FIXTURE_BASE_LIMIT: u64 = 1_000;
const FIXTURE_RATCHET_LIMIT: u64 = 1_500;
const LINES_LIMIT: u64 = 1_525;
const OVER_LINES: u64 = 1_530;
const NEW_MAGIC_VALUE: u64 = 100;
const PYTHON_MAGIC_VALUE: u64 = 50;

const SCOPED_POLICY: &str = r#"
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
[rules.bounds.routine_ccn]
multiplier = 2.0
ratchet = 2.0
[rules.bounds.routine_length]
multiplier = 2.0
ratchet = 2.0

[[rules]]
id = "src-strict"
source_order = 1
priority = 1
scopes = { paths = ["src/strict/**"] }
magic = { examine_threshold = 100 }
[rules.source]
location = "current.toml"
field = "rules.src-strict"
kind = "current"

[[rules]]
id = "upper-docs"
source_order = 2
priority = 1
scopes = { extensions = [".mdoc"] }
stem_pattern = "[A-Z_]+"
[rules.source]
location = "current.toml"
field = "rules.upper-docs"
kind = "current"
[rules.bounds.repo_truth_doc_chars]
unlimited = true
"#;

pub(super) fn scoped_policy() -> RulePolicy {
    let document = toml::from_str(SCOPED_POLICY).expect("scoped policy decodes");
    RulePolicy::compile(document).expect("scoped policy compiles")
}

#[test]
fn declarations_freeze_ids_units_parsers_scopes_and_order() {
    assert_eq!(
        STRUCTURAL_ARMS.map(|arm| arm.id()),
        [
            GateId::FileShape,
            GateId::Complexity,
            GateId::MagicNumbers,
            GateId::MarkdownLinks,
        ]
    );
    assert_eq!(STRUCTURAL_METRICS.map(|metric| metric.id()), MetricId::ALL);
    assert_eq!(STRUCTURAL_METRICS[0].unit(), MetricUnit::SourceLines);
    assert_eq!(
        STRUCTURAL_METRICS[2].key(),
        gate::metrics::KeyShape::PathFunction
    );
    assert_eq!(
        STRUCTURAL_METRICS[4].owner(),
        GateId::RepoDocs,
        "repo-doc owns document-character enforcement"
    );
    let complexity = STRUCTURAL_ARMS[1];
    assert_eq!(
        complexity.parser().id(),
        ParserId::TreeSitterComplexityV1,
        "complexity is measured by the vendored grammars, not an external tool"
    );
    assert!(
        complexity
            .scope()
            .exclusions()
            .contains(&"source classes with no vendored grammar"),
        "a selected class this build cannot parse is excluded by name"
    );
    assert!(
        STRUCTURAL_ARMS[0]
            .scope()
            .exclusions()
            .contains(&"repository-truth documents")
    );
    let markdown = STRUCTURAL_ARMS[3];
    assert_eq!(markdown.parser().id(), ParserId::MarkdownTrackedTargetsV1);
    assert_eq!(
        markdown.metrics(),
        [
            MetricId::MarkdownLinkInvalidTargets,
            MetricId::RustDocMarkdownTargetAbsences,
        ]
    );
    let doc_targets = STRUCTURAL_METRICS[7];
    assert_eq!(doc_targets.unit(), MetricUnit::TargetAbsences);
    assert_eq!(doc_targets.parser(), ParserId::MarkdownTrackedTargetsV1);
    assert_eq!(doc_targets.owner(), GateId::MarkdownLinks);
}

/// Both doors bind a measurement to the parser its own arm declares.
///
/// The two metrics are crossed rather than each tested against a made-up parser
/// identity, because a wrong identity that names no arm at all would be refused
/// by anything that checks the field is populated. Naming the *other* arm's real
/// parser is what makes the check specific to this metric's declaration.
#[test]
fn measured_parser_evidence_binds_the_parser_its_arm_declares() {
    let policy = scoped_policy();
    let complexity_rule = resolve_metric_rule(
        &policy,
        MetricId::RoutineCcn,
        &PolicyContext::new().with_path("src/app.rs"),
    )
    .expect("complexity rule resolves");
    let complexity_key = MetricKey::function("src/app.rs", "run").unwrap();
    assert_parser_refused_by_both(
        MetricId::RoutineCcn,
        complexity_key.clone(),
        5,
        "routine_ccn",
        ParserEvidence::Native {
            parser: ParserId::Utf8FileShapeV1,
        },
        &complexity_rule,
    );

    let file_rule = resolve_metric_rule(
        &policy,
        MetricId::FileLines,
        &PolicyContext::new().with_path("src/app.rs"),
    )
    .expect("file rule resolves");
    assert_parser_refused_by_both(
        MetricId::FileLines,
        MetricKey::path("src/app.rs").unwrap(),
        12,
        "file_loc",
        ParserEvidence::Native {
            parser: ParserId::TreeSitterComplexityV1,
        },
        &file_rule,
    );

    let declared = ParserEvidence::Native {
        parser: ParserId::TreeSitterComplexityV1,
    };
    MetricEvidence::new(
        MetricId::RoutineCcn,
        complexity_key.clone(),
        EvidenceState::measured(bounded_measurement_with_parser(
            MetricId::RoutineCcn,
            5,
            "routine_ccn",
            declared.clone(),
        )),
    )
    .expect("the declared complexity parser is measured");
    MetricStateBinding::new(
        MetricId::RoutineCcn,
        complexity_key,
        5,
        declared,
        complexity_rule,
    )
    .expect("the declared complexity parser enters state binding");
}

#[test]
fn native_file_and_document_parsers_count_lines_bytes_and_characters() {
    let source = "alpha\r\nbeta\u{2028}gamma\n".as_bytes();
    let shape = measure_file_shape("src/app.rs", source).expect("UTF-8 source measures");
    assert_eq!(shape.path(), "src/app.rs");
    assert_eq!(shape.lines(), 3);
    assert_eq!(shape.bytes(), u64::try_from(source.len()).unwrap());
    for separator in [
        "\n", "\r", "\u{000b}", "\u{000c}", "\u{001c}", "\u{001d}", "\u{001e}", "\u{0085}",
        "\u{2028}", "\u{2029}",
    ] {
        let text = format!("left{separator}right{separator}");
        assert_eq!(
            measure_file_shape("src/separators.rs", text.as_bytes())
                .expect("every Python line separator measures")
                .lines(),
            2,
            "separator {separator:?}"
        );
    }
    let invalid = measure_file_shape("src/invalid.rs", b"a\xffb\nc")
        .expect_err("invalid UTF-8 source blobs are refused");
    assert!(matches!(
        invalid,
        ParseError::Unsupported {
            ref source_field,
            ..
        } if source_field == "policy.file_shape.source_suffixes"
    ));
    assert!(matches!(
        invalid.into_evidence_state(),
        EvidenceState::Unsupported { source_field, .. }
            if source_field == "policy.file_shape.source_suffixes"
    ));
    assert_eq!(
        measure_file_shape("src/empty.rs", b"")
            .expect("empty text is measured, not omitted")
            .lines(),
        0
    );

    let document = measure_repository_document("README.md", b"a\xffb")
        .expect("invalid UTF-8 is decoded with replacement");
    assert_eq!(document.characters(), 3);
    assert!(matches!(
        measure_file_shape("src/image.rs", b"a\0b"),
        Err(ParseError::Unsupported { source_field, .. })
            if source_field == "policy.file_shape.source_suffixes"
    ));
    assert!(matches!(
        measure_repository_document("README.md", b"a\0b"),
        Err(ParseError::Unsupported { source_field, .. })
            if source_field == "policy.repo_truth.docs"
    ));
}

/// Two Rust routines that differ in every measured quantity.
///
/// Both are named `new`, which is what makes the qualified identity load-bearing
/// rather than decorative: measured by bare name they would collide, and this
/// pair is ordinary Rust rather than a contrivance built to trip the parser.
const COMPLEXITY_RUST: &str = "\
impl Alpha {
    fn new(flag: bool) -> Self {
        if flag && flag {
            return Self;
        }
        Self
    }
}

impl Beta {
    fn new() -> Self {
        Self
    }
}
";

/// One Python routine, proving the table answers for more than the host grammar.
const COMPLEXITY_PYTHON: &str = "\
class Runner:
    def run(self, items):
        for item in items:
            if item:
                return item
        return None
";

/// The same three decision forms in each remaining vendored grammar.
const COMPLEXITY_JAVASCRIPT: &str = "\
class Runner {
    run(items) {
        for (const item of items) {
            if (item && item.ready) {
                return item;
            }
        }
    }
}
";

const COMPLEXITY_CSHARP: &str = "\
class Runner {
    int Run(int[] items) {
        foreach (var item in items) {
            if (item > 0 && item < items.Length) {
                return item;
            }
        }
        return 0;
    }
}
";

const COMPLEXITY_LUA: &str = "\
function run(items)
    for _, item in ipairs(items) do
        if item and item.ready then
            return item
        end
    end
end
";

#[test]
fn complexity_measures_each_routine_rather_than_reporting_one_shape_for_all() {
    let rust = measure_complexity("src/app.rs", COMPLEXITY_RUST).expect("Rust source measures");
    let measured = rust
        .iter()
        .map(|routine| (routine.function(), routine.ccn(), routine.length()))
        .collect::<Vec<_>>();
    // The pair is asserted whole so a parser that answered with one constant --
    // every routine complexity 1, or every routine the same length -- cannot
    // satisfy it. Both quantities differ across these two routines, and both
    // names differ only in the scope that qualifies them.
    assert_eq!(
        measured,
        [("Alpha::new", 3, 6), ("Beta::new", 1, 3)],
        "each routine carries its own decisions and its own span"
    );
    for routine in &rust {
        assert_eq!(routine.path(), "src/app.rs");
    }

    let python =
        measure_complexity("src/app.py", COMPLEXITY_PYTHON).expect("Python source measures");
    assert_eq!(
        python
            .iter()
            .map(|routine| (routine.function(), routine.ccn(), routine.length()))
            .collect::<Vec<_>>(),
        [("Runner::run", 3, 5)],
        "a second vendored grammar measures by the same rules"
    );

    for (path, source, expected) in [
        ("src/app.js", COMPLEXITY_JAVASCRIPT, ("Runner::run", 4, 7)),
        ("src/App.cs", COMPLEXITY_CSHARP, ("Runner::Run", 4, 8)),
        ("src/app.lua", COMPLEXITY_LUA, ("run", 4, 7)),
    ] {
        let routines = measure_complexity(path, source).expect("the vendored grammar measures");
        assert_eq!(
            routines
                .iter()
                .map(|routine| (routine.function(), routine.ccn(), routine.length()))
                .collect::<Vec<_>>(),
            [expected],
            "{path} measures a real routine through its vendored grammar"
        );
    }

    let unsupported = measure_complexity("src/app.c", "").expect_err(
        "a selected class with no vendored grammar is named rather than measured empty",
    );
    assert!(matches!(
        unsupported,
        ParseError::Unsupported {
            ref source_field,
            ref reason,
            ..
        } if source_field == "policy.languages.complexity" && reason.contains(".c")
    ));

    let malformed = measure_complexity("src/broken.rs", "fn broken( { if true {}")
        .expect_err("an error-containing parse tree is refused rather than partially measured");
    assert!(matches!(
        malformed,
        ParseError::Malformed { ref input, .. } if input.contains("src/broken.rs")
    ));

    let trait_impls = measure_complexity(
        "src/traits.rs",
        "struct Worker;\n\
         impl First for Worker { fn run(&self) {} }\n\
         impl Second for Worker { fn run(&self) {} }\n",
    )
    .expect("two trait impl methods on one type keep distinct qualified identities");
    assert_eq!(
        trait_impls
            .iter()
            .map(|routine| routine.function())
            .collect::<Vec<_>>(),
        ["<Worker as First>::run", "<Worker as Second>::run"]
    );

    let configured = measure_complexity(
        "src/configured.rs",
        "#[cfg(unix)]\n\
         impl Worker { fn selected() { if true {} } }\n\
         #[cfg(not(unix))]\n\
         impl Worker { fn selected() {} }\n",
    )
    .expect("only the compiled target's Rust declaration exists");
    assert_eq!(configured.len(), 1);
    assert_eq!(configured[0].function(), "Worker::selected");
    assert_eq!(configured[0].ccn(), if cfg!(unix) { 2 } else { 1 });

    let unknown_configuration = measure_complexity(
        "src/uncertain.rs",
        "#[cfg(feature = \"first\")] fn selected() {}\n\
         #[cfg(feature = \"second\")] fn selected() {}\n",
    )
    .expect_err("unknown source features cannot silently select one routine");
    assert!(matches!(
        unknown_configuration,
        ParseError::Collision { .. }
    ));

    let collision = measure_complexity(
        "src/dup.rs",
        "impl Alpha {\n    fn new() {}\n    fn new() {}\n}\n",
    )
    .expect_err("two routines that qualify to one identity are refused");
    assert!(matches!(collision, ParseError::Collision { .. }));
}

#[test]
fn magic_delta_matches_supported_comparisons_and_refuses_ast_substitution() {
    let baseline = format!("if (value > {EXISTING_MAGIC_VALUE}) {{}}\n");
    let current = format!(
        concat!(
            "if (value > {existing}) {{}}\n",
            "if (count == 5) {{}}\n",
            "let named = 99;\n",
            "if (size <= {new}) {{}}\n",
            "// if (commented > 500) {{}}\n",
            "let markup = `if (copy > 600) {{}}`;\n",
        ),
        existing = EXISTING_MAGIC_VALUE,
        new = NEW_MAGIC_VALUE
    );
    let findings = scan_magic_regressions("src/app.rs", &current, Some(&baseline), 10)
        .expect("C-grammar source scans");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].line(), 4);
    assert_eq!(findings[0].literal(), "100");
    let separated = scan_magic_regressions(
        "src/app.rs",
        &format!("ignored\u{2028}x > {NEW_MAGIC_VALUE}"),
        None,
        10,
    )
    .expect("Unicode line separators advance magic-number line numbers");
    assert_eq!(separated[0].line(), 2);
    let unclosed_comment =
        scan_magic_regressions("src/app.rs", &format!("/* x > {NEW_MAGIC_VALUE}"), None, 10)
            .expect("an unclosed block-comment marker is not a comment");
    assert_eq!(unclosed_comment[0].literal(), "100");
    let pair = format!("{EXISTING_MAGIC_VALUE} < {NEW_MAGIC_VALUE}");
    let pair = scan_magic_regressions("src/app.rs", &pair, None, 10)
        .expect("one comparison is matched once");
    assert_eq!(
        pair.iter().map(MagicFinding::literal).collect::<Vec<_>>(),
        [EXISTING_MAGIC_VALUE.to_string()]
    );
    let chain = format!("{PYTHON_MAGIC_VALUE} < {EXISTING_MAGIC_VALUE} < {NEW_MAGIC_VALUE}");
    let chain = scan_magic_regressions("src/app.rs", &chain, None, 10)
        .expect("comparison chains match without overlap");
    assert_eq!(
        chain.iter().map(MagicFinding::literal).collect::<Vec<_>>(),
        [
            PYTHON_MAGIC_VALUE.to_string(),
            EXISTING_MAGIC_VALUE.to_string()
        ]
    );
    let consumed = format!("5 < {NEW_MAGIC_VALUE}");
    assert!(
        scan_magic_regressions("src/app.rs", &consumed, None, 10)
            .expect("a below-threshold match still consumes its comparison")
            .is_empty()
    );

    assert!(matches!(
        scan_magic_regressions(
            "src/app.py",
            &format!("if x > {PYTHON_MAGIC_VALUE}: pass"),
            None,
            10
        ),
        Err(ParseError::Unsupported { source_field, .. })
            if source_field == "policy.languages.magic"
    ));
    assert!(matches!(
        scan_magic_regressions(
            "src/app.txt",
            &format!("x > {PYTHON_MAGIC_VALUE}"),
            None,
            10
        ),
        Err(ParseError::NotApplicable { .. })
    ));
}

#[test]
fn markdown_links_use_projected_inventory_and_exact_ascii_case() {
    let tracked = vec![
        "README.md".to_owned(),
        "Docs/Guide.md".to_owned(),
        "images/logo.png".to_owned(),
    ];
    let text = concat!(
        "# Local\n",
        "[Guide](docs/guide.md)\n",
        "[External](https://example.com)\n",
        "[Anchor](#local)\n",
    );
    let mut anchors = MarkdownAnchorInventory::default();
    anchors
        .insert("README.md", text)
        .expect("source anchors index");
    anchors
        .insert("Docs/Guide.md", "# Guide")
        .expect("target anchors index");
    let findings =
        scan_markdown_links("README.md", text, &tracked, &anchors).expect("tracked Markdown scans");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].resolved_path(), "docs/guide.md");
    assert_eq!(findings[0].expected(), "Docs/Guide.md");
    let separated = scan_markdown_links(
        "README.md",
        "plain\u{2028}[Guide](docs/guide.md)",
        &tracked,
        &anchors,
    )
    .expect("Unicode line separators advance link line numbers");
    assert_eq!(separated[0].line(), 2);

    assert!(matches!(
        scan_markdown_links("README.md", "[x](Döcs.md)", &tracked, &anchors),
        Err(ParseError::Unsupported { source_field, .. })
            if source_field == "markdown link target"
    ));
    let collision = vec!["Docs/Guide.md".to_owned(), "docs/guide.md".to_owned()];
    assert!(matches!(
        scan_markdown_links("README.md", "", &collision, &anchors),
        Err(ParseError::Collision { .. })
    ));
}

#[test]
fn markdown_links_resolve_heading_slugs_duplicate_suffixes_and_explicit_anchors() {
    let tracked = vec!["README.md".to_owned(), "Docs/Guide.md".to_owned()];
    let source = concat!(
        "# Local section\n",
        "[Local](#local-section)\n",
        "[Manual](Docs/Guide.md#manual)\n",
        "[Repeated](Docs/Guide.md#repeated-1)\n",
        "[Missing](Docs/Guide.md#gone)\n",
        "[Manual reference][manual-anchor]\n",
        "[manual-anchor]: Docs/Guide.md#manual\n",
        "[Punctuation](Docs/Guide.md#l-lane--the-place)\n",
        "[Symbols kept](Docs/Guide.md#draft-→-review-🚀)\n",
        "[Symbols dropped](Docs/Guide.md#draft--review-)\n",
    );
    let target = concat!(
        "# Guide\n",
        "<a id=\"manual\"></a>\n",
        "## Repeated\n",
        "## Repeated\n",
        "## L-LANE — the place\n",
        "## Draft → review 🚀\n",
    );
    let mut anchors = MarkdownAnchorInventory::default();
    anchors
        .insert("README.md", source)
        .expect("source anchors index");
    anchors
        .insert("Docs/Guide.md", target)
        .expect("target anchors index");

    let findings = scan_markdown_links("README.md", source, &tracked, &anchors)
        .expect("tracked Markdown scans");
    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0].line(), 5);
    assert_eq!(findings[0].fragment(), Some("gone"));
    assert_eq!(findings[0].raw_target(), "Docs/Guide.md#gone");
    assert_eq!(findings[1].line(), 9);
    assert_eq!(findings[1].fragment(), Some("draft-→-review-🚀"));
}

#[test]
fn rust_doc_markdown_citations_use_live_inventory_and_refuse_the_unfixed_arm() {
    let tracked = vec!["README.md".to_owned()];
    let unfixed = scan_rust_doc_markdown_citations(
        "proof/src/config.rs",
        "/// Compiled defaults were documented by `CONFIG.md`.",
        &tracked,
    )
    .expect("unfixed Rust doc comment scans");
    assert_eq!(unfixed.citations(), 1);
    assert_eq!(unfixed.findings().len(), 1);
    assert_eq!(unfixed.findings()[0].source_path(), "proof/src/config.rs");
    assert_eq!(unfixed.findings()[0].line(), 1);
    assert_eq!(unfixed.findings()[0].target_path(), "CONFIG.md");
    assert_eq!(rust_doc_citation_arm_state(&unfixed), BatteryState::Failed);

    let fixed = scan_rust_doc_markdown_citations(
        "proof/src/config.rs",
        "/** The current contract is summarized by `README.md`. */",
        &tracked,
    )
    .expect("fixed Rust doc comment scans");
    assert_eq!(fixed.citations(), 1);
    assert!(fixed.findings().is_empty());
    assert_eq!(rust_doc_citation_arm_state(&fixed), BatteryState::Passed);

    let live_target = "docs/LIVE-INVENTORY-PROBE.md".to_owned();
    let text = "//! The live contract is `docs/LIVE-INVENTORY-PROBE.md`.";
    let absent = scan_rust_doc_markdown_citations("src/lib.rs", text, &tracked)
        .expect("an absent live target scans");
    assert_eq!(absent.findings().len(), 1);
    let mut expanded = tracked;
    expanded.push(live_target);
    let present = scan_rust_doc_markdown_citations("src/lib.rs", text, &expanded)
        .expect("the same live target scans after inventory movement");
    assert!(
        present.findings().is_empty(),
        "the parser restated a document list instead of reading the supplied tracked inventory"
    );

    let ordinary = scan_rust_doc_markdown_citations(
        "src/lib.rs",
        r####"
        // `MISSING.md` is an ordinary comment, not a doc citation.
        //// `ALSO-MISSING.md` is an ordinary four-slash comment.
        /*** `STILL-MISSING.md` is an ordinary block comment. */
        const NAME: &str = "TRACKED.md";
        const MULTILINE: &str = r#"
        /// `STRING-CONTENT.md` remains string content on its own source line.
        "#;
        "####,
        &[],
    )
    .expect("ordinary comments and strings scan");
    assert_eq!(ordinary.citations(), 0);
    assert!(ordinary.findings().is_empty());
}

/// A prose citation is judged, and a longer suffix is not one.
///
/// The first case carries no code span: a documentation citation can be
/// ordinary prose, so requiring backticks would admit a missing target that a
/// reader still expects to resolve. Reading the target back proves the refusal
/// names the document it could not resolve, not merely that something was
/// wrong.
///
/// The second case pins the match boundary. Without a closing backtick to
/// delimit it, a Markdown name sits inside every longer suffix, and a pattern
/// that stopped there would read one out of `page.mdoc` and then refuse the
/// file for not tracking a document nobody wrote.
#[test]
fn a_prose_citation_is_judged_and_a_longer_suffix_is_not_one() {
    let tracked = vec!["README.md".to_owned()];

    let prose = scan_rust_doc_markdown_citations(
        "gate/tests/markdown_refs.rs",
        "//! MISSING.md documents this rule in ordinary prose.\n",
        &tracked,
    )
    .expect("a prose citation scans");
    assert_eq!(prose.citations(), 1);
    assert_eq!(prose.findings().len(), 1);
    assert_eq!(
        prose.findings()[0].source_path(),
        "gate/tests/markdown_refs.rs"
    );
    assert_eq!(prose.findings()[0].line(), 1);
    assert_eq!(
        prose.findings()[0].target_path(),
        "MISSING.md",
        "the refusal has to name the document it could not resolve"
    );
    assert_eq!(rust_doc_citation_arm_state(&prose), BatteryState::Failed);

    let resolved = scan_rust_doc_markdown_citations(
        "src/lib.rs",
        "//! The contract this states is README.md's, carried without a code span.\n",
        &tracked,
    )
    .expect("a resolved prose citation scans");
    assert_eq!(resolved.citations(), 1);
    assert!(resolved.findings().is_empty());

    let suffix = scan_rust_doc_markdown_citations(
        "src/lib.rs",
        "//! The reference/page.mdoc bundle is not a Markdown document.\n",
        &tracked,
    )
    .expect("a longer suffix scans");
    assert_eq!(
        suffix.citations(),
        0,
        "a longer suffix carries no citation, so nothing may be read out of it"
    );
}

#[test]
fn rust_doc_markdown_allowances_admit_their_fixture_citation_only_at_its_source() {
    for allowance in RUST_DOC_MARKDOWN_CITATION_ALLOWANCES {
        let target = allowance.target_path();
        let text = format!("/// The fixture names `{target}`.");
        let admitted = scan_rust_doc_markdown_citations(allowance.source_path(), &text, &[])
            .expect("declared citation scans");
        assert_eq!(admitted.citations(), 1);
        assert!(admitted.findings().is_empty());
    }

    for allowance in RUST_DOC_MARKDOWN_CITATION_ALLOWANCES {
        let target = allowance.target_path();
        let text = format!("/// An undeclared source names `{target}`.");
        let refused = scan_rust_doc_markdown_citations("src/lib.rs", &text, &[])
            .expect("source-bound fixture citation scans elsewhere");
        assert_eq!(refused.findings().len(), 1);
    }
}

fn rust_doc_citation_arm_state(scan: &RustDocMarkdownCitationScan) -> BatteryState {
    let metrics = vec![
        MetricEvidence::new(
            MetricId::MarkdownLinkInvalidTargets,
            MetricKey::Arm,
            EvidenceState::NotApplicable {
                reason: "fixture contains no tracked Markdown source".to_owned(),
            },
        )
        .expect("case metric fixture evidence"),
        MetricEvidence::new(
            MetricId::RustDocMarkdownTargetAbsences,
            MetricKey::path("proof/src/config.rs").expect("citation metric path"),
            EvidenceState::measured(
                MetricMeasurement::new(
                    scan.findings().len() as u64,
                    ParserEvidence::Native {
                        parser: ParserId::MarkdownTrackedTargetsV1,
                    },
                )
                .with_findings(
                    scan.findings()
                        .iter()
                        .map(|finding| MetricFinding {
                            path: finding.source_path().to_owned(),
                            line: finding.line(),
                            target: finding.target_path().to_owned(),
                            expected: None,
                        })
                        .collect(),
                ),
            ),
        )
        .expect("citation metric fixture evidence"),
    ];
    ArmEvidence::Metrics {
        report: StructuralArmEvidence::new(GateId::MarkdownLinks, metrics)
            .expect("complete Markdown arm fixture"),
    }
    .state()
    .expect("Markdown arm state")
}

#[test]
fn policy_binding_preserves_scoped_rules_magic_and_stem_patterns() {
    let policy = scoped_policy();
    let file = resolve_metric_rule(
        &policy,
        MetricId::FileLines,
        &PolicyContext::new().with_path("src/app.rs"),
    )
    .expect("scoped file rule resolves");
    assert_eq!((file.base_limit(), file.ratchet_limit()), (20, 40));
    assert!(file.binding().rule_id().is_some());
    assert!(file.binding().match_basis().contains("src/**"));

    let magic = resolve_magic_metric(
        &policy,
        &PolicyContext::new().with_path("src/strict/app.rs"),
    )
    .expect("scoped magic rule resolves");
    assert_eq!(magic.threshold(), 100);

    let upper = resolve_metric_rule(
        &policy,
        MetricId::RepositoryDocumentCharacters,
        &PolicyContext::new().with_path("docs/DOCS_MAIN.mdoc"),
    )
    .expect("stem-pattern rule resolves");
    assert!(upper.unlimited());
    assert!(
        upper
            .binding()
            .match_basis()
            .contains("dialect=rust-regex-1")
    );

    let wrong_stem = resolve_metric_rule(
        &policy,
        MetricId::RepositoryDocumentCharacters,
        &PolicyContext::new().with_path("docs/guide.mdoc"),
    )
    .expect("nonmatching stem uses global policy");
    assert_eq!(wrong_stem.base_limit(), 1000);
    assert!(wrong_stem.binding().rule_id().is_none());
}

#[test]
fn missing_rule_context_refuses_instead_of_selecting_a_global_default() {
    let policy = scoped_policy();
    assert!(resolve_metric_rule(&policy, MetricId::FileLines, &PolicyContext::new()).is_err());
}

#[test]
fn evidence_vocabulary_is_total() {
    let states = [
        EvidenceState::Unavailable {
            reason: "selected complexity source could not be read".to_owned(),
        },
        EvidenceState::unsupported(
            "policy.languages.magic",
            "Python AST classification is not compiled",
            "remove .py or compile an exact parser",
        ),
        EvidenceState::Unmeasured {
            reason: "selected bytes were not acquired".to_owned(),
        },
        EvidenceState::Timeout {
            milliseconds: 30_000,
            reason: "bounded structural scan exceeded runtime".to_owned(),
        },
        EvidenceState::Failed {
            reason: "structural acquisition ended without complete evidence".to_owned(),
            exit_code: Some(2),
        },
        EvidenceState::NotApplicable {
            reason: "no tracked Markdown sources".to_owned(),
        },
    ];
    for metric in MetricId::ALL {
        for (index, state) in states.iter().cloned().enumerate() {
            let row = MetricEvidence::new(metric, MetricKey::Arm, state)
                .expect("every metric admits every explicit nonmeasurement state");
            assert_eq!(row.state().is_skip(), index == 5);
        }
        let row = measured_evidence(metric);
        assert!(matches!(row.state(), EvidenceState::Measured { .. }));
    }
}

#[test]
fn reports_require_every_arm_metric_and_paired_scope_key() {
    let arms = vec![
        not_applicable_arm(GateId::MarkdownLinks),
        not_applicable_arm(GateId::FileShape),
        not_applicable_arm(GateId::MagicNumbers),
        not_applicable_arm(GateId::Complexity),
    ];
    let documents = vec![
        MetricEvidence::new(
            MetricId::RepositoryDocumentCharacters,
            MetricKey::Arm,
            EvidenceState::NotApplicable {
                reason: "no repository-truth documents".to_owned(),
            },
        )
        .unwrap(),
    ];
    let report = MetricReport::new(arms, documents).expect("complete report admits");
    assert_eq!(
        report
            .arms()
            .iter()
            .map(|arm| arm.gate_id())
            .collect::<Vec<_>>(),
        STRUCTURAL_ARMS.map(|arm| arm.id())
    );
    assert_eq!(report.canonical_json(), report.canonical_json());

    let line = MetricEvidence::new(
        MetricId::FileLines,
        MetricKey::path("src/a.rs").unwrap(),
        EvidenceState::measured(bounded_measurement(MetricId::FileLines, 12, "file_loc")),
    )
    .unwrap();
    let bytes = MetricEvidence::new(
        MetricId::FileBytes,
        MetricKey::path("src/b.rs").unwrap(),
        EvidenceState::measured(bounded_measurement(MetricId::FileBytes, 100, "file_bytes")),
    )
    .unwrap();
    assert!(StructuralArmEvidence::new(GateId::FileShape, vec![line, bytes]).is_err());
}

/// A recorded arm execution carries exactly the reason its own evidence
/// recomputes, so a retained report cannot substitute another explanation.
#[test]
fn an_arm_execution_admits_only_the_reason_its_evidence_recomputes() {
    let lines = file_row(
        MetricId::FileLines,
        "src/app.rs",
        OVER_LINES,
        Some(LINES_LIMIT),
        None,
    );
    let bytes = file_row(MetricId::FileBytes, "src/app.rs", 900, None, None);
    let arm = ArmEvidence::Metrics {
        report: StructuralArmEvidence::new(GateId::FileShape, vec![bytes, lines])
            .expect("paired file-shape rows admit"),
    };
    let reason = arm.reason().expect("file-shape reason");

    assert_eq!(arm.state().expect("file-shape state"), BatteryState::Failed);
    ArmExecution::new(BatteryState::Failed, reason.as_str(), None, arm.clone())
        .expect("the recomputed reason validates");
    assert!(
        ArmExecution::new(
            BatteryState::Failed,
            format!("{reason}; edited").as_str(),
            None,
            arm
        )
        .is_err(),
        "a reason the evidence does not recompute is refused"
    );
}

/// A breach a pending sweep task holds informs and passes, naming the task;
/// the same breach unheld refuses, as it does once the task settles.
#[test]
fn a_sweep_held_breach_passes_naming_its_task() {
    let task: wire::TaskHandle = "GATE-dXcL8gr1q".parse().expect("task handle");
    let judge = |cover: Option<wire::TaskHandle>| {
        let arm = ArmEvidence::Metrics {
            report: StructuralArmEvidence::new(
                GateId::FileShape,
                vec![
                    file_row(MetricId::FileBytes, "src/app.rs", 900, None, None),
                    file_row(
                        MetricId::FileLines,
                        "src/app.rs",
                        OVER_LINES,
                        Some(LINES_LIMIT),
                        cover,
                    ),
                ],
            )
            .expect("paired file-shape rows admit"),
        };
        (
            arm.state().expect("file-shape state"),
            arm.reason().expect("file-shape reason"),
        )
    };

    let (held, reason) = judge(Some(task.clone()));
    assert_eq!(held, BatteryState::Passed);
    assert!(
        reason.contains(task.as_str()),
        "the notice names the task: {reason}"
    );
    assert_eq!(judge(None).0, BatteryState::Failed);
}

fn not_applicable_arm(gate: GateId) -> StructuralArmEvidence {
    let declaration = STRUCTURAL_ARMS
        .into_iter()
        .find(|arm| arm.id() == gate)
        .unwrap();
    let evidence = declaration
        .metrics()
        .iter()
        .map(|metric| {
            MetricEvidence::new(
                *metric,
                MetricKey::Arm,
                EvidenceState::NotApplicable {
                    reason: "declared scope has no selected input".to_owned(),
                },
            )
            .unwrap()
        })
        .collect();
    StructuralArmEvidence::new(gate, evidence).unwrap()
}

fn bounded_measurement(metric: MetricId, value: u64, field: &str) -> MetricMeasurement {
    let parser = match metric {
        MetricId::FileLines | MetricId::FileBytes => ParserId::Utf8FileShapeV1,
        MetricId::RoutineCcn | MetricId::RoutineLength => ParserId::TreeSitterComplexityV1,
        MetricId::RepositoryDocumentCharacters => ParserId::Utf8DocumentCharactersV1,
        _ => unreachable!("fixture helper only builds bounded measurements"),
    };
    bounded_measurement_with_parser(metric, value, field, ParserEvidence::Native { parser })
}

fn bounded_measurement_with_parser(
    metric: MetricId,
    value: u64,
    field: &str,
    parser: ParserEvidence,
) -> MetricMeasurement {
    let kind = match metric {
        MetricId::FileLines => "file_loc",
        MetricId::FileBytes => "file_bytes",
        MetricId::RoutineCcn => "complexity_ccn",
        MetricId::RoutineLength => "complexity_length",
        MetricId::RepositoryDocumentCharacters => "repo_doc_chars",
        _ => unreachable!("fixture helper only builds bounded measurements"),
    };
    MetricMeasurement::new(value, parser)
        .with_rule(
            RuleBinding::new(
                field,
                None::<String>,
                "compiled policy globals",
                "no named rule selected",
            )
            .unwrap(),
        )
        .with_latch(
            LatchBinding::new(
                kind,
                LatchLimits {
                    base: FIXTURE_BASE_LIMIT,
                    ratchet: FIXTURE_RATCHET_LIMIT,
                    active: FIXTURE_RATCHET_LIMIT,
                    unlimited: false,
                },
                LatchOutcome {
                    over_limit: false,
                    over_base: false,
                    ratchet_breach: false,
                },
                "clear",
            )
            .unwrap(),
        )
}

/// One file-shape row: latched past `limit` when given, else clear and in
/// bound; a latched row may be held by a sweep task.
fn file_row(
    metric: MetricId,
    path: &str,
    value: u64,
    limit: Option<u64>,
    cover: Option<wire::TaskHandle>,
) -> MetricEvidence {
    let kind = match metric {
        MetricId::FileLines => "file_loc",
        MetricId::FileBytes => "file_bytes",
        _ => unreachable!("fixture helper only builds file-shape rows"),
    };
    let mut measurement = bounded_measurement(metric, value, kind);
    if let Some(limit) = limit {
        assert!(value > limit, "a latched fixture measures past its limit");
        let latch = LatchBinding::new(
            kind,
            LatchLimits {
                base: FIXTURE_BASE_LIMIT,
                ratchet: limit,
                active: limit,
                unlimited: false,
            },
            LatchOutcome {
                over_limit: true,
                over_base: true,
                ratchet_breach: true,
            },
            "latched",
        );
        measurement = measurement.with_latch(latch.unwrap().with_cover(cover).unwrap());
    }
    let key = MetricKey::path(path).unwrap();
    MetricEvidence::new(metric, key, EvidenceState::measured(measurement)).unwrap()
}

fn assert_parser_refused_by_both(
    metric: MetricId,
    key: MetricKey,
    value: u64,
    field: &str,
    parser: ParserEvidence,
    rule: &ResolvedMetricRule,
) {
    MetricEvidence::new(
        metric,
        key.clone(),
        EvidenceState::measured(bounded_measurement_with_parser(
            metric,
            value,
            field,
            parser.clone(),
        )),
    )
    .expect_err("mismatched parser evidence cannot become metric evidence");

    MetricStateBinding::new(metric, key, value, parser, rule.clone())
        .expect_err("mismatched parser evidence cannot enter state binding");
}

fn measured_evidence(metric: MetricId) -> MetricEvidence {
    let key = match metric {
        MetricId::RoutineCcn | MetricId::RoutineLength => {
            MetricKey::function("src/app.rs", "run").unwrap()
        }
        _ => MetricKey::path("src/app.rs").unwrap(),
    };
    let measurement = match metric {
        MetricId::FileLines => bounded_measurement(metric, 12, "file_loc"),
        MetricId::FileBytes => bounded_measurement(metric, 100, "file_bytes"),
        MetricId::RoutineCcn => bounded_measurement(metric, 5, "routine_ccn"),
        MetricId::RoutineLength => bounded_measurement(metric, 17, "routine_length"),
        MetricId::RepositoryDocumentCharacters => {
            bounded_measurement(metric, 100, "repo_truth_doc_chars")
        }
        MetricId::MagicNumberRegressions => MetricMeasurement::new(
            1,
            ParserEvidence::Native {
                parser: ParserId::CGrammarMagicDeltaV1,
            },
        )
        .with_rule(
            RuleBinding::new(
                "magic.examine_threshold",
                None::<String>,
                "compiled policy globals",
                "no named rule selected",
            )
            .unwrap(),
        )
        .with_threshold(10),
        MetricId::MarkdownLinkInvalidTargets | MetricId::RustDocMarkdownTargetAbsences => {
            MetricMeasurement::new(
                1,
                ParserEvidence::Native {
                    parser: ParserId::MarkdownTrackedTargetsV1,
                },
            )
            // The one breach this fixture counts has to name itself, because a
            // zero-tolerance measurement that names nothing is not encodable.
            .with_findings(vec![MetricFinding {
                path: "docs/vocabulary.md".to_owned(),
                line: 1,
                target: "docs/Vocabulary.md".to_owned(),
                expected: Some("docs/vocabulary.md".to_owned()),
            }])
        }
    };
    MetricEvidence::new(metric, key, EvidenceState::measured(measurement)).unwrap()
}
