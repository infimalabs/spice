//! Runtime cache invalidation and live projection regressions.

use super::*;

/// One durable envelope around a fact body, at a position nothing else uses.
fn envelope(
    position: u64,
    actor: &str,
    origin: &str,
    fact: &serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "v": 1,
        "seq": position,
        "at": "2030-04-12T18:44:09.271Z",
        "actor": actor,
        "origin": origin,
        "fact": fact,
    })
}

/// The one expiry every timed row in the inventory carries.
const EXPIRES: &str = "2030-04-12T19:44:09.271Z";

/// One fact, and the cached reads it invalidates.
type StaleRow = (serde_json::Value, [bool; 3]);

/// Facts from `steer::*`, the namespace only the inbox is drawn from.
fn steer_rows() -> Vec<StaleRow> {
    vec![
        (
            serde_json::json!({
                "kind": "steer::sent",
                "directive": REQUEST,
                "key": "QmTKOamJ",
                "recipient": format!("lane:{LANE}"),
                "text": "look at the tile",
                "sender": "spice",
                "expires": EXPIRES,
            }),
            [false, false, true],
        ),
        (
            serde_json::json!({"kind": "steer::canceled", "key": "QmTKOamJ", "recipient": format!("lane:{LANE}")}),
            [false, false, true],
        ),
    ]
}

/// Facts from `lane::*`, which move the roster and inbox together.
fn lane_rows() -> Vec<StaleRow> {
    vec![
        (
            serde_json::json!({
                "kind": "lane::seated",
                "lane": LANE,
                "team": LANE,
                "worktree": "/tmp/lane",
                "life": "continuous",
                "vendor": "omp",
            }),
            [true, true, true],
        ),
        (
            serde_json::json!({
                "kind": "lane::exited",
                "lane": LANE,
                "occupant": REQUEST,
                "code": 0,
            }),
            [false, true, true],
        ),
    ]
}

/// Task and claim changes move board evidence; claims also move lane activity.
fn task_and_claim_rows() -> Vec<StaleRow> {
    vec![
        (
            serde_json::json!({"kind": "task::phased", "task": "1kKRRsDPx", "phase": "review"}),
            [true, false, false],
        ),
        (
            serde_json::json!({
                "kind": "task::done",
                "task": "1kKRRsDPx",
                "validation": "the suite is green",
                "source": {"proven": {"heads": []}},
            }),
            [true, false, false],
        ),
        (
            serde_json::json!({
                "kind": "claim::won",
                "task": "1kKRRsDPx",
                "actor": "principal:00000000-0000-0000-0000-000000000000",
                "lane": LANE,
                "occupant": REQUEST,
                "epoch": EPOCH,
                "until": EXPIRES,
                "cell": {"topic": "/app", "phase": "todo"},
                "source": {"proven": {"heads": []}},
            }),
            [true, true, false],
        ),
        (
            serde_json::json!({"kind": "claim::freed", "task": "1kKRRsDPx", "actor": "principal:00000000-0000-0000-0000-000000000000"}),
            [true, true, false],
        ),
    ]
}

/// Preparation decisions and execution authorizations move their controller
/// and selected output heads even though they live outside the task namespace.
fn task_head_rows() -> Vec<StaleRow> {
    vec![
        (
            serde_json::json!({
                "kind": "proposal::authorized",
                "authorization": {
                    "decision": {"controller": "1kKRRsDPx", "decision": "1"},
                    "selected_outputs": ["dVsYdDj7d"],
                },
            }),
            [true, false, false],
        ),
        (
            serde_json::json!({
                "kind": "proposal::decided",
                "decision": {
                    "decision": {"controller": "1kKRRsDPx", "decision": "1"},
                    "source": {"kind": "journal", "fact": "1"},
                    "basis": {"statement": 1},
                    "outcome": {
                        "kind": "resolved",
                        "outputs": [{
                            "output": "dVsYdDj7d",
                            "verdict": "accepted",
                        }],
                    },
                },
            }),
            [true, false, false],
        ),
    ]
}

