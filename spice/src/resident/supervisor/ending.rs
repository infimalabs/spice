//! Endings a detached supervisor published that its notice never delivered.
//!
//! A supervisor tells the resident its agent ended with one unbound datagram,
//! sent once and never confirmed. The durable state beside it is what a
//! restart adopts from, so the same record lets a running resident judge a
//! seated occupant whose notice was lost instead of holding the lane until
//! the next restart happens to sweep it.

use std::io;
use std::path::PathBuf;

use wire::CanonicalUuid;

use super::{
    CommandResult, Phase, RuntimePaths, State, environment_fault, matches_state, process_exists,
    read_state, store_fault, supervisor_directory,
};

pub(super) const SUPERVISOR_DISAPPEARED: &str =
    "detached supervisor disappeared while its agent was running";
pub(super) const FAILED_WITHOUT_DETAIL: &str = "detached supervisor failed without detail";
pub(super) const DID_NOT_FINISH_STARTING: &str = "detached supervisor did not finish starting";

/// How a detached supervisor says its agent ended, read from durable state.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::resident) enum PublishedEnding {
    /// The supervisor reaped its agent and recorded the exit.
    Exited,
    /// The supervisor recorded a failure, or vanished mid-run without one.
    Failed(String),
}

/// The durable state one supervisor published for one lane occupant.
///
/// Absent state is not a fault: a launch that never reached its supervisor
/// leaves nothing behind. State naming another lane or occupant is refused,
/// because a seat and the record beneath it must agree before either is
/// acted on.
pub(super) fn durable_state(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> CommandResult<Option<(PathBuf, State)>> {
    let path = supervisor_directory(paths, lane, occupant).join("state.json");
    let state = match read_state(&path) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(environment_fault(
                "lane::supervisor-state-unavailable",
                &error.to_string(),
            ));
        }
    };
    if !matches_state(&state, lane, occupant) {
        return Err(store_fault(
            "lane::supervisor-state-mismatched",
            "detached supervisor state names another lane or occupant",
        ));
    }
    Ok(Some((path, state)))
}

pub(super) fn failure_detail(failure: Option<String>) -> String {
    failure.unwrap_or_else(|| FAILED_WITHOUT_DETAIL.to_owned())
}

