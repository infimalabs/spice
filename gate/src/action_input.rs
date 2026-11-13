//! Complete content closures for reusable staged gate actions.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use wire::{Digest, Domain};

use crate::{ChangeReachObject, ChangeSelectionReport, GateId, SelectedFileMode, SelectedTree};

/// Current action-input manifest schema.
pub const ACTION_INPUT_MANIFEST_VERSION: u64 = 1;

/// Whether one current packaged arm can reuse prior proof.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionReuseClass {
    /// Staged content and declared non-file inputs determine the result.
    Cacheable,
    /// Reuse additionally requires exact committed ancestry or baseline state.
    AncestrySensitive,
    /// Mutable admission-time state prevents reusable proof.
    LiveOnly,
}

/// Classify one current canonical battery arm under the evidence contract.
#[must_use]
pub const fn gate_action_reuse_class(gate_id: GateId) -> Option<ActionReuseClass> {
    match gate_id {
        GateId::PlanningPhase | GateId::Staging => Some(ActionReuseClass::LiveOnly),
        GateId::MergeIntegrity
        | GateId::RepoDocs
        | GateId::FileShape
        | GateId::Complexity
        | GateId::MagicNumbers => Some(ActionReuseClass::AncestrySensitive),
        GateId::RepoShape
        | GateId::ConfigKeyValidity
        | GateId::ConfigFalseDisable
        | GateId::ConfigTrackedTrust
        | GateId::Formatters
        | GateId::LocalPaths
        | GateId::Taste
        | GateId::RustTypecheck
        | GateId::AppWebTypecheck
        | GateId::JavascriptUnused
        | GateId::CsharpUnusedCandidates
        | GateId::PythonTypecheck
        | GateId::EnvPolicy
        | GateId::EnvNameLedger
        | GateId::MarkdownLinks
        | GateId::Reachability
        | GateId::SymbolReachability
        | GateId::PythonUnused
        | GateId::AssertionFreeTests
        | GateId::PrivateInternals => Some(ActionReuseClass::Cacheable),
    }
}

/// Git mode admitted for one action input.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum GitFileMode {
    /// Git regular blob mode `100644`.
    #[serde(rename = "100644")]
    Regular,
    /// Git executable blob mode `100755`.
    #[serde(rename = "100755")]
    Executable,
}

impl From<SelectedFileMode> for GitFileMode {
    fn from(value: SelectedFileMode) -> Self {
        match value {
            SelectedFileMode::Regular => Self::Regular,
            SelectedFileMode::Executable => Self::Executable,
        }
    }
}

/// One present or deleted path in a reusable action closure.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case", tag = "presence")]
pub enum ActionInputManifestEntry {
    /// Exact bytes and executable mode from the immutable candidate tree.
    Present {
        path: String,
        mode: GitFileMode,
        blob: Digest,
    },
    /// A declared input absent from the candidate tree.
    Deleted { path: String },
}

impl ActionInputManifestEntry {
    /// Canonical repository-relative path.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Present { path, .. } | Self::Deleted { path } => path,
        }
    }
}

/// Closed classes of non-file state which may affect an action result.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionNonFileKind {
    Baseline,
    BoundLedger,
    ClaimState,
    Declaration,
    Environment,
    GateSet,
    Inventory,
    Policy,
    Toolchain,
}

/// One named non-file input and its canonical digest.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionNonFileInput {
    kind: ActionNonFileKind,
    name: String,
    digest: Digest,
}

impl ActionNonFileInput {
    /// Construct one typed non-file input.
    pub fn new(
        kind: ActionNonFileKind,
        name: impl Into<String>,
        digest: Digest,
    ) -> Result<Self, ActionInputError> {
        let input = Self {
            kind,
            name: name.into(),
            digest,
        };
        validate_name(&input.name)?;
        Ok(input)
    }

    #[must_use]
    pub const fn kind(&self) -> ActionNonFileKind {
        self.kind
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }
}

/// Complete path expansion for one non-path selection object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionInputExpansion {
    object: ChangeReachObject,
    paths: Vec<String>,
}

