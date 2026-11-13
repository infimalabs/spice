//! Fail-closed policy-arm selection for the ordinary staged-change gate.
//!
//! This module decides only which canonical battery arms are owed. It executes
//! none of them and performs no repository walk: the command rim supplies the
//! exact changed paths and the source digest they came from. Changes to an
//! authority that can alter the selector itself selects direct exhaustive
//! execution; without repository-authored policy, an ordinary path class this
//! build does not understand retains only the generic local-path arm. An
//! unclassified path the staged tree no longer holds is a removal that owes
//! the compile arms proving nothing still includes it. The explicit xtask
//! current-source selector retains its richer source-tree
//! knowledge in a separate module under the same local-only contract.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use wire::Digest;

use crate::GateId;

mod current_source;
#[cfg(not(target_arch = "wasm32"))]
mod language_reach;
mod policy;
mod reach;

pub use current_source::select_current_source_change_gates;
#[cfg(not(target_arch = "wasm32"))]
pub use language_reach::compile_language_reach;
pub use policy::{ChangeSelectionPolicy, ChangeSelectionPolicyInputs};
pub use reach::{
    ChangeArmReach, ChangeReachAuthority, ChangeReachEdge, ChangeReachMode, ChangeReachObject,
    ChangeReachOrigin,
};
use reach::{direct_reach, validate_reach};

/// Current staged-change policy-selection schema.
pub const CHANGE_SELECTION_REPORT_VERSION: u64 = 1;

/// The bounded execution mode admitted by the ordinary gate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeSelectionMode {
    /// Every changed path belongs to a known local class.
    Local,
}

/// One changed path's provenance for one selected canonical policy arm.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSelectionEdge {
    source: String,
    gate_id: GateId,
}

impl ChangeSelectionEdge {
    fn new(source: &str, gate_id: GateId) -> Self {
        Self {
            source: source.to_owned(),
            gate_id,
        }
    }

    /// Exact repository-relative staged path.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Canonical battery arm selected for the path.
    #[must_use]
    pub const fn gate_id(&self) -> GateId {
        self.gate_id
    }
}

/// Typed staged-path-to-policy plan consumed by the ordinary gate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeSelectionReport {
    version: u64,
    source: Digest,
    mode: ChangeSelectionMode,
    changed_sources: Vec<String>,
    applicable_gate_ids: Vec<GateId>,
    gate_ids: Vec<GateId>,
    edges: Vec<ChangeSelectionEdge>,
    reach: Vec<ChangeArmReach>,
}

impl ChangeSelectionReport {
    /// Current report schema.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Immutable staged source the selection describes.
    #[must_use]
    pub const fn source(&self) -> &Digest {
        &self.source
    }

    /// Bounded local execution.
    #[must_use]
    pub const fn mode(&self) -> ChangeSelectionMode {
        self.mode
    }

    /// Sorted exact staged paths supplied by the command rim.
    #[must_use]
    pub fn changed_sources(&self) -> &[String] {
        &self.changed_sources
    }

    /// Canonical packaged arms applicable to this repository and policy.
    #[must_use]
    pub fn applicable_gate_ids(&self) -> &[GateId] {
        &self.applicable_gate_ids
    }

    /// Canonically ordered synchronous battery arms owed by the change.
    #[must_use]
    pub fn gate_ids(&self) -> &[GateId] {
        &self.gate_ids
    }

    /// Canonical changed-source-to-arm provenance for the selected plan.
    ///
    /// Direct selection retains every exact path-to-arm reason. When one
    /// compiled authority selects exhaustive execution, the graph is a compact
    /// spanning proof: every changed source and every selected arm still has an
    /// edge, without manufacturing their Cartesian product.
    #[must_use]
    pub fn edges(&self) -> &[ChangeSelectionEdge] {
        &self.edges
    }

    /// Canonically ordered object reach for every selected arm.
    #[must_use]
    pub fn reach(&self) -> &[ChangeArmReach] {
        &self.reach
    }

    /// Exact object reach for one selected arm, when selected.
    #[must_use]
    pub fn reach_for(&self, gate_id: GateId) -> Option<&ChangeArmReach> {
        self.reach
            .binary_search_by_key(&gate_id, ChangeArmReach::gate_id)
            .ok()
            .map(|index| &self.reach[index])
    }

