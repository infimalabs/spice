//! Canonical object reach carried by one ordinary change-selection report.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{ALWAYS, ChangeSelectionEdge, ChangeSelectionError};
use crate::GateId;

/// Whether one arm is narrowed to proven objects or explicitly escalated.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case", tag = "mode")]
pub enum ChangeReachMode {
    /// Only the objects named by the record may be consumed.
    Focused,
    /// A compiled authority requires the complete declared object inventory.
    Exhaustive {
        authority: ChangeReachAuthority,
        reason: String,
    },
}

/// Closed authority vocabulary allowed to escalate one arm to exhaustive reach.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeReachAuthority {
    SelectionPolicy,
    Workspace,
    Toolchain,
    DependencyGraph,
    LanguageGraph,
}

/// One typed object an ordinary selected arm is authorized to consume.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ChangeReachObject {
    /// Repository state which cannot be reduced to one tracked path.
    Repository,
    /// One normalized repository-relative path.
    Path { path: String },
    /// One package identity in a declared package graph.
    Package { package: String },
    /// One package-qualified build or test target.
    Target { package: String, target: String },
    /// One named check within the selected arm.
    Check { check: String },
}

impl ChangeReachObject {
    fn path(path: &str) -> Self {
        Self::Path {
            path: path.to_owned(),
        }
    }
}

/// Why one reached object belongs to one selected arm.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case", tag = "origin")]
pub enum ChangeReachOrigin {
    /// The compiled arm always observes repository state.
    Always,
    /// One exact changed source directly selected the object.
    Changed { source: String },
    /// One changed source reached the object through another declared object.
    Dependency {
        source: String,
        via: ChangeReachObject,
    },
}

impl ChangeReachOrigin {
    /// Changed source from which this provenance descends, when any.
    #[must_use]
    pub fn source(&self) -> Option<&str> {
        match self {
            Self::Always => None,
            Self::Changed { source } | Self::Dependency { source, .. } => Some(source),
        }
    }
}

/// One reached object and the exact provenance that selected it.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeReachEdge {
    object: ChangeReachObject,
    origin: ChangeReachOrigin,
}

impl ChangeReachEdge {
    /// Bind an always-on object to its compiled repository-state origin.
    #[must_use]
    pub fn always(object: ChangeReachObject) -> Self {
        Self {
            object,
            origin: ChangeReachOrigin::Always,
        }
    }

    /// Bind one object directly to one exact changed source.
    #[must_use]
    pub fn changed(source: impl Into<String>, object: ChangeReachObject) -> Self {
        Self {
            object,
            origin: ChangeReachOrigin::Changed {
                source: source.into(),
            },
        }
    }

    /// Bind one reached object through another object already in the arm plan.
    #[must_use]
    pub fn dependency(
        source: impl Into<String>,
        via: ChangeReachObject,
        object: ChangeReachObject,
    ) -> Self {
        Self {
            object,
            origin: ChangeReachOrigin::Dependency {
                source: source.into(),
                via,
            },
        }
    }

    /// Reached object this provenance establishes.
    #[must_use]
    pub const fn object(&self) -> &ChangeReachObject {
        &self.object
    }

    /// Direct, dependency, or always-on origin for the object.
    #[must_use]
    pub const fn origin(&self) -> &ChangeReachOrigin {
        &self.origin
    }
}

/// Complete object reach for one selected canonical arm.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeArmReach {
    gate_id: GateId,
    mode: ChangeReachMode,
    objects: Vec<ChangeReachObject>,
    provenance: Vec<ChangeReachEdge>,
}

impl ChangeArmReach {
    /// Build focused reach for later validation by the enclosing report.
    #[must_use]
    pub fn focused(
        gate_id: GateId,
        objects: Vec<ChangeReachObject>,
        provenance: Vec<ChangeReachEdge>,
    ) -> Self {
        Self {
            gate_id,
            mode: ChangeReachMode::Focused,
            objects,
            provenance,
        }
    }

