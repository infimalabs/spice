//! Reference-transaction consumption of immutable candidate admissions.

use std::fs;

use serde::{Deserialize, Serialize};
use wire::{Digest, Domain, Instant};

use super::admission::{canonical_object_id, canonical_reference};
use super::{
    AdmissionBundle, AdmissionId, EvidenceStoreError, Layout, MAX_RECEIPT_BYTES, ReceiptId,
    publish_immutable, read_optional_regular, read_receipt, remove_optional_file, stats,
};

const CONSUMPTION_VERSION: u64 = 1;

/// Stable identity of the complete queued reference update set.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AdmissionTransactionId(Digest);

impl AdmissionTransactionId {
    pub(super) const fn from_digest(digest: Digest) -> Self {
        Self(digest)
    }

    #[must_use]
    pub fn derive(canonical_updates: &[u8]) -> Self {
        let mut framed = b"reference-transaction-consumption-v1\0".to_vec();
        framed.extend_from_slice(canonical_updates);
        Self(Digest::compute(Domain::GateAdmission, &framed))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// One queued commit and the candidate tree it would install on a branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmissionCandidate {
    branch: String,
    parent: Option<String>,
    commit: String,
    tree: String,
}

impl AdmissionCandidate {
    pub fn new(
        branch: impl Into<String>,
        parent: Option<String>,
        commit: impl Into<String>,
        tree: impl Into<String>,
    ) -> Result<Self, EvidenceStoreError> {
        let value = Self {
            branch: branch.into(),
            parent,
            commit: commit.into(),
            tree: tree.into(),
        };
        validate_candidate(&value)?;
        Ok(value)
    }
}

fn validate_candidate(candidate: &AdmissionCandidate) -> Result<(), EvidenceStoreError> {
    if !canonical_reference(&candidate.branch)
        || !canonical_object_id(&candidate.commit)
        || !canonical_object_id(&candidate.tree)
        || candidate
            .parent
            .as_deref()
            .is_some_and(|parent| !canonical_object_id(parent))
    {
        return Err(EvidenceStoreError::new(
            "gate-evidence::admission-consumption-candidate-invalid",
            "admission consumption candidate has a noncanonical branch, parent, commit, or tree",
        ));
    }
    Ok(())
}

/// Durable credit proving which admission authorized one committed update.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionConsumption {
    version: u64,
    transaction: AdmissionTransactionId,
    entries: Vec<ConsumptionEntry>,
}

impl AdmissionConsumption {
    #[must_use]
    pub const fn transaction(&self) -> &AdmissionTransactionId {
        &self.transaction
    }

