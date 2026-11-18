//! Cross-crate proof that indexed CAS admission and canonical replay agree.

use journal::{FactCause, GuardError, Journal, JournalGuard, JournalHead, JournalWrite};
use wire::body::{SteerCanceled, SteerEscalated, SteerSent, SteeringStage};
use wire::{Actor, CanonicalUuid, Disposition, Instant, Origin, Producer, SteeringHead};

#[test]
fn indexed_race_winner_matches_canonical_replay() {
    let mut journal = Journal::in_memory().expect("steering journal");
    journal.apply(send_write()).expect("publication applies");
    let observed = journal
        .read(|read| read.steering_key("1kJK6N3Rx"))
        .expect("indexed head reads")
        .expect("indexed head exists");

    let winner = cancel_write(&observed);
    let loser = cancel_write(&observed);
    journal.apply(winner).expect("one indexed admission wins");
    journal
        .apply(loser)
        .expect_err("the stale indexed admission loses");

    let indexed_head = journal
        .read(|read| read.steering_key("1kJK6N3Rx"))
        .expect("winning indexed head reads")
        .expect("winning indexed head exists");
    let indexed = steer::Ledger::from_authority_heads([indexed_head.body()])
        .expect("indexed standing reduces");
    let canonical =
        steer::Ledger::from_snapshot(&journal.fact_snapshot().expect("canonical audit reads"))
            .expect("canonical audit reduces");

    assert_eq!(indexed, canonical);
}

#[test]
fn stale_disposition_cannot_erase_a_winning_escalation_from_the_index() {
    let mut journal = Journal::in_memory().expect("steering journal");
    journal.apply(send_write()).expect("publication applies");
    let observed = journal
        .read(|read| read.steering_key("1kJK6N3Rx"))
        .expect("indexed head reads")
        .expect("indexed head exists");

    let stale = cancel_write(&observed);
    journal
        .apply(escalate_write(&observed))
        .expect("escalation wins");
    journal
        .apply(stale)
        .expect_err("disposition built from the pre-escalation head loses");

    let indexed_head = journal
        .read(|read| read.steering_key("1kJK6N3Rx"))
        .expect("winning indexed head reads")
        .expect("winning indexed head exists");
    let indexed = steer::Ledger::from_authority_heads([indexed_head.body()])
        .expect("indexed standing reduces");
    let canonical =
        steer::Ledger::from_snapshot(&journal.fact_snapshot().expect("canonical audit reads"))
            .expect("canonical audit reduces");

    assert_eq!(indexed, canonical);
    assert_eq!(indexed_head.body().stage, SteeringStage::Reminded);
    assert_eq!(indexed_head.body().escalations.len(), 1);
}

fn send_write() -> JournalWrite {
    let body = SteerSent {
        sender: Actor::TEST_PRINCIPAL,
        room: None,
        directive: directive(),
        fanout: None,
        recipients: Vec::new(),
        key: "1kJK6N3Rx".to_owned(),
        recipient: Actor::Lane(lane()),
        text: "hold the indexed line".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires: expiry(),
    };
    let minted_micros = body
        .key
        .parse::<steer::Key>()
        .expect("current steering key")
        .minted_micros();
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin(),
        now(),
        Producer::CommandDoor,
    );
    let pending = write
        .fact(body.clone(), FactCause::Root)
        .expect("sent fact builds");
    write.steering_head(pending, move |seq| SteeringHead {
        sender: body.sender,
        room: body.room.clone(),
        directive: body.directive,
        fanout: body.fanout,
        recipients: Vec::new(),
        key: body.key,
        recipient: body.recipient,
        text: body.text,
        control: body.control,
        attachments: body.attachments,
        published: now(),
        expires: body.expires,
        stage: SteeringStage::Published,
        escalations: Vec::new(),
        disposition: Disposition::Live,
        origin: origin(),
        created: seq,
        minted_micros,
    });
    write
}

fn cancel_write(observed: &JournalHead<SteeringHead>) -> JournalWrite {
    let expected = observed.clone();
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin(),
        now(),
        Producer::CommandDoor,
    );
    write.guard(JournalGuard::new(move |read| {
        let current = read
            .steering_head(expected.body().directive)?
            .ok_or_else(|| GuardError::refused("directive disappeared"))?;
        if current.position() != expected.position()
            || current.body().disposition != Disposition::Live
        {
            return Err(GuardError::refused("directive standing moved"));
        }
        Ok(())
    }));
    let pending = write
        .fact(
            SteerCanceled {
                key: observed.body().key.clone(),
                recipient: observed.body().recipient.clone(),
                reason: None,
            },
            FactCause::Existing(observed.position()),
        )
        .expect("cancel fact builds");
    let mut next = observed.body().clone();
    next.disposition = Disposition::Canceled;
    write.steering_head(pending, move |_| next);
    write
}

fn escalate_write(observed: &JournalHead<SteeringHead>) -> JournalWrite {
    let expected = observed.clone();
    let body = SteerEscalated {
        directive: observed.body().directive,
        key: observed.body().key.clone(),
        recipient: observed.body().recipient.clone(),
        stage: SteeringStage::Reminded,
    };
    let mut write = JournalWrite::new(Actor::Spice, origin(), later(), Producer::BusTimer);
    write.guard(JournalGuard::new(move |read| {
        let current = read
            .steering_head(expected.body().directive)?
            .ok_or_else(|| GuardError::refused("directive disappeared"))?;
        if current.position() != expected.position()
            || current.body().disposition != Disposition::Live
            || current.body().stage != expected.body().stage
        {
            return Err(GuardError::refused("directive standing moved"));
        }
        Ok(())
    }));
    let pending = write
        .fact(body.clone(), FactCause::Existing(observed.body().created))
        .expect("escalation fact builds");
    let next =
        steer::Ledger::transition_head(observed.body(), steer::Steering::Escalated(&body), later())
            .expect("escalation head builds");
    write.steering_head(pending, move |_| next);
    write
}

fn directive() -> CanonicalUuid {
    "018f82d4-6f59-7d06-92e3-bce913116399"
        .parse()
        .expect("directive")
}

fn lane() -> CanonicalUuid {
    "018f82d4-6f59-7d06-92e3-bce913116302"
        .parse()
        .expect("lane")
}

fn origin() -> Origin {
    "resident:018f82d4-6f59-7d06-92e3-bce913116304"
        .parse()
        .expect("origin")
}

fn now() -> Instant {
    "2030-04-12T18:44:09.271Z".parse().expect("instant")
}

fn later() -> Instant {
    "2030-04-12T18:45:09.271Z".parse().expect("later instant")
}

fn expiry() -> Instant {
    "2030-04-12T19:44:09.271Z".parse().expect("expiry")
}
