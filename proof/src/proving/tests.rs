use std::cell::Cell;

use wire::manifest::ReleaseTarget;

use super::ProofError;
use super::fixture::Fixture;
use crate::evidence::admit_receipt;
use crate::replay::{CheckKind, CheckStanding, ReplayError};

#[test]
fn a_clean_replay_runs_once_and_verifies_the_receipt_it_derives() {
    let fixture = Fixture::new(ReleaseTarget::X86_64UnknownLinuxMusl);
    let replay = fixture.clean_replay();
    let runs = Cell::new(0);

    let proved = fixture
        .staged()
        .bind(fixture.target, || {
            runs.set(runs.get() + 1);
            Ok(replay.clone())
        })
        .expect("clean replay proves");
    let receipt = admit_receipt(&wire::encode(&fixture.receipt(&replay)).expect("receipt encodes"))
        .expect("derived receipt admits");
    let evidence = proved.verify(&receipt).expect("derived receipt verifies");

    assert_eq!(runs.get(), 1);
    assert_eq!(evidence.target(), fixture.target);
    assert_eq!(evidence.payload_digest(), proved.payload().digest());
    assert_eq!(evidence.receipt_digest(), receipt.digest());
    assert_eq!(evidence.battery_digest(), replay.battery_digest());
    assert_eq!(evidence.replay_digest(), replay.replay_digest());
}

#[test]
fn target_mismatch_infrastructure_and_non_clean_standings_stay_distinct() {
    let fixture = Fixture::new(ReleaseTarget::X86_64UnknownLinuxMusl);
    let ran = Cell::new(false);
    let mismatch = fixture
        .staged()
        .bind(ReleaseTarget::Aarch64AppleDarwin, || {
            ran.set(true);
            Ok(fixture.clean_replay())
        })
        .err()
        .expect("environment target mismatch refuses");
    assert!(matches!(mismatch, ProofError::EnvironmentTarget { .. }));
    assert!(!ran.get(), "a mismatched target never replays");

    let infrastructure = fixture
        .staged()
        .bind(fixture.target, || {
            Err(ReplayError::fixture(CheckKind::Gate, "runner unavailable"))
        })
        .err()
        .expect("infrastructure failure refuses");
    assert!(matches!(infrastructure, ProofError::Replay(_)));

    for standing in [
        CheckStanding::Failed { code: Some(1) },
        CheckStanding::TimedOut { code: None },
        CheckStanding::Cancelled { code: Some(9) },
        CheckStanding::OutputTruncated,
        CheckStanding::OutputIncomplete,
    ] {
        let refused = fixture.refused_replay(standing);
        let error = fixture
            .staged()
            .bind(fixture.target, || Ok(refused))
            .err()
            .expect("non-clean replay refuses");
        let ProofError::Refused(refusal) = error else {
            panic!("non-clean standing lost its typed refusal")
        };
        assert_eq!(refusal.replay().checks()[0].standing(), standing);
    }
}