    pub fn commits(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.commit.as_str())
    }

    pub fn admissions(&self) -> impl Iterator<Item = &AdmissionId> {
        self.entries.iter().map(|entry| &entry.admission)
    }

    pub(super) fn admission_references(&self) -> impl Iterator<Item = (&str, &AdmissionId)> {
        self.entries
            .iter()
            .map(|entry| (entry.tree.as_str(), &entry.admission))
    }

    fn encode(&self) -> Result<Vec<u8>, EvidenceStoreError> {
        validate(self)?;
        wire::encode(self).map_err(|error| {
            EvidenceStoreError::new(
                "gate-evidence::admission-consumption-invalid",
                error.to_string(),
            )
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ConsumptionEntry {
    branch: String,
    parent: Option<String>,
    commit: String,
    tree: String,
    admission: AdmissionId,
}

pub(super) fn prepare(
    layout: &Layout,
    transaction: &AdmissionTransactionId,
    candidates: &[AdmissionCandidate],
    now: Instant,
) -> Result<bool, EvidenceStoreError> {
    let mut candidates = candidates.to_vec();
    candidates.sort_by(|left, right| {
        (&left.branch, &left.commit, &left.tree).cmp(&(&right.branch, &right.commit, &right.tree))
    });
    if candidates.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(EvidenceStoreError::new(
            "gate-evidence::admission-consumption-candidates-invalid",
            "reference transaction repeats one admission candidate",
        ));
    }

    let mut entries = Vec::new();
    for candidate in &candidates {
        let Some((admission, bundle)) = matching_admission(layout, candidate, now)? else {
            continue;
        };
        for action in bundle.actions() {
            read_receipt(layout, action.receipt(), Some(action.action_key()))?;
        }
        entries.push(ConsumptionEntry {
            branch: candidate.branch.clone(),
            parent: candidate.parent.clone(),
            commit: candidate.commit.clone(),
            tree: candidate.tree.clone(),
            admission,
        });
    }
    if entries.is_empty() {
        return Ok(false);
    }
    let pending = AdmissionConsumption {
        version: CONSUMPTION_VERSION,
        transaction: transaction.clone(),
        entries,
    };
    let bytes = pending.encode()?;
    if let Some(credit) = read_record(&layout.admission_credit_path(transaction), transaction)? {
        if credit != pending {
            return Err(EvidenceStoreError::new(
                "gate-evidence::admission-credit-mismatch",
                "durable credit differs from the repeated prepared transaction",
            ));
        }
        return Ok(true);
    }
    publish_immutable(&layout.pending_admission_path(transaction), &bytes)?;
    Ok(true)
}

pub(super) fn commit(
    layout: &Layout,
    transaction: &AdmissionTransactionId,
    at: Instant,
) -> Result<Option<AdmissionConsumption>, EvidenceStoreError> {
    commit_with(layout, transaction, at, || Ok(()))
}

pub(super) fn commit_with(
    layout: &Layout,
    transaction: &AdmissionTransactionId,
    at: Instant,
    after_credit: impl FnOnce() -> Result<(), EvidenceStoreError>,
) -> Result<Option<AdmissionConsumption>, EvidenceStoreError> {
    let credit_path = layout.admission_credit_path(transaction);
    let existing = read_record(&credit_path, transaction)?;
    let pending_path = layout.pending_admission_path(transaction);
    let Some(pending) = read_record(&pending_path, transaction)? else {
        return Ok(existing);
    };
    if let Some(existing) = &existing {
        if existing != &pending {
            return Err(EvidenceStoreError::new(
                "gate-evidence::admission-credit-mismatch",
                "durable credit differs from its retained pending transaction",
            ));
        }
    } else {
        publish_immutable(&credit_path, &pending.encode()?)?;
    }
    after_credit()?;
    remove_optional_file(&pending_path)?;
    if existing.is_none() {
        // The credit is already durable; its statistics are best effort.
        let _ = admitted_receipts(layout, &pending)
            .and_then(|receipts| stats::record(layout, &receipts, stats::Event::Admission, at));
    }
    Ok(Some(pending))
}

/// Every receipt the admissions of one consumption batch vouch for.
fn admitted_receipts(
    layout: &Layout,
    consumption: &AdmissionConsumption,
) -> Result<Vec<ReceiptId>, EvidenceStoreError> {
    let mut receipts = Vec::new();
    for (tree, admission) in consumption.admission_references() {
        let path = layout.admission_path(tree, admission);
        let bytes = read_optional_regular(&path, MAX_RECEIPT_BYTES as u64)?.ok_or_else(|| {
            EvidenceStoreError::new(
                "gate-evidence::record-missing",
                format!("{} does not exist", path.display()),
            )
        })?;
        let bundle = AdmissionBundle::decode(&bytes)?;
        receipts.extend(
            bundle
                .actions()
                .iter()
                .map(|action| action.receipt().clone()),
        );
    }
    Ok(receipts)
}

pub(super) fn abort(
    layout: &Layout,
    transaction: &AdmissionTransactionId,
) -> Result<bool, EvidenceStoreError> {
    let path = layout.pending_admission_path(transaction);
    let existed = read_record(&path, transaction)?.is_some();
    remove_optional_file(&path)?;
    Ok(existed)
}

fn matching_admission(
    layout: &Layout,
    candidate: &AdmissionCandidate,
    now: Instant,
) -> Result<Option<(AdmissionId, AdmissionBundle)>, EvidenceStoreError> {
    let Some((id, bundle)) =
        super::admission::lookup_current(layout, &candidate.tree, candidate.parent.as_deref())?
    else {
        let directory = layout.admission_tree_path(&candidate.tree);
        return match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Ok(metadata) if metadata.is_dir() => Err(EvidenceStoreError::new(
                "gate-evidence::admission-candidate-mismatch",
                format!(
                    "candidate {} has admissions, but none match {} at its first parent",
                    candidate.commit, candidate.branch
                ),
            )),
            Ok(_) => Err(EvidenceStoreError::new(
                "gate-evidence::admission-entry-invalid",
                format!(
                    "candidate admission path {} is not a directory",
                    directory.display()
                ),
            )),
            Err(error) => Err(EvidenceStoreError::io(
                "inspect candidate admissions",
                &directory,
                error,
            )),
        };
    };
    if bundle.created_at() > now || now >= bundle.expires_at() {
        return Err(EvidenceStoreError::new(
            "gate-evidence::admission-candidate-mismatch",
            format!(
                "candidate {} on {} has no current admission",
                candidate.commit, candidate.branch
            ),
        ));
    }
    Ok(Some((id, bundle)))
}

pub(super) fn read_record(
    path: &std::path::Path,
    transaction: &AdmissionTransactionId,
) -> Result<Option<AdmissionConsumption>, EvidenceStoreError> {
    let Some(bytes) = read_optional_regular(path, MAX_RECEIPT_BYTES as u64)? else {
        return Ok(None);
    };
    let record: AdmissionConsumption = wire::decode(&bytes).map_err(|error| {
        EvidenceStoreError::new(
            "gate-evidence::admission-consumption-invalid",
            error.to_string(),
        )
    })?;
    validate(&record)?;
    if record.transaction != *transaction {
        return Err(EvidenceStoreError::new(
            "gate-evidence::admission-consumption-identity-mismatch",
            "admission consumption record does not match its transaction path",
        ));
    }
    if record.encode()? != bytes {
        return Err(EvidenceStoreError::new(
            "gate-evidence::admission-consumption-noncanonical",
            "admission consumption bytes are not canonical",
        ));
    }
    Ok(Some(record))
}

fn validate(record: &AdmissionConsumption) -> Result<(), EvidenceStoreError> {
    if record.version != CONSUMPTION_VERSION || record.entries.is_empty() {
        return Err(EvidenceStoreError::new(
            "gate-evidence::admission-consumption-invalid",
            "admission consumption has an unsupported version or no entries",
        ));
    }
    for entry in &record.entries {
        validate_candidate(&AdmissionCandidate {
            branch: entry.branch.clone(),
            parent: entry.parent.clone(),
            commit: entry.commit.clone(),
            tree: entry.tree.clone(),
        })?;
    }
    if record.entries.windows(2).any(|pair| {
        (&pair[0].branch, &pair[0].commit, &pair[0].tree)
            >= (&pair[1].branch, &pair[1].commit, &pair[1].tree)
    }) {
        return Err(EvidenceStoreError::new(
            "gate-evidence::admission-consumption-invalid",
            "admission consumption entries must be sorted and unique",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence_store::invariant_fixture::{Fixture, admission};

    #[test]
    fn transaction_identity_is_stable_and_order_sensitive() {
        let first = AdmissionTransactionId::derive(b"a\0b");
        assert_eq!(first, AdmissionTransactionId::derive(b"a\0b"));
        assert_ne!(first, AdmissionTransactionId::derive(b"b\0a"));
    }

    #[test]
    fn candidate_paths_and_git_identities_are_canonical_before_store_access() {
        for candidate in [
            AdmissionCandidate::new(
                "main",
                None,
                "1111111111111111111111111111111111111111",
                "2222222222222222222222222222222222222222",
            ),
            AdmissionCandidate::new("refs/heads/main", None, "not-an-object", "../../outside"),
        ] {
            assert_eq!(
                candidate.unwrap_err().reason(),
                "gate-evidence::admission-consumption-candidate-invalid"
            );
        }
    }

    #[test]
    fn mismatched_immutable_parent_refuses_before_pending_publication() {
        let fixture = Fixture::new("admission-mismatch");
        let store = fixture.store();
        let tree = "0123456789abcdef0123456789abcdef01234567";
        store
            .publish_admission(&admission(
                tree,
                Some("89abcdef0123456789abcdef0123456789abcdef"),
                b"parent authority",
                "2026-09-04T12:00:00.000Z",
                "2026-09-04T12:10:00.000Z",
            ))
            .unwrap();
        let transaction = AdmissionTransactionId::derive(b"mismatched transaction");
        let candidate = AdmissionCandidate::new(
            "refs/heads/main",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned()),
            "1111111111111111111111111111111111111111",
            tree,
        )
        .unwrap();
        let error = store
            .prepare_admission_consumption(
                &transaction,
                &[candidate],
                "2026-09-04T12:05:00.000Z".parse().unwrap(),
            )
            .unwrap_err();
        assert_eq!(
            error.reason(),
            "gate-evidence::admission-candidate-mismatch"
        );
        assert!(
            store
                .commit_admission_consumption(
                    &transaction,
                    "2026-09-04T12:05:00.000Z".parse().unwrap()
                )
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn current_admission_index_ignores_accumulated_history() {
        let fixture = Fixture::new("admission-current-index");
        let store = fixture.store();
        let tree = "0123456789abcdef0123456789abcdef01234567";
        let older = store
            .publish_admission(&admission(
                tree,
                None,
                b"older",
                "2026-09-04T12:00:00.000Z",
                "2026-09-04T12:20:00.000Z",
            ))
            .unwrap();
        let newer = store
            .publish_admission(&admission(
                tree,
                None,
                b"newer",
                "2026-09-04T12:05:00.000Z",
                "2026-09-04T12:15:00.000Z",
            ))
            .unwrap();
        assert_ne!(older, newer);
        for index in 0..16 {
            fs::write(
                store
                    .layout
                    .admission_tree_path(tree)
                    .join(format!("old-{index}")),
                b"malformed history",
            )
            .unwrap();
        }
        let transaction = AdmissionTransactionId::derive(b"indexed transaction");
        let candidate = AdmissionCandidate::new(
            "refs/heads/renamed",
            None,
            "1111111111111111111111111111111111111111",
            tree,
        )
        .unwrap();
        store
            .prepare_admission_consumption(
                &transaction,
                &[candidate],
                "2026-09-04T12:06:00.000Z".parse().unwrap(),
            )
            .unwrap();
        let credit = store
            .commit_admission_consumption(&transaction, "2026-09-04T12:06:00.000Z".parse().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(credit.admissions().collect::<Vec<_>>(), [&newer]);
    }

    #[test]
    fn interrupted_commit_replays_retained_pending_credit() {
        let fixture = Fixture::new("admission-credit-recovery");
        let store = fixture.store();
        let tree = "0123456789abcdef0123456789abcdef01234567";
        store
            .publish_admission(&admission(
                tree,
                None,
                b"credit recovery",
                "2026-09-04T12:00:00.000Z",
                "2026-09-04T12:10:00.000Z",
            ))
            .unwrap();
        let transaction = AdmissionTransactionId::derive(b"credit recovery transaction");
        let candidate = AdmissionCandidate::new(
            "refs/heads/main",
            None,
            "1111111111111111111111111111111111111111",
            tree,
        )
        .unwrap();
        store
            .prepare_admission_consumption(
                &transaction,
                &[candidate],
                "2026-09-04T12:05:00.000Z".parse().unwrap(),
            )
            .unwrap();
        let error = store
            .with_lock(|layout| {
                commit_with(
                    layout,
                    &transaction,
                    "2026-09-04T12:05:00.000Z".parse().unwrap(),
                    || {
                        Err(EvidenceStoreError::new(
                            "fixture::interrupted",
                            "stop after durable credit",
                        ))
                    },
                )
            })
            .unwrap_err();
        assert_eq!(error.reason(), "fixture::interrupted");
        let credit = store
            .with_lock(|layout| {
                read_record(&layout.admission_credit_path(&transaction), &transaction)
            })
            .unwrap();
        assert!(credit.is_some());
        assert!(
            store
                .commit_admission_consumption(
                    &transaction,
                    "2026-09-04T12:05:00.000Z".parse().unwrap()
                )
                .unwrap()
                .is_some()
        );
        assert!(!store.abort_admission_consumption(&transaction).unwrap());
    }
}