/// Facts from `journal::*` and `team::*`, which do not move these cached reads.
/// These rows separate a per-read mapping from one that widens every read together.
fn journal_and_team_rows() -> Vec<StaleRow> {
    vec![
        (
            serde_json::json!({
                "kind": "journal::ingested",
                "lane": LANE,
                "vendor": "omp",
                "stream": "transcript",
                "source": REQUEST,
                "from": {"member": 0, "offset": 0},
                "to": {"member": 0, "offset": 4096},
                "usage": [],
                "non_usage_records": 3,
                "residue_records": 0,
            }),
            [false, false, false],
        ),
        (
            serde_json::json!({"kind": "journal::migrated", "from": 1, "to": 2, "epoch": EPOCH}),
            [false, false, false],
        ),
        (
            serde_json::json!({
                "kind": "team::configured",
                "team": LANE,
                "member": REQUEST,
                "from_drive": false,
                "from_drain": false,
                "drive": true,
                "drain": false,
            }),
            [true, true, false],
        ),
        (
            serde_json::json!({
                "kind": "team::arranged",
                "from": [{"team": LANE, "lanes": [LANE]}],
                "teams": [{"team": LANE, "lanes": []}],
            }),
            [true, true, false],
        ),
    ]
}

/// Facts no cached read is drawn from at all. A rule that simply refreshed
/// everything on every fact would pass every row above and fail these.
fn unread_namespace_rows() -> Vec<StaleRow> {
    vec![
        (
            serde_json::json!({"kind": "up::dark", "epoch": EPOCH}),
            [false, false, false],
        ),
        (
            serde_json::json!({
                "kind": "journal::backed",
                "bundle": "/tmp/backup.tar.zst",
                "manifest": [],
            }),
            [false, false, false],
        ),
        (
            serde_json::json!({
                "kind": "narration::cleared",
                "lane": LANE,
                "occupant": REQUEST,
                "moments": 3,
                "prods": 1,
                "relief": "prose",
            }),
            [false, false, false],
        ),
    ]
}

/// Which cached read each fact invalidates, as
/// `[board, lanes, inbox]`.
///
/// This inventory is written out above rather than read back off the mapping it
/// checks, so deleting an arm fails instead of agreeing with itself. The groups
/// are the namespaces the mapping keys on, and every one of them appears twice
/// under different kinds and different producers: a mapping keyed on one exact
/// kind rather than on the namespace fails on the sibling instead of passing on
/// the twin.
fn staleness_inventory() -> Vec<StaleRow> {
    steer_rows()
        .into_iter()
        .chain(lane_rows())
        .chain(task_and_claim_rows())
        .chain(task_head_rows())
        .chain(journal_and_team_rows())
        .chain(unread_namespace_rows())
        .collect()
}

#[test]
fn a_live_fact_invalidates_exactly_the_reads_its_namespace_moves() {
    let session = session(OPERATOR, EPOCH);
    let inventory = staleness_inventory();
    for (fact, expected) in inventory {
        let kind = fact["kind"]
            .as_str()
            .expect("fixture names its kind")
            .to_owned();
        let pushed = fact_frame(envelope(41, "spice", &format!("lane:{LANE}"), &fact));
        let stale = stale_reads(&admitted_stream(&session, &pushed));
        assert_eq!(
            [stale.board(), stale.lanes(), stale.inbox()],
            expected,
            "{kind} invalidated the wrong reads"
        );
        assert_eq!(
            stale.any(),
            expected.contains(&true),
            "{kind} disagrees with its own summary"
        );
    }
}

#[test]
fn a_principal_fact_rereads_each_principal_directory() {
    let session = session(OPERATOR, EPOCH);
    let profile = serde_json::json!({"name": "C Adams", "email": "c@example.invalid"});
    for (fact, expected) in [
        (
            serde_json::json!({
                "kind": "principal::admitted",
                "principal": REQUEST,
                "profile": profile,
                "roles": [],
            }),
            true,
        ),
        (
            serde_json::json!({
                "kind": "principal::profiled",
                "principal": REQUEST,
                "from": {},
                "profile": profile,
            }),
            true,
        ),
        (
            serde_json::json!({"kind": "task::phased", "task": "1kKRRsDPx", "phase": "review"}),
            false,
        ),
    ] {
        let kind = fact["kind"]
            .as_str()
            .expect("fixture names its kind")
            .to_owned();
        let pushed = fact_frame(envelope(41, OWNER_ACTOR, "thread:operator", &fact));
        let stale = stale_reads(&admitted_stream(&session, &pushed));
        assert_eq!(
            [stale.principals(), stale.settings()],
            [expected; 2],
            "{kind} moved a principal directory wrongly"
        );
        if expected {
            assert_eq!(
                [stale.board(), stale.lanes(), stale.inbox(), stale.consent()],
                [false; 4],
                "{kind} invalidated an unrelated read"
            );
        }
    }
}

