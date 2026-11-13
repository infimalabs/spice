use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::{RoomPostAction, SteerControlRequest};

fn room() -> wire::TaskHandle {
    wire::TaskHandle::for_topic(
        &"/app/work_detail"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &"1kKRRsDPx".parse().expect("fixture stamp"),
    )
}

fn recorded() -> wire::rooms::RoomPostAnswer {
    wire::rooms::RoomPostAnswer {
        room: room(),
        publication: wire::rooms::RoomAuthorityStamp::for_micros(1_700_000_000_000_000)
            .expect("fixture publication"),
        outcome: wire::rooms::RoomPostOutcome::RecordedNoCurrentListener {
            next: wire::rooms::RoomPostNext::StartOrDirect,
        },
    }
}

/// A settlement follows the observation that carries its text: a recorded
/// post runs it exactly once, and a refused post reports its reason and never
/// settles.
#[test]
fn a_settlement_runs_only_after_its_observation_is_recorded() {
    let settled = Rc::new(Cell::new(0));
    let refusals = Rc::new(RefCell::new(Vec::new()));
    let post = || {
        let settled = Rc::clone(&settled);
        let refusals = Rc::clone(&refusals);
        RoomPostAction::then(
            room(),
            wire::Seq::new(3).expect("fixture head"),
            "Filed against the wrong surface.".to_owned(),
            move || settled.set(settled.get() + 1),
            move |issue| refusals.borrow_mut().push(issue),
        )
    };

    let refused = post();
    (refused.respond)(Err("room::post-refused".to_owned()));
    assert_eq!(settled.get(), 0, "a refused observation settles nothing");
    assert_eq!(*refusals.borrow(), ["room::post-refused"]);

    let accepted = post();
    (accepted.respond)(Ok(recorded()));
    (accepted.respond)(Ok(recorded()));
    assert_eq!(settled.get(), 1, "one recorded observation settles once");
    assert_eq!(refusals.borrow().len(), 1);
}

fn principal_input() -> wire::rooms::PrincipalInputRef {
    wire::rooms::PrincipalInputRef {
        directive: "018f82d4-6f59-7d06-92e3-bce9131163a0"
            .parse()
            .expect("directive"),
        principal: "018f82d4-6f59-7d06-92e3-bce9131163b0"
            .parse()
            .expect("principal"),
    }
}

#[test]
fn line_requests_keep_explicit_finish_and_only_substantive_raw_bodies() {
    assert_eq!(
        SteerControlRequest::line(
            principal_input(),
            wire::intent::SteerLineAction::Ack,
            "   ".to_owned(),
        ),
        SteerControlRequest::Line(wire::intent::SteerLineControl {
            input: principal_input(),
            action: wire::intent::SteerLineAction::Ack,
            body: None,
        })
    );
    assert_eq!(
        SteerControlRequest::line(
            principal_input(),
            wire::intent::SteerLineAction::Fin,
            "final body".to_owned(),
        ),
        SteerControlRequest::Line(wire::intent::SteerLineControl {
            input: principal_input(),
            action: wire::intent::SteerLineAction::Fin,
            body: Some("final body".to_owned()),
        })
    );
}