impl ActionInputExpansion {
    /// Declare the complete path closure for one reached object.
    pub fn new(
        object: ChangeReachObject,
        mut paths: Vec<String>,
    ) -> Result<Self, ActionInputError> {
        for path in &paths {
            validate_path(path)?;
        }
        paths.sort();
        if paths.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ActionInputError::new(
                "action input expansion contains a duplicate path",
            ));
        }
        Ok(Self { object, paths })
    }
}

/// Caller-supplied dependency and non-file inventory for one selected action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionInputInventory {
    incomplete: Option<String>,
    expansions: Vec<ActionInputExpansion>,
    non_files: Vec<ActionNonFileInput>,
}

impl ActionInputInventory {
    /// Admit a complete inventory after canonicalizing and validating it.
    pub fn complete(
        mut expansions: Vec<ActionInputExpansion>,
        mut non_files: Vec<ActionNonFileInput>,
    ) -> Result<Self, ActionInputError> {
        expansions.sort_by(|left, right| left.object.cmp(&right.object));
        if expansions
            .windows(2)
            .any(|pair| pair[0].object == pair[1].object)
        {
            return Err(ActionInputError::new(
                "action input inventory expands one object more than once",
            ));
        }
        non_files.sort();
        if non_files
            .windows(2)
            .any(|pair| pair[0].kind == pair[1].kind && pair[0].name == pair[1].name)
        {
            return Err(ActionInputError::new(
                "action input inventory names one non-file input more than once",
            ));
        }
        Ok(Self {
            incomplete: None,
            expansions,
            non_files,
        })
    }

    /// Preserve an unresolved dependency inventory as explicit non-cacheability.
    #[must_use]
    pub fn incomplete(reason: impl Into<String>) -> Self {
        Self {
            incomplete: Some(reason.into()),
            expansions: Vec::new(),
            non_files: Vec::new(),
        }
    }
}

/// Canonical content manifest for one reusable action.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionInputManifest {
    version: u64,
    entries: Vec<ActionInputManifestEntry>,
    non_files: Vec<ActionNonFileInput>,
}

impl ActionInputManifest {
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    #[must_use]
    pub fn entries(&self) -> &[ActionInputManifestEntry] {
        &self.entries
    }

    #[must_use]
    pub fn non_files(&self) -> &[ActionNonFileInput] {
        &self.non_files
    }

    /// Domain-separated digest of the canonical manifest bytes.
    pub fn digest(&self) -> Result<Digest, ActionInputError> {
        let encoded = self.encode()?;
        Ok(Digest::compute(Domain::GateInputManifest, &encoded))
    }

    /// Canonical JSON bytes.
    pub fn encode(&self) -> Result<Vec<u8>, ActionInputError> {
        wire::encode(self).map_err(|error| ActionInputError::new(error.to_string()))
    }

    /// Decode only the current canonical schema.
    pub fn decode(bytes: &[u8]) -> Result<Self, ActionInputError> {
        let manifest: Self =
            wire::decode(bytes).map_err(|error| ActionInputError::new(error.to_string()))?;
        manifest.validate()?;
        if manifest.encode()? != bytes {
            return Err(ActionInputError::new(
                "action input manifest bytes are not canonical",
            ));
        }
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), ActionInputError> {
        if self.version != ACTION_INPUT_MANIFEST_VERSION {
            return Err(ActionInputError::new(format!(
                "unsupported action input manifest version {}; expected {ACTION_INPUT_MANIFEST_VERSION}",
                self.version
            )));
        }
        let mut prior = None;
        for entry in &self.entries {
            validate_path(entry.path())?;
            if prior.is_some_and(|path| path >= entry.path()) {
                return Err(ActionInputError::new(
                    "action input manifest paths must be sorted and unique",
                ));
            }
            prior = Some(entry.path());
        }
        if !self.non_files.windows(2).all(|pair| {
            pair[0] < pair[1] && !(pair[0].kind == pair[1].kind && pair[0].name == pair[1].name)
        }) {
            return Err(ActionInputError::new(
                "action input manifest non-file inputs must be sorted and unique",
            ));
        }
        for input in &self.non_files {
            validate_name(&input.name)?;
        }
        Ok(())
    }
}