#[test]
fn an_authorization_rereads_the_controller_and_every_selected_output_detail() {
    let session = session(OPERATOR, EPOCH);
    let authorized = serde_json::json!({
        "kind": "proposal::authorized",
        "authorization": {
            "decision": {"controller": "1kKRRsDPx", "decision": "1"},
            "selected_outputs": ["dVsYdDj7d", "dVsYdDj7k"],
        },
    });
    let pushed = fact_frame(envelope(41, OWNER_ACTOR, "task:1kKRRsDPx", &authorized));
    let stale = stale_reads(&admitted_stream(&session, &pushed));
    assert_eq!(
        stale
            .task_details()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["1kKRRsDPx", "dVsYdDj7d", "dVsYdDj7k"],
        "the completed controller and authorized outputs reread"
    );
}

#[test]
fn only_the_tail_of_a_backlog_invalidates_current_reads() {
    let session = session(OPERATOR, EPOCH);
    let sent = serde_json::json!({
        "kind": "steer::sent",
        "directive": REQUEST,
        "key": "QmTKOamJ",
        "recipient": format!("lane:{LANE}"),
        "text": "look at the tile",
        "sender": "spice",
        "expires": "2030-04-12T19:44:09.271Z",
    });
    let won = serde_json::json!({
        "kind": "claim::won",
        "task": "1kKRRsDPx",
        "actor": "principal:00000000-0000-0000-0000-000000000000",
        "lane": LANE,
        "occupant": REQUEST,
        "epoch": EPOCH,
        "until": "2030-04-12T19:44:09.271Z",
        "cell": {"topic": "/app", "phase": "todo"},
        "source": {"proven": {"heads": []}},
    });
    let lane = format!("lane:{LANE}");
    let ascending = [
        envelope(41, "spice", &lane, &sent),
        envelope(42, OWNER_ACTOR, "task:1kKRRsDPx", &won),
    ];

    let tail = admitted_stream(&session, &packed_frame("facts", &ascending, None, 42));
    let stale = stale_reads(&tail);
    assert_eq!(
        [stale.board(), stale.lanes(), stale.inbox()],
        [true, true, true],
        "a page widens to the union of its rows and no further"
    );

    let continued = admitted_stream(
        &session,
        &packed_frame("facts", &ascending, Some((7, 43)), 99),
    );
    assert_eq!(
        stale_reads(&continued),
        Stale::default(),
        "a page that continues defers to the page that ends the backlog"
    );

    let descending = [
        envelope(42, OWNER_ACTOR, "task:1kKRRsDPx", &won),
        envelope(41, "spice", &lane, &sent),
    ];
    let thread = admitted_stream(&session, &packed_frame("thread", &descending, None, 42));
    assert_eq!(
        stale_reads(&thread),
        Stale::default(),
        "a causal walk is history rather than an advance"
    );
}

#[test]
fn a_steering_retirement_rereads_the_task_its_delivery_was_sent_into() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    runtime.apply_stream_frame(&admitted_stream(
        &session,
        &packed_frame("rooms", &[], None, 40),
    ));
    let lane = format!("lane:{LANE}");
    let sent = serde_json::json!({
        "kind": "steer::sent",
        "directive": REQUEST,
        "key": "QmTKOamJ",
        "recipient": format!("lane:{LANE}"),
        "room": "1kKRRsDPx",
        "text": "Join the task",
        "sender": "spice",
        "expires": EXPIRES,
    });
    let pushed = admitted_stream(&session, &fact_frame(envelope(41, "spice", &lane, &sent)));
    assert_eq!(
        stale_reads(&pushed)
            .task_details()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["1kKRRsDPx"],
        "a delivery into a room rereads that task at once"
    );
    runtime.apply_stream_frame(&pushed);

    let declined = serde_json::json!({
        "kind": "steer::declined",
        "directive": REQUEST,
        "key": "QmTKOamJ",
        "recipient": format!("lane:{LANE}"),
        "evidence": {
            "source": "transcript",
            "epoch": EPOCH,
            "locus": {
                "source": REQUEST, "member": 1, "offset": 40, "length": 7, "event": 0,
                "line": 1, "column": 1,
            },
        },
        "reason": "busy",
    });
    let retired = admitted_stream(
        &session,
        &fact_frame(envelope(42, "spice", &lane, &declined)),
    );
    let mut stale = stale_reads(&retired);
    assert_eq!(stale.task_details().count(), 0);
    stale.resolve_steering_rooms(runtime.room_projection().conversation());
    assert_eq!(
        stale
            .task_details()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["1kKRRsDPx"],
        "the retirement names only its key; the shown delivery supplies the room"
    );

    let unknown = serde_json::json!({"kind": "steer::canceled", "key": "QmTKOamK", "recipient": format!("lane:{LANE}")});
    let mut stale = stale_reads(&admitted_stream(
        &session,
        &fact_frame(envelope(43, "spice", &lane, &unknown)),
    ));
    stale.resolve_steering_rooms(runtime.room_projection().conversation());
    assert_eq!(
        stale.task_details().count(),
        0,
        "a key no shown delivery carries rereads no task"
    );
}

