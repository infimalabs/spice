use super::*;

fn agent_message(
    lane: &str,
    offset: u64,
    authored: &str,
    text: &str,
    activity: Option<&str>,
) -> serde_json::Value {
    let mut message = serde_json::json!({
        "lane": lane,
        "author": LANE_CAPABILITY,
        "locus": {"member": 1, "offset": offset, "length": 20, "event": 0},
        "authored": authored,
        "text": text,
    });
    if let Some(activity) = activity {
        message["activity"] = serde_json::json!(activity);
    }
    message
}

fn message_snapshot(messages: Vec<serde_json::Value>, more: &[&str]) -> Vec<u8> {
    message_snapshot_at(1, messages, more)
}

fn message_snapshot_at(
    generation: u64,
    messages: Vec<serde_json::Value>,
    more: &[&str],
) -> Vec<u8> {
    let lanes = messages
        .iter()
        .filter_map(|message| message["lane"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let frontiers = lanes
        .into_iter()
        .map(|lane| {
            let truncated = more.contains(&lane);
            let mut frontier = serde_json::json!({
                "lane": lane,
                "state": if truncated { "truncated" } else { "complete" },
            });
            if truncated
                && let Some(before) = messages
                    .iter()
                    .filter(|message| message["lane"] == lane)
                    .min_by_key(|message| message["locus"]["offset"].as_u64())
                    .map(|message| message["locus"].clone())
            {
                frontier["before"] = before;
            }
            frontier
        })
        .collect();
    message_snapshot_frame(generation, 256, messages, frontiers)
}

fn history_page(lane: &str, before: u64, messages: Vec<serde_json::Value>, more: bool) -> Vec<u8> {
    history_page_at(1, lane, before, messages, more)
}

fn history_page_at(
    generation: u64,
    lane: &str,
    before: u64,
    messages: Vec<serde_json::Value>,
    more: bool,
) -> Vec<u8> {
    let requested = serde_json::json!({
        "generation": generation,
        "lane": lane,
        "before": {"member": 1, "offset": before, "length": 20, "event": 0},
    });
    let next = messages
        .iter()
        .min_by_key(|message| message["locus"]["offset"].as_u64())
        .map(|message| message["locus"].clone())
        .or_else(|| more.then(|| requested["before"].clone()));
    let mut frontier = serde_json::json!({
        "lane": lane,
        "state": if more { "truncated" } else { "complete" },
    });
    if let Some(next) = next {
        frontier["before"] = next;
    }
    serde_json::to_vec(&serde_json::json!({
        "stream": "message-history",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "requested": [requested],
        "generation": transcript_generation(generation, messages, vec![frontier]),
    }))
    .expect("message history serializes")
}

fn apply(runtime: &mut AppRuntime, session: &Session, frame: &[u8]) -> bool {
    runtime
        .apply_stream_frame(&admitted_stream(session, frame))
        .changed()
}

fn target_offset(runtime: &AppRuntime, lane: &str) -> Option<u64> {
    runtime
        .message_history_target(uuid(lane))
        .and_then(|target| target.before.map(|before| before.offset))
}

#[test]
fn hidden_turn_history_advances_to_older_prose_without_rendering_a_card() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let snapshot = message_snapshot(
        vec![agent_message(LANE, 100, T10, "newest prose", None)],
        &[LANE],
    );
    apply(&mut runtime, &session, &snapshot);

    let turns = history_page(
        LANE,
        100,
        vec![agent_message(LANE, 50, T09, "", Some("turn"))],
        true,
    );
    apply(&mut runtime, &session, &turns);
    assert_eq!(target_offset(&runtime, LANE), Some(50));
    assert_eq!(
        runtime
            .messages()
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["newest prose"],
    );

    let older = history_page(
        LANE,
        50,
        vec![agent_message(LANE, 25, T08, "older prose", None)],
        false,
    );
    apply(&mut runtime, &session, &older);
    assert_eq!(
        runtime
            .messages()
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["older prose", "newest prose"],
    );
    assert_eq!(
        runtime.newest_lane_activity(uuid(LANE)).unwrap().detail(),
        "newest prose"
    );
    assert!(runtime.message_history_target(uuid(LANE)).is_none());
}

#[test]
fn older_history_pages_never_displace_a_lanes_newest_header_prose() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let snapshot = message_snapshot(
        vec![agent_message(LANE, 100, T10, "newest prose", None)],
        &[LANE],
    );
    apply(&mut runtime, &session, &snapshot);
    let older = history_page(
        LANE,
        100,
        vec![
            agent_message(LANE, 25, T08, "oldest prose", None),
            agent_message(LANE, 50, T09, "older prose", None),
        ],
        false,
    );
    apply(&mut runtime, &session, &older);

    let (message, prose) = runtime
        .latest_lane_prose(uuid(LANE))
        .expect("the lane authored prose");
    assert_eq!(
        (message.text.as_str(), prose.as_str()),
        ("newest prose", "newest prose")
    );
}

