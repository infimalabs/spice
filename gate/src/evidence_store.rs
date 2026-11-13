//! Git-common immutable storage for proof-aware gate receipts.
//!
//! Receipt objects are content-addressed and never replaced. An action index is
//! the only mutable pointer and is published atomically under one cross-process
//! lock. Durable production leases use the same lock, so expiry takeover,
//! renewal, publication, and garbage collection have one serial history across
//! every linked worktree.
//!
//! The store lives at `.spice/cache/gate/v<EVIDENCE_STORE_VERSION>`. Every
//! digest-keyed record directory fans out like Git's loose objects, as
//! `DIR/<2 hex>/<rest>`. Beside each receipt, `receipt-stats` keeps mutable
//! usage counts that never touch the receipt's bytes.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sill::{LockFile, LockName};
use wire::{Actor, CanonicalUuid, Digest, Domain, Instant, TaskHandle};

use crate::execution_diagnostics::{ExecutionDiagnostics, OUTPUT_TAIL_LIMIT};

mod admission;
mod consumption;
mod fingerprint;
mod layout;
mod stats;
use layout::*;

pub use admission::{
    AdmissionAction, AdmissionBundle, AdmissionBundleFields, AdmissionId, AdmissionLiveCheck,
    ReceiptExpectation,
};
pub use consumption::{AdmissionCandidate, AdmissionConsumption, AdmissionTransactionId};
pub use fingerprint::{
    ActionFingerprintAuthority, ActionFingerprintAuthorityFields, ActionFingerprintManifest,
    ActionFingerprintManifestFields, ActionFingerprintResolution,
};
pub use stats::ReceiptStats;

/// Current on-disk evidence-store schema.
pub const EVIDENCE_STORE_VERSION: u64 = 1;
/// Current immutable receipt envelope schema.
pub const EVIDENCE_RECEIPT_VERSION: u64 = 1;

const LEASE_VERSION: u64 = 1;
const MAX_RECEIPT_BYTES: usize = 4 * 1024 * 1024;
static NEXT_LEASE: AtomicU64 = AtomicU64::new(0);

/// Identity of one canonical action specification and complete input closure.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ActionKey(Digest);

impl ActionKey {
    /// Digest spelling used for paths and crossing records.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ActionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Derive an action key from an already-canonical action contract.
///
/// Canonical field ordering and closure completeness belong to the action
/// contract. This boundary supplies the fixed domain separator.
#[must_use]
pub fn derive_action_key(canonical_action: &[u8]) -> ActionKey {
    ActionKey(Digest::compute(Domain::GateActionKey, canonical_action))
}

/// Content identity of one immutable receipt envelope.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ReceiptId(Digest);

impl ReceiptId {
    /// Digest spelling used for paths and crossing records.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for ReceiptId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Audit and credit metadata attached by the proof-aware runner.
///
/// Like every diagnostic, the producer never enters an action key, input
/// manifest, or typed result.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<CanonicalUuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<CanonicalUuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<Actor>,
    /// Digest of the executing appliance's executable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appliance_generation: Option<Digest>,
    /// `ARCH-OS` of the executing appliance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Build profile of the executing appliance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Correlates every receipt one proving run produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<CanonicalUuid>,
    /// The arm this receipt proved, as `gate prove` names it; the action key
    /// alone is a digest and never says which arm it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// Fields supplied by one trusted proof-aware execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PassReceiptFields {
    pub action_key: ActionKey,
    pub action_spec: Digest,
    pub input_manifest: Digest,
    pub typed_result: Digest,
    pub started_at: Instant,
    pub finished_at: Instant,
    pub duration_millis: u64,
    pub artifacts: Vec<Digest>,
    pub producer: ProducerRecord,
    pub diagnostics: Option<ExecutionDiagnostics>,
}

/// Immutable passing receipt validated against the approved evidence contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PassReceipt {
    version: u64,
    action_key: ActionKey,
    action_spec: Digest,
    input_manifest: Digest,
    typed_result: Digest,
    passed: bool,
    started_at: Instant,
    finished_at: Instant,
    duration_millis: u64,
    artifacts: Vec<Digest>,
    producer: ProducerRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diagnostics: Option<ExecutionDiagnostics>,
}