/// Judge whether a seated occupant already ended, by its supervisor state.
///
/// This reads the record a restart adopts from without arming an observer,
/// so a lane that is still live costs one file read and one liveness probe.
/// A supervisor that is still alive is left to its notice whatever it has
/// published, because it sends that notice the moment its record lands and
/// exits right after; the sweep speaks only for one that is gone. A running
/// record whose supervisor is gone is the same failure adoption reports.
pub(in crate::resident) fn published_ending(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> CommandResult<Option<PublishedEnding>> {
    let Some((_, state)) = durable_state(paths, lane, occupant)? else {
        return Ok(None);
    };
    if matches!(state.phase, Phase::Starting) || process_exists(state.supervisor)? {
        return Ok(None);
    }
    let ending = match state.phase {
        Phase::Exited => PublishedEnding::Exited,
        Phase::Failed => PublishedEnding::Failed(failure_detail(state.failure)),
        Phase::Running | Phase::Idle | Phase::Starting => {
            PublishedEnding::Failed(SUPERVISOR_DISAPPEARED.to_owned())
        }
    };
    Ok(Some(ending))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::super::test_support::record_exited_for_tests;
    use super::super::{
        CompletionField, PersistedCompletion, STATE_VERSION, command, now_millis, publish_json,
    };
    use super::*;

    fn uuid(last: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131165{last:02}")
            .parse()
            .expect("fixture UUID")
    }

    fn paths(root: &Path) -> RuntimePaths {
        RuntimePaths {
            worktree: root.to_path_buf(),
            common: root.join("common"),
            runtime: root.join("runtime"),
            live: root.join("runtime").join("runtime"),
            data: root.join("data"),
            journal: root.join("journal"),
            resident: root.join("resident"),
            serve: root.join("serve"),
            lane_serve: root.join("lane-serve"),
            degraded: root.join("degraded"),
            tape: root.join("lanes"),
            supervisors: root.join("lanes"),
            steering_media: root.join("lanes"),
            stage: root.join("stage"),
        }
    }

    fn session(root: &Path, lane: CanonicalUuid, occupant: CanonicalUuid) -> wire::LaneSession {
        wire::LaneSession {
            lane,
            occupant,
            worktree: root.to_string_lossy().into_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
            epoch: uuid(9),
            stream: "stdout".to_owned(),
            identity: "ending-session".to_owned(),
            locus: wire::TapeLocus {
                member: 0,
                offset: 0,
                length: 1,
            },
        }
    }

    fn running_state(
        root: &Path,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        supervisor: u32,
    ) -> State {
        State {
            version: STATE_VERSION,
            supervisor,
            worktree: root.to_string_lossy().into_owned(),
            vendor: "omp".to_owned(),
            lane,
            occupant,
            phase: Phase::Running,
            group: Some(412),
            session: Some(session(root, lane, occupant)),
            claim: None,
            git: Some("fixture-head".to_owned()),
            started_millis: Some(now_millis().expect("fixture time")),
            runtime_millis: Some(4 * 60 * 60 * 1_000),
            grace_millis: Some(20_000),
            reap_millis: Some(20_000),
            completion: CompletionField::none(),
            failure: None,
            capture_drained: false,
        }
    }

    fn fixture_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "spice-published-ending-{tag}-{}-{}",
            std::process::id(),
            now_millis().expect("fixture time")
        ));
        fs::create_dir_all(&root).expect("fixture root");
        root
    }

    #[test]
    fn an_absent_record_and_a_live_supervisor_are_left_to_their_notice() {
        let root = fixture_root("live");
        let paths = paths(&root);
        let (lane, occupant) = (uuid(1), uuid(2));
        assert_eq!(
            published_ending(&paths, lane, occupant).expect("absent state reads"),
            None
        );

        let directory = supervisor_directory(&paths, lane, occupant);
        fs::create_dir_all(&directory).expect("supervisor directory");
        let live = running_state(&root, lane, occupant, std::process::id());
        publish_json(&directory.join("state.json"), &live).expect("running state publishes");
        assert_eq!(
            published_ending(&paths, lane, occupant).expect("running state reads"),
            None
        );

        // Whatever a live supervisor has published, its notice is on the way.
        let mut starting = live.clone();
        starting.phase = Phase::Starting;
        let mut exited = live.clone();
        exited.phase = Phase::Exited;
        exited.completion = CompletionField::some(PersistedCompletion::Exited { status_raw: 0 });
        let mut failed = live;
        failed.phase = Phase::Failed;
        failed.failure = Some("agent refused to start".to_owned());
        for published in [starting, exited, failed] {
            publish_json(&directory.join("state.json"), &published).expect("state publishes");
            assert_eq!(
                published_ending(&paths, lane, occupant).expect("live state reads"),
                None,
                "{:?}",
                published.phase
            );
        }
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn a_drain_interrupts_a_driven_occupant_at_its_door_before_any_signal() {
        let root = fixture_root("drain");
        let paths = paths(&root);
        let (lane, occupant) = (uuid(1), uuid(2));
        let directory = supervisor_directory(&paths, lane, occupant);
        fs::create_dir_all(&directory).expect("supervisor directory");
        let live = running_state(&root, lane, occupant, std::process::id());
        let state = directory.join("state.json");
        publish_json(&state, &live).expect("running state publishes");
        let door_path = command::kick_path(&paths, lane);
        let door = door::Door::bind(&door_path, 256).expect("door binds");

        let launch = super::super::state_launch(&paths, state, live).expect("live adopts");
        launch
            .process
            .terminate()
            .expect("the door takes the interrupt, so no signal is sent");
        let kick = door.recv(None).expect("the interrupt arrives");
        assert_eq!(
            serde_json::from_slice::<command::Kick>(&kick).expect("a kick"),
            command::Kick::Abort
        );
        drop(door);
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn exited_failed_and_vanished_supervisors_publish_their_ending() {
        let root = fixture_root("ended");
        let paths = paths(&root);
        let (lane, occupant) = (uuid(3), uuid(4));
        record_exited_for_tests(&paths, lane, occupant, session(&root, lane, occupant))
            .expect("exit record writes");
        assert_eq!(
            published_ending(&paths, lane, occupant).expect("exited state reads"),
            Some(PublishedEnding::Exited)
        );

        let directory = supervisor_directory(&paths, lane, occupant);
        let mut failed = running_state(&root, lane, occupant, i32::MAX.unsigned_abs());
        failed.phase = Phase::Failed;
        failed.failure = Some("agent refused to start".to_owned());
        publish_json(&directory.join("state.json"), &failed).expect("failed state publishes");
        assert_eq!(
            published_ending(&paths, lane, occupant).expect("failed state reads"),
            Some(PublishedEnding::Failed("agent refused to start".to_owned()))
        );

        // No process can carry the largest positive pid, so a running record
        // naming it is a supervisor that died before publishing an ending.
        let vanished = running_state(&root, lane, occupant, i32::MAX.unsigned_abs());
        publish_json(&directory.join("state.json"), &vanished).expect("vanished state publishes");
        assert_eq!(
            published_ending(&paths, lane, occupant).expect("vanished state reads"),
            Some(PublishedEnding::Failed(SUPERVISOR_DISAPPEARED.to_owned()))
        );
        fs::remove_dir_all(root).expect("fixture cleanup");
    }

    #[test]
    fn a_record_naming_another_occupant_is_refused_as_a_store_fault() {
        let root = fixture_root("mismatch");
        let paths = paths(&root);
        let (lane, occupant, other) = (uuid(5), uuid(6), uuid(7));
        let directory = supervisor_directory(&paths, lane, occupant);
        fs::create_dir_all(&directory).expect("supervisor directory");
        let foreign = running_state(&root, lane, other, std::process::id());
        publish_json(&directory.join("state.json"), &foreign).expect("foreign state publishes");
        let refused = published_ending(&paths, lane, occupant)
            .expect_err("a record naming another occupant is refused");
        assert!(
            format!("{refused:?}").contains("lane::supervisor-state-mismatched"),
            "{refused:?}"
        );
        fs::remove_dir_all(root).expect("fixture cleanup");
    }
}
