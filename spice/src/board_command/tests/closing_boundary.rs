use super::*;

/// The boundary a closing fact recorded, exactly as it reached the journal.
fn closing_boundary(bus: &Bus, kind: wire::FactKind) -> Option<serde_json::Value> {
    let facts = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("closing boundary facts read");
    let closing = facts
        .facts()
        .iter()
        .find(|fact| fact.fact.kind() == kind)
        .expect("the closing fact is durable");
    closing.fact.body().get("source").cloned()
}

/// What a lane with no accounted transcript proves at a closing boundary.
///
/// Empty heads is a real answer: the lane opens at position zero.
fn proven_at_zero() -> serde_json::Value {
    serde_json::json!({ "proven": { "heads": [] } })
}

/// The generation and position the seeded lane's one accounted head carries.
///
/// Written out rather than derived from the seeded head, so the assertion pins
/// content a wrong-lane read cannot produce instead of restating whatever the
/// fixture happened to build.
const SEEDED_SOURCE: &str = "018f82d4-6f59-7d06-92e3-00000000004d";
const SEEDED_MEMBER: u64 = 4;
const SEEDED_OFFSET: u64 = 512;

/// Seed one lane's accounted transcript so its boundary is addressable.
///
/// Every lane in this harness opens with no transcript, and an empty `Proven`
/// is byte-identical for every lane: a boundary read for the wrong lane, or for
/// no lane at all, compares equal to the right one and the loss never shows.
/// `transcript_source_heads` is filtered by lane, so seeding exactly one lane
/// makes the read addressable -- that lane answers with this head while every
/// other lane still answers with none. The same hazard is guarded the same way
/// in `board/tests/suite/allocation.rs`.
fn seed_transcript(bus: &Bus, lane: CanonicalUuid) {
    let vendor: wire::Name = "omp".parse().expect("fixture vendor");
    let source: CanonicalUuid = SEEDED_SOURCE.parse().expect("fixture source generation");
    let next = body::TranscriptPosition {
        member: SEEDED_MEMBER,
        offset: SEEDED_OFFSET,
    };
    let head = wire::TranscriptSourceHead {
        lane,
        vendor: vendor.clone(),
        stream: "session".to_owned(),
        source,
        next,
    };
    let batch = body::JournalIngested {
        lane,
        vendor,
        stream: "session".to_owned(),
        source,
        supersedes: None,
        from: body::TranscriptPosition {
            member: 1,
            offset: 0,
        },
        to: next,
        usage: Vec::new(),
        non_usage_records: 1,
        residue_records: 0,
    };
    let origin = format!("lane:{lane}")
        .parse()
        .expect("fixture transcript origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("current fixture instant");
    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::ResidentRecognition);
    let pending = write
        .fact(batch, FactCause::Root)
        .expect("fixture ingestion fact builds");
    write.transcript_source_head(pending, move |_seq| head);
    match bus
        .commit(write)
        .expect("fixture transcript reaches writer")
    {
        CommitOutcome::Committed(_) => {}
        outcome => panic!("fixture transcript did not commit: {outcome:?}"),
    }
}

/// The boundary the seeded lane proves, spelled out independently of the seed.
fn proven_with_seeded_head(lane: CanonicalUuid) -> serde_json::Value {
    serde_json::json!({
        "proven": {
            "heads": [{
                "lane": lane.to_string(),
                "vendor": "omp",
                "stream": "session",
                "source": SEEDED_SOURCE,
                "next": { "member": SEEDED_MEMBER, "offset": SEEDED_OFFSET },
            }]
        }
    })
}

/// Pins the closing-boundary reads in `task_done` and `prepare_review` before
/// they write `TaskDone` and `TaskReviewed`.
///
/// The review is issued by a second lane, because a review routes to a peer,
/// and the boundary recorded is the reviewing lane's own. That last property is
/// what the seed exists for: only the reviewing lane is given an accounted
/// transcript, so the two lanes no longer prove the same bytes. Reading the
/// completing lane's boundary for the review yields empty heads and reds this
/// test, as does a completion boundary that ignored its own lane.
#[test]
fn a_finding_review_records_the_reviewing_lanes_own_closing_boundary() {
    let (_root, bus, epoch, seats) = fixture("closing-boundary-review", 2);
    let lane_one = Actor::Lane(seats[0].lane);
    let lane_two = Actor::Lane(seats[1].lane);
    seed_transcript(&bus, seats[1].lane);
    declare_session(&bus, epoch, &seats[1], uuid(9_201), 201);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLOSE-rev1",
        "Complete and review",
        None,
    );
    advance_task_to_review(&bus, epoch, &seats[0], &lane_one, &task);
    complete_review_with_finding(&bus, epoch, &seats[1], &lane_two, &task);

    assert_eq!(
        closing_boundary(&bus, wire::FactKind::TaskReviewed),
        Some(proven_with_seeded_head(seats[1].lane)),
        "recording a review must retain the boundary it evaluated at, and must \
         read it for the reviewing lane: only that lane has an accounted \
         transcript, so any other lane would prove empty heads here"
    );
    assert_eq!(
        closing_boundary(&bus, wire::FactKind::TaskDone),
        Some(proven_at_zero()),
        "the completing lane has no accounted transcript of its own, so its \
         boundary stays proven at zero even though the journal now holds a \
         head: a boundary that ignored its lane would carry the seeded head here"
    );
}
