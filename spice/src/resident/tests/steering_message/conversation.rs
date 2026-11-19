//! An operator route opens an explicit pending item only for `#KEY` of a
//! direction the operator sent this lane.

use steering::{Published, Unpublished};
use wire::rooms::RoomConversed;

use super::*;

impl Lanes {
    pub(super) fn conversations(&self) -> Vec<RoomConversed> {
        self.bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read")
            .facts()
            .iter()
            .filter_map(|envelope| envelope.fact.read::<RoomConversed>())
            .collect()
    }

    /// Author `text` on `lane`, account it, and hear its newest packet.
    pub(super) fn say(&self, lane: u8, text: &str) -> Result<Published, Unpublished> {
        self.author(lane, text);
        self.account(lane);
        let (_, outcomes) = self.hear_newest(lane);
        let [outcome] = outcomes.try_into().expect("one packet per block");
        outcome
    }
}

fn refused(outcome: &Result<Published, Unpublished>) -> &str {
    match outcome {
        Err(Unpublished::Refused { reason, .. } | Unpublished::Recorded { reason, .. }) => reason,
        other => panic!("the packet refused: {other:?}"),
    }
}

/// `#KEY` routes to the operator about a direction the operator sent this
/// lane; another lane's direction, a peer message's key, an unknown key, or
/// an occupant addressed about a direction publishes nothing.
#[test]
fn an_operator_concern_names_the_direction_it_points_at() {
    let lanes = Lanes::new("conv-direction");
    let (sender, first) = (lanes.address(SENDER), lanes.address(FIRST));

    let own = lanes.direct(SENDER, 60, "which first");
    let theirs = lanes.direct(FIRST, 62, "hold the merge");
    assert!(
        lanes
            .say(FIRST, &format!("SYN {sender}: peer note"))
            .is_ok()
    );
    let peer = lanes
        .pending(SENDER)
        .into_iter()
        .map(|pending| pending.key)
        .find(|key| *key != own)
        .expect("the peer message is pending");
    for (selector, reason) in [
        (format!("#{theirs}"), "syn::input-unauthorized"),
        ("#dVwg6Zqlf".to_owned(), "syn::input-unknown"),
        (format!("{first}#{own}"), "syn::input-audience"),
    ] {
        let outcome = lanes.say(SENDER, &format!("SYN {selector}: about it"));
        assert_eq!(refused(&outcome), reason, "{selector}");
    }

    let partial = lanes.say(
        SENDER,
        &format!("SYN {first} #{theirs}: keep the valid branch"),
    );
    let Ok(Published::Sent {
        lanes: reached,
        unreached,
        ..
    }) = &partial
    else {
        panic!("the valid branch publishes independently: {partial:?}");
    };
    assert_eq!(reached, &[test_uuid(FIRST)]);
    assert_eq!(
        unreached,
        &[format!(
            "#{theirs} syn::input-unauthorized (#{theirs} is not an input this lane received)"
        )]
    );
    assert!(
        lanes
            .pending(FIRST)
            .iter()
            .any(|input| input.text == "keep the valid branch")
    );
    lanes
        .say(SENDER, &format!("SYN #{peer}: about it"))
        .expect("an authorized input key opens a linked peer line");
    assert!(
        lanes
            .pending(FIRST)
            .iter()
            .any(|input| input.text == "about it")
    );
    assert!(lanes.conversations().is_empty());

    lanes.answer(SENDER, &[&own], &format!("ACK {own}"));
    lanes.answer(SENDER, &[&own], &format!("FIN {own}: Handled."));
    assert!(
        lanes.pending(SENDER).iter().all(|input| input.key != own),
        "the nod retires operator direction and the later FIN answers it"
    );
    lanes
        .say(SENDER, &format!("SYN #{own}: which first?"))
        .expect("a closed direction still continues by SYN");
    let directions = lanes
        .conversations()
        .into_iter()
        .flat_map(|record| record.routes)
        .map(|route| route.direction)
        .collect::<Vec<_>>();
    assert_eq!(directions, [Some(own)]);
    lanes.finish();
}

