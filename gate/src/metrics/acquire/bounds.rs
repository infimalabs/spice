//! Bounded metric rows and the exact observations their decisions consumed.

use super::*;

/// One measured quantity's row, judged against the limit it was decided under.
///
/// A bound that cannot be resolved, bound, or decided is this row's own
/// unavailability rather than the family's refusal, matching what an unreadable
/// file gets: the paths whose bounds did resolve still speak for the arm.
pub(super) fn bounded_row(
    source: &MetricSource<'_>,
    path: &str,
    key: &MetricKey,
    metric: MetricId,
    value: u64,
    committed: Option<u64>,
) -> Result<(MetricEvidence, Option<(BoundDecision, LatchObservation)>), MetricAcquisitionError> {
    match bounded_evidence(source, path, key, metric, value, committed) {
        Ok((evidence, decision)) => Ok((evidence, Some(decision))),
        Err(reason) => {
            MetricEvidence::new(metric, key.clone(), EvidenceState::Unavailable { reason })
                .map(|evidence| (evidence, None))
                .map_err(MetricAcquisitionError::Evidence)
        }
    }
}

/// Resolve one path's rule, decide its bound, and carry both on the value.
fn bounded_evidence(
    source: &MetricSource<'_>,
    path: &str,
    key: &MetricKey,
    metric: MetricId,
    value: u64,
    committed: Option<u64>,
) -> Result<(MetricEvidence, (BoundDecision, LatchObservation)), String> {
    let name = metric.as_str();
    let rule = resolve_metric_rule(
        source.contract.rule_policy(),
        metric,
        &PolicyContext::new().with_path(path),
    )
    .map_err(|error| format!("{name} bound did not resolve for {path:?}: {error}"))?;
    let committed_over_ratchet = rule.changed_committed_ratchet_breach(committed, value);
    let binding = MetricStateBinding::new(
        metric,
        key.clone(),
        value,
        ParserEvidence::Native {
            parser: ParserId::Utf8FileShapeV1,
        },
        rule,
    )
    .map_err(|error| format!("{name} did not bind for {path:?}: {error}"))?;
    let actor_inputs = source
        .actor
        .actor_inputs(path)
        .map_err(|error| format!("actor provenance for selected source {path:?}: {error}"))?;
    let (decision, observation) = binding
        .observe_decision(source.latches, committed_over_ratchet, &actor_inputs)
        .map_err(|error| format!("active {name} bound for {path:?}: {error}"))?;
    let evidence = binding
        .judged(source.latches, &decision)
        .map_err(|error| format!("{name} evidence for {path:?}: {error}"))?;
    Ok((evidence, (decision, observation)))
}

/// One routine's bounded row and exact durable-state observation.
pub(super) fn routine_evidence(
    source: &MetricSource<'_>,
    path: &str,
    function: &str,
    metric: MetricId,
    value: u64,
    committed: Option<u64>,
) -> Result<(MetricEvidence, Option<(BoundDecision, LatchObservation)>), MetricAcquisitionError> {
    let key = MetricKey::function(path, function).map_err(MetricAcquisitionError::Evidence)?;
    match bound_routine(source, path, key.clone(), metric, value, committed) {
        Ok((evidence, decision)) => Ok((evidence, Some(decision))),
        Err(reason) => MetricEvidence::new(metric, key, EvidenceState::Unavailable { reason })
            .map(|evidence| (evidence, None))
            .map_err(MetricAcquisitionError::Evidence),
    }
}

fn bound_routine(
    source: &MetricSource<'_>,
    path: &str,
    key: MetricKey,
    metric: MetricId,
    value: u64,
    committed: Option<u64>,
) -> Result<(MetricEvidence, (BoundDecision, LatchObservation)), String> {
    let rule = resolve_metric_rule(
        source.contract.rule_policy(),
        metric,
        &PolicyContext::new().with_path(path),
    )
    .map_err(|error| {
        format!(
            "{} bound did not resolve for {path:?}: {error}",
            metric.as_str()
        )
    })?;
    let committed_over_ratchet = rule.changed_committed_ratchet_breach(committed, value);
    let binding = MetricStateBinding::new(
        metric,
        key,
        value,
        ParserEvidence::Native {
            parser: ParserId::TreeSitterComplexityV1,
        },
        rule,
    )
    .map_err(|error| format!("{} did not bind for {path:?}: {error}", metric.as_str()))?;
    let actor_inputs = source
        .actor
        .actor_inputs(path)
        .map_err(|error| format!("actor provenance for {path:?}: {error}"))?;
    let (decision, observation) = binding
        .observe_decision(source.latches, committed_over_ratchet, &actor_inputs)
        .map_err(|error| {
            format!(
                "{} bound did not decide for {path:?}: {error}",
                metric.as_str()
            )
        })?;
    let evidence = binding
        .judged(source.latches, &decision)
        .map_err(|error| format!("{} evidence for {path:?}: {error}", metric.as_str()))?;
    Ok((evidence, (decision, observation)))
}
