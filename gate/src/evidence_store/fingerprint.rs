//! Canonical reusable-action fingerprints for bounded receipt lookup.

use serde::{Deserialize, Serialize};
use wire::Digest;

use crate::ChangeSelectionReport;

use super::admission::canonical_object_id;
use super::{
    ActionKey, EvidenceStoreError, Layout, MAX_RECEIPT_BYTES, ReceiptExpectation,
    derive_action_key, publish_immutable, publish_json,
};

const FINGERPRINT_AUTHORITY_VERSION: u64 = 1;
const FINGERPRINT_MANIFEST_VERSION: u64 = 1;
const FINGERPRINT_INDEX_VERSION: u64 = 1;
const FINGERPRINT_IDENTITY: &[u8] = b"spice.gate.action-fingerprint-manifest.v1\0";
const FINGERPRINT_INDEX_BYTES: u64 = 64 * 1024;
const MAX_FINGERPRINT_ACTIONS: usize = super::admission::MAX_ADMISSION_ACTIONS;
const MAX_FINGERPRINT_BYTES: usize = MAX_RECEIPT_BYTES;
const MAX_FINGERPRINT_READ_BYTES: u64 = 4 * 1024 * 1024;

/// Inputs that can be validated without reconstructing Cargo or app plans.
#[derive(Clone, Debug)]
pub struct ActionFingerprintAuthorityFields {
    pub candidate_tree: String,
    pub baseline: Option<String>,
    pub source: Digest,
    pub policy: Digest,
    pub gate_set: Digest,
    pub tools: Digest,
    pub environment: Digest,
    pub cargo_source: Option<Digest>,
}

/// Complete fast-path authority for one reusable-action fingerprint manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionFingerprintAuthority {
    version: u64,
    candidate_tree: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    baseline: Option<String>,
    source: Digest,
    policy: Digest,
    gate_set: Digest,
    tools: Digest,
    environment: Digest,
    #[serde(skip_serializing_if = "Option::is_none")]
    cargo_source: Option<Digest>,
}

impl ActionFingerprintAuthority {
    pub fn new(fields: ActionFingerprintAuthorityFields) -> Result<Self, EvidenceStoreError> {
        let authority = Self {
            version: FINGERPRINT_AUTHORITY_VERSION,
            candidate_tree: fields.candidate_tree,
            baseline: fields.baseline,
            source: fields.source,
            policy: fields.policy,
            gate_set: fields.gate_set,
            tools: fields.tools,
            environment: fields.environment,
            cargo_source: fields.cargo_source,
        };
        authority.validate()?;
        Ok(authority)
    }

    #[must_use]
    pub fn candidate_tree(&self) -> &str {
        &self.candidate_tree
    }

    #[must_use]
    pub const fn source(&self) -> &Digest {
        &self.source
    }

    pub fn key(&self) -> Result<ActionKey, EvidenceStoreError> {
        let bytes = wire::encode(self).map_err(|error| {
            EvidenceStoreError::new(
                "gate-evidence::fingerprint-authority-invalid",
                error.to_string(),
            )
        })?;
        Ok(derive_action_key(&bytes))
    }

    fn validate(&self) -> Result<(), EvidenceStoreError> {
        if self.version != FINGERPRINT_AUTHORITY_VERSION {
            return Err(EvidenceStoreError::new(
                "gate-evidence::fingerprint-version-unsupported",
                format!(
                    "fingerprint authority version {} is not {FINGERPRINT_AUTHORITY_VERSION}",
                    self.version
                ),
            ));
        }
        if !canonical_object_id(&self.candidate_tree)
            || self
                .baseline
                .as_ref()
                .is_some_and(|value| !canonical_object_id(value))
        {
            return Err(EvidenceStoreError::new(
                "gate-evidence::fingerprint-source-invalid",
                "fingerprint candidate tree or baseline is not canonical",
            ));
        }
        Ok(())
    }
}

/// Fields supplied after explicit proof planning completes.
#[derive(Clone, Debug)]
pub struct ActionFingerprintManifestFields {
    pub authority: ActionFingerprintAuthority,
    pub selection: ChangeSelectionReport,
    pub cargo_tests: bool,
    pub actions: Vec<ReceiptExpectation>,
}

