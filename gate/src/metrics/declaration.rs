use serde::{Deserialize, Serialize};

use crate::manifest::GateId;

/// Stable identity of one structural quantity or finding population.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricId {
    FileLines,
    FileBytes,
    RoutineCcn,
    RoutineLength,
    RepositoryDocumentCharacters,
    MagicNumberRegressions,
    MarkdownLinkInvalidTargets,
    RustDocMarkdownTargetAbsences,
}

impl MetricId {
    /// Every structural metric in deterministic declaration order.
    pub const ALL: [Self; 8] = [
        Self::FileLines,
        Self::FileBytes,
        Self::RoutineCcn,
        Self::RoutineLength,
        Self::RepositoryDocumentCharacters,
        Self::MagicNumberRegressions,
        Self::MarkdownLinkInvalidTargets,
        Self::RustDocMarkdownTargetAbsences,
    ];

    /// Stable current identity spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileLines => "file.lines",
            Self::FileBytes => "file.bytes",
            Self::RoutineCcn => "routine.ccn",
            Self::RoutineLength => "routine.length",
            Self::RepositoryDocumentCharacters => "repository-document.characters",
            Self::MagicNumberRegressions => "magic-number.regressions",
            Self::MarkdownLinkInvalidTargets => "markdown-link.invalid-targets",
            Self::RustDocMarkdownTargetAbsences => "rust-doc-markdown.target-absences",
        }
    }
}

/// Canonical unit attached to one metric identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetricUnit {
    SourceLines,
    Bytes,
    CyclomaticComplexity,
    RoutineLines,
    UnicodeScalarValues,
    Regressions,
    InvalidTargets,
    TargetAbsences,
}

impl MetricUnit {
    /// Whether a measured value is itself the finding, so only zero admits.
    ///
    /// A magnitude is bounded by a policy rule and fails by exceeding it. A
    /// violation count has no rule to exceed — the count *is* the violation — so
    /// its bound cannot be configured and is fixed at zero here.
    ///
    /// A regression count belongs to the second group even though its metric
    /// does carry a threshold, because that threshold bounds a different
    /// quantity. `magic.examine_threshold` is the magnitude at which one literal
    /// becomes worth examining at all, applied per literal while the file is
    /// scanned; by the time a count exists, every literal in it has already
    /// cleared that bar. Reading it a second time as an allowance for the count
    /// would let a file introduce as many new magic numbers as the smallest one
    /// the policy bothers to look at, which is a number chosen to answer an
    /// unrelated question.
    #[must_use]
    pub const fn admits_only_zero(self) -> bool {
        match self {
            Self::InvalidTargets | Self::Regressions | Self::TargetAbsences => true,
            Self::SourceLines
            | Self::Bytes
            | Self::CyclomaticComplexity
            | Self::RoutineLines
            | Self::UnicodeScalarValues => false,
        }
    }

    /// Stable unit spelling retained in reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceLines => "source-lines",
            Self::Bytes => "bytes",
            Self::CyclomaticComplexity => "cyclomatic-complexity",
            Self::RoutineLines => "routine-lines",
            Self::UnicodeScalarValues => "unicode-scalar-values",
            Self::Regressions => "regressions",
            Self::InvalidTargets => "invalid-targets",
            Self::TargetAbsences => "target-absences",
        }
    }
}

/// Shape of the durable state and report key used by a metric.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyShape {
    Path,
    PathFunction,
}

/// Stable compiled parser identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ParserId {
    Utf8FileShapeV1,
    TreeSitterComplexityV1,
    Utf8DocumentCharactersV1,
    CGrammarMagicDeltaV1,
    MarkdownTrackedTargetsV1,
}

impl ParserId {
    /// Stable parser identity spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Utf8FileShapeV1 => "utf8-file-shape-v1",
            Self::TreeSitterComplexityV1 => "tree-sitter-complexity-v1",
            Self::Utf8DocumentCharactersV1 => "utf8-document-characters-v1",
            Self::CGrammarMagicDeltaV1 => "c-grammar-magic-delta-v1",
            Self::MarkdownTrackedTargetsV1 => "markdown-tracked-targets-v1",
        }
    }
}

/// One exact compiled allowance for a Rust doc-comment Markdown citation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RustDocMarkdownCitationAllowance {
    source_path: &'static str,
    target_path: &'static str,
    reason: &'static str,
}

impl RustDocMarkdownCitationAllowance {
    #[must_use]
    pub const fn source_path(self) -> &'static str {
        self.source_path
    }

    #[must_use]
    pub const fn target_path(self) -> &'static str {
        self.target_path
    }

    #[must_use]
    pub const fn reason(self) -> &'static str {
        self.reason
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn matches(self, source_path: &str, target_path: &str) -> bool {
        self.target_path == target_path && self.source_path == source_path
    }
}

