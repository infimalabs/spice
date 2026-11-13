use super::*;
use board::similarity::NARROW_MATCH_FLOOR;

fn word(index: usize) -> String {
    let first = char::from(b'a' + ((index / (26 * 26)) % 26) as u8);
    let second = char::from(b'a' + ((index / 26) % 26) as u8);
    let third = char::from(b'a' + (index % 26) as u8);
    format!("term{first}{second}{third}")
}

fn words(range: Range<usize>) -> String {
    range.map(word).collect::<Vec<_>>().join(" ")
}

fn stamp(micros: i64) -> TaskStamp {
    TaskStamp::for_micros(micros).unwrap()
}

fn close(left: f64, right: f64) {
    assert!((left - right).abs() < 1e-12, "{left} != {right}");
}

#[test]
fn conversation_keeps_only_what_the_author_wrote() {
    let prose = "\
ACK dWKbTlwPq: implementing evidence cleaning now.

The resident derives `QuoteAttribution` from canonical transcript sources while retaining authored paragraphs.

SYN 783J: Coordinate snapshot cursor publication boundaries carefully.

> SYN 7851: peerpacketbody from another sender
lazycontinuation stays quoted

> > nestedquote material

````text
fencedword
```
stillfenced
````

    indentedcode

See [quotedlink](spice://session/source) or <spice://quote/autolinked> for details.
";
    let conversation = Conversation::read(prose).expect("authored prose admits");
    let text = conversation.text();
    for kept in [
        "implementing evidence cleaning now.",
        "QuoteAttribution",
        "Coordinate snapshot cursor publication boundaries carefully.",
        "for details.",
    ] {
        assert!(text.contains(kept), "{kept:?} missing from {text:?}");
    }
    for hidden in [
        "dWKbTlwPq",
        "ACK",
        "peerpacketbody",
        "lazycontinuation",
        "nestedquote",
        "fencedword",
        "stillfenced",
        "indentedcode",
        "quotedlink",
        "autolinked",
    ] {
        assert!(!text.contains(hidden), "{hidden:?} leaked into {text:?}");
    }
}

#[test]
fn admission_passages_and_the_byte_bound_are_explicit() {
    assert!(Conversation::read(&words(0..MESSAGE_TERMS - 1)).is_none());
    assert!(Conversation::read(&words(0..MESSAGE_TERMS)).is_some());
    assert!(
        Conversation::read(&format!("ACK dWKbTlwPq: noted.\n\n> {}", words(0..40))).is_none(),
        "quoted material never admits an acknowledgement-only reply"
    );

    let single = Conversation::read(&words(0..40)).unwrap();
    assert_eq!(
        single.passages().count(),
        0,
        "one block is already the whole view"
    );

    let prose = format!(
        "{}\n\n{}\n\n- {}\n- {}\n\n{}\n\n{}\n\n{}",
        words(0..5),
        words(100..112),
        words(200..212),
        words(300..311),
        words(400..412),
        words(500..512),
        words(600..612),
    );
    let conversation = Conversation::read(&prose).unwrap();
    assert_eq!(
        conversation.passages().collect::<Vec<_>>(),
        [
            words(100..112),
            words(200..212),
            words(400..412),
            words(500..512)
        ],
        "the first four paragraphs or list items with enough distinct terms, in order"
    );

    let prose = format!(
        "{}\n\n{}\n\nafterwardmarker {}",
        words(0..12),
        "élan ".repeat(1000),
        words(700..712)
    );
    let bounded = Conversation::read(&prose).unwrap();
    assert!(bounded.text().len() <= CONVERSATION_BYTES);
    assert!(bounded.text().len() > CONVERSATION_BYTES - 'é'.len_utf8() - 4);
    assert!(!bounded.text().contains("afterwardmarker"));
}

#[test]
fn relevance_is_the_best_view_unchanged_by_unrelated_or_repeated_evidence() {
    let frequency = FrequencyProfile::of([
        TermCounts::from_texts([words(0..30).as_str()]),
        TermCounts::from_texts([words(100..130).as_str()]),
    ]);
    let subject = "snapshot cursor publication boundary retention paging";
    let design_counts = TermCounts::from_texts([subject]);
    let design_vector = frequency.prepare(&design_counts).unwrap();
    let design_identity = stamp(1);
    let design = "design";

    let relevant = format!("{}\n\n{subject} {}", words(0..24), words(800..806));
    let mut profile = EvidenceProfile::default();
    profile
        .insert(Evidence::conversation(
            10_u32,
            &Conversation::read(&relevant).unwrap(),
        ))
        .unwrap();

    let summary = |profile: &mut EvidenceProfile<u32>| {
        let found = profile
            .select(
                &frequency,
                [DesignCandidate::new(
                    &design_identity,
                    &design,
                    &design_vector,
                    NARROW_MATCH_FLOOR,
                )],
            )
            .unwrap()
            .expect("the passage clears the floor");
        (*found.evidence, found.view, found.score, found.terms)
    };
    let (evidence, view, score, terms) = summary(&mut profile);
    assert_eq!((evidence, view), (10, View::Passage(1)));
    let passage = TermCounts::from_texts([format!("{subject} {}", words(800..806)).as_str()]);
    close(score, frequency.score(&design_counts, &passage).unwrap());
    let whole = TermCounts::from_texts([relevant.as_str()]);
    assert!(frequency.score(&design_counts, &whole).unwrap() < score);
    assert!(terms.contains(&"snapshot".to_owned()));

    profile
        .insert(Evidence::task(
            11,
            &words(900..910),
            "",
            None,
            &[],
            &words(910..930),
        ))
        .unwrap();
    profile
        .insert(Evidence::conversation(
            12,
            &Conversation::read(&words(1000..1040)).unwrap(),
        ))
        .unwrap();
    assert_eq!(
        summary(&mut profile),
        (evidence, view, score, terms.clone())
    );

    let repeated = Evidence::conversation(13, &Conversation::read(&relevant).unwrap());
    assert_eq!(
        profile.insert(repeated).map_err(|card| *card.key()),
        Err(13)
    );
    assert_eq!(summary(&mut profile), (evidence, view, score, terms));
    assert_eq!(profile.evidence().count(), 3);
}

#[test]
fn task_pitch_contributes_to_matching_and_card_identity() {
    let pitch = "snapshot cursor publication boundary";
    let frequency = FrequencyProfile::of([TermCounts::from_texts([pitch])]);
    let vector = frequency
        .prepare(&TermCounts::from_texts([pitch]))
        .expect("pitch terms are weightable");
    let matching = Evidence::task(14_u32, "Unrelated statement", pitch, None, &[], "");
    let different = Evidence::task(
        15_u32,
        "Unrelated statement",
        "different routing problem",
        None,
        &[],
        "",
    );
    assert_ne!(
        matching.digest(),
        different.digest(),
        "pitch bytes belong to a task evidence card's identity"
    );

    let mut profile = EvidenceProfile::default();
    profile.insert(matching).unwrap();
    let (identity, design) = (stamp(1), "design");
    let found = profile
        .select(
            &frequency,
            [DesignCandidate::new(
                &identity,
                &design,
                &vector,
                NARROW_MATCH_FLOOR,
            )],
        )
        .unwrap()
        .expect("the task pitch matches the Design");
    assert_eq!((*found.evidence, found.view), (14, View::Statement));
}

#[test]
fn each_evidence_class_keeps_its_newest_distinct_cards() {
    let mut profile = EvidenceProfile::default();
    for key in 0..=TASK_CARDS {
        profile
            .insert(Evidence::task(
                key,
                &format!("task evidence number {key}"),
                "",
                None,
                &[],
                "checked",
            ))
            .unwrap();
    }
    for key in 0..=CONVERSATION_CARDS {
        let prose = format!(
            "{} distinct{key}",
            words(key * 20..key * 20 + MESSAGE_TERMS)
        );
        profile
            .insert(Evidence::conversation(
                1_000 + key,
                &Conversation::read(&prose).unwrap(),
            ))
            .unwrap();
    }

    let tasks = profile
        .evidence()
        .filter(|evidence| evidence.kind() == Kind::Task)
        .map(|evidence| *evidence.key())
        .collect::<Vec<_>>();
    let conversations = profile
        .evidence()
        .filter(|evidence| evidence.kind() == Kind::Conversation)
        .map(|evidence| *evidence.key())
        .collect::<Vec<_>>();
    assert_eq!(tasks, (1..=TASK_CARDS).rev().collect::<Vec<_>>());
    assert_eq!(
        conversations,
        (1..=CONVERSATION_CARDS)
            .rev()
            .map(|key| 1_000 + key)
            .collect::<Vec<_>>()
    );
}

#[test]
fn ties_prefer_the_lower_design_then_the_newer_card_then_the_earlier_view() {
    let subject = "snapshot cursor publication boundary retention paging";
    let frequency = FrequencyProfile::of([TermCounts::from_texts([subject])]);
    let vector = frequency
        .prepare(&TermCounts::from_texts([subject]))
        .unwrap();
    let (early, late) = (stamp(1), stamp(2));
    let (lower, higher) = ("lower", "higher");

    let mut profile = EvidenceProfile::default();
    profile
        .insert(Evidence::task(20_u32, subject, "", None, &[], ""))
        .unwrap();
    let found = profile
        .select(
            &frequency,
            [
                DesignCandidate::new(&late, &higher, &vector, NARROW_MATCH_FLOOR),
                DesignCandidate::new(&early, &lower, &vector, NARROW_MATCH_FLOOR),
            ],
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        (*found.design, *found.evidence, found.view),
        (lower, 20, View::Statement),
        "equal statement and completion views keep the earlier view"
    );

    profile
        .insert(Evidence::task(21, subject, "", None, &[], "the"))
        .unwrap();
    let found = profile
        .select(
            &frequency,
            [DesignCandidate::new(
                &early,
                &lower,
                &vector,
                NARROW_MATCH_FLOOR,
            )],
        )
        .unwrap()
        .unwrap();
    assert_eq!(*found.evidence, 21, "an equal newer card wins");
}
#[test]
fn measurement_matches_selection_without_changing_retained_evidence() {
    let subject = "snapshot cursor publication boundary retention paging";
    let frequency = FrequencyProfile::of([
        TermCounts::from_texts([subject]),
        TermCounts::from_texts(["unrelated allocation workflow vocabulary"]),
    ]);
    let vector = frequency
        .prepare(&TermCounts::from_texts([subject]))
        .unwrap();
    let mut profile = EvidenceProfile::default();
    profile
        .insert(Evidence::task(
            30_u32,
            "unrelated statement",
            "",
            Some(subject),
            &[],
            "checked",
        ))
        .unwrap();
    profile
        .insert(Evidence::conversation(
            31,
            &Conversation::read(&format!("{} {subject}", words(600..620))).unwrap(),
        ))
        .unwrap();
    let retained = profile
        .evidence()
        .map(|evidence| (*evidence.key(), evidence.digest()))
        .collect::<Vec<_>>();

    let measured = {
        let first = profile.measure(&frequency, &vector).unwrap().unwrap();
        let second = profile.measure(&frequency, &vector).unwrap().unwrap();
        let measured = (
            *first.evidence,
            first.view,
            first.digest,
            first.score,
            first.terms.clone(),
        );
        assert_eq!(
            measured,
            (
                *second.evidence,
                second.view,
                second.digest,
                second.score,
                second.terms.clone(),
            ),
            "repeated diagnostic measurement is stable"
        );
        measured
    };
    assert_eq!(
        profile
            .evidence()
            .map(|evidence| (*evidence.key(), evidence.digest()))
            .collect::<Vec<_>>(),
        retained,
        "measurement does not alter retained cards"
    );

    let identity = stamp(1);
    let selected = profile
        .select(
            &frequency,
            [DesignCandidate::new(&identity, &"candidate", &vector, 0.0)],
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        measured,
        (
            *selected.evidence,
            selected.view,
            selected.digest,
            selected.score,
            selected.terms.clone(),
        ),
        "diagnostic measurement and production selection use the same best-view ranking"
    );
}

#[test]
fn candidate_floors_admit_the_boundary_without_changing_score_order() {
    let evidence_text = words(0..10);
    let lower_text = format!("{} {}", words(0..5), words(10..15));
    let corpus = format!("{} {}", words(0..15), words(20..25));
    let frequency = FrequencyProfile::of([TermCounts::from_texts([corpus.as_str()])]);
    let evidence_counts = TermCounts::from_texts([evidence_text.as_str()]);
    let higher_vector = frequency.prepare(&evidence_counts).unwrap();
    let lower_counts = TermCounts::from_texts([lower_text.as_str()]);
    let lower_vector = frequency.prepare(&lower_counts).unwrap();
    let lower_score = frequency.score(&evidence_counts, &lower_counts).unwrap();
    let higher_score = frequency.score(&evidence_counts, &evidence_counts).unwrap();
    let above_lower_score = f64::from_bits(lower_score.to_bits() + 1);
    let (lower_identity, higher_identity) = (stamp(1), stamp(2));

    let mut profile = EvidenceProfile::default();
    profile
        .insert(Evidence::task(22_u32, &evidence_text, "", None, &[], ""))
        .unwrap();
    assert!(
        profile
            .select(
                &frequency,
                [DesignCandidate::new(
                    &lower_identity,
                    &"lower",
                    &lower_vector,
                    lower_score,
                )],
            )
            .unwrap()
            .is_some(),
        "a score exactly on its candidate's floor qualifies"
    );
    assert!(
        profile
            .select(
                &frequency,
                [DesignCandidate::new(
                    &lower_identity,
                    &"lower",
                    &lower_vector,
                    above_lower_score,
                )],
            )
            .unwrap()
            .is_none(),
        "a score below its candidate's floor does not qualify"
    );

    let found = profile
        .select(
            &frequency,
            [
                DesignCandidate::new(&lower_identity, &"lower", &lower_vector, 0.0),
                DesignCandidate::new(&higher_identity, &"higher", &higher_vector, higher_score),
            ],
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        *found.design, "higher",
        "candidate floors decide qualification, not ranking"
    );
}

#[test]
fn vectors_from_another_frequency_generation_are_refused() {
    let first = || {
        FrequencyProfile::of([
            TermCounts::from_texts(["snapshot cursor"]),
            TermCounts::from_texts(["publication boundary"]),
        ])
    };
    assert_eq!(first().generation(), first().generation());
    let second = FrequencyProfile::of([TermCounts::from_texts(["snapshot cursor"])]);
    assert_ne!(first().generation(), second.generation());

    let design = TermCounts::from_texts(["snapshot cursor publication boundary"]);
    let stale = first().prepare(&design).unwrap();
    let current = second.prepare(&design).unwrap();
    assert_eq!(
        stale.cosine(&current),
        Err(GenerationMismatch {
            left: stale.generation(),
            right: current.generation(),
        })
    );

    let mut profile = EvidenceProfile::default();
    profile
        .insert(Evidence::task(
            1_u32,
            "snapshot cursor publication boundary",
            "",
            None,
            &[],
            "",
        ))
        .unwrap();
    let identity = stamp(1);
    assert!(
        profile
            .select(
                &second,
                [DesignCandidate::new(
                    &identity,
                    &"design",
                    &stale,
                    NARROW_MATCH_FLOOR,
                )],
            )
            .is_err()
    );
    assert!(
        profile
            .select(
                &second,
                [DesignCandidate::new(
                    &identity,
                    &"design",
                    &current,
                    NARROW_MATCH_FLOOR,
                )],
            )
            .unwrap()
            .is_some(),
        "the same profile matches once the Design is weighted under the current fit"
    );
}