impl PassReceipt {
    /// Admit the canonical result of one successful proof-aware execution.
    pub fn new(fields: PassReceiptFields) -> Result<Self, EvidenceStoreError> {
        let receipt = Self {
            version: EVIDENCE_RECEIPT_VERSION,
            action_key: fields.action_key,
            action_spec: fields.action_spec,
            input_manifest: fields.input_manifest,
            typed_result: fields.typed_result,
            passed: true,
            started_at: fields.started_at,
            finished_at: fields.finished_at,
            duration_millis: fields.duration_millis,
            artifacts: fields.artifacts,
            producer: fields.producer,
            diagnostics: fields.diagnostics,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    /// Action this receipt proves.
    #[must_use]
    pub const fn action(&self) -> &ActionKey {
        &self.action_key
    }

    /// Canonical action specification proven by this receipt.
    #[must_use]
    pub const fn action_spec(&self) -> &Digest {
        &self.action_spec
    }

    /// Complete input-manifest identity proven by this receipt.
    #[must_use]
    pub const fn input_manifest(&self) -> &Digest {
        &self.input_manifest
    }

    /// Canonical identity of the typed successful result.
    #[must_use]
    pub const fn typed_result(&self) -> &Digest {
        &self.typed_result
    }

    /// Who produced this receipt.
    #[must_use]
    pub const fn producer(&self) -> &ProducerRecord {
        &self.producer
    }

    /// Where the execution's time went, when the producer recorded it.
    #[must_use]
    pub const fn diagnostics(&self) -> Option<&ExecutionDiagnostics> {
        self.diagnostics.as_ref()
    }

    /// Canonical receipt envelope bytes.
    pub fn encode(&self) -> Result<Vec<u8>, EvidenceStoreError> {
        self.validate()?;
        let bytes = wire::encode(self).map_err(|error| {
            EvidenceStoreError::new("gate-evidence::receipt-invalid", error.to_string())
        })?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(EvidenceStoreError::new(
                "gate-evidence::receipt-size-invalid",
                format!("receipt envelope length {} is over the limit", bytes.len()),
            ));
        }
        Ok(bytes)
    }

    /// Admit the current schema in its one canonical spelling.
    pub fn decode(bytes: &[u8]) -> Result<Self, EvidenceStoreError> {
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(EvidenceStoreError::new(
                "gate-evidence::receipt-size-invalid",
                format!("receipt envelope length {} is over the limit", bytes.len()),
            ));
        }
        let receipt: Self = wire::decode(bytes).map_err(|error| {
            EvidenceStoreError::new("gate-evidence::receipt-invalid", error.to_string())
        })?;
        receipt.validate()?;
        if receipt.encode()? != bytes {
            return Err(EvidenceStoreError::new(
                "gate-evidence::receipt-noncanonical",
                "receipt bytes do not match their canonical encoding",
            ));
        }
        Ok(receipt)
    }

    fn validate(&self) -> Result<(), EvidenceStoreError> {
        if self.version != EVIDENCE_RECEIPT_VERSION {
            return Err(EvidenceStoreError::new(
                "gate-evidence::receipt-version-unsupported",
                format!(
                    "receipt version {} is not {EVIDENCE_RECEIPT_VERSION}",
                    self.version
                ),
            ));
        }
        if self
            .diagnostics
            .as_ref()
            .and_then(|diagnostics| diagnostics.output_tail.as_ref())
            .is_some_and(|tail| tail.len() > OUTPUT_TAIL_LIMIT)
        {
            return Err(EvidenceStoreError::new(
                "gate-evidence::receipt-diagnostics-invalid",
                format!("receipt output tail exceeds {OUTPUT_TAIL_LIMIT} bytes"),
            ));
        }
        if !self.passed {
            return Err(EvidenceStoreError::new(
                "gate-evidence::receipt-not-passing",
                "only passing evidence may enter the reusable store",
            ));
        }
        if self.finished_at < self.started_at {
            return Err(EvidenceStoreError::new(
                "gate-evidence::receipt-time-invalid",
                "receipt finish precedes its start",
            ));
        }
        if self.artifacts.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(EvidenceStoreError::new(
                "gate-evidence::receipt-artifacts-invalid",
                "receipt artifact digests must be sorted and unique",
            ));
        }
        Ok(())
    }

    /// Content identity of the complete envelope.
    pub fn id(&self) -> Result<ReceiptId, EvidenceStoreError> {
        Ok(ReceiptId(Digest::compute(
            Domain::GatePassReceipt,
            &self.encode()?,
        )))
    }
}

/// Result of publishing one receipt and resolving its action index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Publication {
    receipt: ReceiptId,
    selected: ReceiptId,
    object_created: bool,
    index_updated: bool,
}

impl Publication {
    #[must_use]
    pub const fn receipt(&self) -> &ReceiptId {
        &self.receipt
    }
    #[must_use]
    pub const fn selected(&self) -> &ReceiptId {
        &self.selected
    }
    #[must_use]
    pub const fn object_created(&self) -> bool {
        self.object_created
    }
    #[must_use]
    pub const fn index_updated(&self) -> bool {
        self.index_updated
    }
}

/// Durable authority to produce evidence for one action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceLease {
    action: ActionKey,
    token: Digest,
    owner: String,
    expires_at_millis: u64,
}

impl EvidenceLease {
    #[must_use]
    pub const fn action(&self) -> &ActionKey {
        &self.action
    }
    #[must_use]
    pub fn token(&self) -> &str {
        self.token.as_str()
    }
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }
    #[must_use]
    pub const fn expires_at_millis(&self) -> u64 {
        self.expires_at_millis
    }
}

/// One deterministic lease acquisition outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeaseAcquisition {
    Acquired {
        lease: EvidenceLease,
        recovered: bool,
    },
    Busy {
        owner: String,
        expires_at_millis: u64,
    },
    Failed {
        owner: String,
        reason: String,
        detail: String,
        fault: bool,
        expires_at_millis: u64,
    },
}

/// Bounded collection result. Ambiguous bytes are retained and named.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectionReport {
    removed_objects: usize,
    removed_leases: usize,
    removed_admissions: usize,
    removed_pending_consumptions: usize,
    removed_admission_credits: usize,
    retained: Vec<String>,
}

impl CollectionReport {
    #[must_use]
    pub const fn removed_objects(&self) -> usize {
        self.removed_objects
    }
    #[must_use]
    pub const fn removed_leases(&self) -> usize {
        self.removed_leases
    }
    #[must_use]
    pub const fn removed_admissions(&self) -> usize {
        self.removed_admissions
    }
    #[must_use]
    pub const fn removed_pending_consumptions(&self) -> usize {
        self.removed_pending_consumptions
    }
    #[must_use]
    pub const fn removed_admission_credits(&self) -> usize {
        self.removed_admission_credits
    }
    #[must_use]
    pub fn retained(&self) -> &[String] {
        &self.retained
    }
}