    /// Replace direct path reach with a dependency-expanded canonical plan.
    ///
    /// The report remains the admission boundary: replacement must still cover
    /// every selected arm exactly once, prove every object from one selected
    /// source, and retain all original path-to-arm provenance.
    pub fn with_reach(mut self, reach: Vec<ChangeArmReach>) -> Result<Self, ChangeSelectionError> {
        self.reach = reach;
        self.validate()?;
        Ok(self)
    }

    /// Canonical JSON bytes for this selection.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        wire::encode(self)
    }

    /// Decode only a canonical, internally complete selection.
    pub fn decode(bytes: &[u8]) -> Result<Self, ChangeSelectionError> {
        let report: Self = wire::decode(bytes).map_err(|error| {
            ChangeSelectionError::new(format!("invalid change selection: {error}"))
        })?;
        report.validate()?;
        let canonical = report
            .encode()
            .map_err(|error| ChangeSelectionError::new(error.to_string()))?;
        if canonical != bytes {
            return Err(ChangeSelectionError::new(
                "change-selection report bytes are not canonical",
            ));
        }
        Ok(report)
    }

    fn validate(&self) -> Result<(), ChangeSelectionError> {
        if self.version != CHANGE_SELECTION_REPORT_VERSION {
            return Err(ChangeSelectionError::new(format!(
                "unsupported change-selection version {}; expected {CHANGE_SELECTION_REPORT_VERSION}",
                self.version,
            )));
        }
        validate_sorted_unique("changed source", &self.changed_sources)?;
        if self
            .changed_sources
            .iter()
            .any(|path| !valid_repository_path(path))
        {
            return Err(ChangeSelectionError::new(
                "change selection contains an invalid repository-relative path",
            ));
        }
        validate_gate_inventory("applicable", &self.applicable_gate_ids)?;
        validate_gate_inventory("selected", &self.gate_ids)?;
        if self
            .gate_ids
            .iter()
            .any(|gate_id| !self.applicable_gate_ids.contains(gate_id))
        {
            return Err(ChangeSelectionError::new(
                "change selection names an arm inapplicable to this repository",
            ));
        }
        for required in ALWAYS {
            if !self.applicable_gate_ids.contains(&required) || !self.gate_ids.contains(&required) {
                return Err(ChangeSelectionError::new(format!(
                    "change selection omitted always-on arm {required}"
                )));
            }
        }
        if !self.edges.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(ChangeSelectionError::new(
                "change-selection edges must be sorted and unique",
            ));
        }
        for edge in &self.edges {
            if !self.changed_sources.contains(&edge.source)
                || !self.gate_ids.contains(&edge.gate_id)
            {
                return Err(ChangeSelectionError::new(
                    "change-selection edge names an unselected source or arm",
                ));
            }
        }
        if let Some(path) = self.changed_sources.iter().find(|path| {
            !self
                .edges
                .iter()
                .any(|edge| edge.source.as_str() == path.as_str())
        }) {
            return Err(ChangeSelectionError::new(format!(
                "changed source {path:?} has no policy-selection edge"
            )));
        }
        if !self.changed_sources.is_empty()
            && let Some(gate_id) = self
                .gate_ids
                .iter()
                .find(|gate_id| !self.edges.iter().any(|edge| edge.gate_id == **gate_id))
        {
            return Err(ChangeSelectionError::new(format!(
                "selected arm {gate_id} has no changed-source policy edge"
            )));
        }
        validate_reach(
            &self.reach,
            &self.changed_sources,
            &self.gate_ids,
            &self.edges,
        )?;
        Ok(())
    }
}

/// Select every applicable packaged policy arm owed by exact staged paths.
///
/// This function is pure and intentionally has no repository parameter. The
/// supplied policy was compiled from the same retained staged tree. A path it
/// cannot prove local refuses rather than omitting execution. A known compiled
/// selection authority conservatively selects every applicable arm with
/// repository-exhaustive reach, so no retained verifier result is required.
pub fn select_change_gates(
    source: Digest,
    changed_sources: &[String],
    policy: &ChangeSelectionPolicy,
) -> Result<ChangeSelectionReport, ChangeSelectionError> {
    let applicable = policy.applicable_gates(changed_sources);
    let authority_source = changed_sources
        .iter()
        .find(|path| policy.selection_authority(path));
    let exhaustive = authority_source.is_some();
    let mut report = select_with(source, changed_sources, applicable.clone(), |path| {
        if exhaustive {
            Ok(applicable.clone())
        } else {
            policy.gates_for_path(path)
        }
    })?;
    if let Some(authority_source) = authority_source {
        report = with_exhaustive_authority_reach(
            report,
            "compiled packaged selection authority changed",
            authority_source,
        )?;
    }
    Ok(report)
}