/// Exact reusable actions selected under one fast-path authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionFingerprintManifest {
    version: u64,
    authority: ActionFingerprintAuthority,
    selection: ChangeSelectionReport,
    cargo_tests: bool,
    actions: Vec<ReceiptExpectation>,
}

/// One manifest and its exact receipt lookup results read under the same lock.
pub struct ActionFingerprintResolution {
    manifest: ActionFingerprintManifest,
    receipts: Vec<Option<super::AdmissionAction>>,
    files_read: u64,
    bytes_read: u64,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ActionFingerprintIndex {
    version: u64,
    authority: ActionFingerprintAuthority,
    manifest: Digest,
}

impl ActionFingerprintResolution {
    pub(super) fn new(
        manifest: ActionFingerprintManifest,
        receipts: Vec<Option<super::AdmissionAction>>,
        budget: &super::ReadBudget,
    ) -> Self {
        Self {
            manifest,
            receipts,
            files_read: budget.files(),
            bytes_read: budget.bytes(),
        }
    }

    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        ActionFingerprintManifest,
        Vec<Option<super::AdmissionAction>>,
    ) {
        (self.manifest, self.receipts)
    }

    #[must_use]
    pub const fn files_read(&self) -> u64 {
        self.files_read
    }

    #[must_use]
    pub const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

impl ActionFingerprintManifest {
    /// Maximum reusable actions admitted by one fingerprint manifest.
    pub const MAX_ACTIONS: usize = MAX_FINGERPRINT_ACTIONS;
    /// Maximum canonical bytes admitted by one fingerprint manifest.
    pub const MAX_BYTES: usize = MAX_FINGERPRINT_BYTES;
    /// Maximum aggregate bytes read while resolving a fingerprint and its receipts.
    pub const MAX_READ_BYTES: u64 = MAX_FINGERPRINT_READ_BYTES;

    pub fn new(fields: ActionFingerprintManifestFields) -> Result<Self, EvidenceStoreError> {
        let manifest = Self {
            version: FINGERPRINT_MANIFEST_VERSION,
            authority: fields.authority,
            selection: fields.selection,
            cargo_tests: fields.cargo_tests,
            actions: fields.actions,
        };
        manifest.encode()?;
        Ok(manifest)
    }

    #[must_use]
    pub const fn authority(&self) -> &ActionFingerprintAuthority {
        &self.authority
    }

    #[must_use]
    pub const fn selection(&self) -> &ChangeSelectionReport {
        &self.selection
    }

    #[must_use]
    pub const fn cargo_tests(&self) -> bool {
        self.cargo_tests
    }

    #[must_use]
    pub fn actions(&self) -> &[ReceiptExpectation] {
        &self.actions
    }