#[test]
fn older_member_history_merges_without_replacing_the_live_activity_head() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let newest = serde_json::json!({
        "lane": LANE,
        "author": LANE_CAPABILITY,
        "locus": {"member": 1, "offset": 100, "length": 20, "event": 0},
        "authored": "2030-04-12T18:44:10.271Z",
        "text": "newest visible activity",
    });
    let snapshot = message_snapshot(vec![newest], &[LANE]);
    assert!(
        runtime
            .apply_stream_frame(&admitted_stream(&session, &snapshot))
            .changed()
    );
    assert!(runtime.message_history_target(uuid(LANE)).is_some());

    let history = history_page(
        LANE,
        100,
        vec![serde_json::json!({
            "lane": LANE,
            "author": LANE_CAPABILITY,
            "locus": {"member": 1, "offset": 50, "length": 20, "event": 0},
            "authored": "2030-04-12T18:44:09.271Z",
            "text": "older retained history",
        })],
        false,
    );
    assert!(
        runtime
            .apply_stream_frame(&admitted_stream(&session, &history))
            .changed()
    );
    assert_eq!(
        runtime
            .messages()
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["older retained history", "newest visible activity"],
    );
    assert_eq!(runtime.activities().len(), 1);
    assert_eq!(runtime.activities()[0].detail(), "newest visible activity");
    assert!(runtime.message_history_target(uuid(LANE)).is_none());
}

#[test]
fn history_rejects_a_new_card_inside_the_retained_interval() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let snapshot = message_snapshot(
        vec![
            agent_message(LANE, 100, T10, "retained edge", None),
            agent_message(LANE, 200, T12, "retained head", None),
        ],
        &[LANE],
    );
    assert!(apply(&mut runtime, &session, &snapshot));
    assert_eq!(target_offset(&runtime, LANE), Some(100));

    let incompatible = history_page(
        LANE,
        100,
        vec![agent_message(LANE, 150, T11, "not an edge backfill", None)],
        false,
    );
    assert!(!apply(&mut runtime, &session, &incompatible));
    assert_eq!(target_offset(&runtime, LANE), Some(100));
    assert_eq!(
        runtime
            .messages()
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["retained edge", "retained head"],
    );
}

#[test]
fn an_older_reconnect_snapshot_cannot_roll_back_the_transcript_generation() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let current = message_snapshot_at(
        2,
        vec![agent_message(LANE, 200, T12, "current generation", None)],
        &[],
    );
    assert!(apply(&mut runtime, &session, &current));

    assert!(!runtime.reconnect(&accepted));
    let stale = message_snapshot_at(
        1,
        vec![agent_message(LANE, 100, T10, "stale generation", None)],
        &[],
    );
    let application = runtime.apply_stream_frame(&admitted_stream(&session, &stale));
    assert!(!application.changed());
    assert_eq!(application.directives(), [StreamDirective::Reconnect]);
    assert_eq!(
        runtime
            .messages()
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["current generation"],
    );
}

const T07: &str = "2030-04-12T18:44:07.271Z";
const T08: &str = "2030-04-12T18:44:08.271Z";
const T09: &str = "2030-04-12T18:44:09.271Z";
const T10: &str = "2030-04-12T18:44:10.271Z";
const T11: &str = "2030-04-12T18:44:11.271Z";
const T12: &str = "2030-04-12T18:44:12.271Z";
const T13: &str = "2030-04-12T18:44:13.271Z";
const T14: &str = "2030-04-12T18:44:14.271Z";
const T15: &str = "2030-04-12T18:44:15.271Z";
const T16: &str = "2030-04-12T18:44:16.271Z";

fn seed_three_member_frontiers(runtime: &mut AppRuntime, session: &Session) {
    let snapshot = message_snapshot(
        vec![
            agent_message(LANE, 100, T12, "first head", None),
            agent_message(
                SECOND_LANE,
                200,
                T11,
                "second compaction",
                Some("compaction"),
            ),
            agent_message(THIRD_LANE, 300, T13, "third head", None),
        ],
        &[LANE, THIRD_LANE],
    );
    assert!(apply(runtime, session, &snapshot));
    assert_eq!(target_offset(runtime, LANE), Some(100));
    assert_eq!(target_offset(runtime, SECOND_LANE), None);
    assert_eq!(target_offset(runtime, THIRD_LANE), Some(300));
}

