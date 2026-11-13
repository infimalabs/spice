//! What each capability is shown, and what it is offered.
//!
//! Every principal here is decoded from an acceptance document rather than
//! constructed, because that is the only way the app can obtain one: the
//! resident derives actor and rights when it accepts the connection, and a
//! test that built a `Principal` directly would be proving the app agrees with
//! itself about authority it is not allowed to assert.

use app::{Admitted, Declined, Principal, Session, SteerPane};
use serde::{Deserialize, Serialize};
use wire::{
    Accepted, Answer, CanonicalUuid, Command, CommandKind, Digest, Name, TrustState, Witness,
};

/// Capability the lane session opens with.
const LANE_CAPABILITY: &str = "018f82d4-6f59-7d06-92e3-bce9131163c6";
/// Generation this session addresses.
const EPOCH: &str = "018f82d4-6f59-7d06-92e3-bce9131163c3";
/// The lane a lane-scoped capability is bound to.
const LANE: &str = "018f82d4-6f59-7d06-92e3-bce9131163c2";
/// Identity this fixture mints for a submitted command.
const COMMAND: &str = "018f82d4-6f59-7d06-92e3-bce9131163c4";
/// Identity of some other command in flight at the same moment.
const OTHER_COMMAND: &str = "018f82d4-6f59-7d06-92e3-bce9131163c5";

/// Parse a canonical uuid fixture, which is spelled correctly by construction.
fn uuid(text: &str) -> CanonicalUuid {
    text.parse().expect("fixture uuid is canonical")
}

fn lane_session() -> Session {
    Session::open(uuid(LANE_CAPABILITY), uuid(EPOCH))
}

/// The body of a command and of the reply that answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Note {
    task: String,
    note: String,
}

/// The resident's acceptance naming `actor`, as bytes off the socket.
fn accepted_bytes(actor: &str, epoch: &str) -> Vec<u8> {
    format!(r#"{{ "wire": 1, "actor": "{actor}", "epoch": "{epoch}", "release": "1.2.3" }}"#)
        .into_bytes()
}

/// The resident's admitted principal receipt for one capability-scoped session.
fn accepted(session: &Session, actor: &str, epoch: &str) -> Admitted<Accepted> {
    session
        .admit_accepted(&accepted_bytes(actor, epoch))
        .expect("the fixture acceptance is admitted")
}

/// The principal the resident named, admitted the way panes must receive it.
fn principal(session: &Session, actor: &str) -> Principal {
    Principal::admitted(&accepted(session, actor, EPOCH))
}

/// A session the resident accepted as one seated lane.
fn lane() -> Principal {
    principal(&lane_session(), &format!("lane:{LANE}"))
}

/// The one witness `task::note` requires.
fn existence() -> Vec<Witness> {
    vec![Witness::Existence {
        task: "1kKRRsDPx".parse().expect("fixture task stamp"),
    }]
}

/// The two witnesses `consent::decide` requires.
fn consent_witnesses() -> Vec<Witness> {
    vec![
        Witness::Consent {
            request: uuid(OTHER_COMMAND),
            answer: Answer::Asked,
        },
        Witness::Rule {
            name: "review".parse::<Name>().expect("fixture rule name"),
        },
    ]
}

/// The one witness `trust::grant` requires.
fn trust() -> Vec<Witness> {
    vec![Witness::Trust {
        digest: "a".repeat(64).parse::<Digest>().expect("fixture digest"),
        standing: TrustState::Pended,
    }]
}

/// A note body, as one command's registered DTO.
fn note() -> Note {
    Note {
        task: "SPICE-1kKRRsDPx".to_owned(),
        note: "observed in review".to_owned(),
    }
}

/// A command a lane is offered, built through the pane that offers it.
fn noted(principal: &Principal) -> Command<Note> {
    SteerPane::over(principal)
        .command(uuid(COMMAND), CommandKind::TaskNote, existence(), note())
        .expect("a lane is offered task::note")
}

#[test]
fn a_lane_builds_the_commands_it_is_offered() {
    let lane = lane();

    let command = noted(&lane);

    assert_eq!(command.kind, CommandKind::TaskNote);
    assert_eq!(command.id, uuid(COMMAND));
    assert_eq!(command.expected, existence());
    assert!(command.witnessed().is_ok());
}

#[test]
fn a_command_that_does_not_satisfy_its_law_is_declined_before_it_is_built() {
    let lane = lane();

    let error = SteerPane::over(&lane)
        .command(uuid(COMMAND), CommandKind::TaskNote, trust(), note())
        .expect_err("task::note does not witness trust");

    assert!(matches!(error, Declined::Law(_)), "{error:?}");
}

#[test]
fn no_lane_surface_renders_the_capability_its_session_presented() {
    let session = lane_session();
    let acceptance = accepted(&session, &format!("lane:{LANE}"), EPOCH);
    let lane = Principal::admitted(&acceptance);
    let steer = SteerPane::over(&lane);
    let command = noted(&lane);

    let rendered = format!("{session:?} {acceptance:?} {lane:?} {steer:?} {command:?}");

    assert!(
        !rendered.contains(LANE_CAPABILITY),
        "a lane-scoped surface rendered the session capability: {rendered}"
    );
    assert!(
        rendered.contains(LANE),
        "the surface should still name the lane it belongs to"
    );
}

#[test]
fn a_lane_is_refused_the_consent_decision() {
    let lane = lane();

    // The consent law names predicates and no capability; the registry row
    // states the capability, and this is the behaviour that statement buys.
    assert!(!SteerPane::over(&lane).offers(CommandKind::ConsentDecide));

    let declined = SteerPane::over(&lane)
        .command(
            uuid(COMMAND),
            CommandKind::ConsentDecide,
            consent_witnesses(),
            note(),
        )
        .expect_err("a lane holds no consent decision");

    // The decline names the command and the principal, so the refusal is
    // legible without re-deriving either from the call site.
    let Declined::Authority { kind, actor } = declined else {
        panic!("a lane is declined for authority rather than for witnesses: {declined}");
    };
    assert_eq!(kind, CommandKind::ConsentDecide);
    assert_eq!(actor.to_string(), format!("lane:{LANE}"));
}