/// One Git-common evidence store shared by linked worktrees.
#[derive(Clone, Debug)]
pub struct EvidenceStore {
    layout: Layout,
}

fn publish_batch(
    layout: &Layout,
    receipts: &[&PassReceipt],
    boundary: &mut impl FnMut(PublicationBoundary) -> Result<(), EvidenceStoreError>,
) -> Result<Vec<Publication>, EvidenceStoreError> {
    if receipts.is_empty() {
        return Ok(Vec::new());
    }

    let mut actions = BTreeSet::new();
    let mut entries = Vec::with_capacity(receipts.len());
    let mut prepared = Vec::with_capacity(receipts.len());
    for receipt in receipts {
        if !actions.insert(receipt.action().clone()) {
            return Err(EvidenceStoreError::new(
                "gate-evidence::publication-action-duplicate",
                format!("batch names action {} more than once", receipt.action()),
            ));
        }
        let bytes = receipt.encode()?;
        let id = receipt.id()?;
        let object_preexisting =
            match read_optional_regular(&layout.object_path(&id), MAX_RECEIPT_BYTES as u64)? {
                Some(existing) if existing == bytes => true,
                Some(_) => {
                    return Err(EvidenceStoreError::new(
                        "gate-evidence::immutable-object-mismatch",
                        format!(
                            "existing object {} has different bytes",
                            layout.object_path(&id).display()
                        ),
                    ));
                }
                None => false,
            };
        let current = read_index(layout, receipt.action())?;
        let selected = current
            .as_ref()
            .map_or_else(|| id.clone(), |index| index.receipt.clone().min(id.clone()));
        let index_updated = current
            .as_ref()
            .is_none_or(|index| index.receipt != selected);
        entries.push(PublicationJournalEntry {
            action: receipt.action().clone(),
            receipt: id.clone(),
            previous: current,
            object_preexisting,
        });
        prepared.push(PreparedPublication {
            bytes,
            publication: Publication {
                receipt: id,
                selected,
                object_created: !object_preexisting,
                index_updated,
            },
        });
    }

    let journal = PublicationJournal {
        version: EVIDENCE_STORE_VERSION,
        entries,
    };
    publish_publication(layout, &journal)?;
    let publication = (|| {
        boundary(PublicationBoundary::Prepared)?;
        for (index, (entry, prepared)) in journal.entries.iter().zip(&prepared).enumerate() {
            publish_immutable(&layout.object_path(&entry.receipt), &prepared.bytes)?;
            boundary(PublicationBoundary::Object(index))?;
            if prepared.publication.index_updated {
                publish_json(
                    &layout.index_path(&entry.action),
                    &ActionIndex::new(entry.action.clone(), prepared.publication.selected.clone()),
                )?;
            }
            boundary(PublicationBoundary::Index(index))?;
        }
        publish_publication(layout, &PublicationJournal::idle())?;
        Ok(prepared
            .into_iter()
            .map(|prepared| prepared.publication)
            .collect())
    })();
    publication.map_err(|error| rollback_after_failure(layout, &journal, error))
}

fn recover_publication(layout: &Layout) -> Result<(), EvidenceStoreError> {
    let Some(journal) = read_publication(layout)? else {
        return Ok(());
    };
    validate_publication_journal(&journal)?;
    if journal.entries.is_empty() {
        return Ok(());
    }
    rollback_publication(layout, &journal)
}

fn rollback_after_failure(
    layout: &Layout,
    journal: &PublicationJournal,
    error: EvidenceStoreError,
) -> EvidenceStoreError {
    match rollback_publication(layout, journal) {
        Ok(()) => error,
        Err(recovery) => EvidenceStoreError::new(
            "gate-evidence::publication-recovery-failed",
            format!("publication failed with {error}; rollback failed with {recovery}"),
        ),
    }
}

fn rollback_publication(
    layout: &Layout,
    journal: &PublicationJournal,
) -> Result<(), EvidenceStoreError> {
    validate_publication_journal(journal)?;
    for entry in journal.entries.iter().rev() {
        let path = layout.index_path(&entry.action);
        if let Some(previous) = &entry.previous {
            read_receipt(layout, &previous.receipt, Some(&entry.action))?;
            publish_json(&path, previous)?;
        } else {
            remove_optional_file(&path)?;
        }
    }
    for entry in journal.entries.iter().rev() {
        if !entry.object_preexisting {
            remove_optional_file(&layout.object_path(&entry.receipt))?;
        }
    }
    publish_publication(layout, &PublicationJournal::idle())
}

fn validate_publication_journal(journal: &PublicationJournal) -> Result<(), EvidenceStoreError> {
    if journal.version != EVIDENCE_STORE_VERSION {
        return Err(EvidenceStoreError::new(
            "gate-evidence::publication-journal-version-unsupported",
            format!(
                "publication journal version {} is not {EVIDENCE_STORE_VERSION}",
                journal.version
            ),
        ));
    }
    let mut actions = BTreeSet::new();
    for entry in &journal.entries {
        if !actions.insert(&entry.action) {
            return Err(EvidenceStoreError::new(
                "gate-evidence::publication-journal-invalid",
                format!("publication journal repeats action {}", entry.action),
            ));
        }
        if entry
            .previous
            .as_ref()
            .is_some_and(|previous| previous.action != entry.action)
        {
            return Err(EvidenceStoreError::new(
                "gate-evidence::publication-journal-invalid",
                format!("publication journal mismatches action {}", entry.action),
            ));
        }
    }
    Ok(())
}

