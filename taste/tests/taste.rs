use gate::PolicyContext;
use listen::{Event, Locus, Prose, ProseRole, Usage};
use taste::{
    Bag, MAX_JUDGE_SOURCE_BYTES, Taste, Trigger, TriggerError, bag_identity, normalize_prose,
    reminder_identity,
};
use wire::FactKind;

const BEHAVIOR: &str = include_str!("fixtures/behavior.txt");
const MENTION: &str = include_str!("fixtures/mention.txt");

fn locus(text: &str) -> Locus {
    Locus::new(7, 31, text.len() as u64)
}

fn prose(text: &str) -> Event {
    Event::Prose(Prose {
        role: ProseRole::Assistant,
        text: text.to_owned(),
        phase: Some("final".to_owned()),
        media: Vec::new(),
        external_media: Vec::new(),
        locus: locus(text),
    })
}

fn fallback_taste() -> Taste {
    Taste::new(
        "fallbacks".parse().expect("taste name"),
        ["fall back", "fallback", "polling"],
        "Keep one deterministic path.",
    )
    .expect("taste fixture")
}

fn matching_hits(bag: &Bag, event: &Event) -> Vec<taste::Hit> {
    bag.hits(event, &PolicyContext::new())
        .expect("unconstrained taste context")
}

#[test]
fn exact_alphabetic_words_and_phrases_follow_one_matching_rule() {
    let bag = Bag::new(vec![fallback_taste()]).expect("bag");
    let hits = matching_hits(&bag, &prose(BEHAVIOR));

    assert_eq!(
        hits.iter().map(|hit| hit.trigger()).collect::<Vec<_>>(),
        vec!["fall back", "polling"],
    );
    assert!(hits.iter().all(|hit| hit.context() == BEHAVIOR));
    assert!(hits.iter().all(|hit| hit.locus() == locus(BEHAVIOR)));
    assert_eq!(hits[0].span().end - hits[0].span().first, 2);
    assert_eq!(hits[1].span().end - hits[1].span().first, 1);

    let exact = Taste::new(
        "shims".parse().expect("taste name"),
        ["shim"],
        "Replace the old shape.",
    )
    .expect("taste fixture");
    let exact = Bag::new(vec![exact]).expect("bag");
    let exact_hits = matching_hits(&exact, &prose("A shimmer is not a shim."));
    assert_eq!(exact_hits.len(), 1, "a bare word matches a whole token");
}

#[test]
fn a_hit_names_the_exact_line_and_byte_column_of_its_first_token() {
    let bag = Bag::new(vec![fallback_taste()]).expect("bag");
    let hits = matching_hits(&bag, &prose("prefix\n  Polling now"));

    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].line(), hits[0].column()), (2, 3));

    let unicode = matching_hits(&bag, &prose("é: polling"));
    assert_eq!(unicode.len(), 1);
    assert_eq!(unicode[0].column(), 5, "the column counts UTF-8 bytes");
}

#[test]
fn judge_source_selects_the_unicode_sentence_containing_the_exact_match() {
    let taste = Taste::new(
        "literal".parse().expect("taste name"),
        ["élan vital"],
        "Keep the relevant evidence.",
    )
    .expect("taste fixture");
    let bag = Bag::new(vec![taste]).expect("bag");
    let text = "Ignore this sentence. \nUse ÉLAN—VITAL now! Ignore this too?";
    let hit = matching_hits(&bag, &prose(text))
        .into_iter()
        .next()
        .expect("the Unicode phrase matches");
    let matched = hit.match_range();

    assert_eq!(&text[matched], "ÉLAN—VITAL");
    assert_eq!(hit.judge_source(), "Use ÉLAN—VITAL now!");
}

#[test]
fn judge_source_keeps_only_the_adjacent_sentences_spanned_by_one_match() {
    let bag = Bag::new(vec![fallback_taste()]).expect("bag");
    let text = "Unrelated head. Fall.\nBack now! Unrelated tail.";
    let hit = matching_hits(&bag, &prose(text))
        .into_iter()
        .next()
        .expect("the phrase spans punctuation and a line boundary");

    assert_eq!(&text[hit.match_range()], "Fall.\nBack");
    assert_eq!(hit.judge_source(), "Fall.\nBack now!");
    assert_eq!(
        reminder_identity(hit.taste(), hit.context()),
        reminder_identity(hit.taste(), text),
        "judge extraction does not narrow whole-message identity",
    );
}

#[test]
fn a_phrase_span_wider_than_bounded_evidence_is_not_a_match() {
    let taste = Taste::new(
        "literal".parse().expect("taste name"),
        ["élan vital"],
        "Keep the relevant evidence.",
    )
    .expect("taste fixture");
    let bag = Bag::new(vec![taste]).expect("bag");
    let text = format!("élan{}vital", " ".repeat(MAX_JUDGE_SOURCE_BYTES));

    assert!(matching_hits(&bag, &prose(&text)).is_empty());
}