    /// Build explicit exhaustive reach for later validation by the enclosing report.
    #[must_use]
    pub fn exhaustive(
        gate_id: GateId,
        authority: ChangeReachAuthority,
        reason: impl Into<String>,
        objects: Vec<ChangeReachObject>,
        provenance: Vec<ChangeReachEdge>,
    ) -> Self {
        Self {
            gate_id,
            mode: ChangeReachMode::Exhaustive {
                authority,
                reason: reason.into(),
            },
            objects,
            provenance,
        }
    }

    /// Canonical arm whose execution this record bounds.
    #[must_use]
    pub const fn gate_id(&self) -> GateId {
        self.gate_id
    }

    /// Focused or explicit exhaustive reach mode.
    #[must_use]
    pub const fn mode(&self) -> &ChangeReachMode {
        &self.mode
    }

    /// Sorted unique object inventory authorized for this arm.
    #[must_use]
    pub fn objects(&self) -> &[ChangeReachObject] {
        &self.objects
    }

    /// Sorted unique provenance edges proving the inventory.
    #[must_use]
    pub fn provenance(&self) -> &[ChangeReachEdge] {
        &self.provenance
    }
}

pub(super) fn direct_reach(
    changed_sources: &[String],
    gate_ids: &[GateId],
    selection_edges: &[ChangeSelectionEdge],
) -> Vec<ChangeArmReach> {
    gate_ids
        .iter()
        .map(|gate_id| direct_arm_reach(*gate_id, changed_sources, selection_edges))
        .collect()
}

fn direct_arm_reach(
    gate_id: GateId,
    changed_sources: &[String],
    selection_edges: &[ChangeSelectionEdge],
) -> ChangeArmReach {
    let always = ALWAYS.contains(&gate_id);
    let mut objects = if always {
        vec![ChangeReachObject::Repository]
    } else {
        selection_edges
            .iter()
            .filter(|edge| edge.gate_id() == gate_id)
            .map(|edge| ChangeReachObject::path(edge.source()))
            .collect::<Vec<_>>()
    };
    objects.sort();
    objects.dedup();

    let mut provenance = Vec::new();
    if always {
        provenance.push(ChangeReachEdge::always(ChangeReachObject::Repository));
    }
    for edge in selection_edges
        .iter()
        .filter(|edge| edge.gate_id() == gate_id)
    {
        let object = if always {
            ChangeReachObject::Repository
        } else {
            ChangeReachObject::path(edge.source())
        };
        provenance.push(ChangeReachEdge::changed(edge.source(), object));
    }
    provenance.sort();
    provenance.dedup();
    debug_assert!(
        changed_sources.is_empty() || provenance.iter().any(|edge| edge.origin.source().is_some())
    );
    ChangeArmReach::focused(gate_id, objects, provenance)
}

pub(super) fn validate_reach(
    reaches: &[ChangeArmReach],
    changed_sources: &[String],
    gate_ids: &[GateId],
    selection_edges: &[ChangeSelectionEdge],
) -> Result<(), ChangeSelectionError> {
    let reached_gates = reaches
        .iter()
        .map(ChangeArmReach::gate_id)
        .collect::<Vec<_>>();
    if reached_gates != gate_ids {
        return Err(ChangeSelectionError::new(
            "change reach must cover each selected arm once in canonical order",
        ));
    }
    for reach in reaches {
        validate_arm_reach(reach, changed_sources, selection_edges)?;
    }
    Ok(())
}

fn validate_arm_reach(
    reach: &ChangeArmReach,
    changed_sources: &[String],
    selection_edges: &[ChangeSelectionEdge],
) -> Result<(), ChangeSelectionError> {
    require_sorted_unique("reach object", &reach.objects)?;
    require_sorted_unique("reach provenance", &reach.provenance)?;
    validate_mode(reach)?;
    for object in &reach.objects {
        validate_object(object)?;
    }
    for edge in &reach.provenance {
        validate_provenance(reach, edge, changed_sources, selection_edges)?;
    }
    validate_proven_objects(reach)?;
    validate_dependency_sources(reach)?;
    validate_dependency_graph(reach)?;
    validate_selection_edges(reach, selection_edges)
}

