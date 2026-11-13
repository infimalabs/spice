//! A peer message's sender hears each outcome that reached it as no text: a
//! lapse, a bare FIN or a bare NAK. A reason reaches it as a reply instead.

use super::*;
use crate::resident::steering::{LineOutcome, LineOutcomeNotice, line_outcomes};
use wire::intent::PendingDirective;

/// Open one peer line from the sender to the first lane and return its input.
fn opened(lanes: &Lanes) -> PendingDirective {
    let first = lanes.address(FIRST);
    lanes.author(
        SENDER,
        &format!("SYN {first}: is the parser settled?\nSay so."),
    );
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    lanes.declare(SENDER);
    lanes.declare(FIRST);
    let [input] = lanes
        .pending(FIRST)
        .try_into()
        .unwrap_or_else(|pending| panic!("one input is pending: {pending:?}"));
    input
}

/// Every notice recorded after `before`, for a sender seated by its fixture
/// occupant.
fn told_since(lanes: &Lanes, before: u64) -> Vec<(CanonicalUuid, LineOutcomeNotice)> {
    let first = wire::Seq::new(before + 1).expect("a later position is positive");
    let last = wire::Seq::new(lanes.cursor()).expect("the journal has facts");
    line_outcomes(&lanes.bus, first, last, |lane| {
        (lane == test_uuid(SENDER)).then_some(test_uuid(SENDER + 1))
    })
    .expect("outcomes read")
}

fn only_notice(told: Vec<(CanonicalUuid, LineOutcomeNotice)>) -> LineOutcomeNotice {
    let [(lane, notice)] = told
        .try_into()
        .unwrap_or_else(|told| panic!("one notice is told: {told:?}"));
    assert_eq!(
        lane,
        test_uuid(SENDER),
        "the notice goes to the sending lane"
    );
    assert_eq!(notice.occupant, test_uuid(SENDER + 1));
    notice
}

/// A peer message that lapses tells its sender, once, that it went
/// unanswered after its whole lifetime, quoting its first line.
#[test]
fn a_lapsed_peer_message_tells_its_sender_it_went_unanswered() {
    let lanes = Lanes::new("outcome-lapse");
    let opening = opened(&lanes);
    let before = lanes.cursor();
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
    let notice = only_notice(told_since(&lanes, before));
    assert_eq!(notice.input, opening.directive);
    assert_eq!(notice.excerpt, "is the parser settled?");
    let lifetime = steer::config::EXPIRY.as_secs() / 60;
    assert_eq!(notice.outcome, LineOutcome::Lapsed { minutes: lifetime });
    lanes.finish();
}

/// A bare FIN or NAK still settles its input and closes the line, and its
/// sender hears which one ended it.
#[test]
fn a_bare_fin_or_nak_tells_its_sender_final_or_declined() {
    for (control, disposition, outcome) in [
        ("FIN", wire::Disposition::Acked, LineOutcome::Final),
        ("NAK", wire::Disposition::Declined, LineOutcome::Declined),
    ] {
        let lanes = Lanes::new(&format!("outcome-bare-{control}"));
        let opening = opened(&lanes);
        let before = lanes.cursor();
        lanes.answer(
            FIRST,
            &[&opening.key],
            &format!("{control} {}", opening.key),
        );
        let settled = lanes
            .bus
            .read(move |read| read.steering_head(opening.directive))
            .expect("steering reads")
            .expect("the input stays durable");
        assert_eq!(settled.body().disposition, disposition);
        assert!(
            lanes.inbox(SENDER).lines.is_empty(),
            "{control} closes the line"
        );
        let notice = only_notice(told_since(&lanes, before));
        assert_eq!((notice.input, notice.outcome), (opening.directive, outcome));
        lanes.finish();
    }
}

/// A NAK with a reason delivers that reason as a reply, so no notice repeats it.
#[test]
fn a_reason_reaches_the_sender_as_a_reply_not_a_notice() {
    let lanes = Lanes::new("outcome-reason");
    let opening = opened(&lanes);
    let before = lanes.cursor();
    lanes.answer(
        FIRST,
        &[&opening.key],
        &format!("NAK {}: Main is red.", opening.key),
    );
    let [reply] = lanes
        .pending(SENDER)
        .try_into()
        .unwrap_or_else(|pending| panic!("one reply is pending: {pending:?}"));
    assert!(reply.text.contains("Main is red."), "{reply:?}");
    assert_eq!(told_since(&lanes, before), []);
    lanes.finish();
}
