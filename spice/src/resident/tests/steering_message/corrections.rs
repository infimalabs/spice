//! A packet the residency refused whole owes its authoring lane one keyed
//! correction, which only the context that authored the packet may hear.

use super::*;

/// Hear the sender's tape and answer the corrections its lane now holds.
fn refused(lanes: &Lanes) -> Vec<wire::intent::PendingDirective> {
    let (outcomes, diagnostics) = lanes.hear(SENDER, test_uuid(SENDER + 1));
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome, Err(steering::Unpublished::Recorded { .. }))),
        "{outcomes:?}\n{diagnostics}"
    );
    lanes.pending(SENDER)
}

fn disposition(lanes: &Lanes, directive: CanonicalUuid) -> wire::Disposition {
    lanes
        .bus
        .read(move |read| read.steering_head(directive))
        .expect("steering reads")
        .expect("the correction stays durable")
        .body()
        .disposition
}

/// Start `occupant` on a vacated lane through the birth record every wake
/// commits: resuming the retained session, or fresh after reading `replaced`.
fn start(
    lanes: &Lanes,
    lane: u8,
    occupant: CanonicalUuid,
    replaced: Option<Vec<journal::JournalHead<wire::SteeringHead>>>,
) -> CommandResult<()> {
    let lane = test_uuid(lane);
    let observed = current_residency(&lanes.bus, lanes.epoch).expect("residency reads");
    let mut next = observed.body().clone();
    let row = next
        .lanes
        .iter_mut()
        .find(|row| row.lane == lane)
        .expect("fixture lane exists");
    assert!(row.occupant.is_none(), "the lane is vacant");
    row.occupant = Some(occupant);
    row.lineage.push(occupant);
    row.disposition = "occupied".to_owned();
    let proven = crate::resident::lane_surface::ProvenOccupant {
        occupant,
        group: 123,
        session: wire::LaneSession {
            lane,
            occupant,
            worktree: row.worktree.clone(),
            vendor: row.vendor.clone(),
            launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
            epoch: lanes.epoch,
            stream: "stdout".to_owned(),
            identity: format!("session-{occupant}"),
            locus: wire::TapeLocus {
                member: 0,
                offset: 0,
                length: 64,
            },
        },
        source: occupant,
        git: "current".to_owned(),
        fresh_session: replaced.is_some(),
        replaced: replaced.unwrap_or_default(),
    };
    let mut starting = write(Actor::Spice, lanes.epoch, Producer::ResidentRecognition)?;
    crate::resident::shutdown::guard_residency(&mut starting, &observed);
    let declared = starting
        .fact(
            crate::resident::lane_surface::session_fact(&proven.session, proven.source),
            FactCause::Existing(observed.position()),
        )
        .expect("declaration fact is valid");
    let started = proven.started(
        &mut starting,
        lane,
        lanes.epoch,
        declared,
        "lane::fact-invalid",
    )?;
    starting.residency_head(started, move |_| next);
    commit(&lanes.bus, starting).map(|_| ())
}

/// The withheld judgment is the packet's one decision and the correction its
/// one telling: each names the exact packet, and neither a reread nor a
/// restart judges, opens, or publishes anything more.
#[test]
fn a_withheld_packet_opens_one_correction_that_no_reread_or_restart_repeats() {
    let lanes = Lanes::new("syn-correction");
    let sender = lanes.address(SENDER);
    lanes.declare(SENDER);
    lanes.author(SENDER, &format!("SYN {sender}: myself"));
    lanes.author(SENDER, &format!("SYN {sender}: again"));

    let opened = refused(&lanes);
    assert_eq!(lanes.withheld().len(), 2, "each packet is judged once");
    assert_eq!(
        opened.len(),
        2,
        "each judgment opens one correction: {opened:?}"
    );
    assert_ne!(opened[0].key, opened[1].key);
    for body in ["myself", "again"] {
        let naming = opened
            .iter()
            .filter(|correction| correction.text.contains(&format!("\n{body}\n")))
            .count();
        assert_eq!(naming, 1, "one correction repeats `{body}`: {opened:?}");
    }
    assert!(
        opened
            .iter()
            .all(|correction| correction.from.is_none() && !correction.wakes),
        "a correction names no peer and wakes nobody: {opened:?}"
    );
    assert!(
        lanes.messages().is_empty() && lanes.queued_messages().is_empty(),
        "a refused packet reaches no peer"
    );
    let snapshot = lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("correction authority reads");
    for judgment in snapshot
        .facts()
        .iter()
        .filter(|envelope| envelope.fact.read::<wire::body::SteerWithheld>().is_some())
    {
        let corrections = snapshot
            .facts()
            .iter()
            .filter(|envelope| {
                envelope.cause == Some(judgment.seq)
                    && envelope.fact.read::<wire::body::SteerSent>().is_some()
            })
            .collect::<Vec<_>>();
        assert_eq!(corrections.len(), 1, "each source owns one correction");
        assert_eq!(
            corrections[0].at, judgment.at,
            "the correction commits with its source judgment"
        );
    }

    let before = lanes.cursor();
    let (reread, _) = lanes.hear(SENDER, test_uuid(SENDER + 1));
    assert!(
        reread
            .iter()
            .all(|outcome| matches!(outcome, Err(steering::Unpublished::Withheld { .. }))),
        "{reread:?}"
    );
    assert_eq!(lanes.cursor(), before, "a reread writes nothing");
    lanes.restart();
    lanes.restart();
    assert_eq!(lanes.pending(SENDER), opened, "a restart opens nothing");
    assert_eq!(lanes.withheld().len(), 2, "a restart judges nothing");
    lanes.finish();
}

