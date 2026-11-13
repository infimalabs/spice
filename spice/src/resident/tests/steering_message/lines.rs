//! Both lanes of a peer line read one projection of it: a pending input names
//! its line by key, and each lane's open lines say whom the line waits on.

use super::*;
use crate::resident::steering::Unpublished;
use wire::body::{PeerAdmission, PeerBodyKind, PeerLineAction};
use wire::intent::{LineAnswer, LineStanding, PeerLine, PendingDirective};

fn only(lanes: &Lanes, lane: u8) -> PendingDirective {
    let [input] = lanes
        .pending(lane)
        .try_into()
        .unwrap_or_else(|pending| panic!("one input is pending: {pending:?}"));
    input
}

fn line(lanes: &Lanes, lane: u8) -> PeerLine {
    let [line] = lanes
        .inbox(lane)
        .lines
        .try_into()
        .unwrap_or_else(|lines| panic!("one line is open: {lines:?}"));
    line
}

/// An open line carries the same root and input keys from either lane, names
/// the other lane by address, and waits on exactly the lane holding a pending
/// input. Its first return is spent once, and a FIN leaves the final body
/// pending on a line no longer open.
#[test]
fn both_lanes_read_one_line_by_the_same_keys() {
    let lanes = Lanes::new("line-view");
    let (sender, first) = (lanes.address(SENDER), lanes.address(FIRST));
    lanes.author(SENDER, &format!("SYN {first}: is the parser settled?"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let opening = only(&lanes, FIRST);
    assert_eq!(
        opening.line,
        Some(LineStanding {
            root: opening.key.clone(),
            parent: None,
            body: PeerBodyKind::Open,
            covered: Vec::new(),
            admission: Some(PeerAdmission::Active),
            answer: None,
            closed: None,
        })
    );
    let (here, there) = (line(&lanes, FIRST), line(&lanes, SENDER));
    assert_eq!(
        (here.root.as_str(), here.counterpart, here.waiting()),
        (opening.key.as_str(), Some(sender), "waits on this lane")
    );
    assert_eq!(
        (there.root.as_str(), there.counterpart, there.waiting()),
        (opening.key.as_str(), Some(first), "waits on the other lane")
    );
    assert!(!here.returned, "nothing has answered the opening yet");

    lanes.answer(
        FIRST,
        &[&opening.key],
        &format!("ACK {}: Nearly.", opening.key),
    );
    let returned = only(&lanes, SENDER);
    assert_eq!(
        returned.line,
        Some(LineStanding {
            root: opening.key.clone(),
            parent: None,
            body: PeerBodyKind::Continue,
            covered: vec![opening.key.clone()],
            admission: Some(PeerAdmission::Active),
            answer: None,
            closed: None,
        })
    );
    let there = line(&lanes, SENDER);
    assert_eq!(there.waiting(), "waits on this lane");
    assert!(
        there.returned,
        "the answer spent the opening's first return"
    );
    assert_eq!(
        there
            .inputs
            .iter()
            .map(|input| (
                input.key.as_str(),
                input.held,
                input.pending(),
                input.answer.clone()
            ))
            .collect::<Vec<_>>(),
        [
            (
                opening.key.as_str(),
                false,
                false,
                Some(LineAnswer::Answered {
                    key: returned.key.clone(),
                    body: PeerBodyKind::Continue,
                })
            ),
            (returned.key.as_str(), true, true, None),
        ]
    );

    lanes.answer(
        SENDER,
        &[&returned.key],
        &format!("FIN {}: Ship it.", returned.key),
    );
    let last = only(&lanes, FIRST);
    assert_eq!(
        last.line,
        Some(LineStanding {
            root: opening.key.clone(),
            parent: None,
            body: PeerBodyKind::Final,
            covered: vec![returned.key.clone()],
            admission: Some(PeerAdmission::Active),
            answer: None,
            closed: Some(PeerLineAction::Finish),
        })
    );
    assert!(
        lanes.inbox(FIRST).lines.is_empty() && lanes.inbox(SENDER).lines.is_empty(),
        "a closed line is no longer open on either lane"
    );
    let seal: steer::Key = "1kQnrzNZx".parse().expect("fixture seal");
    let rendered =
        steer::inbox::render_pending(&seal, std::slice::from_ref(&last), last.authored, |_, _| {
            None
        });
    assert!(
        rendered.contains(&format!(
            "key={} stage=published expires={} line={} body=final covers={} from=lane:",
            last.key, last.expires, opening.key, returned.key
        )),
        "the notice names the line and covered input by key: {rendered}"
    );
    lanes.finish();
}

/// A fresh `SYN #KEY` opens a line linked from an input its lane only saw,
/// and from no input a delivered body already answered.
#[test]
fn a_fresh_syn_links_only_from_an_input_no_body_answered() {
    let lanes = Lanes::new("line-link");
    let first = lanes.address(FIRST);
    lanes.author(SENDER, &format!("SYN {first}: is the parser settled?"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let opening = only(&lanes, FIRST);
    lanes.answer(
        FIRST,
        &[&opening.key],
        &format!("ACK {}: noted", opening.key),
    );
    assert!(lanes.pending(SENDER).is_empty(), "a nod delivers no body");

    lanes
        .say(FIRST, &format!("SYN #{}: One more thing.", opening.key))
        .expect("an input only seen still links a fresh SYN");
    let linked = only(&lanes, SENDER);
    assert_eq!(
        linked.line,
        Some(LineStanding {
            root: linked.key.clone(),
            parent: Some(opening.key.clone()),
            body: PeerBodyKind::Open,
            covered: Vec::new(),
            admission: Some(PeerAdmission::Active),
            answer: None,
            closed: None,
        })
    );

    lanes.answer(
        SENDER,
        &[&linked.key],
        &format!("ACK {}: Go on.", linked.key),
    );
    match lanes.say(SENDER, &format!("SYN #{}: again", linked.key)) {
        Err(Unpublished::Refused { reason, .. } | Unpublished::Recorded { reason, .. }) => {
            assert_eq!(reason, "syn::input-unauthorized");
        }
        other => panic!("an answered input links nothing: {other:?}"),
    }
    lanes.finish();
}

/// A later input selector resolving onto the first route's seat refuses on its
/// own and cannot make the surviving direct route look linked from that input.
#[test]
fn a_refused_later_route_cannot_reparent_the_valid_branch() {
    let lanes = Lanes::new("line-link-repeated");
    let (sender, first) = (lanes.address(SENDER), lanes.address(FIRST));
    lanes.author(SENDER, &format!("SYN {first}: is the parser settled?"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let opening = only(&lanes, FIRST);
    lanes.answer(
        FIRST,
        &[&opening.key],
        &format!("ACK {}: noted", opening.key),
    );

    lanes
        .say(
            FIRST,
            &format!(
                "SYN {sender} #{}: the direct route remains independent",
                opening.key
            ),
        )
        .expect("the first valid route publishes");
    let linked = only(&lanes, SENDER);
    assert_eq!(
        linked
            .line
            .as_ref()
            .expect("the peer body opens a line")
            .parent,
        None,
        "a refused later route cannot supply the accepted route's parent"
    );
    lanes.finish();
}

/// The inputs whose retirement ended a line unanswered.
fn unanswered(lanes: &Lanes) -> Vec<CanonicalUuid> {
    lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("transitions read")
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::body::SteerLineTransition>())
        .filter(|transition| transition.action == PeerLineAction::Unanswered)
        .map(|transition| transition.input)
        .collect()
}

/// Every peer body fact names where its text lies in the event that wrote it
/// and digests exactly that text: an opening after leading prose, and a keyed
/// reply whose clause has surrounding whitespace and a following packet.
#[test]
fn a_peer_body_records_the_range_and_digest_of_its_event_text() {
    let lanes = Lanes::new("line-body-span");
    let (sender, first) = (lanes.address(SENDER), lanes.address(FIRST));
    let opening_event = format!("Checking in.\nSYN {first}:  is the parser settled?  ");
    lanes.author(SENDER, &opening_event);
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let opening = only(&lanes, FIRST);
    let reply_event = format!(
        "FIN {}:   Ship it.  \nSYN {sender}: one more thing",
        opening.key
    );
    lanes.answer(FIRST, &[&opening.key], &reply_event);

    let bodies: Vec<_> = lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("facts read")
        .facts()
        .iter()
        .filter_map(|envelope| {
            let fact = &envelope.fact;
            fact.read::<wire::body::SteerMessaged>()
                .map(|body| (body.peer_body, body.text, body.span))
                .or_else(|| {
                    fact.read::<wire::body::SteerQueued>()
                        .map(|body| (body.peer_body, body.text, body.span))
                })
        })
        .collect();
    for (kind, event, expected) in [
        (PeerBodyKind::Open, &opening_event, "is the parser settled?"),
        (PeerBodyKind::Final, &reply_event, "Ship it."),
    ] {
        let (_, text, span) = bodies
            .iter()
            .find(|(body, ..)| *body == Some(kind))
            .unwrap_or_else(|| panic!("a {kind:?} body is recorded: {bodies:?}"));
        assert_eq!(text, expected);
        let start = usize::try_from(span.range.start).expect("the start is an offset");
        let end = usize::try_from(span.range.end).expect("the end is an offset");
        assert_eq!(&event[start..end], text, "the range slices out the body");
        assert_eq!(span.digest, wire::QuoteTarget::text_digest(text));
    }
    lanes.finish();
}

/// A lapsed input can never be acknowledged, so the write that lapses it ends
/// its line unanswered, apart from FIN, NAK and the automatic close. Recovery
/// still restores the input under its key on the closed line, and its holder's
/// ACK settles it without reopening the line.
#[test]
fn a_lapsed_input_ends_its_line_unanswered_and_stays_recoverable() {
    let lanes = Lanes::new("line-lapse-ends");
    let first = lanes.address(FIRST);
    lanes.author(SENDER, &format!("SYN {first}: is the parser settled?"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let opening = only(&lanes, FIRST);
    let lapse = steering::SteeringDeadline {
        directive: opening.directive,
        key: opening.key.parse().expect("the delivered key parses"),
        recipient: Actor::Lane(test_uuid(FIRST)),
        expires: opening.expires,
        transition: steering::DeadlineTransition::Lapse,
    };
    assert_eq!(
        steering::apply_deadline(&lanes.bus, &lapse, opening.expires),
        Ok(true)
    );
    assert_eq!(unanswered(&lanes), [opening.directive]);
    assert!(
        lanes.inbox(FIRST).lines.is_empty() && lanes.inbox(SENDER).lines.is_empty(),
        "a line ended unanswered is open on neither lane"
    );

    assert!(matches!(
        lanes.recover_directive(opening.directive),
        Outcome::Reply(_)
    ));
    let restored = only(&lanes, FIRST);
    assert_eq!(restored.key, opening.key, "recovery never re-keys");
    assert_eq!(
        restored.line.as_ref().and_then(|line| line.closed),
        Some(PeerLineAction::Unanswered)
    );
    lanes.answer(FIRST, &[&opening.key], &format!("ACK {}", opening.key));
    let settled = lanes
        .bus
        .read(move |read| read.steering_head(opening.directive))
        .expect("steering reads")
        .expect("the input stays durable");
    assert_eq!(settled.body().disposition, wire::Disposition::Acked);
    assert!(
        lanes.pending(SENDER).is_empty() && lanes.inbox(FIRST).lines.is_empty(),
        "a seen ACK on the closed line delivers nothing and reopens nothing"
    );
    assert_eq!(
        unanswered(&lanes),
        [opening.directive],
        "the line ends once"
    );
    lanes.finish();
}

/// Withdrawing a line's live input ends the line unanswered in the same write.
#[test]
fn a_withdrawn_input_ends_its_line_unanswered() {
    let lanes = Lanes::new("line-withdraw-ends");
    let first = lanes.address(FIRST);
    lanes.author(SENDER, &format!("SYN {first}: is the parser settled?"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let opening = only(&lanes, FIRST);
    lanes.withdraw(opening.directive);
    assert_eq!(unanswered(&lanes), [opening.directive]);
    assert!(lanes.inbox(FIRST).lines.is_empty() && lanes.inbox(SENDER).lines.is_empty());
    lanes.finish();
}
