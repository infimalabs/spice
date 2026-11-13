use super::*;

#[test]
fn lane_snapshots_project_exact_member_renewal_state() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let predecessor = wire::RenewalIdentity {
        occupant: uuid(LANE_CAPABILITY),
        session: "session-predecessor".to_owned(),
        thread: "thread-predecessor".to_owned(),
    };
    let renewal = wire::LaneRenewalRecord {
        team: uuid(LANE),
        lane: uuid(LANE),
        revision: 9,
        state: wire::LaneRenewalState::Requested {
            predecessor,
            team_slot: 0,
        },
    };
    let lanes = lane_response_with_renewals(
        &[
            (LANE, LANE, Some(LANE_CAPABILITY), true),
            (SECOND_LANE, LANE, Some(SECRET_LANE_CAPABILITY), true),
        ],
        vec![renewal.clone()],
    );
    let admitted: app::Admitted<Response<wire::intent::LaneAnswer>> = session
        .admit_frame(&lanes)
        .expect("lane response is admitted");

    assert!(runtime.apply_lanes(&admitted));
    let view = runtime.lanes().expect("lane snapshot is projected");
    assert_eq!(view.renewal(uuid(LANE), uuid(LANE)), Some(&renewal));
    assert_eq!(view.renewal(uuid(LANE), uuid(SECOND_LANE)), None);
    assert_eq!(view.renewal(uuid(THIRD_LANE), uuid(LANE)), None);
}