pub(super) fn with_exhaustive_authority_reach(
    mut report: ChangeSelectionReport,
    reason: &str,
    authority_source: &str,
) -> Result<ChangeSelectionReport, ChangeSelectionError> {
    if !report
        .changed_sources
        .iter()
        .any(|source| source == authority_source)
        || report.gate_ids.iter().any(|gate_id| {
            !report
                .edges
                .iter()
                .any(|edge| edge.source() == authority_source && edge.gate_id() == *gate_id)
        })
    {
        return Err(ChangeSelectionError::new(format!(
            "selection authority {authority_source:?} did not select every exhaustive arm"
        )));
    }

    let anchor = ALWAYS[0];
    let mut edges = report
        .changed_sources
        .iter()
        .map(|source| ChangeSelectionEdge::new(source, anchor))
        .chain(
            report
                .gate_ids
                .iter()
                .copied()
                .map(|gate_id| ChangeSelectionEdge::new(authority_source, gate_id)),
        )
        .collect::<Vec<_>>();
    edges.sort();
    edges.dedup();
    report.edges = edges;

    let reach = report
        .reach()
        .iter()
        .map(|current| {
            let gate_id = current.gate_id();
            let mut provenance = Vec::new();
            if ALWAYS.contains(&gate_id) {
                provenance.push(ChangeReachEdge::always(ChangeReachObject::Repository));
            }
            provenance.extend(
                report
                    .edges()
                    .iter()
                    .filter(|edge| edge.gate_id() == gate_id)
                    .map(|edge| {
                        ChangeReachEdge::changed(edge.source(), ChangeReachObject::Repository)
                    }),
            );
            provenance.sort();
            provenance.dedup();
            ChangeArmReach::exhaustive(
                gate_id,
                ChangeReachAuthority::SelectionPolicy,
                reason,
                vec![ChangeReachObject::Repository],
                provenance,
            )
        })
        .collect();
    report = report.with_reach(reach)?;
    Ok(report)
}

