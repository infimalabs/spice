//! Complete leaf registry and stable routing metadata.

use serde::Serialize;
use wire::ConfigSource;

/// Serialized shape of one leaf value.
///
/// Semantic validation still belongs to the domain type named by
/// [`Root`](crate::config::Root).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueKind {
    /// One boolean.
    Boolean,
    /// One string.
    String,
    /// One ordered string list.
    StringList,
    /// One unsigned integer.
    UnsignedInteger,
    /// One finite floating-point number.
    Float,
    /// One duration word.
    Duration,
    /// One ordered duration-word list.
    DurationList,
    /// One consent answer.
    Answer,
    /// One lane life variant.
    Life,
    /// One complete named-team policy table, including its compiled rules.
    TeamPolicy,
}

impl ValueKind {
    /// Stable metadata spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::String => "string",
            Self::StringList => "string-list",
            Self::UnsignedInteger => "unsigned-integer",
            Self::Float => "float",
            Self::Duration => "duration",
            Self::DurationList => "duration-list",
            Self::Answer => "answer",
            Self::Life => "life",
            Self::TeamPolicy => "team-policy-table",
        }
    }
}

/// A compiled default represented as explicit absence or a serde scalar.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum Builtin {
    /// An optional fixed leaf has no compiled value.
    Absent,
    /// One string-shaped value.
    String(&'static str),
    /// One ordered string-shaped list.
    Strings(&'static [&'static str]),
    /// One unsigned integer.
    UnsignedInteger(u64),
    /// One finite floating-point number.
    Float(f64),
    /// One boolean.
    Boolean(bool),
}

/// Which authored layer owns a leaf. Only tracked policy is ever tracked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyClass {
    /// Behavior that is owned by tracked policy except for an explicit flag.
    Policy,
    /// Per-worktree launch preference that never carries executable authority
    /// and is never tracked: a Git-common default with a per-worktree override.
    WorktreeLocal,
    /// Machine-specific operation, set only in Git-common local config.
    MachineLocal,
}

impl PolicyClass {
    /// Stable metadata spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::WorktreeLocal => "worktree-local",
            Self::MachineLocal => "machine-local",
        }
    }
}

/// One command flag that may supply a configuration value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlagMapping {
    command: &'static [&'static str],
    flag: &'static str,
}

impl FlagMapping {
    const fn new(command: &'static [&'static str], flag: &'static str) -> Self {
        Self { command, flag }
    }

    /// Command path carrying the flag.
    #[must_use]
    pub const fn command(self) -> &'static [&'static str] {
        self.command
    }

    /// Exact long flag spelling.
    #[must_use]
    pub const fn flag(self) -> &'static str {
        self.flag
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TemplateShape {
    Fixed,
    Named,
    Indexed,
}

/// One unambiguous segment of a concrete configuration leaf path.
///
/// Keys remain whole even when a domain admits punctuation in a dynamic name;
/// indexes are separate from keys rather than encoded into display text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathSegment<'a> {
    /// One table or field key.
    Key(&'a str),
    /// One ordered row index.
    Index(usize),
}

/// One fixed or dynamic leaf path template.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathTemplate {
    rendered: &'static str,
    shape: TemplateShape,
}

impl PathTemplate {
    const fn fixed(rendered: &'static str) -> Self {
        Self {
            rendered,
            shape: TemplateShape::Fixed,
        }
    }

    const fn named(rendered: &'static str) -> Self {
        Self {
            rendered,
            shape: TemplateShape::Named,
        }
    }

    const fn indexed(rendered: &'static str) -> Self {
        Self {
            rendered,
            shape: TemplateShape::Indexed,
        }
    }

    /// Stable path spelling, using `<name>` and `[]` for dynamic segments.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.rendered
    }

    /// Whether this template covers one concrete leaf path.
    #[must_use]
    pub fn matches(self, path: &[PathSegment<'_>]) -> bool {
        self.covers(path, true)
    }

    pub(crate) fn accepts_prefix(self, path: &[PathSegment<'_>]) -> bool {
        self.covers(path, false)
    }

    fn covers(self, path: &[PathSegment<'_>], exact: bool) -> bool {
        let mut at = 0;
        for segment in self.rendered.split('.') {
            if at == path.len() {
                return !exact;
            }
            if segment == "<name>" {
                if !matches!(path.get(at), Some(PathSegment::Key(_))) {
                    return false;
                }
                at += 1;
            } else if let Some(key) = segment.strip_suffix("[]") {
                if path.get(at) != Some(&PathSegment::Key(key)) {
                    return false;
                }
                at += 1;
                if at == path.len() {
                    return !exact;
                }
                if !matches!(path.get(at), Some(PathSegment::Index(_))) {
                    return false;
                }
                at += 1;
            } else {
                if path.get(at) != Some(&PathSegment::Key(segment)) {
                    return false;
                }
                at += 1;
            }
        }
        at == path.len()
    }
}

/// Metadata and routing policy for one leaf template.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Descriptor {
    template: PathTemplate,
    kind: ValueKind,
    builtin: Option<Builtin>,
    layers: &'static [ConfigSource],
    class: PolicyClass,
    flag: Option<FlagMapping>,
}

impl Descriptor {
    /// Fixed or dynamic path template.
    #[must_use]
    pub const fn template(self) -> PathTemplate {
        self.template
    }

    /// Serialized value shape.
    #[must_use]
    pub const fn kind(self) -> ValueKind {
        self.kind
    }

    /// Compiled default; `None` is reserved for operator-created dynamic rows.
    #[must_use]
    pub const fn builtin(self) -> Option<Builtin> {
        self.builtin
    }

    /// Admitted authored layers in increasing precedence order.
    #[must_use]
    pub const fn layers(self) -> &'static [ConfigSource] {
        self.layers
    }

    /// Tracked-policy, worktree-local, or machine-local classification.
    #[must_use]
    pub const fn policy_class(self) -> PolicyClass {
        self.class
    }

    /// Command flag supplying this leaf, when one exists.
    #[must_use]
    pub const fn flag(self) -> Option<FlagMapping> {
        self.flag
    }
}

