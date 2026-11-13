//! Same-generation reconnect coverage for a packed app fact backlog.

use app::{AppRuntime, Session, StreamDirective};
use wire::{CanonicalUuid, Seq, StreamFrame, ViewSubject};

const OPERATOR: &str = "018f82d4-6f59-7d06-92e3-bce9131163c1";
const EPOCH: &str = "018f82d4-6f59-7d06-92e3-bce9131163c3";
const REQUEST: &str = "018f82d4-6f59-7d06-92e3-bce9131163c4";

fn uuid(text: &str) -> CanonicalUuid {
    text.parse().expect("fixture UUID is canonical")
}

fn seq(value: u64) -> Seq {
    Seq::new(value).expect("fixture sequence is nonzero")
}

fn acceptance(session: &Session) -> app::Admitted<wire::Accepted> {
    session
        .admit_accepted(
            format!(r#"{{"wire":1,"actor":"principal:00000000-0000-0000-0000-000000000000","epoch":"{EPOCH}","release":"1.2.3"}}"#)
                .as_bytes(),
        )
        .expect("acceptance is admitted")
}

fn task_fact(position: u64, cause: Option<u64>) -> serde_json::Value {
    let mut value = serde_json::json!({
        "v": 1,
        "seq": position,
        "at": "2030-04-12T18:44:09.271Z",
        "actor": "principal:00000000-0000-0000-0000-000000000000",
        "origin": "task:1kKRRsDPx",
        "fact": {
            "kind": "task::phased",
            "task": "1kKRRsDPx",
            "phase": "review",
        },
    });
    if let Some(cause) = cause {
        value["cause"] = serde_json::json!(cause);
    }
    value
}

fn packed_frame(rows: &[serde_json::Value], overflow: Option<(u64, u64)>) -> Vec<u8> {
    let mut packed = serde_json::json!({"rows": rows, "horizon": 4});
    if let Some((omitted, continuation)) = overflow {
        packed["overflow"] = serde_json::json!({
            "omitted": omitted,
            "continuation": continuation,
        });
    }
    serde_json::to_vec(&serde_json::json!({
        "stream": "facts",
        "v": 1,
        "id": REQUEST,
        "epoch": EPOCH,
        "packed": packed,
    }))
    .expect("packed stream fixture serializes")
}

fn admitted(session: &Session, bytes: &[u8]) -> app::Admitted<StreamFrame> {
    session
        .admit_stream(bytes)
        .expect("stream frame is admitted")
}

#[test]
fn replayed_prefix_cannot_move_reconnect_before_the_backlog_origin() {
    let session = Session::open(uuid(OPERATOR), uuid(EPOCH));
    let accepted = acceptance(&session);
    let mut runtime = AppRuntime::new(&accepted);
    let first = admitted(
        &session,
        &packed_frame(&[task_fact(1, None), task_fact(2, Some(1))], Some((2, 3))),
    );
    let second = admitted(
        &session,
        &packed_frame(&[task_fact(3, Some(2)), task_fact(4, Some(3))], None),
    );
    runtime.apply_stream_frame(&first);
    runtime.apply_stream_frame(&second);
    let settled = runtime
        .resume()
        .expect("complete backlog settles its horizon");
    assert!(!runtime.reconnect(&acceptance(&session)));

    assert_eq!(
        runtime.apply_stream_frame(&first).directives(),
        vec![StreamDirective::Continue {
            subject: ViewSubject::Facts,
            cursor: wire::StreamCursor {
                epoch: uuid(EPOCH),
                seq: seq(3),
            },
        }],
        "a replayed prefix may still advance its producer continuation"
    );
    assert_eq!(runtime.resume(), Some(settled));
}