#[test]
fn oversized_sentence_evidence_is_utf8_safe_match_centered_and_marked() {
    let bag = Bag::new(vec![fallback_taste()]).expect("bag");
    let trailing = format!("Polling {}.", "é".repeat(800));
    let leading = format!("{} polling.", "é".repeat(800));

    let trailing_hit = matching_hits(&bag, &prose(&trailing))
        .into_iter()
        .next()
        .expect("leading trigger matches");
    let leading_hit = matching_hits(&bag, &prose(&leading))
        .into_iter()
        .next()
        .expect("trailing trigger matches");
    let trailing_source = trailing_hit.judge_source();
    let leading_source = leading_hit.judge_source();

    assert!(trailing_source.starts_with("Polling "));
    assert!(trailing_source.ends_with('…'));
    assert!(leading_source.starts_with('…'));
    assert!(leading_source.ends_with(" polling."));
    assert!(trailing_source.len() <= MAX_JUDGE_SOURCE_BYTES);
    assert!(leading_source.len() <= MAX_JUDGE_SOURCE_BYTES);
    assert_eq!(trailing_source, trailing_hit.judge_source());
    assert_eq!(leading_source, leading_hit.judge_source());
}

#[test]
fn reminder_identity_frames_taste_and_whole_normalized_prose() {
    let fallback: wire::Name = "fallbacks".parse().expect("taste name");
    let polling: wire::Name = "polling".parse().expect("taste name");
    let first = reminder_identity(&fallback, "  Keep\nONE path. ");

    assert_eq!(normalize_prose("  Keep\nONE path. "), "keep one path.");
    assert_eq!(first, reminder_identity(&fallback, "keep one   PATH."));
    assert_ne!(first, reminder_identity(&fallback, "keep one path!"));
    assert_ne!(first, reminder_identity(&polling, "keep one path."));
}

#[test]
fn bag_identity_covers_ordered_names_triggers_and_vendors() {
    let original = Bag::new(vec![fallback_taste()]).expect("bag");
    let same = Bag::new(vec![fallback_taste()]).expect("same bag");
    let changed = Bag::new(vec![
        Taste::new(
            "fallbacks".parse().expect("taste name"),
            ["fall back", "fallback", "polling"],
            "Use a different vendor.",
        )
        .expect("changed taste"),
    ])
    .expect("changed bag");

    assert_eq!(bag_identity(&original), bag_identity(&same));
    assert_ne!(bag_identity(&original), bag_identity(&changed));
    assert_ne!(
        bag_identity(&original),
        reminder_identity(&"fallbacks".parse().expect("taste name"), BEHAVIOR),
        "bag and prose identities occupy distinct domains"
    );
}

#[test]
fn non_alphabetic_and_empty_triggers_are_refused() {
    for invalid in ["fall*", "fall* back", "f*llback", "fall-back", "word2"] {
        assert!(matches!(
            Trigger::parse(invalid),
            Err(TriggerError::NotWord(_))
        ));
    }
    assert_eq!(Trigger::parse("  "), Err(TriggerError::Empty));
}

#[test]
fn matching_is_literal_case_insensitive_and_alphabetic() {
    let taste = Taste::new(
        "literal".parse().expect("taste name"),
        ["Polling", "fall back", "élan vital"],
        "Use literal selectors.",
    )
    .expect("literal taste");
    let bag = Bag::new(vec![taste]).expect("bag");

    let hits = matching_hits(&bag, &prose("POLLING, then fall\nback with ÉLAN—VITAL."));
    assert_eq!(
        hits.iter().map(|hit| hit.trigger()).collect::<Vec<_>>(),
        ["Polling", "fall back", "élan vital"]
    );

    for adjacent in [
        "poll",
        "polls",
        "polled",
        "pollster",
        "repolling",
        "pollings",
        "polling2",
        "2polling",
    ] {
        assert!(
            matching_hits(&bag, &prose(adjacent)).is_empty(),
            "unlisted variation {adjacent:?} must remain inert"
        );
    }
    assert!(
        matching_hits(&bag, &prose("fall 2 back")).is_empty(),
        "a digit-bearing run must remain a phrase boundary"
    );
}

#[test]
fn only_typed_assistant_prose_is_a_candidate_surface() {
    let bag = Bag::new(vec![fallback_taste()]).expect("bag");
    let event = Event::Usage(Usage {
        turns: 1,
        input_tokens: 3,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: 2,
        reasoning_output_tokens: None,
        locus: Locus::new(1, 0, 10),
    });

    assert!(matching_hits(&bag, &event).is_empty());
}

#[test]
fn a_harmless_mention_still_selects_a_candidate_the_runtime_must_weigh() {
    let bag = Bag::new(vec![fallback_taste()]).expect("bag");

    let hit = matching_hits(&bag, &prose(MENTION))
        .into_iter()
        .next()
        .expect("mention still selects a candidate");

    // The bag is a selector, not a verdict. Runtime policy decides whether the
    // candidate reaches direct steering or the optional resident adjudicator.
    assert_eq!(hit.taste().as_str(), "fallbacks");
    assert_eq!(hit.context(), MENTION);
}

#[test]
fn direct_reminders_and_judgments_have_distinct_producer_contracts() {
    let expected = [FactKind::TasteJudged, FactKind::TasteReminded];
    assert!(
        expected
            .iter()
            .all(|kind| kind.as_str().starts_with("taste::"))
    );
    assert_eq!(
        FactKind::TasteReminded.producer(),
        wire::Producer::ResidentRecognition,
        "the disabled path remains resident recognition"
    );
    assert_eq!(
        FactKind::TasteJudged.producer(),
        wire::Producer::Judge,
        "every durable verdict belongs to the judge producer"
    );
}
