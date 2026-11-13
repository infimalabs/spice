//! A principal route mints its own steering input on its own line in the
//! publication's write, and that input lapses, recovers, and cancels through
//! the shared actor-addressed steering lifecycle.

use std::collections::BTreeSet;

use wire::Disposition;
use wire::body::{PeerBodyKind, PeerLineAction};
use wire::rooms::{PrincipalInputRef, RoomConversed};

use super::*;
use crate::resident::steering::{DeadlineTransition, SteeringDeadline, apply_deadline};

/// The principal input one operator-input SYN beside a lane route minted.
struct Minted {
    input: PrincipalInputRef,
    conversed: wire::Envelope<wire::AnyFact>,
    head: wire::SteeringHead,
}

impl Lanes {
    fn mint_principal_input(&self) -> Minted {
        let key = self.direct(SENDER, 20, "ship when green");
        let first = self.address(FIRST);
        self.say(SENDER, &format!("SYN #{key} {first}: ship it?"))
            .expect("the operator SYN publishes");
        let [conversed] = self
            .bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read")
            .facts()
            .iter()
            .filter(|envelope| envelope.fact.read::<RoomConversed>().is_some())
            .cloned()
            .collect::<Vec<_>>()
            .try_into()
            .expect("one conversation record");
        let publication = conversed
            .fact
            .read::<RoomConversed>()
            .expect("conversation record")
            .publication;
        let principal = self
            .bootstrap()
            .principal()
            .expect("the bootstrap operator is a principal");
        let input = PrincipalInputRef::published(&publication, principal);
        let head = self.input_head(input.directive);
        Minted {
            input,
            conversed,
            head,
        }
    }

    fn input_head(&self, directive: CanonicalUuid) -> wire::SteeringHead {
        self.bus
            .read(move |read| read.steering_head(directive))
            .expect("steering index reads")
            .expect("the principal route minted its input")
            .into_body()
    }

    fn line_head(&self, line: CanonicalUuid) -> journal::PeerLineHead {
        self.bus
            .read(move |read| read.peer_line_head(line))
            .expect("peer lines read")
            .expect("the input opened its line")
    }

    fn lapse(&self, head: &wire::SteeringHead) -> bool {
        apply_deadline(
            &self.bus,
            &SteeringDeadline {
                directive: head.directive,
                key: head.key.parse().expect("indexed key"),
                recipient: head.recipient.clone(),
                expires: head.expires,
                transition: DeadlineTransition::Lapse,
            },
            head.expires,
        )
        .expect("the deadline applies")
    }
}

/// The input addresses exactly its principal from the authoring lane, carries
/// the raw body on a line it opens, is caused by its publication, and is
/// listed by the principal's pending read, beside an independent lane route.
#[test]
fn a_principal_route_mints_its_input_and_line_with_the_publication() {
    let lanes = Lanes::new("principal-input-mint");
    let Minted {
        input,
        conversed,
        head,
    } = lanes.mint_principal_input();
    let principal = Actor::Principal(input.principal);
    let author = Actor::Lane(test_uuid(SENDER));

    assert_eq!(head.recipient, principal);
    assert_eq!(head.sender, author);
    assert_eq!(head.text, "ship it?");
    assert_eq!(head.disposition, Disposition::Live);
    assert_eq!(head.fanout, None);
    let (line, body, covered) = head
        .control
        .as_deref()
        .and_then(wire::parse_peer_line_control)
        .expect("the input carries ordinary peer-line control");
    assert_eq!(
        (line.root, line.parent, line.opening, body),
        (input.directive, None, input.directive, PeerBodyKind::Open)
    );
    assert!(covered.is_empty());
    let created = head.created;
    let creating = lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("facts read")
        .facts()
        .iter()
        .find(|envelope| envelope.seq == created)
        .cloned()
        .expect("the creating fact is durable");
    assert_eq!(
        creating.cause,
        Some(conversed.seq),
        "a reader returns from the input to its publication"
    );

    let opened = lanes.line_head(input.directive);
    assert_eq!(
        opened.participants(),
        &BTreeSet::from([author, principal.clone()])
    );
    assert_eq!(opened.terminal(), None);

    let listed = lanes
        .bus
        .read(move |read| {
            read.pending_steering(Some(&principal), journal::FactPageStart::After(None))
        })
        .expect("principal pending reads");
    assert_eq!(
        listed
            .rows()
            .iter()
            .map(|row| row.body().directive)
            .collect::<Vec<_>>(),
        [input.directive]
    );
    let [(_, sibling)] = lanes
        .messages()
        .try_into()
        .expect("the lane route is its own peer message");
    assert_eq!(sibling.lane, test_uuid(FIRST));
    assert_ne!(sibling.directive, input.directive);
    lanes.finish();
}

/// The input lapses at its expiry and ends its line unanswered without
/// touching the sibling lane route; operator recovery returns it live under
/// a fresh expiry, and operator cancellation retires it, each answering its
/// principal recipient.
#[test]
fn a_principal_input_lapses_recovers_and_cancels_through_shared_steering() {
    let lanes = Lanes::new("principal-input-lifecycle");
    let Minted { input, head, .. } = lanes.mint_principal_input();
    let principal = Actor::Principal(input.principal);

    assert!(lanes.lapse(&head), "the due input lapses");
    assert!(!lanes.lapse(&head), "a lapsed input lapses once");
    let lapsed = lanes.input_head(input.directive);
    assert_eq!(lapsed.disposition, Disposition::Lapsed);
    assert_eq!(
        lanes
            .line_head(input.directive)
            .terminal()
            .map(|terminal| terminal.action),
        Some(PeerLineAction::Unanswered)
    );
    let [(_, sibling)] = lanes.messages().try_into().expect("one lane route");
    let sibling_directive = sibling.directive;
    assert_eq!(
        lanes.input_head(sibling_directive).disposition,
        Disposition::Live,
        "a sibling route keeps its own standing"
    );

    let Outcome::Reply(Reply {
        body: CommandAnswer::Directive(recovered),
        ..
    }) = lanes.recover_directive(input.directive)
    else {
        panic!("operator recovery of a lapsed principal input commits");
    };
    assert_eq!(recovered.recipient, principal);
    assert_eq!(recovered.disposition, Disposition::Live);
    let live = lanes.input_head(input.directive);
    assert_eq!(live.disposition, Disposition::Live);
    assert!(live.expires > head.expires, "recovery renews the expiry");

    lanes.withdraw(input.directive);
    assert_eq!(
        lanes.input_head(input.directive).disposition,
        Disposition::Canceled
    );
    lanes.finish();
}
