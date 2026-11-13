//! Supervisor records a test leaves behind without running a supervisor.

use std::io;

use wire::CanonicalUuid;

use super::{
    CompletionField, PersistedCompletion, Phase, RuntimePaths, STATE_VERSION, State, now_millis,
    publish_json, supervisor_directory,
};

/// Leave the record a supervisor writes once its agent has exited, standing
/// in for a reap the resident never got to account.
pub(in crate::resident) fn record_exited_for_tests(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    session: wire::LaneSession,
) -> io::Result<()> {
    let mut state = ended_state(lane, occupant, session, Phase::Exited)?;
    state.completion = CompletionField::some(PersistedCompletion::Exited { status_raw: 0 });
    publish_state(paths, state)
}

/// Leave the record a supervisor writes when it could not keep its agent,
/// standing in for a failure notice the resident never received.
pub(in crate::resident) fn record_failed_for_tests(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    session: wire::LaneSession,
    failure: &str,
) -> io::Result<()> {
    let mut state = ended_state(lane, occupant, session, Phase::Failed)?;
    state.failure = Some(failure.to_owned());
    publish_state(paths, state)
}

fn ended_state(
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    session: wire::LaneSession,
    phase: Phase,
) -> io::Result<State> {
    Ok(State {
        version: STATE_VERSION,
        // No process can carry the largest positive pid, so the record reads
        // as one whose supervisor sent its notice and is gone.
        supervisor: i32::MAX.unsigned_abs(),
        worktree: session.worktree.clone(),
        vendor: session.vendor.as_str().to_owned(),
        lane,
        occupant,
        phase,
        // No live process group can carry this id, so nothing a test reaps
        // through this record could ever be signalled.
        group: Some(i32::MAX.unsigned_abs()),
        session: Some(session),
        claim: None,
        git: Some("fixture-head".to_owned()),
        started_millis: Some(now_millis()?),
        runtime_millis: Some(4 * 60 * 60 * 1_000),
        grace_millis: Some(20_000),
        reap_millis: Some(20_000),
        completion: CompletionField::none(),
        failure: None,
        capture_drained: false,
    })
}

fn publish_state(paths: &RuntimePaths, state: State) -> io::Result<()> {
    let directory = supervisor_directory(paths, state.lane, state.occupant);
    std::fs::create_dir_all(&directory)?;
    publish_json(&directory.join("state.json"), &state)
}
