use super::*;
use wire::intent::LaneJobPhase;

#[test]
fn operator_inspection_preserves_delivery_and_stop_targets_only_one_job() {
    let root = scratch("spice-job-control");
    let lane: CanonicalUuid = "018f82d4-6f59-7d06-92e3-bce913116411".parse().unwrap();
    let jobs = terminal::Jobs::beneath(&root, lane);
    let command = jobs.record_for_tests("build --release", None, Some(600_000));
    let other = jobs.record_for_tests("serve", None, None);
    let target = LaneJobRef {
        scope: LaneJobScope::new(1, 0),
        id: command.id().to_owned(),
    };
    let peer = LaneJobRef {
        scope: target.scope,
        id: other.id().to_owned(),
    };
    assert_eq!(
        inspect(&jobs, &target, false).unwrap().phase,
        LaneJobPhase::Running
    );
    assert_eq!(
        inspect(&jobs, &target, true).unwrap().phase,
        LaneJobPhase::Stopping
    );
    assert_eq!(
        inspect(&jobs, &peer, false).unwrap().phase,
        LaneJobPhase::Running
    );
    command.end_for_tests(wire::decode(br#"{"kind":"exited","code":3}"#).unwrap());
    let ended = inspect(&jobs, &target, true).unwrap();
    assert_eq!(ended.phase, LaneJobPhase::Ended);
    assert!(ended.ended_millis.is_some());
    let mut retained = jobs
        .kept()
        .unwrap()
        .into_iter()
        .map(|job| job.id().to_owned())
        .collect::<Vec<_>>();
    let mut expected = vec![target.id, peer.id];
    retained.sort();
    expected.sort();
    assert_eq!(retained, expected);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_job_key_from_another_lane_cannot_inspect_or_stop_its_keeper() {
    let root = scratch("spice-job-scope");
    let lane: CanonicalUuid = "018f82d4-6f59-7d06-92e3-bce913116411".parse().unwrap();
    let jobs = terminal::Jobs::beneath(&root, lane);
    let other_lane: CanonicalUuid = "018f82d4-6f59-7d06-92e3-bce913116412".parse().unwrap();
    let other = terminal::Jobs::beneath(&root, other_lane);
    let command = jobs.record_for_tests("build", None, Some(600_000));
    let target = LaneJobRef {
        scope: LaneJobScope::new(1, 0),
        id: command.id().to_owned(),
    };
    assert_eq!(
        inspect(&other, &target, true).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        inspect(&jobs, &target, false).unwrap().phase,
        LaneJobPhase::Running
    );
    for id in ["../request.json", "/tmp/unrelated", "not a job"] {
        let target = LaneJobRef {
            scope: target.scope,
            id: id.to_owned(),
        };
        assert_eq!(
            inspect(&jobs, &target, true).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

fn scratch(prefix: &str) -> std::path::PathBuf {
    let micros = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros(),
    )
    .unwrap();
    let key = steer::Key::for_micros(micros).unwrap();
    std::env::temp_dir().join(format!("{prefix}-{key}"))
}