impl EvidenceStore {
    /// Open or initialize the current store below an exact Git common directory.
    pub fn open(common_git: &Path) -> Result<Self, EvidenceStoreError> {
        Ok(Self {
            layout: Layout::initialize(common_git)?,
        })
    }

    /// Store root, primarily for inspection and diagnostics.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.layout.root
    }

    /// Publish one immutable object and atomically select it for its action.
    pub fn publish(&self, receipt: &PassReceipt) -> Result<Publication, EvidenceStoreError> {
        self.publish_batch(std::iter::once(receipt))
            .map(|mut publications| publications.remove(0))
    }

    /// Publish every receipt as one reader-visible transaction.
    pub fn publish_batch<'a>(
        &self,
        receipts: impl IntoIterator<Item = &'a PassReceipt>,
    ) -> Result<Vec<Publication>, EvidenceStoreError> {
        self.publish_batch_with(receipts, |_| Ok(()))
    }

    /// Publish receipts only while each caller retains its exact live action lease.
    pub fn publish_batch_leased<'a>(
        &self,
        entries: impl IntoIterator<Item = (&'a PassReceipt, &'a EvidenceLease)>,
        now: SystemTime,
    ) -> Result<Vec<Publication>, EvidenceStoreError> {
        let now = millis(now)?;
        let entries = entries.into_iter().collect::<Vec<_>>();
        self.with_lock(|layout| {
            for (receipt, lease) in &entries {
                if receipt.action() != lease.action() {
                    return Err(EvidenceStoreError::new(
                        "gate-evidence::lease-action-mismatch",
                        format!(
                            "receipt action {} differs from lease action {}",
                            receipt.action(),
                            lease.action(),
                        ),
                    ));
                }
                let record = require_lease(layout, lease)?;
                if record.failure.is_some() {
                    return Err(EvidenceStoreError::new(
                        "gate-evidence::lease-terminal",
                        format!(
                            "lease for {} already carries a terminal failure",
                            lease.action
                        ),
                    ));
                }
                if record.expires_at_millis <= now {
                    return Err(EvidenceStoreError::new(
                        "gate-evidence::lease-expired",
                        format!("lease expired at {}", record.expires_at_millis),
                    ));
                }
            }
            let receipts = entries
                .iter()
                .map(|(receipt, _)| *receipt)
                .collect::<Vec<_>>();
            publish_batch(layout, &receipts, &mut |_| Ok(()))
        })
    }

    fn publish_batch_with<'a>(
        &self,
        receipts: impl IntoIterator<Item = &'a PassReceipt>,
        mut boundary: impl FnMut(PublicationBoundary) -> Result<(), EvidenceStoreError>,
    ) -> Result<Vec<Publication>, EvidenceStoreError> {
        let receipts = receipts.into_iter().collect::<Vec<_>>();
        self.with_lock(|layout| publish_batch(layout, &receipts, &mut boundary))
    }

    /// Read the currently selected receipt for an action.
    pub fn lookup(&self, action: &ActionKey) -> Result<Option<PassReceipt>, EvidenceStoreError> {
        self.with_lock(|layout| {
            let Some(index) = read_index(layout, action)? else {
                return Ok(None);
            };
            read_receipt(layout, &index.receipt, Some(action)).map(Some)
        })
    }

    /// Resolve every expected receipt under one store lock and verify its bound authorities.
    pub fn lookup_batch_exact(
        &self,
        expected: &[ReceiptExpectation],
    ) -> Result<Vec<Option<AdmissionAction>>, EvidenceStoreError> {
        admission::validate_receipt_expectations(expected)?;
        self.with_lock(|layout| lookup_batch_exact(layout, expected))
    }

    /// Publish one immutable reusable-action fingerprint manifest.
    pub fn publish_action_fingerprints(
        &self,
        manifest: &ActionFingerprintManifest,
    ) -> Result<ActionKey, EvidenceStoreError> {
        self.with_lock(|layout| fingerprint::publish(layout, manifest))
    }

    /// Read the exact fingerprint manifest for one current fast-path authority.
    pub fn lookup_action_fingerprints(
        &self,
        authority: &ActionFingerprintAuthority,
    ) -> Result<Option<ActionFingerprintManifest>, EvidenceStoreError> {
        self.with_lock(|layout| fingerprint::lookup(layout, authority))
    }

    /// Read one exact fingerprint manifest and all of its receipts under one lock.
    pub fn lookup_fingerprint_receipts(
        &self,
        authority: &ActionFingerprintAuthority,
    ) -> Result<Option<ActionFingerprintResolution>, EvidenceStoreError> {
        self.with_lock(|layout| {
            let mut budget = ReadBudget::new(ActionFingerprintManifest::MAX_READ_BYTES);
            let Some(manifest) = fingerprint::lookup_bounded(layout, authority, &mut budget)?
            else {
                return Ok(None);
            };
            let receipts = lookup_batch_exact_bounded(layout, manifest.actions(), &mut budget)?;
            Ok(Some(ActionFingerprintResolution::new(
                manifest, receipts, &budget,
            )))
        })
    }

    /// Publish one immutable candidate admission after validating its complete schema.
    pub fn publish_admission(
        &self,
        bundle: &AdmissionBundle,
    ) -> Result<AdmissionId, EvidenceStoreError> {
        self.with_lock(|layout| admission::publish(layout, bundle))
    }

    /// Verify current admissions and idempotently record their pending consumption.
    pub fn prepare_admission_consumption(
        &self,
        transaction: &AdmissionTransactionId,
        candidates: &[AdmissionCandidate],
        now: Instant,
    ) -> Result<bool, EvidenceStoreError> {
        self.with_lock(|layout| consumption::prepare(layout, transaction, candidates, now))
    }

    /// Promote one exact pending consumption batch to immutable durable credit.
    ///
    /// A newly credited batch counts one admission at `at` on every receipt its
    /// admissions name; that count is best effort and never fails the commit.
    pub fn commit_admission_consumption(
        &self,
        transaction: &AdmissionTransactionId,
        at: Instant,
    ) -> Result<Option<AdmissionConsumption>, EvidenceStoreError> {
        self.with_lock(|layout| consumption::commit(layout, transaction, at))
    }

    /// Remove only one exact pending consumption batch.
    pub fn abort_admission_consumption(
        &self,
        transaction: &AdmissionTransactionId,
    ) -> Result<bool, EvidenceStoreError> {
        self.with_lock(|layout| consumption::abort(layout, transaction))
    }

    /// Acquire a durable single-flight lease, replacing it only at expiry.
    pub fn acquire_lease(
        &self,
        action: &ActionKey,
        owner: &str,
        now: SystemTime,
        ttl: Duration,
    ) -> Result<LeaseAcquisition, EvidenceStoreError> {
        validate_owner(owner)?;
        let now = millis(now)?;
        let ttl = u64::try_from(ttl.as_millis()).map_err(|_| {
            EvidenceStoreError::new(
                "gate-evidence::lease-duration-invalid",
                "lease duration exceeds u64 milliseconds",
            )
        })?;
        if ttl == 0 {
            return Err(EvidenceStoreError::new(
                "gate-evidence::lease-duration-invalid",
                "lease duration must be nonzero",
            ));
        }
        let expires = now.checked_add(ttl).ok_or_else(|| {
            EvidenceStoreError::new(
                "gate-evidence::lease-duration-invalid",
                "lease expiry overflows",
            )
        })?;
        self.with_lock(|layout| {
            let existing = read_lease(layout, action)?;
            if let Some(record) = &existing
                && record.expires_at_millis > now
            {
                if let Some(failure) = &record.failure {
                    return Ok(LeaseAcquisition::Failed {
                        owner: record.owner.clone(),
                        reason: failure.reason.clone(),
                        detail: failure.detail.clone(),
                        fault: failure.fault,
                        expires_at_millis: record.expires_at_millis,
                    });
                }
                return Ok(LeaseAcquisition::Busy {
                    owner: record.owner.clone(),
                    expires_at_millis: record.expires_at_millis,
                });
            }
            let recovered = existing.is_some();
            let token = lease_token(action, owner, now, expires);
            let record = LeaseRecord::new(action.clone(), token.clone(), owner.to_owned(), expires);
            publish_json(&layout.lease_path(action), &record)?;
            Ok(LeaseAcquisition::Acquired {
                lease: record.into(),
                recovered,
            })
        })
    }

    /// Extend the exact live lease without changing its token.
    pub fn renew_lease(
        &self,
        lease: &EvidenceLease,
        now: SystemTime,
        ttl: Duration,
    ) -> Result<EvidenceLease, EvidenceStoreError> {
        let now = millis(now)?;
        let ttl = u64::try_from(ttl.as_millis()).map_err(|_| {
            EvidenceStoreError::new(
                "gate-evidence::lease-duration-invalid",
                "lease duration exceeds u64 milliseconds",
            )
        })?;
        if ttl == 0 {
            return Err(EvidenceStoreError::new(
                "gate-evidence::lease-duration-invalid",
                "lease duration must be nonzero",
            ));
        }
        let expires = now.checked_add(ttl).ok_or_else(|| {
            EvidenceStoreError::new(
                "gate-evidence::lease-duration-invalid",
                "lease expiry overflows",
            )
        })?;
        self.with_lock(|layout| {
            let record = require_lease(layout, lease)?;
            if record.failure.is_some() {
                return Err(EvidenceStoreError::new(
                    "gate-evidence::lease-terminal",
                    format!(
                        "lease for {} already carries a terminal failure",
                        lease.action
                    ),
                ));
            }
            if record.expires_at_millis <= now {
                return Err(EvidenceStoreError::new(
                    "gate-evidence::lease-expired",
                    format!("lease expired at {}", record.expires_at_millis),
                ));
            }
            let renewed = LeaseRecord::new(
                lease.action.clone(),
                lease.token.clone(),
                lease.owner.clone(),
                expires,
            );
            publish_json(&layout.lease_path(&lease.action), &renewed)?;
            Ok(renewed.into())
        })
    }

    /// Release only the lease carrying the caller's exact token.
    pub fn release_lease(&self, lease: &EvidenceLease) -> Result<(), EvidenceStoreError> {
        self.with_lock(|layout| {
            require_lease(layout, lease)?;
            remove_file(&layout.lease_path(&lease.action))
        })
    }

    /// Publish one bounded terminal failure for every waiter on this exact lease.
    pub fn fail_lease(
        &self,
        lease: &EvidenceLease,
        reason: &str,
        detail: &str,
        fault: bool,
    ) -> Result<(), EvidenceStoreError> {
        validate_lease_failure(reason, detail)?;
        let mut end = detail.len().min(16 * 1024);
        while !detail.is_char_boundary(end) {
            end -= 1;
        }
        let detail = &detail[..end];
        self.with_lock(|layout| {
            let mut record = require_lease(layout, lease)?;
            record.failure = Some(LeaseFailure {
                reason: reason.to_owned(),
                detail: detail.to_owned(),
                fault,
            });
            publish_json(&layout.lease_path(&lease.action), &record)
        })
    }

    /// Remove at most `limit` old unreachable records and expired leases.
    ///
    /// `minimum_age` retains young admissions, pending transactions, credits,
    /// and receipts. Every retained root keeps its admission and receipt graph.
    pub fn collect(
        &self,
        now: SystemTime,
        minimum_age: Duration,
        limit: usize,
    ) -> Result<CollectionReport, EvidenceStoreError> {
        self.with_lock(|layout| collect_layout(layout, now, minimum_age, limit))
    }

    fn with_lock<T>(
        &self,
        operation: impl FnOnce(&Layout) -> Result<T, EvidenceStoreError>,
    ) -> Result<T, EvidenceStoreError> {
        self.layout.validate()?;
        let name = LockName::new("gate-evidence-store").expect("static lock name is valid");
        let lock = LockFile::new(name, self.layout.lock.clone());
        let _possession = lock
            .acquire()
            .map_err(|error| EvidenceStoreError::io("acquire store lock", lock.path(), error))?;
        self.layout.validate()?;
        recover_publication(&self.layout)?;
        operation(&self.layout)
    }
}