/// Startup recovery passes silently over an acknowledged direction holding
/// only its nod, sees an answer retained after it left the live index, and
/// records that answer's one use exactly once.
#[test]
fn startup_recovers_a_post_ack_operator_answer() {
    let lanes = Lanes::new("conv-direction-answer-recovery");
    lanes.declare(SENDER);
    let direction = lanes.direct(SENDER, 63, "finish after the work");
    lanes.answer(SENDER, &[&direction], &format!("ACK {direction}"));
    let receipt = {
        let key = direction.clone();
        lanes
            .bus
            .read(move |read| {
                read.steering_key(&key)
                    .map(|head| head.expect("the acknowledged direction remains").position())
            })
            .expect("the direction reads")
    };
    let answered = || {
        lanes
            .bus
            .read(move |read| read.steering_answered(receipt))
            .expect("answer evidence reads")
    };
    assert!(!answered(), "the nod has not spent answer authority");
    assert_eq!(
        lanes.restart(),
        "",
        "a direction holding only its nod is settled, not pending"
    );
    assert!(!answered(), "recovering the nod spends no answer authority");

    let message = serde_json::json!({
        "type": "message_end",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": format!("FIN {direction}: finished")}],
            "stopReason": "stop",
        },
    });
    lanes
        .tape(SENDER)
        .tee(format!("{message}\n").as_bytes())
        .expect("the unheard answer is durable");

    lanes.restart();
    assert!(answered(), "startup records the retained FIN answer");
    lanes.restart();
    assert!(
        answered(),
        "a used historical receipt is absent from later recovery"
    );
    lanes.finish();
}

/// A peer that writes an input's key where an occupant belongs hears the
/// `#KEY` spelling, and no route of that packet publishes, while a recorded
/// address in the same position routes as written.
#[test]
fn a_key_written_as_a_recipient_refuses_with_the_input_spelling() {
    let lanes = Lanes::new("conv-key-recipient");
    let first = lanes.address(FIRST);
    let key = lanes.direct(SENDER, 60, "which first");
    let leading_zero = format!("0{}", &key[1..]);
    lanes
        .say(SENDER, &format!("SYN {first}: a recorded address"))
        .expect("a recorded address routes as written");
    for (selectors, spelled) in [
        (key.clone(), key.clone()),
        (format!("{first} {key}"), key.clone()),
        (leading_zero.clone(), leading_zero),
    ] {
        let outcome = lanes.say(SENDER, &format!("SYN {selectors}: about it"));
        let Err(Unpublished::Refused { reason, detail } | Unpublished::Recorded { reason, detail }) =
            &outcome
        else {
            panic!("a key-spelled recipient refuses the packet: {outcome:?}");
        };
        assert_eq!(reason, "syn::recipient-key", "{selectors}");
        assert!(
            detail.contains(&format!("SYN #{spelled}: BODY")),
            "{selectors}: {detail}"
        );
    }
    let heard = lanes
        .pending(FIRST)
        .into_iter()
        .map(|input| input.text)
        .collect::<Vec<_>>();
    assert_eq!(
        heard,
        ["a recorded address"],
        "no route of a refused packet publishes"
    );
    lanes.finish();
}

/// The operator reads a lane's SYN body from the author's transcript, the
/// bytes `spice room inbox` prints, beside the direction it concerns with the
/// images the operator attached; an unknown publication reads as missing
/// rather than as some other text.
#[test]
fn an_operator_item_body_reads_back_from_its_authoring_transcript() {
    let lanes = Lanes::new("conv-operator-text");
    let capture = wire::Media::from_bytes("capture.png", "image/png", b"\x89PNG\r\n\x1a\n\0")
        .expect("fixture image admits");
    let text = format!(
        "ship when green\n\n{}",
        capture
            .reference_markdown()
            .expect("fixture image references")
    );
    let key = lanes.direct_with(
        lanes.bootstrap(),
        SENDER,
        20,
        &text,
        std::slice::from_ref(&capture),
    );
    lanes
        .say(SENDER, &format!("SYN #{key}: ship it?"))
        .expect("the concern publishes");
    let [concern] = lanes
        .conversations()
        .try_into()
        .expect("one conversation record");
    let read = |publication| {
        let outcome = crate::room_command::conversation_text(
            &lanes.bus,
            &lanes.paths.tape,
            &wire::intent::RoomConversationText { publication },
        );
        let wire::Outcome::Reply(reply) = outcome else {
            panic!("the conversation text read answers: {outcome:?}");
        };
        let wire::intent::CommandAnswer::RoomConversationText(answer) = reply.body else {
            panic!("another answer shape: {:?}", reply.body);
        };
        *answer
    };
    let answer = read(concern.publication.clone());
    assert_eq!(
        answer.content,
        wire::intent::RoomConversationContent::Text {
            text: "ship it?".to_owned()
        }
    );
    assert_eq!(
        answer.direction,
        Some(wire::intent::RoomConcernedDirection {
            key,
            text,
            attachments: vec![capture],
        })
    );
    assert_eq!(
        read("dVrD244Jz".parse().expect("fixture publication")).content,
        wire::intent::RoomConversationContent::Unavailable {
            unavailable: wire::rooms::RoomSourceUnavailable::Missing {}
        }
    );
    lanes.finish();
}