#[test]
fn coordinated_history_rejects_duplicate_response_frontiers_before_mutation() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    seed_three_member_frontiers(&mut runtime, &session);
    let requested = vec![
        serde_json::json!({
            "generation": 1,
            "lane": LANE,
            "before": {"member": 1, "offset": 100, "length": 20, "event": 0},
        }),
        serde_json::json!({
            "generation": 1,
            "lane": THIRD_LANE,
            "before": {"member": 1, "offset": 300, "length": 20, "event": 0},
        }),
    ];
    let duplicate = serde_json::json!({"lane": LANE, "state": "complete"});
    let response = serde_json::to_vec(&serde_json::json!({
        "stream": "message-history",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "requested": requested,
        "generation": transcript_generation(
            1,
            vec![agent_message(LANE, 50, T09, "must not be merged", None)],
            vec![duplicate.clone(), duplicate],
        ),
    }))
    .expect("duplicate-frontier history serializes");

    assert!(!apply(&mut runtime, &session, &response));
    assert_eq!(target_offset(&runtime, LANE), Some(100));
    assert_eq!(target_offset(&runtime, THIRD_LANE), Some(300));
    assert!(
        runtime
            .messages()
            .iter()
            .all(|message| message.text != "must not be merged")
    );
}

#[test]
fn a_nonadvancing_open_page_preserves_the_same_cursor() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    seed_three_member_frontiers(&mut runtime, &session);

    let empty = history_page(LANE, 100, Vec::new(), true);
    assert!(
        apply(&mut runtime, &session, &empty),
        "the admitted completion must wake explicit-demand tracking",
    );
    assert_eq!(target_offset(&runtime, LANE), Some(100));
    assert_eq!(target_offset(&runtime, THIRD_LANE), Some(300));

    assert!(apply(&mut runtime, &session, &empty));
    assert_eq!(target_offset(&runtime, LANE), Some(100));
    assert_eq!(target_offset(&runtime, THIRD_LANE), Some(300));
}

#[test]
fn a_live_generation_cannot_reopen_an_exhausted_lane_until_a_snapshot_does() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let retained = vec![agent_message(LANE, 100, T12, "retained head", None)];
    assert!(apply(
        &mut runtime,
        &session,
        &message_snapshot(retained.clone(), &[LANE]),
    ));
    let last = history_page(
        LANE,
        100,
        vec![agent_message(LANE, 50, T10, "oldest page", None)],
        false,
    );
    assert!(apply(&mut runtime, &session, &last));
    assert_eq!(runtime.message_history_target(uuid(LANE)), None);

    let live = message_generation_frame(
        2,
        vec![agent_message(LANE, 150, T14, "live work", None)],
        vec![serde_json::json!({
            "lane": LANE,
            "state": "truncated",
            "before": {"member": 1, "offset": 100, "length": 20, "event": 0},
        })],
    );
    assert!(apply(&mut runtime, &session, &live));
    assert_eq!(
        runtime.message_history_target(uuid(LANE)),
        None,
        "the retained edge must not restart an exhausted lane",
    );

    let replaced = message_snapshot_at(3, retained, &[LANE]);
    assert!(apply(&mut runtime, &session, &replaced));
    assert_eq!(target_offset(&runtime, LANE), Some(100));
    let later = message_generation_frame(
        4,
        vec![agent_message(LANE, 160, T14, "later work", None)],
        vec![serde_json::json!({
            "lane": LANE,
            "state": "truncated",
            "before": {"member": 1, "offset": 100, "length": 20, "event": 0},
        })],
    );
    assert!(apply(&mut runtime, &session, &later));
    assert_eq!(
        target_offset(&runtime, LANE),
        Some(100),
        "a snapshot reopens the lane for every later generation",
    );
}