const TRACKED: &[ConfigSource] = &[ConfigSource::Tracked];
const LOCAL_WORKTREE: &[ConfigSource] = &[ConfigSource::Local, ConfigSource::Worktree];
const LOCAL: &[ConfigSource] = &[ConfigSource::Local];
/// A binding to one worktree's own build output: a Git-common default would
/// hand every other worktree a value sealed for a different commit and tree.
const WORKTREE: &[ConfigSource] = &[ConfigSource::Worktree];
const TRACKED_FLAG: &[ConfigSource] = &[ConfigSource::Tracked, ConfigSource::Flag];

macro_rules! configuration_registry {
    ($(
        $shape:ident $path:literal {
            kind: $kind:ident,
            builtin: $builtin:expr,
            layers: $layers:ident,
            class: $class:ident,
            flag: $flag:expr,
        }
    )+) => {
        const DESCRIPTORS: &[Descriptor] = &[
            $(Descriptor {
                template: PathTemplate::$shape($path),
                kind: ValueKind::$kind,
                builtin: $builtin,
                layers: $layers,
                class: PolicyClass::$class,
                flag: $flag,
            }),+
        ];
    };
}

configuration_registry! {
    named "team.<name>" {
        kind: TeamPolicy,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    named "commands.<name>" {
        kind: StringList,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "wrappers.spice" {
        kind: String,
        builtin: Some(Builtin::Absent),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "rtk.executable" {
        kind: String,
        builtin: Some(Builtin::String("rtk")),
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    fixed "judge.enabled" {
        kind: Boolean,
        builtin: Some(Builtin::Absent),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.env_names" {
        kind: StringList,
        builtin: Some(Builtin::Strings(&[])),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.pre_commit_builtins" {
        kind: Boolean,
        builtin: Some(Builtin::Boolean(true)),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    indexed "policy.pre_commit[].label" {
        kind: String,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    indexed "policy.pre_commit[].run" {
        kind: StringList,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    indexed "policy.pre_commit[].scopes.paths" {
        kind: StringList,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.test_engines" {
        kind: StringList,
        builtin: Some(Builtin::Absent),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.commit_message.allowed_trailers" {
        kind: StringList,
        builtin: Some(Builtin::Absent),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.commit_message.blocked_trailers" {
        kind: StringList,
        builtin: Some(Builtin::Absent),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.repo_truth.docs" {
        kind: StringList,
        builtin: Some(Builtin::Absent),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.file_shape.source_suffixes" {
        kind: StringList,
        builtin: Some(Builtin::Absent),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.languages.complexity" {
        kind: StringList,
        builtin: Some(Builtin::Absent),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.languages.magic" {
        kind: StringList,
        builtin: Some(Builtin::Absent),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.package_roots" {
        kind: StringList,
        builtin: Some(Builtin::Strings(&[])),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.generated_paths" {
        kind: StringList,
        builtin: Some(Builtin::Strings(&[])),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.name_cluster_threshold" {
        kind: UnsignedInteger,
        builtin: Some(Builtin::Absent),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.test_paths" {
        kind: StringList,
        builtin: Some(Builtin::Strings(&[])),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.package.boundary_underscore_pattern" {
        kind: String,
        builtin: Some(Builtin::Absent),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "policy.admission" {
        kind: StringList,
        builtin: Some(Builtin::Strings(&[])),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    named "taste.<name>.words" {
        kind: StringList,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    named "taste.<name>.message" {
        kind: String,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    named "taste.<name>.enabled" {
        kind: Boolean,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    named "taste.<name>.scopes.paths" {
        kind: StringList,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    named "taste.<name>.scopes.extensions" {
        kind: StringList,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    named "taste.<name>.scopes.vendors" {
        kind: StringList,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    named "taste.<name>.scopes.models" {
        kind: StringList,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    named "taste.<name>.scopes.phases" {
        kind: StringList,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "consent.posture" {
        kind: Answer,
        builtin: Some(Builtin::String("queue")),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    indexed "consent.rule[].name" {
        kind: String,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    indexed "consent.rule[].tool" {
        kind: String,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    indexed "consent.rule[].answer" {
        kind: Answer,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    indexed "consent.rule[].budget" {
        kind: UnsignedInteger,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "gate.walk.skip" {
        kind: StringList,
        builtin: Some(Builtin::Strings(&[])),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "board.next.horizon" {
        kind: Duration,
        builtin: Some(Builtin::String("2h")),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "lane.life" {
        kind: Life,
        builtin: Some(Builtin::String("Drive")),
        layers: TRACKED_FLAG,
        class: Policy,
        flag: Some(FlagMapping::new(&["lane", "add"], "--life")),
    }
    fixed "lane.lease" {
        kind: Duration,
        builtin: Some(Builtin::String("15m")),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "lane.runtime" {
        kind: Duration,
        builtin: Some(Builtin::String("24h")),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "lane.vendor" {
        kind: String,
        builtin: Some(Builtin::String("omp")),
        layers: TRACKED_FLAG,
        class: Policy,
        flag: Some(FlagMapping::new(&["lane", "add"], "--vendor")),
    }
    fixed "lane.deaths" {
        kind: UnsignedInteger,
        builtin: Some(Builtin::UnsignedInteger(3)),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "lane.young" {
        kind: Duration,
        builtin: Some(Builtin::String("30s")),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "lane.hold" {
        kind: Duration,
        builtin: Some(Builtin::String("30m")),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "lane.narration" {
        kind: UnsignedInteger,
        builtin: Some(Builtin::UnsignedInteger(24)),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "steer.expiry" {
        kind: Duration,
        builtin: Some(Builtin::String("3600s")),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "steer.resend" {
        kind: DurationList,
        builtin: Some(Builtin::Strings(&["60s", "1800s", "2700s"])),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "vendor.omp.binary" {
        kind: String,
        builtin: Some(Builtin::String("omp")),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "vendor.omp.model" {
        kind: String,
        builtin: Some(Builtin::String(vendor::DEFAULT_OMP_MODEL)),
        layers: LOCAL_WORKTREE,
        class: WorktreeLocal,
        flag: None,
    }
    fixed "vendor.omp.effort" {
        kind: String,
        builtin: Some(Builtin::String("high")),
        layers: LOCAL_WORKTREE,
        class: WorktreeLocal,
        flag: None,
    }
    fixed "vendor.omp.fast_mode" {
        kind: Boolean,
        builtin: Some(Builtin::Boolean(false)),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "vendor.omp.lsp" {
        kind: Boolean,
        builtin: Some(Builtin::Boolean(true)),
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "vendor.omp.auto_compact_window_tokens" {
        kind: UnsignedInteger,
        builtin: None,
        layers: TRACKED,
        class: Policy,
        flag: None,
    }
    fixed "listen.window" {
        kind: Duration,
        builtin: Some(Builtin::String("90s")),
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    fixed "lock.busy" {
        kind: UnsignedInteger,
        builtin: Some(Builtin::UnsignedInteger(75)),
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    fixed "lock.taken" {
        kind: UnsignedInteger,
        builtin: Some(Builtin::UnsignedInteger(76)),
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    fixed "lock.full" {
        kind: UnsignedInteger,
        builtin: Some(Builtin::UnsignedInteger(77)),
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    named "lock.<name>.path" {
        kind: String,
        builtin: None,
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    named "lock.<name>.directory" {
        kind: String,
        builtin: None,
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    named "lock.<name>.shards" {
        kind: UnsignedInteger,
        builtin: None,
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    fixed "serve.pack.rows" {
        kind: UnsignedInteger,
        builtin: Some(Builtin::UnsignedInteger(40)),
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    fixed "say.backend" {
        kind: String,
        builtin: Some(Builtin::String("system")),
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    fixed "say.voices" {
        kind: StringList,
        builtin: Some(Builtin::Strings(&[])),
        layers: LOCAL_WORKTREE,
        class: WorktreeLocal,
        flag: None,
    }
    fixed "say.words_per_minute" {
        kind: UnsignedInteger,
        builtin: Some(Builtin::UnsignedInteger(
            serve::speech::DEFAULT_WORDS_PER_MINUTE as u64,
        )),
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    fixed "proof.container" {
        kind: String,
        builtin: Some(Builtin::Absent),
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    fixed "proof.runner" {
        kind: String,
        builtin: Some(Builtin::Absent),
        layers: WORKTREE,
        class: WorktreeLocal,
        flag: None,
    }
    fixed "release.darwin.identity" {
        kind: String,
        builtin: Some(Builtin::Absent),
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
    fixed "release.darwin.notary_profile" {
        kind: String,
        builtin: Some(Builtin::Absent),
        layers: LOCAL,
        class: MachineLocal,
        flag: None,
    }
}

/// Complete descriptor registry in stable configuration order.
#[must_use]
pub const fn descriptors() -> &'static [Descriptor] {
    DESCRIPTORS
}

/// Find the sole descriptor covering a concrete leaf path.
#[must_use]
pub fn descriptor(path: &[PathSegment<'_>]) -> Option<&'static Descriptor> {
    DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.template.matches(path))
}
