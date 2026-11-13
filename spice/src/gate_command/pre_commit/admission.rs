//! Evidence admission for one exact staged candidate.

use gate::{AdmissionBundle, AdmissionBundleFields, EvidenceStore};
use serde::{Deserialize, Serialize};
use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::{Digest, Domain};

use super::super::prove::{fingerprint_generation_detail, missing_proof_detail};
use super::WorkerFailure;

const ADMISSION_LIFETIME_SECONDS: i64 = 600;
const ADMISSION_REUSE_WINDOW_SECONDS: i64 = 300;
const ADMISSION_STABILITY_ATTEMPTS: usize = 4;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AdmissionReport {
    source: Digest,
    candidate_tree: String,
    admission: gate::AdmissionId,
    actions: usize,
    live_checks: usize,
}

impl AdmissionReport {
    pub(super) const fn source(&self) -> &Digest {
        &self.source
    }
}

pub(super) struct AdmissionRun {
    report: AdmissionReport,
    staged: spice::change_gate::StagedActionSelection,
}

impl AdmissionRun {
    pub(super) fn into_parts(self) -> (AdmissionReport, spice::change_gate::StagedActionSelection) {
        (self.report, self.staged)
    }
}

pub(super) fn run(
    claim: gate::ClaimCapture,
    repository: &Repository,
    ambient: &Ambient,
    anchor: spice::gate::PreparedArm<'static>,
    audit: Option<&mut super::ApplianceGenerationAudit>,
    profile: &mut super::profile::Profile,
) -> Result<AdmissionRun, WorkerFailure> {
    let store =
        EvidenceStore::open(repository.common_dir()).map_err(|error| evidence_failure(&error))?;
    let receipt_first = matches!(claim, gate::ClaimCapture::Idle);
    let claim_digest = Digest::compute(
        Domain::GateRepositoryAuthority,
        &wire::encode(&claim)
            .map_err(|error| failure("gate::pre-commit-claim-capture-invalid", error, true))?,
    );
    let tool_started = std::time::Instant::now();
    let lookup = super::super::prove::prepare_admission_from_anchor(repository, anchor)
        .map_err(prove_failure)?;
    profile.tool_authority(tool_started.elapsed());
    let early_live_checks = if receipt_first {
        None
    } else {
        match lookup.execute_live_checks() {
            Ok(checks) => Some(checks),
            Err(error) => {
                let _ = lookup.reject_state();
                return Err(prove_failure(error));
            }
        }
    };
    let evidence_started = std::time::Instant::now();
    let resolution = lookup_evidence(&store, &lookup)?;
    profile.fingerprint_index_receipt_reads(evidence_started.elapsed());
    if let Some(audit) = audit {
        super::finish_appliance_generation_audit(audit, profile)?;
    }
    let remaining_started = std::time::Instant::now();
    let (manifest, resolved) = resolution.into_parts();
    let mut preparation = lookup.restore(&manifest).map_err(prove_failure)?;
    let missing = manifest
        .actions()
        .iter()
        .zip(&resolved)
        .filter_map(|(expected, observed)| observed.is_none().then_some(expected.action()))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let _ = preparation.reject_state();
        return Err(WorkerFailure {
            reason: "gate::pre-commit-proof-missing".to_owned(),
            detail: missing_proof_detail(&missing),
            fault: false,
        });
    }
    let actions = resolved
        .into_iter()
        .map(|action| action.expect("missing receipts were handled above"))
        .collect::<Vec<_>>();
    let live_checks = match settle_admission_state(
        &mut preparation,
        repository,
        ambient,
        &claim,
        early_live_checks,
    ) {
        Ok(checks) => checks,
        Err(error) => {
            let _ = preparation.reject_state();
            return Err(prove_failure(error));
        }
    };
    let (created_at, expires_at) = admission_window()?;
    let live_check_count = live_checks.len();
    let bundle = AdmissionBundle::new(AdmissionBundleFields {
        candidate_tree: preparation.candidate_tree().to_string(),
        parent: preparation.head(),
        selection: preparation.selection().clone(),
        policy: preparation.policy().clone(),
        gate_set: preparation.gate_set().clone(),
        actions,
        live_checks,
        claim: claim_digest,
        created_at,
        expires_at,
    })
    .map_err(|error| evidence_failure(&error))?;
    let admission = store
        .publish_admission(&bundle)
        .map_err(|error| evidence_failure(&error))?;
    let report = AdmissionReport {
        source: preparation.source().clone(),
        candidate_tree: preparation.candidate_tree().to_string(),
        admission,
        actions: manifest.actions().len(),
        live_checks: live_check_count,
    };
    profile.remaining_admission(remaining_started.elapsed());
    Ok(AdmissionRun {
        report,
        staged: preparation.into_staged(),
    })
}

