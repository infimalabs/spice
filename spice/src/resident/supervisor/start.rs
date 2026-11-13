//! The resident's wait for a detached supervisor to publish its start.

use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use wire::CanonicalUuid;

use super::events::StateSignal;
use super::{
    CommandResult, Phase, State, environment_fault, matches_state, read_state, store_fault,
};

/// Wait until the supervisor at `path` publishes a running or exited start.
///
/// `wait` blocks for the next observer signal, answering `None` when the given
/// duration passes without one. Signals only prompt a read: the published
/// state is the authority, so the horizon reads it once more before reporting
/// a timeout, and a start the observer never announced still surfaces.
pub(super) fn await_published(
    path: &Path,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    horizon: Duration,
    mut wait: impl FnMut(Duration) -> io::Result<Option<StateSignal>>,
) -> CommandResult<State> {
    let started = Instant::now();
    let mut supervisor_exited = false;
    let mut horizon_passed = false;
    loop {
        if let Some(state) = published(path, lane, occupant)? {
            return Ok(state);
        }
        if supervisor_exited {
            return Err(environment_fault(
                "lane::supervisor-start-failed",
                "detached supervisor exited before publishing a proven session",
            ));
        }
        let remaining = horizon.saturating_sub(started.elapsed());
        if horizon_passed || remaining.is_zero() {
            let directory = path.parent().unwrap_or(path);
            return Err(environment_fault(
                "lane::supervisor-start-timeout",
                &format!(
                    "detached supervisor did not publish a proven session before its start \
                     horizon; its state.json and supervisor.log are in {}",
                    directory.display()
                ),
            ));
        }
        match wait(remaining) {
            Ok(Some(StateSignal::Changed)) => {}
            Ok(Some(StateSignal::SupervisorExited)) => supervisor_exited = true,
            Ok(Some(StateSignal::ObserverFailed(error))) => {
                return Err(environment_fault(
                    "lane::supervisor-observer-failed",
                    &error,
                ));
            }
            Err(error) => {
                return Err(environment_fault(
                    "lane::supervisor-observer-failed",
                    &error.to_string(),
                ));
            }
            Ok(None) => horizon_passed = true,
        }
    }
}

/// The published start, if the supervisor has finished starting.
fn published(
    path: &Path,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> CommandResult<Option<State>> {
    match read_state(path) {
        Ok(state) if !matches_state(&state, lane, occupant) => Err(store_fault(
            "lane::supervisor-state-mismatched",
            "detached supervisor published another lane or occupant",
        )),
        Ok(state) if matches!(state.phase, Phase::Running | Phase::Idle | Phase::Exited) => {
            Ok(Some(state))
        }
        Ok(state) if state.phase == Phase::Failed => Err(store_fault(
            "lane::supervisor-start-failed",
            state
                .failure
                .as_deref()
                .unwrap_or("supervisor failed without detail"),
        )),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(environment_fault(
            "lane::supervisor-state-unavailable",
            &error.to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::super::{CompletionField, STATE_VERSION, now_millis, publish_json};
    use super::*;

    fn uuid(last: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131165{last:02}")
            .parse()
            .expect("fixture UUID")
    }

    fn state(lane: CanonicalUuid, occupant: CanonicalUuid, phase: Phase) -> State {
        State {
            version: STATE_VERSION,
            supervisor: i32::MAX.unsigned_abs(),
            worktree: "/fixture".to_owned(),
            vendor: "omp".to_owned(),
            lane,
            occupant,
            phase,
            group: Some(i32::MAX.unsigned_abs()),
            session: None,
            claim: None,
            git: Some("fixture-head".to_owned()),
            started_millis: None,
            runtime_millis: None,
            grace_millis: None,
            reap_millis: None,
            completion: CompletionField::none(),
            capture_drained: false,
            failure: None,
        }
    }

    fn supervisor_directory(tag: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "spice-supervisor-start-{tag}-{}-{}",
            std::process::id(),
            now_millis().expect("fixture time")
        ));
        fs::create_dir_all(&directory).expect("supervisor directory");
        directory
    }

    /// Wait as an observer that never delivers a signal: the supervisor
    /// publishes `published` during the wait, and the horizon then passes.
    fn unannounced(
        path: &Path,
        published: Option<State>,
    ) -> impl FnMut(Duration) -> io::Result<Option<StateSignal>> {
        let mut published = published;
        move |_| {
            if let Some(state) = published.take() {
                publish_json(path, &state).expect("state publishes");
            }
            Ok(None)
        }
    }

    fn fault(outcome: CommandResult<State>) -> wire::Fault {
        match outcome {
            Err(wire::Outcome::Fault(fault)) => fault,
            Err(_) => panic!("start resolved to a non-fault outcome"),
            Ok(state) => panic!("start resolved to phase {:?}", state.phase),
        }
    }

    #[test]
    fn a_failure_published_without_a_signal_surfaces_at_the_horizon() {
        let directory = supervisor_directory("failed");
        let path = directory.join("state.json");
        let (lane, occupant) = (uuid(1), uuid(2));
        let mut failed = state(lane, occupant, Phase::Failed);
        failed.failure = Some("HOME is not set".to_owned());
        publish_json(&path, &state(lane, occupant, Phase::Starting)).expect("starting publishes");

        let fault = fault(await_published(
            &path,
            lane,
            occupant,
            Duration::from_secs(60),
            unannounced(&path, Some(failed)),
        ));

        assert_eq!(fault.reason, "lane::supervisor-start-failed");
        assert_eq!(fault.detail, "HOME is not set");
        fs::remove_dir_all(directory).expect("fixture cleanup");
    }

    #[test]
    fn a_running_start_published_without_a_signal_is_the_launch() {
        let directory = supervisor_directory("running");
        let path = directory.join("state.json");
        let (lane, occupant) = (uuid(3), uuid(4));

        let started = await_published(
            &path,
            lane,
            occupant,
            Duration::from_secs(60),
            unannounced(&path, Some(state(lane, occupant, Phase::Running))),
        )
        .unwrap_or_else(|_| panic!("a running start is the launch"));

        assert_eq!(started.phase, Phase::Running);
        assert_eq!((started.lane, started.occupant), (lane, occupant));
        fs::remove_dir_all(directory).expect("fixture cleanup");
    }

    #[test]
    fn a_start_never_published_times_out_naming_its_directory() {
        let directory = supervisor_directory("silent");
        let path = directory.join("state.json");
        let (lane, occupant) = (uuid(5), uuid(6));
        publish_json(&path, &state(lane, occupant, Phase::Starting)).expect("starting publishes");

        let fault = fault(await_published(
            &path,
            lane,
            occupant,
            Duration::from_secs(60),
            unannounced(&path, None),
        ));

        assert_eq!(fault.reason, "lane::supervisor-start-timeout");
        assert!(
            fault.detail.contains(&directory.display().to_string()),
            "{}",
            fault.detail
        );
        fs::remove_dir_all(directory).expect("fixture cleanup");
    }
}