#[test]
fn a_live_generation_keeps_a_hydrated_lane_paging_from_its_deepest_cursor() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let snapshot = message_snapshot(
        vec![agent_message(LANE, 100, T12, "retained head", None)],
        &[LANE],
    );
    assert!(apply(&mut runtime, &session, &snapshot));
    let first = history_page(
        LANE,
        100,
        vec![agent_message(LANE, 50, T10, "first page", None)],
        true,
    );
    assert!(apply(&mut runtime, &session, &first));
    assert_eq!(target_offset(&runtime, LANE), Some(50));

    let second = history_page(
        LANE,
        50,
        vec![agent_message(LANE, 40, T09, "second page", None)],
        true,
    );
    assert!(apply(&mut runtime, &session, &second));
    assert_eq!(target_offset(&runtime, LANE), Some(40));

    let live = message_generation_frame(
        2,
        vec![agent_message(LANE, 150, T14, "live work", None)],
        vec![serde_json::json!({
            "lane": LANE,
            "state": "truncated",
            "before": {"member": 1, "offset": 100, "length": 20, "event": 0},
        })],
    );
    assert!(apply(&mut runtime, &session, &live));
    assert_eq!(
        target_offset(&runtime, LANE),
        Some(40),
        "the retained edge must not replace the hydrated cursor",
    );

    // The page requested before the live generation is answered after it.
    let deeper = history_page(
        LANE,
        40,
        vec![agent_message(LANE, 25, T08, "deeper page", None)],
        true,
    );
    assert!(apply(&mut runtime, &session, &deeper));
    assert_eq!(target_offset(&runtime, LANE), Some(25));
    assert_eq!(
        runtime
            .messages()
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        [
            "deeper page",
            "second page",
            "first page",
            "retained head",
            "live work",
        ],
    );

    let resume = serde_json::to_vec(&serde_json::json!({
        "stream": "message-history",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "requested": [{
            "generation": 1,
            "lane": LANE,
            "before": {"member": 1, "offset": 25, "length": 20, "event": 0},
        }],
        "generation": transcript_generation(
            2,
            Vec::new(),
            vec![serde_json::json!({
                "lane": LANE,
                "state": "truncated",
                "before": {"member": 1, "offset": 100, "length": 20, "event": 0},
            })],
        ),
    }))
    .expect("resume page serializes");
    assert!(apply(&mut runtime, &session, &resume));
    assert_eq!(
        target_offset(&runtime, LANE),
        Some(100),
        "the explicit retained edge replaces the invalidated deep cursor",
    );

    let rebased = history_page_at(
        2,
        LANE,
        100,
        vec![agent_message(LANE, 75, T09, "rebased history", None)],
        false,
    );
    assert!(apply(&mut runtime, &session, &rebased));
    assert_eq!(target_offset(&runtime, LANE), None);
    assert!(
        runtime
            .messages()
            .iter()
            .any(|message| message.text == "rebased history")
    );
}

fn interleave_live_work_and_member_pages(runtime: &mut AppRuntime, session: &Session) {
    let third_page = history_page(
        THIRD_LANE,
        300,
        vec![
            agent_message(THIRD_LANE, 250, T09, "third oldest", None),
            agent_message(THIRD_LANE, 275, T10, "third older", None),
        ],
        true,
    );
    assert!(apply(runtime, session, &third_page));
    assert_eq!(target_offset(runtime, THIRD_LANE), Some(250));
    assert_eq!(target_offset(runtime, LANE), Some(100));

    let live = message_generation_frame(
        2,
        vec![agent_message(LANE, 150, T14, "first live", None)],
        vec![
            serde_json::json!({
                "lane": LANE,
                "state": "truncated",
                "before": {"member": 1, "offset": 100, "length": 20, "event": 0},
            }),
            serde_json::json!({"lane": SECOND_LANE, "state": "complete"}),
            serde_json::json!({
                "lane": THIRD_LANE,
                "state": "truncated",
                "before": {"member": 1, "offset": 250, "length": 20, "event": 0},
            }),
        ],
    );
    assert!(apply(runtime, session, &live));
    assert_eq!(
        runtime.newest_lane_activity(uuid(LANE)).unwrap().detail(),
        "first live"
    );
    assert_eq!(target_offset(runtime, THIRD_LANE), Some(250));

    let stale = history_page_at(
        2,
        THIRD_LANE,
        300,
        vec![agent_message(THIRD_LANE, 260, T08, "stale response", None)],
        false,
    );
    assert!(!apply(runtime, session, &stale));
    assert_eq!(target_offset(runtime, THIRD_LANE), Some(250));
    assert!(
        runtime
            .messages()
            .iter()
            .all(|message| message.text != "stale response")
    );

    let first_page = history_page_at(
        2,
        LANE,
        100,
        vec![agent_message(LANE, 50, T08, "first exhausted", None)],
        false,
    );
    assert!(apply(runtime, session, &first_page));
    assert_eq!(target_offset(runtime, LANE), None);
    assert_eq!(target_offset(runtime, THIRD_LANE), Some(250));

    let final_third_page = history_page_at(
        2,
        THIRD_LANE,
        250,
        vec![agent_message(THIRD_LANE, 225, T07, "third frontier", None)],
        true,
    );
    assert!(apply(runtime, session, &final_third_page));
    assert_eq!(target_offset(runtime, THIRD_LANE), Some(225));
}