    pub fn encode(&self) -> Result<Vec<u8>, EvidenceStoreError> {
        self.validate()?;
        let bytes = wire::encode(self).map_err(|error| {
            EvidenceStoreError::new("gate-evidence::fingerprint-invalid", error.to_string())
        })?;
        if bytes.len() > MAX_FINGERPRINT_BYTES {
            return Err(EvidenceStoreError::new(
                "gate-evidence::fingerprint-size-invalid",
                format!(
                    "fingerprint manifest length {} is over the limit",
                    bytes.len()
                ),
            ));
        }
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, EvidenceStoreError> {
        if bytes.len() > MAX_FINGERPRINT_BYTES {
            return Err(EvidenceStoreError::new(
                "gate-evidence::fingerprint-size-invalid",
                format!(
                    "fingerprint manifest length {} is over the limit",
                    bytes.len()
                ),
            ));
        }
        let manifest: Self = wire::decode(bytes).map_err(|error| {
            EvidenceStoreError::new("gate-evidence::fingerprint-invalid", error.to_string())
        })?;
        manifest.validate()?;
        if manifest.encode()? != bytes {
            return Err(EvidenceStoreError::new(
                "gate-evidence::fingerprint-noncanonical",
                "fingerprint bytes do not match their canonical encoding",
            ));
        }
        Ok(manifest)
    }

    pub(super) fn id(&self) -> Result<Digest, EvidenceStoreError> {
        let bytes = self.encode()?;
        Ok(manifest_id(&bytes))
    }

    fn validate(&self) -> Result<(), EvidenceStoreError> {
        if self.version != FINGERPRINT_MANIFEST_VERSION {
            return Err(EvidenceStoreError::new(
                "gate-evidence::fingerprint-version-unsupported",
                format!(
                    "fingerprint version {} is not {FINGERPRINT_MANIFEST_VERSION}",
                    self.version
                ),
            ));
        }
        self.authority.validate()?;
        if self.actions.len() > Self::MAX_ACTIONS {
            return Err(EvidenceStoreError::new(
                "gate-evidence::fingerprint-action-count-invalid",
                format!(
                    "fingerprint manifest has {} actions over the {} action limit",
                    self.actions.len(),
                    Self::MAX_ACTIONS,
                ),
            ));
        }
        let selection = self.selection.encode().map_err(|error| {
            EvidenceStoreError::new(
                "gate-evidence::fingerprint-selection-invalid",
                error.to_string(),
            )
        })?;
        ChangeSelectionReport::decode(&selection).map_err(|error| {
            EvidenceStoreError::new(
                "gate-evidence::fingerprint-selection-invalid",
                error.to_string(),
            )
        })?;
        if self.selection.source() != self.authority.source() {
            return Err(EvidenceStoreError::new(
                "gate-evidence::fingerprint-selection-mismatch",
                "fingerprint selection names a different staged source",
            ));
        }
        super::admission::validate_receipt_expectations(&self.actions)?;
        if self
            .actions
            .windows(2)
            .any(|pair| pair[0].action() >= pair[1].action())
        {
            return Err(EvidenceStoreError::new(
                "gate-evidence::fingerprint-actions-invalid",
                "fingerprint actions must be sorted by action name",
            ));
        }
        Ok(())
    }
}

impl ActionFingerprintIndex {
    pub(super) fn new(authority: ActionFingerprintAuthority, manifest: Digest) -> Self {
        Self {
            version: FINGERPRINT_INDEX_VERSION,
            authority,
            manifest,
        }
    }

    pub(super) fn encode(&self) -> Result<Vec<u8>, EvidenceStoreError> {
        wire::encode(self).map_err(|error| {
            EvidenceStoreError::new(
                "gate-evidence::fingerprint-index-invalid",
                error.to_string(),
            )
        })
    }

