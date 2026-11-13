use super::*;

fn lane_response_at(
    rows: &[(&str, &str, Option<&str>, bool)],
    position: ReplyPosition,
    topology_revision: u64,
    policy: bool,
) -> Vec<u8> {
    let mut response: Response<wire::intent::LaneAnswer> =
        wire::decode(&lane_response(rows)).expect("base lane response decodes");
    let Outcome::Reply(reply) = &mut response.outcome else {
        unreachable!("base lane response replies")
    };
    reply.position = position;
    reply.body.topology_revision = topology_revision;
    for subscription in &mut reply.body.subscriptions {
        subscription.drive = true;
        subscription.drain = policy;
    }
    wire::encode(&response).expect("positioned lane response encodes")
}

#[test]
fn an_older_lane_snapshot_cannot_replace_a_newer_committed_policy() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let rows = [(LANE, LANE, Some(LANE_CAPABILITY), true)];
    let committed = lane_response_at(
        &rows,
        ReplyPosition::Committed(wire::SeqRange {
            first: seq(50),
            last: seq(50),
        }),
        10,
        true,
    );
    let stale = lane_response_at(&rows, ReplyPosition::Snapshot(seq(41)), 9, false);
    let committed = session
        .admit_frame(&committed)
        .expect("committed lane response admits");
    let stale = session
        .admit_frame(&stale)
        .expect("older lane snapshot admits");

    assert!(runtime.apply_lanes(&committed));
    assert!(!runtime.apply_lanes(&stale));
    let lanes = runtime.lanes().expect("committed lane table remains");
    assert_eq!(lanes.topology_revision, 10);
    assert!(lanes.lanes[0].drain);
}

#[test]
fn an_older_committed_lane_result_cannot_replace_a_later_commit() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let rows = [(LANE, LANE, Some(LANE_CAPABILITY), true)];
    let later = lane_response_at(
        &rows,
        ReplyPosition::Committed(wire::SeqRange {
            first: seq(51),
            last: seq(51),
        }),
        11,
        false,
    );
    let older = lane_response_at(
        &rows,
        ReplyPosition::Committed(wire::SeqRange {
            first: seq(50),
            last: seq(50),
        }),
        10,
        true,
    );
    let later = session
        .admit_frame(&later)
        .expect("later lane result admits");
    let older = session
        .admit_frame(&older)
        .expect("older lane result admits");

    assert!(
        !runtime.lane_reply_superseded(&older),
        "with no lane authority held, nothing supersedes a reply"
    );
    assert!(runtime.apply_lanes(&later));
    assert!(!runtime.lane_reply_superseded(&later));
    assert!(
        runtime.lane_reply_superseded(&older),
        "a later commit has answered for the older one"
    );
    let direct = lane_response_at(&rows, ReplyPosition::Direct {}, 12, true);
    let direct = session
        .admit_frame(&direct)
        .expect("positionless lane response admits structurally");
    assert!(
        !runtime.lane_reply_superseded(&direct),
        "a positionless reply is declined, not superseded"
    );
    assert!(!runtime.apply_lanes(&older));
    let lanes = runtime.lanes().expect("later lane table remains");
    assert_eq!(lanes.topology_revision, 11);
    assert!(!lanes.lanes[0].drain);
}

#[test]
fn a_positionless_lane_reply_is_not_authority() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let direct = lane_response_at(
        &[(LANE, LANE, Some(LANE_CAPABILITY), true)],
        ReplyPosition::Direct {},
        10,
        true,
    );
    let direct = session
        .admit_frame(&direct)
        .expect("positionless lane response admits structurally");

    assert!(!runtime.apply_lanes(&direct));
    assert!(runtime.lanes().is_none());
}

/// A lane fresh from `spice lane add` has neither occupant nor session, and
/// direct operator steering launches it; only a seat that was started and lost
/// its session has no available destination.
#[test]
fn a_never_started_vacant_lane_takes_a_submission() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let mut response: Response<wire::intent::LaneAnswer> = wire::decode(&lane_response(&[
        (LANE, LANE, None, false),
        (SECOND_LANE, SECOND_LANE, None, true),
        (THIRD_LANE, THIRD_LANE, None, false),
    ]))
    .expect("base lane response decodes");
    let Outcome::Reply(reply) = &mut response.outcome else {
        unreachable!("base lane response replies")
    };
    reply.body.lanes[2].lineage = vec![uuid(OPERATOR)];
    let bytes = wire::encode(&response).expect("lane response encodes");
    let admitted = session.admit_frame(&bytes).expect("lane response admits");
    assert!(runtime.apply_lanes(&admitted));

    let lanes = runtime.lanes().expect("lane table is retained");
    let authority = app::watchband::SubmissionAuthority::derive(true, |_| true);
    let target = |lane: &str| {
        let member = lanes.member(uuid(lane)).expect("member is seated");
        authority.target(member.occupant, member.wakeable, uuid(EPOCH))
    };
    assert_eq!(
        target(LANE),
        Some(app::watchband::SubmissionTarget::Vacant { epoch: uuid(EPOCH) }),
        "a never-started seat launches fresh"
    );
    assert_eq!(
        target(SECOND_LANE),
        Some(app::watchband::SubmissionTarget::Vacant { epoch: uuid(EPOCH) }),
        "a bound seat resumes its session"
    );
    assert_eq!(
        target(THIRD_LANE),
        None,
        "a started seat without a session has nothing to resume"
    );
}
