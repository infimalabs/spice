use super::*;

/// The commit instant of `statement`'s topic-origin fact:
/// what the allocator's overdue tier measures a row's wait from.
pub(crate) fn topic_origin_at(
    facts: &bus::TaskFacts,
    statement: &wire::TaskHead,
) -> Option<wire::Instant> {
    facts.at(statement.topic_origin).map(|envelope| envelope.at)
}

/// Project the current allocation hold from durable transition evidence.
///
/// A completion that published repository work asks for independent ownership
/// of the phase it entered. A completion that carried its claim into the next
/// phase needs no hold: the claim itself is the assignment. Task birth and an
/// unrecorded transition into the final declared phase carry no inferred
/// handoff preference. A finding review re-anchors
/// the hold at the finding: the published authors and every reviewer who
/// raised a finding on the phase hand its re-review to a peer first, and stay
/// the allocator's last resort after.
pub(crate) fn phase_handoff(
    facts: &bus::TaskFacts,
    heads: &journal::TaskHeads,
) -> Result<Option<board::PhaseHandoff>, Outcome<CommandAnswer>> {
    let task = &heads.statement().body().born;
    let findings = facts.finding_reviewers(task);
    let (authors, at) = match (phased_completion(facts, heads)?, findings.last()) {
        (None, None) => return Ok(None),
        (Some(phased), None) => phased,
        (phased, Some((finding, _))) => {
            let at = facts
                .at(*finding)
                .ok_or_else(|| {
                    protocol_fault(
                        "board::phase-handoff-finding-missing",
                        "a finding review the authorship index recorded is not in the journal",
                    )
                })?
                .at;
            let authors =
                phased.map_or_else(|| facts.published_authors(task), |(authors, _)| authors);
            (authors, at)
        }
    };
    let prior = authors
        .into_iter()
        .chain(findings.iter().map(|(_, reviewer)| reviewer.clone()));
    Ok(Some(board::PhaseHandoff::from_authors(
        prior,
        handoff_until(at, board::HANDOFF_SECONDS)?,
        handoff_until(at, board::PEER_HANDOFF_SECONDS)?,
    )))
}

/// The authors of the published completion that moved the workflow into its
/// current phase, with the instant it did; `None` when the workflow head is
/// not such a transition.
fn phased_completion(
    facts: &bus::TaskFacts,
    heads: &journal::TaskHeads,
) -> Result<Option<(Vec<Actor>, Instant)>, Outcome<CommandAnswer>> {
    // The phase-zero advance out of draft is the author's act, never a
    // completion handing work to a peer.
    if heads.workflow().body().flow.get(1) == Some(&heads.workflow().body().phase) {
        return Ok(None);
    }
    let workflow = facts
        .at(heads.workflow().position())
        .filter(|envelope| envelope.fact.kind() == wire::FactKind::TaskPhased);
    let Some(workflow) = workflow else {
        return Ok(None);
    };
    let cause = workflow.cause.and_then(|cause| facts.at(cause));
    let completed = cause.ok_or_else(|| {
        protocol_fault(
            "board::phase-handoff-cause-missing",
            "a task phase head has no causal task completion",
        )
    })?;
    let authors = if let Some(done) = completed.fact.read::<body::TaskDone>() {
        if !publishes(&done) {
            return Ok(None);
        }
        vec![completed.actor.clone()]
    } else if let Some(round) = completed.fact.read::<body::TaskRoundCompleted>() {
        if !round.participants.iter().any(|participant| {
            participant
                .git
                .as_ref()
                .is_some_and(|boundary| boundary.local_commits > 0)
        }) {
            return Ok(None);
        }
        round
            .participants
            .into_iter()
            .map(|participant| participant.actor)
            .collect()
    } else {
        return Err(protocol_fault(
            "board::phase-handoff-completion-invalid",
            "a task phase cause is not a registered completion fact",
        ));
    };
    Ok(Some((authors, workflow.at)))
}

fn handoff_until(at: Instant, seconds: i64) -> Result<Instant, Outcome<CommandAnswer>> {
    let until = at
        .get()
        .checked_add(jiff::SignedDuration::from_secs(seconds))
        .map_err(|error| environment_fault("board::phase-handoff-range", error.to_string()))?;
    Instant::new(until)
        .map_err(|error| environment_fault("board::phase-handoff-invalid", error.to_string()))
}

/// Whether a completion published repository history, which asks the phase it
/// entered to go to a peer.
fn publishes(done: &body::TaskDone) -> bool {
    done.git
        .as_ref()
        .is_some_and(|boundary| boundary.local_commits > 0)
}