fn lookup_batch_exact(
    layout: &Layout,
    expected: &[ReceiptExpectation],
) -> Result<Vec<Option<AdmissionAction>>, EvidenceStoreError> {
    expected
        .iter()
        .map(|expected| {
            let Some(index) = read_index(layout, expected.action_key())? else {
                return Ok(None);
            };
            let receipt = read_receipt(layout, &index.receipt, Some(expected.action_key()))?;
            if receipt.action_spec() != expected.action_spec()
                || receipt.input_manifest() != expected.input_manifest()
            {
                return Err(EvidenceStoreError::new(
                    "gate-evidence::receipt-authority-mismatch",
                    format!(
                        "action {} expected spec={} manifest={} but receipt {} binds spec={} manifest={}",
                        expected.action(),
                        expected.action_spec(),
                        expected.input_manifest(),
                        index.receipt,
                        receipt.action_spec(),
                        receipt.input_manifest(),
                    ),
                ));
            }
            Ok(Some(AdmissionAction::resolved(expected, index.receipt)))
        })
        .collect()
}

fn lookup_batch_exact_bounded(
    layout: &Layout,
    expected: &[ReceiptExpectation],
    budget: &mut ReadBudget,
) -> Result<Vec<Option<AdmissionAction>>, EvidenceStoreError> {
    expected
        .iter()
        .map(|expected| {
            let Some((index, receipt)) =
                read_index_and_receipt_bounded(layout, expected.action_key(), budget)?
            else {
                return Ok(None);
            };
            if receipt.action_spec() != expected.action_spec()
                || receipt.input_manifest() != expected.input_manifest()
            {
                return Err(EvidenceStoreError::new(
                    "gate-evidence::receipt-authority-mismatch",
                    format!(
                        "action {} expected spec={} manifest={} but receipt {} binds spec={} manifest={}",
                        expected.action(),
                        expected.action_spec(),
                        expected.input_manifest(),
                        index.receipt,
                        receipt.action_spec(),
                        receipt.input_manifest(),
                    ),
                ));
            }
            Ok(Some(AdmissionAction::resolved(expected, index.receipt)))
        })
        .collect()
}