fn select_with(
    source: Digest,
    changed_sources: &[String],
    applicable_gate_ids: Vec<GateId>,
    mut gates_for_path: impl FnMut(&str) -> Result<Vec<GateId>, ChangeSelectionError>,
) -> Result<ChangeSelectionReport, ChangeSelectionError> {
    let mut changed_sources = changed_sources.to_vec();
    changed_sources.sort();
    changed_sources.dedup();
    if let Some(path) = changed_sources
        .iter()
        .find(|path| !valid_repository_path(path))
    {
        return Err(ChangeSelectionError::new(format!(
            "{path:?} is not a normalized repository-relative path"
        )));
    }

    validate_gate_inventory("applicable", &applicable_gate_ids)?;
    let applicable = applicable_gate_ids.iter().copied().collect::<BTreeSet<_>>();
    let mut selected = ALWAYS.into_iter().collect::<BTreeSet<_>>();
    let mut edges = Vec::new();
    for path in &changed_sources {
        let gates = gates_for_path(path)?;
        let gates = ALWAYS
            .into_iter()
            .chain(gates)
            .filter(|gate_id| applicable.contains(gate_id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        selected.extend(gates.iter().copied());
        edges.extend(
            gates
                .iter()
                .copied()
                .map(|gate_id| ChangeSelectionEdge::new(path, gate_id)),
        );
    }
    let gate_ids = selected.into_iter().collect::<Vec<_>>();
    edges.sort();
    edges.dedup();
    let reach = direct_reach(&changed_sources, &gate_ids, &edges);
    let report = ChangeSelectionReport {
        version: CHANGE_SELECTION_REPORT_VERSION,
        source,
        mode: ChangeSelectionMode::Local,
        changed_sources,
        applicable_gate_ids,
        gate_ids,
        edges,
        reach,
    };
    report.validate()?;
    Ok(report)
}

const ALWAYS: [GateId; 3] = [
    GateId::MergeIntegrity,
    GateId::PlanningPhase,
    GateId::Staging,
];

pub(crate) fn valid_repository_path(path: &str) -> bool {
    valid_text(path)
        && !path.starts_with('/')
        && !path.starts_with("./")
        && !path.split('/').any(|part| part.is_empty() || part == "..")
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains(['\0', '\r', '\n'])
}

fn validate_sorted_unique(label: &str, values: &[String]) -> Result<(), ChangeSelectionError> {
    if values.windows(2).all(|pair| pair[0] < pair[1]) {
        Ok(())
    } else {
        Err(ChangeSelectionError::new(format!(
            "{label} inventory must be sorted and unique"
        )))
    }
}

fn validate_gate_inventory(label: &str, gate_ids: &[GateId]) -> Result<(), ChangeSelectionError> {
    if gate_ids.is_empty()
        || !gate_ids.windows(2).all(|pair| pair[0] < pair[1])
        || gate_ids
            .iter()
            .any(|gate_id| !GateId::BATTERY.contains(gate_id))
    {
        Err(ChangeSelectionError::new(format!(
            "change selection requires unique canonical {label} arms in manifest order"
        )))
    } else {
        Ok(())
    }
}

/// Invalid changed-path input or malformed retained selection evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangeSelectionError {
    detail: String,
}

impl ChangeSelectionError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }

    /// Exact refusal detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for ChangeSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for ChangeSelectionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_changes_select_exhaustively_before_removed_paths_are_classified() {
        let tracked = ["Cargo.toml".to_owned(), "serve/Cargo.toml".to_owned()];
        let package_roots = ["serve".to_owned()];
        let empty = Vec::new();
        let rules = crate::repository_rule_policy(None).expect("packaged rule policy");
        let policy = ChangeSelectionPolicy::new(ChangeSelectionPolicyInputs {
            tracked_sources: &tracked,
            policy_authorities: &empty,
            configured: false,
            rules: &rules,
            file_shape_suffixes: None,
            complexity_suffixes: None,
            magic_suffixes: None,
            package_roots: &package_roots,
            declared_rust_sources: &empty,
            generated_paths: &empty,
            test_paths: &empty,
        })
        .expect("fixture policy compiles");
        let sources = ["Cargo.toml".to_owned(), "retired/Cargo.toml".to_owned()];

        let report = select_change_gates(
            Digest::compute(wire::Domain::GateTree, b"fixture source"),
            &sources,
            &policy,
        )
        .expect("authority changes admit every path into exhaustive selection");

        assert_eq!(report.changed_sources(), sources);
    }

    #[test]
    fn current_and_packaged_selectors_share_repository_document_membership() {
        let generated = ["app/renderer/renderer-00[1-9].mjs".to_owned()];
        let tracked = [
            "docs/guide.md".to_owned(),
            "docs/notes.txt".to_owned(),
            "app/renderer/renderer-001.mjs".to_owned(),
        ];
        let suffixes = [".md".to_owned(), ".mjs".to_owned(), ".txt".to_owned()];
        let rules = crate::repository_rule_policy(None).expect("packaged rule policy");
        let policy = ChangeSelectionPolicy::new(ChangeSelectionPolicyInputs {
            tracked_sources: &tracked,
            policy_authorities: &[],
            configured: true,
            rules: &rules,
            file_shape_suffixes: Some(&suffixes),
            complexity_suffixes: Some(&suffixes),
            magic_suffixes: Some(&suffixes),
            package_roots: &[],
            declared_rust_sources: &[],
            generated_paths: &generated,
            test_paths: &[],
        })
        .expect("fixture policy compiles");

        for (path, document, file_shape) in [
            ("docs/guide.md", true, false),
            ("docs/notes.txt", false, true),
            ("app/renderer/renderer-001.mjs", false, false),
            ("app/renderer/renderer-002.mjs", false, false),
        ] {
            let changed = [path.to_owned()];
            let source = Digest::compute(wire::Domain::GateTree, path.as_bytes());
            let current = select_current_source_change_gates(
                source.clone(),
                &tracked,
                &changed,
                &generated,
                &rules,
            )
            .expect("current-source selection");
            let packaged =
                select_change_gates(source, &changed, &policy).expect("packaged selection");
            for gate_id in [GateId::RepoDocs, GateId::FileShape] {
                assert_eq!(
                    current.gate_ids().contains(&gate_id),
                    packaged.gate_ids().contains(&gate_id),
                    "{path} disagrees on {gate_id}"
                );
            }
            assert_eq!(current.gate_ids().contains(&GateId::RepoDocs), document);
            assert_eq!(current.gate_ids().contains(&GateId::FileShape), file_shape);
        }
    }
}
