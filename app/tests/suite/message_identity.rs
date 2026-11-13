use std::rc::Rc;

use app::{AppRuntime, Session};
use wire::{CanonicalUuid, StreamFrame};

const OPERATOR: &str = "018f82d4-6f59-7d06-92e3-bce9131163c1";
const LANE_CAPABILITY: &str = "018f82d4-6f59-7d06-92e3-bce9131163c6";
const EPOCH: &str = "018f82d4-6f59-7d06-92e3-bce9131163c3";
const REQUEST: &str = "018f82d4-6f59-7d06-92e3-bce9131163c4";
const SECRET_LANE_CAPABILITY: &str = "018f82d4-6f59-7d06-92e3-bce9131163cc";
const LANE: &str = "018f82d4-6f59-7d06-92e3-bce9131163c2";
const SECOND_LANE: &str = "018f82d4-6f59-7d06-92e3-bce9131163cd";

fn uuid(text: &str) -> CanonicalUuid {
    text.parse().expect("fixture uuid is canonical")
}

fn admitted(session: &Session, bytes: &[u8]) -> app::Admitted<StreamFrame> {
    session
        .admit_stream(bytes)
        .expect("stream frame is admitted")
}

fn complete_frontiers(messages: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut lanes = Vec::new();
    for message in messages {
        let lane = message["lane"].clone();
        if !lanes.contains(&lane) {
            lanes.push(lane);
        }
    }
    lanes
        .into_iter()
        .map(|lane| serde_json::json!({"lane": lane, "state": "complete"}))
        .collect()
}

fn generation(
    id: u64,
    messages: Vec<serde_json::Value>,
    frontiers: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "generation": id,
        "horizon": 1,
        "messages": messages,
        "frontiers": frontiers,
    })
}

fn snapshot(messages: Vec<serde_json::Value>) -> Vec<u8> {
    let frontiers = complete_frontiers(&messages);
    snapshot_with_frontiers(messages, frontiers)
}

fn snapshot_with_frontiers(
    messages: Vec<serde_json::Value>,
    frontiers: Vec<serde_json::Value>,
) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "stream": "messages",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "limit": 8,
        "generation": generation(1, messages, frontiers),
    }))
    .expect("message snapshot serializes")
}

fn live(id: u64, message: serde_json::Value) -> Vec<u8> {
    let frontiers = complete_frontiers(std::slice::from_ref(&message));
    serde_json::to_vec(&serde_json::json!({
        "stream": "message-generation",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "generation": generation(id, vec![message], frontiers),
    }))
    .expect("live message generation serializes")
}

fn card_bytes(runtime: &AppRuntime) -> Vec<u8> {
    let messages = runtime
        .messages()
        .iter()
        .map(|message| message.as_ref())
        .collect::<Vec<_>>();
    serde_json::to_vec(&messages).expect("cards serialize")
}

struct MixedMessages {
    older: serde_json::Value,
    boundary: serde_json::Value,
    media: serde_json::Value,
    command: serde_json::Value,
    second_provider: serde_json::Value,
}

fn mixed_messages() -> MixedMessages {
    MixedMessages {
        older: serde_json::json!({
            "lane": LANE,
            "author": LANE_CAPABILITY,
            "locus": {"member": 1, "offset": 10, "length": 20, "event": 0},
            "authored": "2030-04-12T18:44:09.271Z",
            "text": "older prose",
        }),
        boundary: serde_json::json!({
            "lane": LANE,
            "author": LANE_CAPABILITY,
            "locus": {"member": 1, "offset": 30, "length": 20, "event": 0},
            "authored": "2030-04-12T18:44:10.271Z",
            "activity": "compaction",
            "text": "Compacted",
        }),
        media: serde_json::json!({
            "lane": LANE,
            "author": LANE_CAPABILITY,
            "locus": {"member": 1, "offset": 50, "length": 20, "event": 1},
            "authored": "2030-04-12T18:44:11.271Z",
            "activity": "tool",
            "text": "view_image: capture.png succeeded",
            "media": [{
                "name": "capture.png",
                "content_type": "image/png",
                "data_url": "data:image/png;base64,iVBORw0KGgo=",
            }],
        }),
        command: serde_json::json!({
            "lane": LANE,
            "author": LANE_CAPABILITY,
            "locus": {"member": 1, "offset": 50, "length": 20, "event": 0},
            "authored": "2030-04-12T18:44:10.771Z",
            "activity": "tool",
            "text": "view_image: capture.png",
        }),
        second_provider: serde_json::json!({
            "lane": SECOND_LANE,
            "author": SECRET_LANE_CAPABILITY,
            "locus": {"member": 1, "offset": 15, "length": 20, "event": 0},
            "authored": "2030-04-12T18:44:12.271Z",
            "text": "second provider prose",
        }),
    }
}