/// An unrepresentable correction deadline leaves its packet undecided,
/// rather than committing a judgment whose author can never hear it.
#[test]
fn correction_admission_failure_leaves_the_packet_undecided() {
    let lanes = Lanes::new("correction-admission-failure");
    let sender = lanes.address(SENDER);
    lanes.author(SENDER, &format!("SYN {sender}: keep my payload"));
    let config: steer::Config = serde_json::from_value(serde_json::json!({
        "expiry": "18446744073709551615s"
    }))
    .expect("the duration is valid but cannot become a wire deadline");
    let before = lanes.cursor();
    let (outcomes, diagnostics) = lanes.hear_config(
        SENDER,
        test_uuid(SENDER + 1),
        steering::Refusals::Record,
        &config,
    );
    assert!(
        matches!(outcomes.as_slice(),
        [Err(steering::Unpublished::Failed(Outcome::Fault(fault)))]
        if fault.reason == "steer::expiry-invalid"),
        "{outcomes:?}\n{diagnostics}"
    );
    assert_eq!(
        lanes.cursor(),
        before,
        "failed admission commits no judgment"
    );
    assert!(lanes.withheld().is_empty());
    assert!(lanes.pending(SENDER).is_empty());

    let [correction] = <[_; 1]>::try_from(refused(&lanes))
        .expect("the undecided packet can be admitted under a valid horizon");
    assert!(correction.text.contains("\nkeep my payload\n"));
    lanes.restart();
    assert_eq!(lanes.pending(SENDER), [correction]);
    lanes.finish();
}

/// ACK drops a correction and NAK declines it; neither returns a reply, and
/// an answered correction is never reopened by its packet. The same body
/// authored again is a new packet with its own judgment and correction.
#[test]
fn an_answered_correction_stays_answered_and_a_fresh_packet_is_a_new_attempt() {
    let lanes = Lanes::new("syn-correction-answered");
    let sender = lanes.address(SENDER);
    lanes.author(SENDER, &format!("SYN {sender}: myself"));
    lanes.author(SENDER, &format!("SYN {sender}: again"));
    let [dropped, declined] = <[_; 2]>::try_from(refused(&lanes)).expect("two corrections");

    lanes.answer(
        SENDER,
        &[&dropped.key],
        &format!("ACK {}: Dropped.", dropped.key),
    );
    lanes.answer(
        SENDER,
        &[&declined.key],
        &format!("NAK {}: Not resending.", declined.key),
    );
    assert_eq!(
        (
            disposition(&lanes, dropped.directive),
            disposition(&lanes, declined.directive)
        ),
        (wire::Disposition::Acked, wire::Disposition::Declined)
    );
    let before = lanes.cursor();
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    assert_eq!(
        lanes.cursor(),
        before,
        "an answered correction's packet opens nothing again"
    );
    assert!(
        [SENDER, FIRST, SECOND]
            .iter()
            .all(|lane| lanes.pending(*lane).is_empty()),
        "answering a correction replies to nobody"
    );

    lanes.author(SENDER, &format!("SYN {sender}: myself"));
    lanes.hear(SENDER, test_uuid(SENDER + 1));
    let [fresh] = <[_; 1]>::try_from(lanes.pending(SENDER)).expect("the new packet's correction");
    assert_ne!(fresh.directive, dropped.directive);
    assert_eq!(lanes.withheld().len(), 3);
    lanes.finish();
}

/// A correction outlives its author for the successor that resumes the
/// author's session, under the same key. A fresh session never held that
/// context: its start parks the correction where the operator can recover
/// it, and a start that did not account for it seats nobody.
#[test]
fn a_correction_follows_a_resumed_session_and_parks_before_a_fresh_one() {
    let lanes = Lanes::new("syn-correction-succession");
    let sender = lanes.address(SENDER);
    lanes.author(SENDER, &format!("SYN {sender}: myself"));
    let opened = refused(&lanes);
    let [correction] = opened.as_slice() else {
        panic!("one correction opens: {opened:?}");
    };

    lanes.vacate(SENDER);
    start(&lanes, SENDER, test_uuid(41), None).expect("the resumed session starts");
    assert_eq!(
        lanes.pending(SENDER),
        opened,
        "the resumed session holds the same correction"
    );

    lanes.vacate(SENDER);
    assert!(
        start(&lanes, SENDER, test_uuid(42), Some(Vec::new())).is_err(),
        "a fresh start that read no correction seats nobody"
    );
    assert_eq!(lanes.pending(SENDER), opened);
    let live = steering::correction::live(&lanes.bus, test_uuid(SENDER)).expect("steering reads");
    start(&lanes, SENDER, test_uuid(42), Some(live)).expect("the fresh session starts");
    assert!(
        lanes.pending(SENDER).is_empty(),
        "a fresh session hears no predecessor's correction"
    );
    let parked = lanes
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("authority reads")
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read::<wire::steering::SteerDeadLettered>())
        .map(|parked| (parked.directive, parked.reason))
        .collect::<Vec<_>>();
    assert_eq!(
        parked,
        [(
            correction.directive,
            steering::correction::AUTHOR_REPLACED.to_owned()
        )]
    );

    lanes.author(SENDER, &format!("SYN {sender}: late"));
    let (late, _) = lanes.hear(SENDER, test_uuid(SENDER + 1));
    assert!(
        matches!(
            late.last(),
            Some(Err(steering::Unpublished::Recorded { .. }))
        ),
        "{late:?}"
    );
    assert!(
        lanes.pending(SENDER).is_empty(),
        "a packet judged after its author was replaced tells the replacement nothing"
    );

    let recovered = lanes.recover_directive(correction.directive);
    assert!(matches!(recovered, Outcome::Reply(_)), "{recovered:?}");
    assert_eq!(
        lanes.pending(SENDER).len(),
        1,
        "only the operator returns a parked correction to its lane"
    );
    lanes.finish();
}