fn reconnect_and_replay_overlap(
    runtime: &mut AppRuntime,
    session: &Session,
    accepted: &app::Admitted<wire::Accepted>,
) {
    assert!(!runtime.reconnect(accepted));
    let reconnect = message_snapshot_at(
        2,
        vec![
            agent_message(LANE, 175, T15, "first reconnected head", None),
            agent_message(
                SECOND_LANE,
                200,
                T11,
                "second compaction",
                Some("compaction"),
            ),
            agent_message(THIRD_LANE, 350, T16, "third reconnected head", None),
        ],
        &[LANE, THIRD_LANE],
    );
    assert!(apply(runtime, session, &reconnect));
    assert_eq!(runtime.message_history_revision(), 1);
    assert_eq!(target_offset(runtime, LANE), Some(175));
    assert_eq!(target_offset(runtime, THIRD_LANE), Some(350));
    for retained in ["first exhausted", "third frontier", "second compaction"] {
        assert!(
            runtime
                .messages()
                .iter()
                .any(|message| message.text == retained),
            "same-scope reconnect retains hydrated card {retained}",
        );
    }

    let overlap = history_page_at(
        2,
        THIRD_LANE,
        350,
        vec![agent_message(THIRD_LANE, 300, T13, "third head", None)],
        true,
    );
    assert!(
        apply(runtime, session, &overlap),
        "a duplicate overlap still advances its member-local frontier",
    );
    assert_eq!(target_offset(runtime, THIRD_LANE), Some(300));
    let identities = runtime
        .messages()
        .iter()
        .map(|message| (message.lane, message.locus))
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(identities.len(), runtime.messages().len());
    assert_eq!(
        runtime
            .messages()
            .iter()
            .filter(|message| message.activity == wire::AgentActivityKind::Compaction)
            .count(),
        1,
    );
    assert!(
        runtime
            .messages()
            .windows(2)
            .all(|pair| { pair[0].chronology_key() < pair[1].chronology_key() })
    );
}

#[test]
fn three_member_frontiers_progress_independently_across_live_work_and_reconnect() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);

    seed_three_member_frontiers(&mut runtime, &session);
    interleave_live_work_and_member_pages(&mut runtime, &session);
    reconnect_and_replay_overlap(&mut runtime, &session, &accepted);
}

fn failed_history_page(lane: &str, before: u64, faults: serde_json::Value) -> Vec<u8> {
    let mut generation = transcript_generation(
        1,
        Vec::new(),
        vec![serde_json::json!({"lane": lane, "state": "unavailable"})],
    );
    generation["faults"] = faults;
    serde_json::to_vec(&serde_json::json!({
        "stream": "message-history",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "requested": [{
            "generation": 1,
            "lane": lane,
            "before": {"member": 1, "offset": before, "length": 20, "event": 0},
        }],
        "generation": generation,
    }))
    .expect("failed message history serializes")
}

#[test]
fn a_failed_history_page_keeps_the_lane_cursor_and_names_why_until_a_read_succeeds() {
    let session = session(OPERATOR, EPOCH);
    let accepted = acceptance(&session, OWNER_ACTOR, EPOCH);
    let mut runtime = AppRuntime::new(&accepted);
    let snapshot = message_snapshot(
        vec![agent_message(LANE, 100, T10, "newest prose", None)],
        &[LANE],
    );
    apply(&mut runtime, &session, &snapshot);

    let unexplained = failed_history_page(LANE, 100, serde_json::json!([]));
    assert!(!apply(&mut runtime, &session, &unexplained));
    assert_eq!(runtime.message_history_fault(uuid(LANE)), None);

    let failed = failed_history_page(
        LANE,
        100,
        serde_json::json!([{"lane": LANE, "reason": "tape member 8 is unreadable"}]),
    );
    assert!(apply(&mut runtime, &session, &failed));
    assert_eq!(
        runtime.message_history_fault(uuid(LANE)),
        Some("tape member 8 is unreadable"),
    );
    assert_eq!(
        target_offset(&runtime, LANE),
        Some(100),
        "a failed read is neither complete history nor a moved frontier",
    );

    let recovered = history_page(
        LANE,
        100,
        vec![agent_message(LANE, 50, T09, "older prose", None)],
        true,
    );
    assert!(apply(&mut runtime, &session, &recovered));
    assert_eq!(runtime.message_history_fault(uuid(LANE)), None);
    assert_eq!(target_offset(&runtime, LANE), Some(50));
}