fn settle_admission_state(
    preparation: &mut super::super::prove::AdmissionPreparation,
    repository: &Repository,
    ambient: &Ambient,
    claim: &gate::ClaimCapture,
    mut early_live_checks: Option<Vec<gate::AdmissionLiveCheck>>,
) -> Result<Vec<gate::AdmissionLiveCheck>, super::super::prove::ProveFailure> {
    for _ in 0..ADMISSION_STABILITY_ATTEMPTS {
        let live_checks = match early_live_checks.take() {
            Some(checks) => checks,
            None => preparation.execute_live_checks()?,
        };
        if preparation.verify_current(repository, ambient, claim)? {
            continue;
        }
        preparation.accept_state()?;
        if preparation.verify_current(repository, ambient, claim)? {
            continue;
        }
        return Ok(live_checks);
    }
    Err(super::super::prove::ProveFailure::refusal(
        "gate::source-moved",
        format!(
            "staged index did not stabilize after {ADMISSION_STABILITY_ATTEMPTS} semantic recaptures"
        ),
    ))
}

fn lookup_evidence(
    store: &EvidenceStore,
    lookup: &super::super::prove::AdmissionLookup,
) -> Result<gate::ActionFingerprintResolution, WorkerFailure> {
    match store.lookup_fingerprint_receipts(lookup.authority()) {
        Ok(Some(resolution)) => Ok(resolution),
        Ok(None) => {
            let authority = lookup.authority().key().map_err(|error| {
                let _ = lookup.reject_state();
                fingerprint_lookup_failure(&error)
            })?;
            let _ = lookup.reject_state();
            Err(WorkerFailure {
                reason: "gate::pre-commit-fingerprint-missing".to_owned(),
                detail: fingerprint_generation_detail(&authority),
                fault: false,
            })
        }
        Err(error) => {
            let _ = lookup.reject_state();
            Err(fingerprint_lookup_failure(&error))
        }
    }
}

fn fingerprint_lookup_failure(error: &gate::EvidenceStoreError) -> WorkerFailure {
    let reason = if error.reason().starts_with("gate-evidence::fingerprint") {
        "gate::pre-commit-fingerprint-invalid"
    } else {
        "gate::pre-commit-proof-invalid"
    };
    WorkerFailure {
        reason: reason.to_owned(),
        detail: format!(
            "reason={}; {}\ncommand=`spice gate prove --missing`",
            error.reason(),
            error.detail(),
        ),
        fault: false,
    }
}

fn admission_window() -> Result<(wire::Instant, wire::Instant), WorkerFailure> {
    admission_window_at(jiff::Timestamp::now())
}

fn admission_window_at(
    observed: jiff::Timestamp,
) -> Result<(wire::Instant, wire::Instant), WorkerFailure> {
    let epoch_second = observed
        .as_second()
        .div_euclid(ADMISSION_REUSE_WINDOW_SECONDS)
        * ADMISSION_REUSE_WINDOW_SECONDS;
    let created = jiff::Timestamp::new(epoch_second, 0)
        .map_err(|error| failure("gate::pre-commit-clock-invalid", error, true))?;
    let expires = created
        .checked_add(jiff::SignedDuration::from_secs(ADMISSION_LIFETIME_SECONDS))
        .map_err(|error| failure("gate::pre-commit-clock-invalid", error, true))?;
    let created = wire::Instant::new(created)
        .map_err(|error| failure("gate::pre-commit-clock-invalid", error, true))?;
    let expires = wire::Instant::new(expires)
        .map_err(|error| failure("gate::pre-commit-clock-invalid", error, true))?;
    Ok((created, expires))
}

fn evidence_failure(error: &gate::EvidenceStoreError) -> WorkerFailure {
    let reason = if error.reason() == "gate-evidence::receipt-authority-mismatch" {
        "gate::pre-commit-proof-authority-mismatch"
    } else {
        "gate::pre-commit-evidence-corrupt"
    };
    WorkerFailure {
        reason: reason.to_owned(),
        detail: format!("reason={}; {}", error.reason(), error.detail()),
        fault: false,
    }
}

fn prove_failure(error: super::super::prove::ProveFailure) -> WorkerFailure {
    let reason = match error.reason() {
        "gate::source-moved" | "gate::prove-source-moved" => "gate::pre-commit-staged-source-moved",
        "gate::prove-action-not-reusable" | "gate::prove-cargo-action-not-reusable" => {
            "gate::pre-commit-proof-closure-mismatch"
        }
        reason => reason,
    };
    WorkerFailure {
        reason: reason.to_owned(),
        detail: error.detail().to_owned(),
        fault: error.is_fault(),
    }
}

fn failure(reason: &str, detail: impl std::fmt::Display, fault: bool) -> WorkerFailure {
    WorkerFailure {
        reason: reason.to_owned(),
        detail: detail.to_string(),
        fault,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_retries_share_one_bounded_time_epoch() {
        let first = jiff::Timestamp::new(601, 1).expect("valid first timestamp");
        let second = jiff::Timestamp::new(899, 999_999_999).expect("valid second timestamp");

        let first = admission_window_at(first).expect("first admission window");
        let second = admission_window_at(second).expect("second admission window");

        assert_eq!(first, second);
        assert_eq!(first.0.get().as_second(), 600);
        assert_eq!(first.1.get().as_second(), 1_200);
    }
}