fn validate_proven_objects(reach: &ChangeArmReach) -> Result<(), ChangeSelectionError> {
    let mut proven = reach
        .provenance
        .iter()
        .filter(|edge| !matches!(edge.origin, ChangeReachOrigin::Dependency { .. }))
        .map(|edge| edge.object.clone())
        .collect::<BTreeSet<_>>();
    loop {
        let before = proven.len();
        for edge in &reach.provenance {
            if let ChangeReachOrigin::Dependency { via, .. } = &edge.origin
                && proven.contains(via)
            {
                proven.insert(edge.object.clone());
            }
        }
        if proven.len() == before {
            break;
        }
    }
    if reach.objects.iter().all(|object| proven.contains(object)) {
        Ok(())
    } else {
        Err(ChangeSelectionError::new(format!(
            "selected arm {} has an unproven reach object in its dependency graph",
            reach.gate_id
        )))
    }
}

fn validate_dependency_sources(reach: &ChangeArmReach) -> Result<(), ChangeSelectionError> {
    let mut proven = reach
        .provenance
        .iter()
        .filter_map(|edge| match &edge.origin {
            ChangeReachOrigin::Changed { source } => Some((source.clone(), edge.object.clone())),
            ChangeReachOrigin::Always | ChangeReachOrigin::Dependency { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    loop {
        let before = proven.len();
        for edge in &reach.provenance {
            if let ChangeReachOrigin::Dependency { source, via } = &edge.origin
                && proven.contains(&(source.clone(), via.clone()))
            {
                proven.insert((source.clone(), edge.object.clone()));
            }
        }
        if proven.len() == before {
            break;
        }
    }
    if let Some(edge) = reach.provenance.iter().find(|edge| {
        matches!(
            &edge.origin,
            ChangeReachOrigin::Dependency { source, via }
                if !proven.contains(&(source.clone(), via.clone()))
        )
    }) {
        Err(ChangeSelectionError::new(format!(
            "selected arm {} has dependency provenance through an unproven dependency source: {:?}",
            reach.gate_id, edge.origin
        )))
    } else {
        Ok(())
    }
}

fn validate_dependency_graph(reach: &ChangeArmReach) -> Result<(), ChangeSelectionError> {
    let mut indegree = reach
        .objects
        .iter()
        .cloned()
        .map(|object| (object, 0_usize))
        .collect::<BTreeMap<_, _>>();
    let mut dependents = BTreeMap::<ChangeReachObject, BTreeSet<ChangeReachObject>>::new();
    for edge in &reach.provenance {
        let ChangeReachOrigin::Dependency { via, .. } = &edge.origin else {
            continue;
        };
        if dependents
            .entry(via.clone())
            .or_default()
            .insert(edge.object.clone())
        {
            *indegree
                .get_mut(&edge.object)
                .expect("validated dependency object belongs to reach") += 1;
        }
    }
    let mut ready = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(object, _)| object.clone())
        .collect::<Vec<_>>();
    let mut visited = 0_usize;
    while let Some(object) = ready.pop() {
        visited += 1;
        for dependent in dependents.get(&object).into_iter().flatten() {
            let degree = indegree
                .get_mut(dependent)
                .expect("validated dependency object belongs to reach");
            *degree -= 1;
            if *degree == 0 {
                ready.push(dependent.clone());
            }
        }
    }
    if visited == reach.objects.len() {
        Ok(())
    } else {
        Err(ChangeSelectionError::new(format!(
            "selected arm {} has a dependency cycle in its reach graph",
            reach.gate_id
        )))
    }
}

fn validate_mode(reach: &ChangeArmReach) -> Result<(), ChangeSelectionError> {
    match &reach.mode {
        ChangeReachMode::Focused => {
            if reach.objects.contains(&ChangeReachObject::Repository)
                && !ALWAYS.contains(&reach.gate_id)
            {
                return Err(ChangeSelectionError::new(format!(
                    "focused arm {} cannot claim repository-wide reach",
                    reach.gate_id
                )));
            }
        }
        ChangeReachMode::Exhaustive { reason, .. } => {
            require_text("exhaustive reach reason", reason)?;
        }
    }
    Ok(())
}

fn validate_provenance(
    reach: &ChangeArmReach,
    edge: &ChangeReachEdge,
    changed_sources: &[String],
    selection_edges: &[ChangeSelectionEdge],
) -> Result<(), ChangeSelectionError> {
    if !reach.objects.contains(&edge.object) {
        return Err(ChangeSelectionError::new(format!(
            "selected arm {} has provenance for a foreign reach object",
            reach.gate_id
        )));
    }
    match &edge.origin {
        ChangeReachOrigin::Always => {
            if !ALWAYS.contains(&reach.gate_id) || edge.object != ChangeReachObject::Repository {
                return Err(ChangeSelectionError::new(format!(
                    "selected arm {} has invalid always-on reach provenance",
                    reach.gate_id
                )));
            }
        }
        ChangeReachOrigin::Changed { source } => validate_changed_source(source, changed_sources)?,
        ChangeReachOrigin::Dependency { source, via } => {
            validate_changed_source(source, changed_sources)?;
            validate_object(via)?;
            if !reach.objects.contains(via) {
                return Err(ChangeSelectionError::new(format!(
                    "selected arm {} has dependency provenance through a foreign object",
                    reach.gate_id
                )));
            }
        }
    }
    validate_origin_selection(reach, edge, selection_edges)?;
    if let (ChangeReachOrigin::Changed { source }, ChangeReachObject::Path { path }) =
        (&edge.origin, &edge.object)
        && source != path
    {
        return Err(ChangeSelectionError::new(format!(
            "selected arm {} has direct path reach from a different source",
            reach.gate_id
        )));
    }
    Ok(())
}

fn validate_origin_selection(
    reach: &ChangeArmReach,
    edge: &ChangeReachEdge,
    selection_edges: &[ChangeSelectionEdge],
) -> Result<(), ChangeSelectionError> {
    let Some(source) = edge.origin.source() else {
        return Ok(());
    };
    if selection_edges
        .iter()
        .any(|selected| selected.gate_id() == reach.gate_id && selected.source() == source)
    {
        Ok(())
    } else {
        Err(ChangeSelectionError::new(format!(
            "reach source {source:?} did not select arm {}",
            reach.gate_id
        )))
    }
}

fn validate_selection_edges(
    reach: &ChangeArmReach,
    selection_edges: &[ChangeSelectionEdge],
) -> Result<(), ChangeSelectionError> {
    for edge in selection_edges
        .iter()
        .filter(|edge| edge.gate_id() == reach.gate_id)
    {
        if !reach
            .provenance
            .iter()
            .any(|provenance| provenance.origin.source() == Some(edge.source()))
        {
            return Err(ChangeSelectionError::new(format!(
                "selected arm {} has no object provenance for changed source {:?}",
                reach.gate_id,
                edge.source()
            )));
        }
    }
    Ok(())
}

fn validate_changed_source(
    source: &str,
    changed_sources: &[String],
) -> Result<(), ChangeSelectionError> {
    if changed_sources.iter().any(|changed| changed == source) {
        Ok(())
    } else {
        Err(ChangeSelectionError::new(format!(
            "reach provenance names unchanged source {source:?}"
        )))
    }
}

fn validate_object(object: &ChangeReachObject) -> Result<(), ChangeSelectionError> {
    match object {
        ChangeReachObject::Repository => Ok(()),
        ChangeReachObject::Path { path } => {
            if super::valid_repository_path(path) {
                Ok(())
            } else {
                Err(ChangeSelectionError::new(format!(
                    "reach object names invalid repository path {path:?}"
                )))
            }
        }
        ChangeReachObject::Package { package } => require_text("reach package", package),
        ChangeReachObject::Target { package, target } => {
            require_text("reach target package", package)?;
            require_text("reach target", target)
        }
        ChangeReachObject::Check { check } => require_text("reach check", check),
    }
}

fn require_sorted_unique<T: Ord>(label: &str, values: &[T]) -> Result<(), ChangeSelectionError> {
    if !values.is_empty() && values.windows(2).all(|pair| pair[0] < pair[1]) {
        Ok(())
    } else {
        Err(ChangeSelectionError::new(format!(
            "{label} inventory must be nonempty, sorted, and unique"
        )))
    }
}

fn require_text(label: &str, value: &str) -> Result<(), ChangeSelectionError> {
    if super::valid_text(value) {
        Ok(())
    } else {
        Err(ChangeSelectionError::new(format!(
            "{label} must be nonempty single-line text"
        )))
    }
}