#[test]
fn revised_message_snapshot_preserves_unchanged_allocation_identity() {
    let session = Session::open(uuid(OPERATOR), uuid(EPOCH));
    let accepted = session
        .admit_accepted(
            format!(r#"{{"wire":1,"actor":"principal:00000000-0000-0000-0000-000000000000","epoch":"{EPOCH}","release":"1.2.3"}}"#)
                .as_bytes(),
        )
        .expect("acceptance is admitted");
    let mut runtime = AppRuntime::new(&accepted);
    let first = serde_json::json!({
        "lane": LANE,
        "author": LANE_CAPABILITY,
        "locus": {"member": 1, "offset": 10, "length": 20, "event": 0},
        "authored": "2030-04-12T18:44:09.271Z",
        "text": "first author",
    });
    let second = serde_json::json!({
        "lane": SECOND_LANE,
        "author": SECRET_LANE_CAPABILITY,
        "locus": {"member": 1, "offset": 10, "length": 20, "event": 0},
        "authored": "2030-04-12T18:44:10.271Z",
        "text": "second author",
    });
    let bounded_snapshot = |first, second| {
        let messages = vec![first, second];
        let frontiers = complete_frontiers(&messages);
        serde_json::to_vec(&serde_json::json!({
            "stream": "messages",
            "v": 1,
            "id": REQUEST,
            "epoch": EPOCH,
            "limit": 1,
            "generation": generation(1, messages, frontiers),
        }))
        .expect("message snapshot serializes")
    };
    runtime.apply_stream_frame(&admitted(
        &session,
        &bounded_snapshot(first.clone(), second.clone()),
    ));
    let retained_first = Rc::clone(&runtime.messages()[0]);
    let retained_second = Rc::clone(&runtime.messages()[1]);

    let mut revised_second = second.clone();
    revised_second["text"] = serde_json::json!("second author revised");
    assert!(
        runtime
            .apply_stream_frame(&admitted(&session, &live(2, revised_second.clone())))
            .changed(),
        "a live content revision invalidates the owning card"
    );
    assert!(Rc::ptr_eq(&retained_first, &runtime.messages()[0]));
    assert!(!Rc::ptr_eq(&retained_second, &runtime.messages()[1]));
    let retained_revision = Rc::clone(&runtime.messages()[1]);
    assert!(
        !runtime
            .apply_stream_frame(&admitted(&session, &live(3, revised_second)))
            .changed(),
        "replaying the revision is idempotent"
    );
    assert!(Rc::ptr_eq(&retained_first, &runtime.messages()[0]));
    assert!(Rc::ptr_eq(&retained_revision, &runtime.messages()[1]));
    assert_eq!(
        runtime
            .newest_lane_activity(uuid(SECOND_LANE))
            .expect("revised activity remains projected")
            .detail(),
        "second author revised"
    );
}

#[test]
fn final_answer_revision_invalidates_the_ordinary_card() {
    let session = Session::open(uuid(OPERATOR), uuid(EPOCH));
    let accepted = session
        .admit_accepted(
            format!(r#"{{"wire":1,"actor":"principal:00000000-0000-0000-0000-000000000000","epoch":"{EPOCH}","release":"1.2.3"}}"#)
                .as_bytes(),
        )
        .expect("acceptance is admitted");
    let mut runtime = AppRuntime::new(&accepted);
    let ordinary = serde_json::json!({
        "lane": LANE,
        "author": LANE_CAPABILITY,
        "locus": {"member": 1, "offset": 10, "length": 20, "event": 0},
        "authored": "2030-04-12T18:44:09.271Z",
        "text": "same message",
    });
    runtime.apply_stream_frame(&admitted(&session, &snapshot(vec![ordinary.clone()])));
    let retained_ordinary = Rc::clone(&runtime.messages()[0]);

    let mut final_answer = ordinary;
    final_answer["final_answer"] = serde_json::json!(true);
    assert!(
        runtime
            .apply_stream_frame(&admitted(&session, &live(2, final_answer.clone())))
            .changed(),
        "a final-answer revision invalidates the formerly ordinary card"
    );
    assert!(!Rc::ptr_eq(&retained_ordinary, &runtime.messages()[0]));
    assert!(runtime.messages()[0].final_answer);
    let retained_final = Rc::clone(&runtime.messages()[0]);

    assert!(
        !runtime
            .apply_stream_frame(&admitted(&session, &live(3, final_answer)))
            .changed(),
        "replaying the final-answer revision is idempotent"
    );
    assert!(Rc::ptr_eq(&retained_final, &runtime.messages()[0]));
}

#[test]
fn mixed_cards_reduce_identically_across_snapshot_history_and_replay() {
    let session = Session::open(uuid(OPERATOR), uuid(EPOCH));
    let accepted = session
        .admit_accepted(
            format!(r#"{{"wire":1,"actor":"principal:00000000-0000-0000-0000-000000000000","epoch":"{EPOCH}","release":"1.2.3"}}"#)
                .as_bytes(),
        )
        .expect("acceptance is admitted");
    let MixedMessages {
        older,
        boundary,
        media,
        command,
        second_provider,
    } = mixed_messages();

    let mut one_shot = AppRuntime::new(&accepted);
    one_shot.apply_stream_frame(&admitted(
        &session,
        &snapshot(vec![
            second_provider.clone(),
            boundary.clone(),
            command.clone(),
            older.clone(),
            media.clone(),
        ]),
    ));

    let mut staged = AppRuntime::new(&accepted);
    staged.apply_stream_frame(&admitted(
        &session,
        &snapshot_with_frontiers(
            vec![second_provider.clone(), media.clone(), command.clone()],
            vec![
                serde_json::json!({
                    "lane": LANE,
                    "state": "truncated",
                    "before": {"member": 1, "offset": 50, "length": 20, "event": 1},
                }),
                serde_json::json!({
                    "lane": SECOND_LANE,
                    "state": "complete",
                }),
            ],
        ),
    ));
    let retained_media = Rc::clone(&staged.messages()[0]);
    let requested = serde_json::json!({
        "generation": 1,
        "lane": LANE,
        "before": {"member": 1, "offset": 50, "length": 20, "event": 1},
    });
    let history = serde_json::to_vec(&serde_json::json!({
        "stream": "message-history",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "requested": [requested],
        "generation": generation(
            1,
            vec![older, boundary],
            vec![serde_json::json!({"lane": LANE, "state": "complete"})],
        ),
    }))
    .expect("message history serializes");
    staged.apply_stream_frame(&admitted(&session, &history));
    assert!(
        Rc::ptr_eq(&retained_media, &staged.messages()[2]),
        "history backfill does not replace or reposition the retained media allocation"
    );
    assert!(
        !staged
            .apply_stream_frame(&admitted(&session, &live(2, command)))
            .changed(),
        "replayed tool commands remain activity rather than merging into media cards"
    );
    assert!(
        !staged
            .apply_stream_frame(&admitted(&session, &live(3, media)))
            .changed(),
        "replayed media does not duplicate or invalidate its card"
    );
    assert!(
        !staged
            .apply_stream_frame(&admitted(&session, &live(4, second_provider)))
            .changed(),
        "replayed prose does not duplicate or invalidate its card"
    );

    assert_eq!(
        card_bytes(&one_shot),
        card_bytes(&staged),
        "all delivery paths produce byte-equivalent ordered card projections"
    );
    assert_eq!(staged.messages().len(), 4);
    assert!(staged.messages()[1].media.is_empty());
    assert_eq!(staged.messages()[2].media.len(), 1);
}
