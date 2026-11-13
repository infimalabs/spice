//! Receipt-only proof inspection without action planning or execution.

use super::*;

pub(super) fn current(
    repository: &Repository,
    ambient: &Ambient,
    store: &EvidenceStore,
    candidate_tree: &spice::gate::CandidateTree,
    anchor: &spice::gate::PreparedArm<'_>,
    authority: &ActionFingerprintAuthority,
    snapshot: gate::tree::WorktreeDivergence,
) -> Result<ProveReport, ProveFailure> {
    let resolution = match store.lookup_fingerprint_receipts(authority) {
        Ok(Some(resolution)) => resolution,
        Ok(None) => return missing_fingerprint(anchor, authority),
        Err(error) => {
            let _ = anchor.reject_state();
            return Err(evidence_lookup_failure(error));
        }
    };
    let (manifest, resolved) = resolution.into_parts();
    let missing = manifest
        .actions()
        .iter()
        .zip(&resolved)
        .filter_map(|(expected, observed)| observed.is_none().then_some(expected.action()))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let _ = anchor.reject_state();
        return Err(ProveFailure::refusal(
            "gate::prove-receipt-missing",
            missing_proof_detail(&missing),
        ));
    }
    let mut actions = manifest
        .actions()
        .iter()
        .zip(resolved)
        .map(|(expected, observed)| {
            ProveActionReport::present(
                expected.action().to_owned(),
                expected.action_key().clone(),
                observed
                    .expect("missing inspection receipts were handled above")
                    .receipt()
                    .clone(),
            )
        })
        .collect::<Vec<_>>();
    actions.sort_by(|left, right| proof_action_order(&left.action, &right.action));
    if let Err(error) = validate_source(repository, ambient, anchor, candidate_tree) {
        let _ = anchor.reject_state();
        return Err(error);
    }
    anchor.reject_state().map_err(ProveFailure::run)?;
    Ok(ProveReport::new(
        candidate_tree.to_string(),
        authority.source().clone(),
        snapshot,
        actions,
    ))
}

fn missing_fingerprint(
    anchor: &spice::gate::PreparedArm<'_>,
    authority: &ActionFingerprintAuthority,
) -> Result<ProveReport, ProveFailure> {
    let key = authority.key().map_err(|error| {
        let _ = anchor.reject_state();
        evidence_lookup_failure(error)
    })?;
    let _ = anchor.reject_state();
    Err(ProveFailure::refusal(
        "gate::prove-fingerprint-missing",
        fingerprint_generation_detail(&key),
    ))
}

pub(in crate::gate_command) fn missing_proof_detail(actions: &[&str]) -> String {
    let mut detail = actions
        .iter()
        .map(|action| {
            format!("action={action} reason=missing-proof command=`spice gate prove {action}`")
        })
        .collect::<Vec<_>>()
        .join("\n");
    detail.push_str("\naggregate-command=`spice gate prove --missing`");
    detail
}

pub(in crate::gate_command) fn fingerprint_generation_detail(authority: &ActionKey) -> String {
    format!("reason=missing-fingerprint authority={authority} command=`spice gate prove --missing`")
}
