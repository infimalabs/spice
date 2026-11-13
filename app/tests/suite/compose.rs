//! Form decisions preserve accepted input and reject invalid submissions with
//! typed refusals. Human-facing explanations are not fixed output spellings.

use app::compose::{self, Refusal};

#[test]
fn steering_a_seated_lane_returns_the_seat_beside_the_trimmed_message() {
    let (seat, text) =
        compose::steer("  ship it  ", Some(("lane", "occupant"))).expect("a seated lane steers");
    assert_eq!(seat, ("lane", "occupant"));
    assert_eq!(text, "ship it");
}

/// An unseated selection is refused before the message is even read.
///
/// The seat travels inside the accepted result, so this ordering is what makes
/// the caller unable to hold a message without the lane it goes to. Were the
/// message checked first, a blank message on an empty seat would report the
/// wrong problem and the caller would need its own second check.
#[test]
fn steering_with_no_seated_lane_is_refused_before_the_message_is_read() {
    let refusal =
        compose::steer::<(&str, &str)>("ship it", None).expect_err("an unseated selection refuses");
    assert_eq!(refusal, Refusal::Unseated);

    let blank =
        compose::steer::<(&str, &str)>("", None).expect_err("an unseated selection refuses");
    assert_eq!(blank, Refusal::Unseated, "the seat outranks the message");
}

#[test]
fn steering_a_blank_message_is_refused_by_name() {
    for written in ["", " ", "\n\t "] {
        let refusal =
            compose::steer(written, Some("lane")).expect_err("a blank message refuses to send");
        assert!(matches!(refusal, Refusal::Missing { .. }));
    }
}

/// A collapse carries the trimmed draft as its reason and refuses without one.
#[test]
fn collapsing_a_design_needs_a_reason_and_trims_it() {
    for blank in ["", " ", "\n\t "] {
        let refusal = compose::collapse(blank).expect_err("a collapse needs a reason");
        assert!(matches!(refusal, Refusal::Missing { .. }));
    }
    assert_eq!(
        compose::collapse(" the premise did not hold ").expect("a reason collapses"),
        "the premise did not hold"
    );
}