    fn decode(bytes: &[u8]) -> Result<Self, EvidenceStoreError> {
        let index: Self = wire::decode(bytes).map_err(|error| {
            EvidenceStoreError::new(
                "gate-evidence::fingerprint-index-invalid",
                error.to_string(),
            )
        })?;
        if index.version != FINGERPRINT_INDEX_VERSION {
            return Err(EvidenceStoreError::new(
                "gate-evidence::fingerprint-index-version-unsupported",
                format!(
                    "fingerprint index version {} is not {FINGERPRINT_INDEX_VERSION}",
                    index.version
                ),
            ));
        }
        let canonical = index.encode()?;
        if canonical != bytes {
            return Err(EvidenceStoreError::new(
                "gate-evidence::fingerprint-index-noncanonical",
                "fingerprint index bytes do not match their canonical encoding",
            ));
        }
        Ok(index)
    }
}

pub(super) fn publish(
    layout: &Layout,
    manifest: &ActionFingerprintManifest,
) -> Result<ActionKey, EvidenceStoreError> {
    publish_with(layout, manifest, || Ok(()))
}

pub(super) fn publish_with(
    layout: &Layout,
    manifest: &ActionFingerprintManifest,
    after_object: impl FnOnce() -> Result<(), EvidenceStoreError>,
) -> Result<ActionKey, EvidenceStoreError> {
    let bytes = manifest.encode()?;
    let id = manifest.id()?;
    let key = manifest.authority.key()?;
    // Validate the previous object before replacing its derived index. Corrupt
    // evidence remains a refusal; a valid obsolete plan is only a cache entry.
    if lookup(layout, &manifest.authority)?.as_ref() == Some(manifest) {
        return Ok(key);
    }
    publish_immutable(&layout.fingerprint_object_path(&id), &bytes)?;
    after_object()?;
    let index = ActionFingerprintIndex::new(manifest.authority.clone(), id);
    publish_json(&layout.fingerprint_index_path(&key), &index)?;
    Ok(key)
}

pub(super) fn lookup(
    layout: &Layout,
    authority: &ActionFingerprintAuthority,
) -> Result<Option<ActionFingerprintManifest>, EvidenceStoreError> {
    let mut budget = super::ReadBudget::new(MAX_FINGERPRINT_READ_BYTES);
    lookup_bounded(layout, authority, &mut budget)
}

pub(super) fn lookup_bounded(
    layout: &Layout,
    authority: &ActionFingerprintAuthority,
    budget: &mut super::ReadBudget,
) -> Result<Option<ActionFingerprintManifest>, EvidenceStoreError> {
    authority.validate()?;
    let key = authority.key()?;
    let path = layout.fingerprint_index_path(&key);
    let Some(bytes) = budget.read_optional(&path, FINGERPRINT_INDEX_BYTES)? else {
        return Ok(None);
    };
    let index = ActionFingerprintIndex::decode(&bytes)?;
    if &index.authority != authority {
        return Err(EvidenceStoreError::new(
            "gate-evidence::fingerprint-authority-mismatch",
            format!("fingerprint path names authority {key}, body names another authority"),
        ));
    }
    let object = layout.fingerprint_object_path(&index.manifest);
    let bytes = budget
        .read_optional(&object, MAX_FINGERPRINT_BYTES as u64)?
        .ok_or_else(|| {
            EvidenceStoreError::new(
                "gate-evidence::fingerprint-object-missing",
                format!("fingerprint object {} does not exist", object.display()),
            )
        })?;
    let observed = manifest_id(&bytes);
    if observed != index.manifest {
        return Err(EvidenceStoreError::new(
            "gate-evidence::fingerprint-digest-mismatch",
            format!(
                "fingerprint index names {}, object bytes identify {observed}",
                index.manifest
            ),
        ));
    }
    let manifest = ActionFingerprintManifest::decode(&bytes)?;
    if manifest.authority() != authority {
        return Err(EvidenceStoreError::new(
            "gate-evidence::fingerprint-authority-mismatch",
            "fingerprint manifest names another authority",
        ));
    }
    Ok(Some(manifest))
}

fn manifest_id(bytes: &[u8]) -> Digest {
    let mut identity = Vec::with_capacity(FINGERPRINT_IDENTITY.len() + bytes.len());
    identity.extend_from_slice(FINGERPRINT_IDENTITY);
    identity.extend_from_slice(bytes);
    Digest::compute(wire::Domain::GateInputManifest, &identity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence_store::invariant_fixture::{Fixture, receipt};
    use crate::{ChangeSelectionPolicy, ChangeSelectionPolicyInputs, select_change_gates};

    #[test]
    fn fingerprint_publication_recovers_after_object_write_and_authority_drift_misses() {
        let fixture = Fixture::new("fingerprint-recovery");
        let store = fixture.store();
        let first = authority(b"environment-a");
        let action = derive_action_key(b"fingerprinted action");
        let pass = receipt(&action, b"fingerprinted result");
        store.publish(&pass).unwrap();
        let expectation = ReceiptExpectation::new(
            "cargo:typecheck:fixture::lib:fixture",
            action,
            pass.action_spec().clone(),
            pass.input_manifest().clone(),
        )
        .unwrap();
        let first_manifest = manifest(first.clone(), false, vec![expectation]);
        store.publish_action_fingerprints(&first_manifest).unwrap();
        assert_eq!(
            store.lookup_action_fingerprints(&first).unwrap(),
            Some(first_manifest),
        );
        let (_, receipts) = store
            .lookup_fingerprint_receipts(&first)
            .unwrap()
            .unwrap()
            .into_parts();
        assert_eq!(receipts.len(), 1);
        assert!(receipts[0].is_some());
        assert!(
            store
                .lookup_action_fingerprints(&authority(b"environment-b"))
                .unwrap()
                .is_none()
        );

        let interrupted_authority = authority(b"environment-interrupted");
        let interrupted = manifest(interrupted_authority.clone(), true, Vec::new());
        let error = store
            .with_lock(|layout| {
                publish_with(layout, &interrupted, || {
                    Err(EvidenceStoreError::new(
                        "fixture::interrupted",
                        "stop after fingerprint object",
                    ))
                })
            })
            .unwrap_err();
        assert_eq!(error.reason(), "fixture::interrupted");
        assert!(
            store
                .lookup_action_fingerprints(&interrupted_authority)
                .unwrap()
                .is_none()
        );
        store.publish_action_fingerprints(&interrupted).unwrap();
        assert_eq!(
            store
                .lookup_action_fingerprints(&interrupted_authority)
                .unwrap(),
            Some(interrupted),
        );
    }

    #[test]
    fn replanning_replaces_the_fingerprint_without_reusing_obsolete_receipts() {
        let fixture = Fixture::new("fingerprint-replan");
        let store = fixture.store();
        let authority = authority(b"unchanged");
        let old = receipt(&derive_action_key(b"old contract"), b"old result");
        let new = receipt(&derive_action_key(b"new contract"), b"new result");
        let expectation = |pass: &super::super::PassReceipt| {
            ReceiptExpectation::new(
                "cargo:test:fixture::lib:fixture",
                pass.action().clone(),
                pass.action_spec().clone(),
                pass.input_manifest().clone(),
            )
            .unwrap()
        };
        let old_manifest = manifest(authority.clone(), true, vec![expectation(&old)]);
        let new_manifest = manifest(authority.clone(), true, vec![expectation(&new)]);
        store.publish(&old).unwrap();
        store.publish_action_fingerprints(&old_manifest).unwrap();
        let interrupted = store.with_lock(|layout| {
            publish_with(layout, &new_manifest, || {
                Err(EvidenceStoreError::new(
                    "fixture::interrupted",
                    "before index",
                ))
            })
        });
        assert_eq!(interrupted.unwrap_err().reason(), "fixture::interrupted");
        assert_eq!(
            store.lookup_action_fingerprints(&authority).unwrap(),
            Some(old_manifest),
        );
        store.publish_action_fingerprints(&new_manifest).unwrap();
        let (selected, receipts) = store
            .lookup_fingerprint_receipts(&authority)
            .unwrap()
            .unwrap()
            .into_parts();
        assert_eq!(selected, new_manifest);
        assert_eq!(receipts, vec![None]);
        store.publish(&new).unwrap();
        let (_, receipts) = store
            .lookup_fingerprint_receipts(&authority)
            .unwrap()
            .unwrap()
            .into_parts();
        assert_eq!(receipts[0].as_ref().unwrap().action_key(), new.action());
    }

    fn authority(environment: &[u8]) -> ActionFingerprintAuthority {
        ActionFingerprintAuthority::new(ActionFingerprintAuthorityFields {
            candidate_tree: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            baseline: Some("89abcdef0123456789abcdef0123456789abcdef".to_owned()),
            source: Digest::compute(wire::Domain::GateTree, b"source"),
            policy: Digest::compute(wire::Domain::GatePolicy, b"policy"),
            gate_set: Digest::compute(wire::Domain::GateSet, b"gate set"),
            tools: Digest::compute(wire::Domain::GateTool, b"tools"),
            environment: Digest::compute(wire::Domain::GateConfiguration, environment),
            cargo_source: Some(Digest::compute(
                wire::Domain::GateInventory,
                b"cargo source",
            )),
        })
        .unwrap()
    }

    fn manifest(
        authority: ActionFingerprintAuthority,
        cargo_tests: bool,
        actions: Vec<ReceiptExpectation>,
    ) -> ActionFingerprintManifest {
        ActionFingerprintManifest::new(ActionFingerprintManifestFields {
            selection: selection(&authority),
            authority,
            cargo_tests,
            actions,
        })
        .unwrap()
    }

    fn selection(authority: &ActionFingerprintAuthority) -> ChangeSelectionReport {
        let tracked = vec![wire::TRACKED_POLICY_FILE.to_owned()];
        let suffixes = vec![".rs".to_owned()];
        let rules = crate::repository_rule_policy(None).unwrap();
        let policy = ChangeSelectionPolicy::new(ChangeSelectionPolicyInputs {
            tracked_sources: &tracked,
            policy_authorities: &tracked,
            configured: true,
            rules: &rules,
            file_shape_suffixes: Some(&suffixes),
            complexity_suffixes: Some(&suffixes),
            magic_suffixes: Some(&suffixes),
            package_roots: &[],
            declared_rust_sources: &[],
            generated_paths: &[],
            test_paths: &["tests".to_owned()],
        })
        .unwrap();
        select_change_gates(authority.source().clone(), &tracked, &policy).unwrap()
    }
}
