use super::*;

#[test]
fn emitted_reminders_ack_decline_escalate_and_expire_as_ordinary_steering() {
    let fixture = Fixture::new("ordinary-steering");
    let bag = bag(&[
        ("polling", &["polling"], "Wait."),
        ("fallbacks", &["fallback"], "Keep one path."),
    ]);
    fixture.recognize(
        &bag,
        &prose_record(fixture.lane, fixture.occupant, "polling fallback"),
    );
    let snapshot = fixture
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("snapshot");
    let reminders = reminders(&snapshot);
    let mut disposed = steer::Ledger::from_snapshot(&snapshot).expect("ledger");
    disposed
        .absorb(steer::Steering::Acked(&wire::body::SteerAcked {
            directive: reminders[0].directive,
            key: reminders[0].key.clone(),
            recipient: Actor::Lane(fixture.lane),
            evidence: wire::body::SteeringEvidence::Transcript {
                epoch: fixture.epoch,
                locus: reminders[0].locus,
            },
        }))
        .expect("semantic ACK retires reminder");
    disposed
        .absorb(steer::Steering::Declined(&wire::body::SteerDeclined {
            directive: reminders[1].directive,
            key: reminders[1].key.clone(),
            recipient: Actor::Lane(fixture.lane),
            evidence: wire::body::SteeringEvidence::Transcript {
                epoch: fixture.epoch,
                locus: reminders[1].locus,
            },
            reason: "not applicable".to_owned(),
        }))
        .expect("semantic NAK retires reminder");
    assert_eq!(
        disposed.disposition(&reminders[0].key.parse().expect("key")),
        Some(Disposition::Acked)
    );
    assert_eq!(
        disposed.disposition(&reminders[1].key.parse().expect("key")),
        Some(Disposition::Declined)
    );

    let mut lifecycle = steer::Ledger::from_snapshot(&snapshot).expect("ledger");
    lifecycle
        .absorb(steer::Steering::Escalated(&wire::body::SteerEscalated {
            directive: reminders[0].directive,
            key: reminders[0].key.clone(),
            recipient: Actor::Lane(fixture.lane),
            stage: wire::body::SteeringStage::Reminded,
        }))
        .expect("reminder escalates");
    lifecycle
        .absorb(steer::Steering::Lapsed(&wire::body::SteerLapsed {
            key: reminders[0].key.clone(),
            recipient: Actor::Lane(fixture.lane),
            expires: reminders[0].expires,
        }))
        .expect("stable reminder expires");
    assert_eq!(
        lifecycle.disposition(&reminders[0].key.parse().expect("key")),
        Some(Disposition::Lapsed)
    );
    fixture.finish();
}