#[test]
fn only_abnormal_typed_lane_endings_project_durable_lifecycle_boundaries() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let endings = [
        serde_json::json!({
            "kind": "lane::exited", "lane": LANE, "occupant": REQUEST, "code": 0,
        }),
        serde_json::json!({
            "kind": "lane::drained", "lane": LANE, "occupant": REQUEST,
        }),
        serde_json::json!({
            "kind": "lane::exited", "lane": LANE, "occupant": REQUEST, "code": 7,
        }),
        serde_json::json!({
            "kind": "lane::timedout", "lane": LANE, "occupant": REQUEST, "code": -1,
        }),
        serde_json::json!({
            "kind": "lane::cancelled", "lane": LANE, "occupant": REQUEST, "code": -1,
        }),
        serde_json::json!({
            "kind": "lane::signalled", "lane": LANE, "occupant": REQUEST, "signal": 15,
        }),
        serde_json::json!({
            "kind": "lane::killed", "lane": LANE, "occupant": REQUEST,
        }),
        serde_json::json!({
            "kind": "lane::degraded", "lane": LANE,
            "marker": format!("occupant {REQUEST} capture failed: fixture refusal"),
        }),
    ];
    for (index, ending) in endings.into_iter().enumerate() {
        let position = 41 + u64::try_from(index).expect("fixture index fits a sequence");
        let fact = envelope(position, "spice", &format!("lane:{LANE}"), &ending);
        runtime.apply_stream_frame(&admitted_stream(&session, &fact_frame(fact)));
    }

    assert_eq!(
        runtime
            .lifecycle_boundaries()
            .iter()
            .map(|boundary| (boundary.seq(), boundary.occupant(), boundary.cause()))
            .collect::<Vec<_>>(),
        vec![
            (seq(43), Some(uuid(REQUEST)), "exited with code 7"),
            (seq(44), Some(uuid(REQUEST)), "runtime timed out; code -1"),
            (
                seq(45),
                Some(uuid(REQUEST)),
                "supervision cancelled; code -1",
            ),
            (seq(46), Some(uuid(REQUEST)), "ended by external signal 15",),
            (seq(47), Some(uuid(REQUEST)), "ended by resident signal"),
            (seq(48), None, "supervision degraded"),
        ],
        "successful exit and graceful drain are omitted while each abnormal cause stays distinct",
    );
    assert!(
        runtime
            .lifecycle_boundaries()
            .iter()
            .all(|boundary| !boundary.cause().contains(REQUEST)),
        "renderable lifecycle causes expose no occupant identity",
    );

    let duplicate = envelope(
        44,
        "spice",
        &format!("lane:{LANE}"),
        &serde_json::json!({
            "kind": "lane::timedout", "lane": LANE, "occupant": REQUEST, "code": -1,
        }),
    );
    runtime.apply_stream_frame(&admitted_stream(&session, &fact_frame(duplicate)));
    assert_eq!(
        runtime.lifecycle_boundaries().len(),
        6,
        "replayed durable facts do not duplicate lifecycle separators"
    );
}