/// Exact non-tracked Markdown spellings admitted in Rust doc comments.
///
/// Each names a document created inside one test fixture and is bound to that
/// source, so the same missing name anywhere else remains a finding.
pub const RUST_DOC_MARKDOWN_CITATION_ALLOWANCES: [RustDocMarkdownCitationAllowance; 1] =
    [RustDocMarkdownCitationAllowance {
        source_path: "gate/src/battery/tests/file_shape.rs",
        target_path: "app/notes.md",
        reason: "the battery test names this path inside its synthetic repository fixture",
    }];

/// One parser declaration fixed at compile time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParserDeclaration {
    id: ParserId,
    input: &'static str,
}

impl ParserDeclaration {
    #[must_use]
    pub const fn id(self) -> ParserId {
        self.id
    }

    #[must_use]
    pub const fn input(self) -> &'static str {
        self.input
    }
}

/// Complete selected-tree scope of one structural arm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScopeDeclaration {
    inputs: &'static [&'static str],
    exclusions: &'static [&'static str],
    applicability: &'static str,
}

impl ScopeDeclaration {
    #[must_use]
    pub const fn inputs(self) -> &'static [&'static str] {
        self.inputs
    }

    #[must_use]
    pub const fn exclusions(self) -> &'static [&'static str] {
        self.exclusions
    }

    #[must_use]
    pub const fn applicability(self) -> &'static str {
        self.applicability
    }
}

/// One compiled metric and the policy/state inputs it consumes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetricDeclaration {
    id: MetricId,
    owner: GateId,
    unit: MetricUnit,
    key: KeyShape,
    parser: ParserId,
    policy_field: &'static str,
    latch_kind: Option<&'static str>,
}

impl MetricDeclaration {
    #[must_use]
    pub const fn id(self) -> MetricId {
        self.id
    }

    #[must_use]
    pub const fn owner(self) -> GateId {
        self.owner
    }

    #[must_use]
    pub const fn unit(self) -> MetricUnit {
        self.unit
    }

    #[must_use]
    pub const fn key(self) -> KeyShape {
        self.key
    }

    #[must_use]
    pub const fn parser(self) -> ParserId {
        self.parser
    }

    #[must_use]
    pub const fn policy_field(self) -> &'static str {
        self.policy_field
    }

    #[must_use]
    pub const fn latch_kind(self) -> Option<&'static str> {
        self.latch_kind
    }
}

/// One of the four structural arms in fixed battery order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmDeclaration {
    id: GateId,
    parser: ParserDeclaration,
    metrics: &'static [MetricId],
    scope: ScopeDeclaration,
}

impl ArmDeclaration {
    #[must_use]
    pub const fn id(self) -> GateId {
        self.id
    }

    #[must_use]
    pub const fn parser(self) -> ParserDeclaration {
        self.parser
    }

    #[must_use]
    pub const fn metrics(self) -> &'static [MetricId] {
        self.metrics
    }

    #[must_use]
    pub const fn scope(self) -> ScopeDeclaration {
        self.scope
    }
}

const FILE_SHAPE_METRICS: &[MetricId] = &[MetricId::FileLines, MetricId::FileBytes];
const COMPLEXITY_METRICS: &[MetricId] = &[MetricId::RoutineCcn, MetricId::RoutineLength];
const MAGIC_METRICS: &[MetricId] = &[MetricId::MagicNumberRegressions];
const MARKDOWN_METRICS: &[MetricId] = &[
    MetricId::MarkdownLinkInvalidTargets,
    MetricId::RustDocMarkdownTargetAbsences,
];

/// All quantities declared by this slice, including the document-character
/// metric consumed by the earlier `repo-docs` arm.
pub const STRUCTURAL_METRICS: [MetricDeclaration; 8] = [
    metric(
        MetricId::FileLines,
        GateId::FileShape,
        MetricUnit::SourceLines,
        KeyShape::Path,
        ParserId::Utf8FileShapeV1,
        "file_loc",
        Some("file_loc"),
    ),
    metric(
        MetricId::FileBytes,
        GateId::FileShape,
        MetricUnit::Bytes,
        KeyShape::Path,
        ParserId::Utf8FileShapeV1,
        "file_bytes",
        Some("file_bytes"),
    ),
    metric(
        MetricId::RoutineCcn,
        GateId::Complexity,
        MetricUnit::CyclomaticComplexity,
        KeyShape::PathFunction,
        ParserId::TreeSitterComplexityV1,
        "routine_ccn",
        Some("complexity_ccn"),
    ),
    metric(
        MetricId::RoutineLength,
        GateId::Complexity,
        MetricUnit::RoutineLines,
        KeyShape::PathFunction,
        ParserId::TreeSitterComplexityV1,
        "routine_length",
        Some("complexity_length"),
    ),
    metric(
        MetricId::RepositoryDocumentCharacters,
        GateId::RepoDocs,
        MetricUnit::UnicodeScalarValues,
        KeyShape::Path,
        ParserId::Utf8DocumentCharactersV1,
        "repo_truth_doc_chars",
        Some("repo_doc_chars"),
    ),
    metric(
        MetricId::MagicNumberRegressions,
        GateId::MagicNumbers,
        MetricUnit::Regressions,
        KeyShape::Path,
        ParserId::CGrammarMagicDeltaV1,
        "magic.examine_threshold",
        None,
    ),
    metric(
        MetricId::MarkdownLinkInvalidTargets,
        GateId::MarkdownLinks,
        MetricUnit::InvalidTargets,
        KeyShape::Path,
        ParserId::MarkdownTrackedTargetsV1,
        "tracked markdown inventory",
        None,
    ),
    metric(
        MetricId::RustDocMarkdownTargetAbsences,
        GateId::MarkdownLinks,
        MetricUnit::TargetAbsences,
        KeyShape::Path,
        ParserId::MarkdownTrackedTargetsV1,
        "tracked path inventory + exact compiled citation allowances",
        None,
    ),
];