#[derive(Debug)]
pub struct EvidenceStoreError {
    reason: &'static str,
    detail: String,
}

impl EvidenceStoreError {
    fn new(reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }
    fn io(operation: &str, path: &Path, error: std::io::Error) -> Self {
        Self::new(
            "gate-evidence::filesystem",
            format!("{operation} at {}: {error}", path.display()),
        )
    }
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        self.reason
    }
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for EvidenceStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.reason, self.detail)
    }
}

impl std::error::Error for EvidenceStoreError {}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ActionIndex {
    version: u64,
    action: ActionKey,
    receipt: ReceiptId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PublicationJournal {
    version: u64,
    entries: Vec<PublicationJournalEntry>,
}

impl PublicationJournal {
    const fn idle() -> Self {
        Self {
            version: EVIDENCE_STORE_VERSION,
            entries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PublicationJournalEntry {
    action: ActionKey,
    receipt: ReceiptId,
    previous: Option<ActionIndex>,
    object_preexisting: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationBoundary {
    Prepared,
    Object(usize),
    Index(usize),
}

struct PreparedPublication {
    bytes: Vec<u8>,
    publication: Publication,
}

impl ActionIndex {
    fn new(action: ActionKey, receipt: ReceiptId) -> Self {
        Self {
            version: ACTION_INDEX_VERSION,
            action,
            receipt,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LeaseRecord {
    version: u64,
    action: ActionKey,
    token: Digest,
    owner: String,
    expires_at_millis: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<LeaseFailure>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LeaseFailure {
    reason: String,
    detail: String,
    fault: bool,
}

impl LeaseRecord {
    fn new(action: ActionKey, token: Digest, owner: String, expires_at_millis: u64) -> Self {
        Self {
            version: LEASE_VERSION,
            action,
            token,
            owner,
            expires_at_millis,
            failure: None,
        }
    }
}

impl From<LeaseRecord> for EvidenceLease {
    fn from(record: LeaseRecord) -> Self {
        Self {
            action: record.action,
            token: record.token,
            owner: record.owner,
            expires_at_millis: record.expires_at_millis,
        }
    }
}

fn validate_owner(owner: &str) -> Result<(), EvidenceStoreError> {
    if owner.is_empty() || owner.len() > 256 || owner.chars().any(char::is_control) {
        return Err(EvidenceStoreError::new(
            "gate-evidence::lease-owner-invalid",
            "lease owner must be 1..=256 printable bytes",
        ));
    }
    Ok(())
}

fn validate_lease_failure(reason: &str, detail: &str) -> Result<(), EvidenceStoreError> {
    if reason.is_empty() || reason.len() > 256 || reason.chars().any(char::is_control) {
        return Err(EvidenceStoreError::new(
            "gate-evidence::lease-failure-invalid",
            "lease failure reason must be 1..=256 printable bytes",
        ));
    }
    if detail.is_empty() {
        return Err(EvidenceStoreError::new(
            "gate-evidence::lease-failure-invalid",
            "lease failure detail must be nonempty",
        ));
    }
    Ok(())
}

fn millis(time: SystemTime) -> Result<u64, EvidenceStoreError> {
    let value = time
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            EvidenceStoreError::new("gate-evidence::clock-invalid", error.to_string())
        })?
        .as_millis();
    u64::try_from(value).map_err(|_| {
        EvidenceStoreError::new(
            "gate-evidence::clock-invalid",
            "clock exceeds u64 milliseconds",
        )
    })
}

fn lease_token(action: &ActionKey, owner: &str, now: u64, expires: u64) -> Digest {
    let sequence = NEXT_LEASE.fetch_add(1, Ordering::Relaxed);
    let bytes = format!(
        "{}\0{owner}\0{now}\0{expires}\0{}\0{sequence}",
        action,
        std::process::id()
    );
    Digest::compute(Domain::GateEvidenceLease, bytes.as_bytes())
}

fn require_lease(
    layout: &Layout,
    lease: &EvidenceLease,
) -> Result<LeaseRecord, EvidenceStoreError> {
    let record = read_lease(layout, &lease.action)?.ok_or_else(|| {
        EvidenceStoreError::new(
            "gate-evidence::lease-missing",
            format!("no lease exists for {}", lease.action),
        )
    })?;
    if record.token != lease.token {
        return Err(EvidenceStoreError::new(
            "gate-evidence::lease-token-mismatch",
            format!("another lease owns {}", lease.action),
        ));
    }
    Ok(record)
}

#[cfg(test)]
mod invariant_fixture {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    pub(super) struct Fixture(PathBuf);

    impl Fixture {
        pub(super) fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "gate-evidence-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&root).expect("create evidence fixture");
            Self(root)
        }

        pub(super) fn store(&self) -> EvidenceStore {
            EvidenceStore::open(&self.0).expect("open evidence fixture")
        }

        pub(super) fn root(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove evidence fixture");
        }
    }

    pub(super) fn receipt(action: &ActionKey, result: &[u8]) -> PassReceipt {
        PassReceipt::new(PassReceiptFields {
            action_key: action.clone(),
            action_spec: Digest::compute(Domain::GatePolicy, b"action spec"),
            input_manifest: Digest::compute(Domain::GateInventory, b"input manifest"),
            typed_result: Digest::compute(Domain::GateMeasurement, result),
            started_at: "2026-09-04T12:00:00.000Z".parse().unwrap(),
            finished_at: "2026-09-04T12:00:00.100Z".parse().unwrap(),
            duration_millis: 100,
            artifacts: Vec::new(),
            producer: ProducerRecord {
                actor: Some(Actor::Spice),
                ..ProducerRecord::default()
            },
            diagnostics: None,
        })
        .expect("construct evidence fixture receipt")
    }

    pub(super) fn admission(
        tree: &str,
        parent: Option<&str>,
        identity: &[u8],
        created_at: &str,
        expires_at: &str,
    ) -> AdmissionBundle {
        AdmissionBundle::new(AdmissionBundleFields {
            candidate_tree: tree.to_owned(),
            parent: parent.map(str::to_owned),
            selection: Digest::compute(Domain::GateSelection, identity),
            policy: Digest::compute(Domain::GatePolicy, b"policy"),
            gate_set: Digest::compute(Domain::GateSet, b"gate set"),
            actions: Vec::new(),
            live_checks: Vec::new(),
            claim: Digest::compute(Domain::GateRepositoryAuthority, identity),
            created_at: created_at.parse().unwrap(),
            expires_at: expires_at.parse().unwrap(),
        })
        .expect("construct evidence fixture admission")
    }
}

#[cfg(test)]
mod invariant_tests {
    use super::invariant_fixture::{Fixture, receipt};
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn evidence_store_lock_is_runtime_state_not_cache_state() {
        let fixture = Fixture::new("runtime-lock");
        let store = fixture.store();
        store
            .collect(SystemTime::UNIX_EPOCH, Duration::ZERO, 0)
            .expect("take the evidence store lock");
        let state = wire::state_root(fixture.root());
        assert!(state.join("runtime/locks/store.lock").is_file());
        assert!(
            !store.root().join("locks").exists(),
            "the evidence cache contains no locks"
        );
    }

    #[test]
    fn receipt_publication_converges_and_rolls_back_as_one_batch() {
        let fixture = Fixture::new("receipt-atomicity");
        let action = derive_action_key(b"concurrent action");
        let receipts = [receipt(&action, b"a"), receipt(&action, b"b")];
        assert_eq!(
            PassReceipt::decode(&receipts[0].encode().unwrap()).unwrap(),
            receipts[0],
        );
        let expected = receipts
            .iter()
            .map(PassReceipt::id)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .min()
            .unwrap();
        let barrier = Arc::new(Barrier::new(4));
        let workers = (0..4)
            .map(|index| {
                let store = fixture.store();
                let receipt = receipts[index % receipts.len()].clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store.publish(&receipt).unwrap();
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(
            fixture
                .store()
                .lookup(&action)
                .unwrap()
                .unwrap()
                .id()
                .unwrap(),
            expected,
        );

        let actions = [derive_action_key(b"batch-a"), derive_action_key(b"batch-b")];
        let batch = [
            receipt(&actions[0], b"batch-a"),
            receipt(&actions[1], b"batch-b"),
        ];
        let store = fixture.store();
        let error = store
            .publish_batch_with(&batch, |boundary| {
                (boundary != PublicationBoundary::Index(0))
                    .then_some(())
                    .ok_or_else(|| EvidenceStoreError::new("fixture::stop", "after first index"))
            })
            .unwrap_err();
        assert_eq!(error.reason(), "fixture::stop");
        for (action, receipt) in actions.iter().zip(batch) {
            assert!(store.lookup(action).unwrap().is_none());
            assert!(!store.layout.object_path(&receipt.id().unwrap()).exists());
        }
    }

    /// Reuse resolves `receipt` alone in a fresh store by its exact authority.
    fn reused_alone(receipt: &PassReceipt) -> Option<ReceiptId> {
        let fixture = Fixture::new("receipt-reuse");
        let store = fixture.store();
        store.publish(receipt).unwrap();
        let expected = ReceiptExpectation::new(
            "fixture-action",
            receipt.action().clone(),
            receipt.action_spec().clone(),
            receipt.input_manifest().clone(),
        )
        .unwrap();
        store
            .lookup_batch_exact(&[expected])
            .unwrap()
            .remove(0)
            .map(|action| action.receipt().clone())
    }

    #[test]
    fn execution_diagnostics_and_producer_never_change_what_a_receipt_proves() {
        use crate::execution_diagnostics::{
            CargoArtifactCounts, ExecutionDiagnostics, ExecutionPhase, PhaseTiming,
        };

        let action = derive_action_key(b"diagnosed action");
        let bare = receipt(&action, b"result");
        let diagnosed = PassReceipt::new(PassReceiptFields {
            action_key: action.clone(),
            action_spec: bare.action_spec().clone(),
            input_manifest: bare.input_manifest().clone(),
            typed_result: bare.typed_result().clone(),
            started_at: "2026-09-04T13:00:00.000Z".parse().unwrap(),
            finished_at: "2026-09-04T13:02:31.361Z".parse().unwrap(),
            duration_millis: 151_361,
            artifacts: Vec::new(),
            producer: ProducerRecord {
                appliance_generation: Some(Digest::compute(Domain::DistArtifact, b"appliance")),
                target: Some("aarch64-macos".to_owned()),
                profile: Some("release".to_owned()),
                run: Some("00000000-0000-4000-8000-000000000001".parse().unwrap()),
                ..ProducerRecord::default()
            },
            diagnostics: Some(ExecutionDiagnostics {
                queue_millis: Some(3),
                phases: vec![
                    PhaseTiming {
                        phase: ExecutionPhase::Prebuild,
                        millis: 151_292,
                    },
                    PhaseTiming {
                        phase: ExecutionPhase::Execution,
                        millis: 30,
                    },
                ],
                cargo: Some(CargoArtifactCounts {
                    fresh: 12,
                    rebuilt: 340,
                }),
                output_tail: Some("Finished `release` profile".to_owned()),
            }),
        })
        .unwrap();

        assert_eq!(
            PassReceipt::decode(&diagnosed.encode().unwrap()).unwrap(),
            diagnosed
        );
        assert_ne!(bare.id().unwrap(), diagnosed.id().unwrap());
        assert_eq!(bare.action(), diagnosed.action());
        assert_eq!(bare.input_manifest(), diagnosed.input_manifest());
        assert_eq!(bare.typed_result(), diagnosed.typed_result());
        for receipt in [&bare, &diagnosed] {
            assert_eq!(reused_alone(receipt), Some(receipt.id().unwrap()));
        }
    }
    #[test]
    fn only_the_current_receipt_schema_is_admitted() {
        let current = receipt(&derive_action_key(b"versioned action"), b"result");
        let versions = [
            0,
            EVIDENCE_RECEIPT_VERSION.saturating_sub(1),
            EVIDENCE_RECEIPT_VERSION
                .checked_add(1)
                .expect("receipt version has a successor"),
        ];

        for version in versions {
            if version == EVIDENCE_RECEIPT_VERSION {
                continue;
            }
            let mut foreign = current.clone();
            foreign.version = version;
            let bytes = wire::encode(&foreign).unwrap();
            assert_eq!(
                PassReceipt::decode(&bytes).unwrap_err().reason(),
                "gate-evidence::receipt-version-unsupported"
            );
        }
    }
}
