use super::super::evidence::{CONVERSATION_CARDS, Conversation, TASK_CARDS};
use super::*;

fn uuid(last: &str) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131163{last}")
        .parse()
        .expect("fixture uuid")
}

fn declaration(
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    identity: &str,
) -> LaneSessionDeclared {
    LaneSessionDeclared {
        lane,
        occupant,
        worktree: "/worktree".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        launch: wire::body::LaneLaunchIdentity::new("model", "effort"),
        epoch: uuid("03"),
        stream: "session".to_owned(),
        source: uuid("04"),
        identity: identity.to_owned(),
        member: 1,
        offset: 2,
        length: 3,
    }
}

fn task(key: u64, title: &str) -> Evidence<EvidenceKey> {
    Evidence::task(
        EvidenceKey::Task {
            task: TaskStamp::for_micros(i64::try_from(key).unwrap()).unwrap(),
            statement: Seq::new(key * 2 + 1).unwrap(),
            completion: Seq::new(key * 2 + 2).unwrap(),
            support: wire::rooms::DesignSupport::Implemented,
        },
        title,
        "",
        None,
        &[],
        "checked",
    )
}

#[test]
fn every_declared_occupant_of_a_lane_adds_to_one_profile() {
    let (lane, other) = (uuid("01"), uuid("02"));
    let (first, renewed, unknown, neighbor) = (uuid("11"), uuid("12"), uuid("14"), uuid("21"));
    let mut profiles = LaneProfiles::default();

    profiles.declare(&declaration(lane, first, "session-a"));
    assert_eq!(
        profiles.insert(lane, first, task(1, "retained work")),
        Inserted::Stored
    );
    assert_eq!(
        profiles.insert(lane, unknown, task(2, "unknown work")),
        Inserted::Undeclared,
        "an occupant that declared no session retains nothing"
    );
    profiles.vacate(lane, first);
    profiles.declare(&declaration(lane, renewed, "session-b"));
    assert_eq!(
        profiles.insert(lane, renewed, task(3, "renewed work")),
        Inserted::Stored
    );
    profiles.declare(&declaration(other, neighbor, "session-c"));

    let completions = |lane| {
        profiles
            .profile(lane)
            .expect("declared lane profile")
            .evidence()
            .evidence()
            .map(|card| match card.key() {
                EvidenceKey::Task { completion, .. } => completion.get(),
                EvidenceKey::Conversation { .. } => unreachable!("only task cards"),
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(
        completions(lane),
        [4, 8].into(),
        "an earlier occupant's session and the renewed session share the lane's profile"
    );
    assert!(completions(other).is_empty(), "lanes never share cards");
}

/// Retain one conversation card whose distinctive `topic` rides on shared
/// workflow prose long enough to make a card.
fn said(profiles: &mut LaneProfiles, lane: CanonicalUuid, occupant: CanonicalUuid, topic: &str) {
    let prose = format!(
        "{topic} Reviewers inspect integrated commits, verify acceptance criteria, record findings, \
         and release claims afterwards."
    );
    let conversation = Conversation::read(&prose).expect("enough distinct terms for one card");
    let offset = u64::try_from(prose.len()).expect("fixture prose length");
    assert_eq!(
        profiles.insert(
            lane,
            occupant,
            Evidence::conversation(
                EvidenceKey::Conversation {
                    source: uuid("80"),
                    member: 1,
                    offset,
                    length: 20,
                    caused_by: Vec::new(),
                },
                &conversation,
            ),
        ),
        Inserted::Stored
    );
}

/// A residency row naming `identity` as `lane`'s current session.
fn seated(
    lane: CanonicalUuid,
    occupant: Option<CanonicalUuid>,
    identity: &str,
) -> wire::LaneResidency {
    let declared = declaration(lane, occupant.unwrap_or(uuid("99")), identity);
    wire::LaneResidency {
        lane,
        team: lane,
        name: None,
        worktree: declared.worktree.clone(),
        life: "steer".to_owned(),
        vendor: declared.vendor.clone(),
        model: None,
        effort: None,
        fast_mode: false,
        occupant,
        lineage: Vec::new(),
        capabilities: Vec::new(),
        session: Some(wire::LaneSession {
            lane,
            occupant: declared.occupant,
            worktree: declared.worktree,
            vendor: declared.vendor,
            launch: declared.launch,
            epoch: declared.epoch,
            stream: declared.stream,
            identity: declared.identity,
            locus: wire::TapeLocus {
                member: declared.member,
                offset: declared.offset,
                length: declared.length,
            },
        }),
        disposition: if occupant.is_some() {
            "occupied"
        } else {
            "vacant"
        }
        .to_owned(),
    }
}

#[test]
fn the_conversation_background_counts_each_resident_lanes_profile() {
    let (vacated, renewed, removed) = (uuid("01"), uuid("02"), uuid("03"));
    let mut profiles = LaneProfiles::default();

    profiles.declare(&declaration(vacated, uuid("11"), "vacated"));
    said(
        &mut profiles,
        vacated,
        uuid("11"),
        "Closing sweeps retain vacated profile evidence for successors.",
    );
    profiles.vacate(vacated, uuid("11"));

    profiles.declare(&declaration(renewed, uuid("21"), "superseded"));
    said(
        &mut profiles,
        renewed,
        uuid("21"),
        "Superseded renewal chatter about quarantine ledgers and widgets.",
    );
    profiles.declare(&declaration(renewed, uuid("22"), "renewed"));
    said(
        &mut profiles,
        renewed,
        uuid("22"),
        "Fresh renewal session discussing rarity backgrounds and refits.",
    );

    profiles.declare(&declaration(removed, uuid("31"), "removed"));
    said(
        &mut profiles,
        removed,
        uuid("31"),
        "Removed lane talking about telescopes, harbors, and lanterns.",
    );

    let residency = wire::ResidencyHead {
        epoch: uuid("03"),
        state: wire::ResidentState::Seated,
        topology_revision: 0,
        lanes: vec![
            seated(vacated, None, "vacated"),
            seated(renewed, Some(uuid("22")), "renewed"),
        ],
        subscriptions: Vec::new(),
        renewals: Vec::new(),
    };
    let current = |lane| {
        profiles
            .profile(lane)
            .expect("declared profile")
            .evidence()
            .conversation_messages()
    };
    assert_eq!(
        current(renewed).count(),
        2,
        "a renewed lane keeps its superseded session's conversation"
    );
    assert_eq!(
        profiles.conversation_background(&residency),
        Background::of(current(vacated).chain(current(renewed))),
        "the background counts every resident lane, vacant or renewed, never a removed lane",
    );
}

fn crossed(offset: u64) -> TranscriptCursor {
    TranscriptCursor {
        source: uuid("99"),
        member: 1,
        offset,
    }
}

fn conversation(index: usize) -> Evidence<EvidenceKey> {
    let letter = |value: usize| char::from(b'a' + u8::try_from(value % 26).unwrap());
    let prose = (0..12)
        .map(|term| {
            format!(
                "note{}{}{}",
                letter(index / 26),
                letter(index),
                letter(term)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    Evidence::conversation(
        EvidenceKey::Conversation {
            source: uuid("99"),
            member: 1,
            offset: u64::try_from(index).unwrap(),
            length: 1,
            caused_by: Vec::new(),
        },
        &Conversation::read(&prose).expect("twelve distinct terms admit"),
    )
}

fn ready_count(profiles: &LaneProfiles) -> usize {
    profiles
        .opportunities(None, PROFILE_OPPORTUNITIES)
        .rows
        .iter()
        .filter(|row| matches!(row.kind, ProfileOpportunityKind::Ready { .. }))
        .count()
}

#[test]
fn warming_resumes_from_recorded_cursors_until_both_histories_begin() {
    let mut profiles = LaneProfiles::default();
    let key = profiles.declare(&declaration(uuid("01"), uuid("11"), "session-a"));
    let lane = key.lane;
    profiles.begin_warming(lane);
    assert_eq!(profiles.owed_facts(), Some(None));
    assert_eq!(profiles.owed_transcript(), Some((lane, &key.vendor, None)));

    profiles.warmed_facts(Seq::new(20), false);
    profiles.warmed_transcript(lane, crossed(500), false);
    assert_eq!(profiles.owed_facts(), Some(Seq::new(20)));
    assert_eq!(
        profiles.owed_transcript(),
        Some((lane, &key.vendor, Some(crossed(500))))
    );
    assert!(matches!(
        profiles.profile(lane).unwrap().warm_state(),
        WarmState::Incomplete { .. }
    ));

    profiles.warmed_transcript(lane, crossed(0), true);
    assert_eq!(profiles.owed_transcript(), None);
    assert_eq!(
        ready_count(&profiles),
        0,
        "completion history is still owed"
    );
    profiles.warmed_facts(None, true);
    assert!(profiles.profile(lane).unwrap().warm_state().is_ready());
    assert_eq!(profiles.owed_facts(), None);
    profiles.warmed_facts(None, true);
    profiles.settle_full();
    assert_eq!(ready_count(&profiles), 1, "readiness is announced once");
}

#[test]
fn full_classes_complete_warming_without_older_history_displacing_them() {
    let lane = uuid("01");
    let occupant = uuid("11");
    let mut profiles = LaneProfiles::default();
    profiles.declare(&declaration(lane, occupant, "session-a"));
    profiles.begin_warming(lane);
    for index in 0..TASK_CARDS {
        let index = u64::try_from(index).unwrap();
        assert_eq!(
            profiles.insert(lane, occupant, task(index + 100, &format!("live {index}"))),
            Inserted::Stored
        );
    }
    assert_eq!(
        profiles.retain_older(lane, task(1, "older work")),
        Inserted::Full
    );
    profiles.settle_full();
    assert_eq!(
        profiles.owed_facts(),
        None,
        "a full task class owes nothing"
    );
    assert!(profiles.owed_transcript().is_some());

    for index in 0..CONVERSATION_CARDS {
        assert_eq!(
            profiles.retain_older(lane, conversation(index)),
            Inserted::Stored
        );
    }
    assert_eq!(
        profiles.retain_older(lane, conversation(CONVERSATION_CARDS)),
        Inserted::Full
    );
    profiles.settle_full();
    assert!(profiles.profile(lane).unwrap().warm_state().is_ready());
    assert_eq!(ready_count(&profiles), 1);

    let newest = profiles
        .profile(lane)
        .unwrap()
        .evidence()
        .evidence()
        .map(|card| card.key().clone())
        .collect::<Vec<_>>();
    assert_eq!(newest.first(), Some(task(131, "").key()));
    assert_eq!(
        newest.last(),
        Some(conversation(CONVERSATION_CARDS - 1).key()),
        "the walk's oldest find is the oldest card"
    );
}

#[test]
fn failed_warming_keeps_its_cursor_and_cards_distinct_from_ready() {
    let lane = uuid("01");
    let occupant = uuid("11");
    let mut profiles = LaneProfiles::default();
    profiles.declare(&declaration(lane, occupant, "session-a"));
    profiles.begin_warming(lane);
    profiles.retain_older(lane, task(1, "found before failing"));
    profiles.warmed_facts(Seq::new(20), false);
    profiles.facts_failed("history unreadable");

    let profile = profiles.profile(lane).unwrap();
    assert!(matches!(
        profile.warm_state(),
        WarmState::Failed { cursor, .. } if cursor.facts == Seq::new(20)
    ));
    assert_eq!(profile.evidence().evidence().count(), 1);
    assert_eq!(profiles.owed_facts(), None);
    assert_eq!(profiles.owed_transcript(), None);
    profiles.warmed_facts(None, true);
    profiles.warmed_transcript(lane, crossed(0), true);
    assert!(!profiles.profile(lane).unwrap().warm_state().is_ready());
}

#[test]
fn opportunities_are_cursor_addressed_and_exact_repeats_are_silent() {
    let lane = uuid("01");
    let occupant = uuid("11");
    let mut profiles = LaneProfiles::default();
    profiles.declare(&declaration(lane, occupant, "session-a"));
    let evidence = task(1, "retained work");
    let evidence_key = evidence.key().clone();
    let digest = evidence.digest();

    assert_eq!(
        profiles.insert(lane, occupant, evidence.clone()),
        Inserted::Stored
    );
    assert_eq!(
        profiles.insert(lane, occupant, evidence),
        Inserted::Duplicate
    );
    profiles.begin_warming(lane);
    profiles.warmed_transcript(lane, crossed(0), true);
    profiles.warmed_facts(None, true);
    profiles.design_changed(Seq::new(30).unwrap());

    let first = profiles.opportunities(None, 2);
    assert_eq!(first.rows.len(), 2);
    assert!(first.more);
    assert!(matches!(
        &first.rows[0].kind,
        ProfileOpportunityKind::Declared { occupant: seen } if *seen == occupant
    ));
    assert!(matches!(
        &first.rows[1].kind,
        ProfileOpportunityKind::Stored { key, digest: seen }
            if key == &evidence_key && *seen == digest
    ));
    let rest = profiles.opportunities(first.next, PROFILE_OPPORTUNITIES);
    assert_eq!(rest.rows.len(), 2);
    assert!(!rest.more);
    assert!(matches!(
        rest.rows[0].kind,
        ProfileOpportunityKind::Ready { .. }
    ));
    assert!(matches!(
        rest.rows[1].kind,
        ProfileOpportunityKind::DesignChanged { .. }
    ));
}

#[test]
fn opportunity_cursor_reports_when_bounded_retention_was_crossed() {
    let lane = uuid("01");
    let occupant = uuid("11");
    let mut profiles = LaneProfiles::default();
    profiles.declare(&declaration(lane, occupant, "session-a"));
    for revision in 1..=PROFILE_OPPORTUNITIES + 1 {
        profiles.design_changed(Seq::new(u64::try_from(revision).unwrap()).unwrap());
    }

    let page = profiles.opportunities(Some(1), 1);
    assert!(page.missed);
    assert_eq!(page.rows.len(), 1);
    assert!(page.more);
}

#[test]
fn only_a_sealed_request_notice_marks_its_turn_as_design_caused() {
    let requested: TaskHandle = "ROOMS-dXWW9tFMN".parse().expect("fixture handle");
    let unrelated: TaskHandle = "PRIVATE-dWqTJb3nM".parse().expect("fixture handle");
    let event = |text: String| {
        listen::Event::Prose(listen::Prose {
            role: listen::ProseRole::Operator,
            text,
            phase: None,
            media: Vec::new(),
            external_media: Vec::new(),
            locus: listen::Locus::new(1, 0, 1),
        })
    };
    let sealed = event(format!(
        "<system-notice>\nDesign request {requested} \"Prevent feedback\" asks for your view on \
         the Design as a whole.\n</system-notice>"
    ));
    let ordinary = event(format!(
        "I am independently discussing Design request {unrelated} outside a sealed notice."
    ));

    assert_eq!(
        design_request_causes([&ordinary, &sealed]),
        vec![requested.born().expect("fixture birth")]
    );
}

#[test]
fn backward_warming_attributes_a_turn_the_union_of_its_requests_across_windows() {
    let lane = uuid("01");
    let occupant = uuid("11");
    let mut profiles = LaneProfiles::default();
    profiles.declare(&declaration(lane, occupant, "session-a"));
    profiles.begin_warming(lane);
    let aside = "dXWW9tFMN".parse::<TaskStamp>().expect("fixture stamp");
    let opening = "dWqTJb3nM".parse::<TaskStamp>().expect("fixture stamp");
    let retained = |profiles: &LaneProfiles| {
        profiles
            .profile(lane)
            .unwrap()
            .evidence()
            .evidence()
            .map(|evidence| evidence.key().clone())
            .collect::<Vec<_>>()
    };

    profiles.hold_older_conversation(lane, conversation(2));
    profiles.older_turn_causes(lane, std::slice::from_ref(&aside));
    profiles.warmed_transcript(lane, crossed(2), false);
    assert!(
        retained(&profiles).is_empty(),
        "a window boundary inside a turn cannot settle its cards"
    );
    profiles.older_turn_causes(lane, &[]);
    profiles.older_turn_causes(lane, std::slice::from_ref(&opening));
    profiles.resolve_older_conversation_turn(lane);
    let [card] = retained(&profiles).try_into().expect("one card settles");
    assert!(
        !card.independent_of(&aside),
        "the mid-turn request still counts"
    );
    assert!(!card.independent_of(&opening), "the opening request counts");

    profiles.hold_older_conversation(lane, conversation(1));
    profiles.resolve_older_conversation_turn(lane);
    let older = retained(&profiles)
        .into_iter()
        .find(|evidence| *evidence != card)
        .expect("the older turn's card settles");
    assert!(older.independent_of(&aside) && older.independent_of(&opening));
}