const fn metric(
    id: MetricId,
    owner: GateId,
    unit: MetricUnit,
    key: KeyShape,
    parser: ParserId,
    policy_field: &'static str,
    latch_kind: Option<&'static str>,
) -> MetricDeclaration {
    MetricDeclaration {
        id,
        owner,
        unit,
        key,
        parser,
        policy_field,
        latch_kind,
    }
}

/// The four named arms in the exact interval frozen by [`GateId::BATTERY`].
pub const STRUCTURAL_ARMS: [ArmDeclaration; 4] = [
    ArmDeclaration {
        id: GateId::FileShape,
        parser: parser(ParserId::Utf8FileShapeV1, "selected source-file bytes"),
        metrics: FILE_SHAPE_METRICS,
        scope: scope(
            &["staged paths selected by policy.file_shape.source_suffixes"],
            &[
                "generated source patterns",
                "generated lockfiles",
                "repository-truth documents",
                "binary or invalid UTF-8 files",
            ],
            "one selected regular source file",
        ),
    },
    ArmDeclaration {
        id: GateId::Complexity,
        parser: parser(
            ParserId::TreeSitterComplexityV1,
            "selected source parsed by one vendored grammar",
        ),
        metrics: COMPLEXITY_METRICS,
        scope: scope(
            &["staged paths selected by policy.languages.complexity"],
            &[
                "excluded or absent paths",
                "source classes with no vendored grammar",
            ],
            "one named routine in one selected complexity source file",
        ),
    },
    ArmDeclaration {
        id: GateId::MagicNumbers,
        parser: parser(
            ParserId::CGrammarMagicDeltaV1,
            "current and baseline UTF-8 source text",
        ),
        metrics: MAGIC_METRICS,
        scope: scope(
            &["staged paths selected by policy.languages.magic"],
            &[
                "excluded paths",
                "source classes without an exact current parser",
            ],
            "one supported staged source file and its baseline blob state",
        ),
    },
    ArmDeclaration {
        id: GateId::MarkdownLinks,
        parser: parser(
            ParserId::MarkdownTrackedTargetsV1,
            "CommonMark links/headings, tracked paths, and parsed Rust doc comments",
        ),
        metrics: MARKDOWN_METRICS,
        scope: scope(
            &[
                "tracked .md paths after staged rename/delete fold",
                "tracked Rust doc comments containing code-spanned .md paths",
            ],
            &[
                "external URLs",
                "absolute paths",
                "targets outside the tree",
                "exact declared external and fixture citations",
            ],
            "one tracked Markdown or Rust doc-comment source and the complete tracked inventory",
        ),
    },
];

pub(crate) const fn metric_parser_declaration(metric: MetricId) -> ParserDeclaration {
    match metric {
        MetricId::FileLines | MetricId::FileBytes => STRUCTURAL_ARMS[0].parser(),
        MetricId::RoutineCcn | MetricId::RoutineLength => STRUCTURAL_ARMS[1].parser(),
        MetricId::RepositoryDocumentCharacters => parser(
            ParserId::Utf8DocumentCharactersV1,
            "repository-truth document bytes",
        ),
        MetricId::MagicNumberRegressions => STRUCTURAL_ARMS[2].parser(),
        MetricId::MarkdownLinkInvalidTargets | MetricId::RustDocMarkdownTargetAbsences => {
            STRUCTURAL_ARMS[3].parser()
        }
    }
}

const fn parser(id: ParserId, input: &'static str) -> ParserDeclaration {
    ParserDeclaration { id, input }
}

const fn scope(
    inputs: &'static [&'static str],
    exclusions: &'static [&'static str],
    applicability: &'static str,
) -> ScopeDeclaration {
    ScopeDeclaration {
        inputs,
        exclusions,
        applicability,
    }
}