/// Every tile-ladder rung has its own current cause.
///
/// The five rungs are read from three independent inputs — queued direction,
/// typed occupant liveness, and a declared session — so the fixture varies one
/// at a time rather than presenting lanes that differ in every field. The
/// labels are written out here rather than read back from [`LaneStatus`],
/// because a test that asks the enum what its wording is passes after the
/// wording changes.
///
/// The rank is asserted through a shuffled sort because the strip orders its
/// tiles by `Ord` alone: a ladder that named the five states in the right
/// order but derived the wrong comparison would still put the lane waiting on
/// the operator at the far end of the strip.
#[test]
fn a_lane_tile_ranks_queued_direction_above_liveness_and_liveness_above_a_cold_seat() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let bytes = lane_response(&[
        (SECOND_LANE, SECOND_LANE, None, false),
        (LANE, LANE, Some(OPERATOR), false),
        (THIRD_LANE, THIRD_LANE, None, true),
    ]);
    let admitted: app::Admitted<Response<wire::intent::LaneAnswer>> = session
        .admit_frame(&bytes)
        .expect("lane answer is admitted");
    runtime.apply_lanes(&admitted);

    let lanes = runtime
        .lanes()
        .expect("the roster is retained")
        .lanes
        .clone();
    let [never_seated, occupied, seated_before] = <[_; 3]>::try_from(lanes)
        .unwrap_or_else(|rows| panic!("three rows survive admission, got {}", rows.len()));
    let never_seated = never_seated.host().expect("team has one member");
    let occupied = occupied.host().expect("team has one member");
    let seated_before = seated_before.host().expect("team has one member");
    assert_eq!(never_seated.status(0), LaneStatus::Unbound);
    assert_eq!(
        seated_before.status(0),
        LaneStatus::Bound,
        "a seat with a declared session is bound rather than unbound once its occupant leaves"
    );
    assert_eq!(occupied.status(0), LaneStatus::Working);
    assert_eq!(occupied.status(2), LaneStatus::Queued);
    assert_eq!(
        never_seated.status(1),
        LaneStatus::Queued,
        "queued direction outranks liveness even in a seat nobody has ever occupied"
    );
    let mut owed = seated_before.clone();
    owed.jobs.push(wire::intent::LaneJob {
        id: "6ZbQk3mPx".to_owned(),
        command: "cargo test".to_owned(),
        task: None,
        kind: wire::intent::LaneJobKind::Command,
        standing: wire::intent::LaneJobStanding::Unheard,
    });
    assert_eq!(
        (owed.status(0), owed.parked),
        (LaneStatus::Bound, None),
        "a kept job alone changes neither the colour nor the fill"
    );
    owed.parked = Some(wire::body::LanePark::Job {
        until: "2026-08-01T07:00:59.000Z".parse().expect("fixture instant"),
    });
    assert_eq!(
        [owed.status(0), owed.status(1)],
        [LaneStatus::Bound, LaneStatus::Queued],
        "a parked lane keeps the colour its status earns, queued included"
    );
    let mut working = occupied.clone();
    working.parked = owed.parked;
    assert_eq!(
        working.status(0),
        LaneStatus::Working,
        "parking an occupied lane leaves its liveness colour"
    );

    let mut strip = vec![
        LaneStatus::Unbound,
        LaneStatus::Running,
        LaneStatus::Working,
        LaneStatus::Bound,
        LaneStatus::Queued,
    ];
    strip.sort_unstable();
    assert_eq!(
        strip,
        vec![
            LaneStatus::Queued,
            LaneStatus::Working,
            LaneStatus::Running,
            LaneStatus::Bound,
            LaneStatus::Unbound,
        ],
        "the strip reads left to right in the current LaneStatus order"
    );
}

#[test]
fn one_team_groups_multiple_steerable_worktrees_without_losing_member_order() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let bytes = lane_response(&[
        (LANE, EPOCH, Some(OPERATOR), false),
        (SECOND_LANE, EPOCH, None, true),
        (THIRD_LANE, THIRD_LANE, None, false),
    ]);
    let admitted: app::Admitted<Response<wire::intent::LaneAnswer>> = session
        .admit_frame(&bytes)
        .expect("plural team answer is admitted");

    runtime.apply_lanes(&admitted);

    let lane_view = runtime.lanes().expect("the roster is retained");
    assert_eq!(lane_view.topology_revision, 9);
    let lanes = &lane_view.lanes;
    assert_eq!(lanes.len(), 2);
    assert_eq!(lanes[0].lane, uuid(EPOCH));
    assert_eq!(lanes[0].members.len(), 2);
    assert_eq!(lanes[0].members[0].lane, uuid(LANE));
    assert_eq!(lanes[0].members[1].lane, uuid(SECOND_LANE));
    assert_eq!(lanes[1].members[0].lane, uuid(THIRD_LANE));
}

#[test]
fn every_distinct_team_forms_one_console_even_when_members_have_no_name() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let bytes = lane_response(&[
        (LANE, LANE, None, false),
        (SECOND_LANE, SECOND_LANE, None, false),
        (THIRD_LANE, THIRD_LANE, None, false),
    ]);
    let admitted: app::Admitted<Response<wire::intent::LaneAnswer>> = session
        .admit_frame(&bytes)
        .expect("distinct team answer is admitted");

    runtime.apply_lanes(&admitted);

    let teams = &runtime.lanes().expect("the roster is retained").lanes;
    assert_eq!(teams.len(), 3, "distinct team keys never share a console");
    assert_eq!(
        teams.iter().map(|team| team.lane).collect::<Vec<_>>(),
        vec![uuid(LANE), uuid(SECOND_LANE), uuid(THIRD_LANE)]
    );
    assert!(teams.iter().all(|team| {
        team.members.len() == 1 && team.host().is_some_and(|member| member.name.is_none())
    }));
}