/// A reusable manifest or an explicit reason no key may be minted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionInputClosure {
    Reusable(ActionInputManifest),
    NonCacheable(ActionNonCacheable),
}

/// Typed reason an action closure cannot produce reusable proof.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActionNonCacheable {
    LiveOnly,
    UnsupportedAction,
    IncompleteInventory { reason: String },
    MissingObjectExpansion { object: ChangeReachObject },
    MissingBaseline,
    UnexpectedBaseline,
}

/// Compile one selected action's exact reusable input closure.
pub fn compile_action_input(
    tree: &SelectedTree,
    selection: &ChangeSelectionReport,
    gate_id: GateId,
    inventory: ActionInputInventory,
) -> Result<ActionInputClosure, ActionInputError> {
    let Some(class) = gate_action_reuse_class(gate_id) else {
        return Ok(ActionInputClosure::NonCacheable(
            ActionNonCacheable::UnsupportedAction,
        ));
    };
    if class == ActionReuseClass::LiveOnly {
        return Ok(ActionInputClosure::NonCacheable(
            ActionNonCacheable::LiveOnly,
        ));
    }
    if let Some(reason) = inventory.incomplete {
        return Ok(ActionInputClosure::NonCacheable(
            ActionNonCacheable::IncompleteInventory { reason },
        ));
    }
    let Some(reach) = selection.reach_for(gate_id) else {
        return Err(ActionInputError::new(format!(
            "action {gate_id} is absent from the canonical selection"
        )));
    };

    let expansions = inventory
        .expansions
        .into_iter()
        .map(|expansion| (expansion.object, expansion.paths))
        .collect::<BTreeMap<_, _>>();
    let mut paths = BTreeSet::new();
    for object in reach.objects() {
        match object {
            ChangeReachObject::Repository => {
                paths.extend(tree.paths().iter().cloned());
            }
            ChangeReachObject::Path { path } => {
                paths.insert(path.clone());
            }
            object => {
                let Some(expanded) = expansions.get(object) else {
                    return Ok(ActionInputClosure::NonCacheable(
                        ActionNonCacheable::MissingObjectExpansion {
                            object: object.clone(),
                        },
                    ));
                };
                paths.extend(expanded.iter().cloned());
            }
        }
    }

    let baseline_count = inventory
        .non_files
        .iter()
        .filter(|input| input.kind == ActionNonFileKind::Baseline)
        .count();
    match (class, baseline_count) {
        (ActionReuseClass::AncestrySensitive, 0) => {
            return Ok(ActionInputClosure::NonCacheable(
                ActionNonCacheable::MissingBaseline,
            ));
        }
        (ActionReuseClass::Cacheable, 0) | (ActionReuseClass::AncestrySensitive, 1) => {}
        (ActionReuseClass::Cacheable, _) => {
            return Ok(ActionInputClosure::NonCacheable(
                ActionNonCacheable::UnexpectedBaseline,
            ));
        }
        (ActionReuseClass::AncestrySensitive, _) => {
            return Err(ActionInputError::new(
                "ancestry-sensitive action names more than one baseline",
            ));
        }
        (ActionReuseClass::LiveOnly, _) => unreachable!("live-only actions returned above"),
    }

    let entries = paths
        .into_iter()
        .map(|path| manifest_entry(tree, path))
        .collect::<Result<Vec<_>, _>>()?;
    let manifest = ActionInputManifest {
        version: ACTION_INPUT_MANIFEST_VERSION,
        entries,
        non_files: inventory.non_files,
    };
    manifest.validate()?;
    Ok(ActionInputClosure::Reusable(manifest))
}

/// Compile one reusable input closure from an explicit canonical path inventory.
///
/// Proof runners use this boundary for actions below a gate arm, such as one
/// Cargo package-and-target shard. The caller remains responsible for proving
/// that `paths` is the complete action closure; this function validates and
/// canonicalizes the selected-tree entries and non-file authorities.
pub fn compile_explicit_action_input(
    tree: &SelectedTree,
    paths: Vec<String>,
    non_files: Vec<ActionNonFileInput>,
) -> Result<ActionInputManifest, ActionInputError> {
    let inventory = ActionInputInventory::complete(Vec::new(), non_files)?;
    let paths = paths.into_iter().collect::<BTreeSet<_>>();
    if paths.is_empty() {
        return Err(ActionInputError::new(
            "explicit action input closure must name at least one path",
        ));
    }
    let entries = paths
        .into_iter()
        .map(|path| manifest_entry(tree, path))
        .collect::<Result<Vec<_>, _>>()?;
    let manifest = ActionInputManifest {
        version: ACTION_INPUT_MANIFEST_VERSION,
        entries,
        non_files: inventory.non_files,
    };
    manifest.validate()?;
    Ok(manifest)
}

fn manifest_entry(
    tree: &SelectedTree,
    path: String,
) -> Result<ActionInputManifestEntry, ActionInputError> {
    let Some(mode) = tree.mode(&path) else {
        return Ok(ActionInputManifestEntry::Deleted { path });
    };
    let absolute = tree.root().join(&path);
    let bytes = fs::read(&absolute).map_err(|error| {
        ActionInputError::new(format!(
            "cannot read selected action input {path:?}: {error}"
        ))
    })?;
    Ok(ActionInputManifestEntry::Present {
        path,
        mode: mode.into(),
        blob: Digest::compute(Domain::GateFile, &bytes),
    })
}

fn validate_path(path: &str) -> Result<(), ActionInputError> {
    if path.is_empty()
        || path.contains('\\')
        || Path::new(path)
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ActionInputError::new(format!(
            "invalid action input path {path:?}"
        )));
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), ActionInputError> {
    if name.is_empty() || name.trim() != name || name.chars().any(char::is_control) {
        return Err(ActionInputError::new(format!(
            "invalid action non-file input name {name:?}"
        )));
    }
    Ok(())
}

/// Malformed closure declaration, selected input, or canonical manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionInputError {
    detail: String,
}

impl ActionInputError {
    pub(crate) fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ActionInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for ActionInputError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Selector, test_repository};

    #[test]
    fn explicit_manifest_reads_only_exact_staged_bytes() {
        let fixture = test_repository::Fixture::repository("action-input-closure");
        fixture.write("selected.rs", "pub const VALUE: u8 = 1;\n");
        fixture.write("unrelated.rs", "pub const OTHER: u8 = 1;\n");
        test_repository::stage_all(fixture.path());
        let tree = Selector::staged()
            .materialize(fixture.path())
            .expect("materialize staged closure");
        let manifest =
            compile_explicit_action_input(&tree, vec!["selected.rs".to_owned()], Vec::new())
                .expect("compile exact staged closure");
        let expected = manifest.digest().expect("digest staged closure");
        assert_eq!(
            manifest
                .entries()
                .iter()
                .map(ActionInputManifestEntry::path)
                .collect::<Vec<_>>(),
            ["selected.rs"],
        );

        fixture.write("selected.rs", "pub const VALUE: u8 = 2;\n");
        fixture.write("unrelated.rs", "pub const OTHER: u8 = 2;\n");
        let unchanged =
            compile_explicit_action_input(&tree, vec!["selected.rs".to_owned()], Vec::new())
                .expect("recompile retained staged closure");
        assert_eq!(unchanged.digest().unwrap(), expected);

        test_repository::stage(fixture.path(), &["selected.rs"]);
        let moved = Selector::staged()
            .materialize(fixture.path())
            .expect("materialize moved staged closure");
        let moved =
            compile_explicit_action_input(&moved, vec!["selected.rs".to_owned()], Vec::new())
                .expect("compile moved staged closure");
        assert_ne!(moved.digest().unwrap(), expected);
    }
}
