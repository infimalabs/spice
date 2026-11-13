use app::narration::{narration_utterance, prepared_utterance};
use wire::AgentActivityKind;

const CARD: &str = "\
Landed the retune on the seat row.

> restate the model, do not diff it

The pin lives beside the vendor default now.

```rust
fn retune(seat: &mut Seat) {
    seat.model = None;
}
```

Verify is clean for spice and wire.";

#[test]
fn narration_speaks_the_edges_of_a_card_and_nothing_it_only_shows() {
    assert_eq!(
        narration_utterance(AgentActivityKind::Prose, CARD).as_deref(),
        Some("Landed the retune on the seat row. Verify is clean for spice and wire."),
    );
    assert_eq!(
        prepared_utterance(CARD).as_deref(),
        Some(
            "Landed the retune on the seat row. The pin lives beside the vendor default now. \
             Verify is clean for spice and wire."
        ),
        "operator-asked playback keeps every visible segment and still drops quotes and code",
    );
}

#[test]
fn a_card_that_shows_no_words_submits_nothing() {
    // A compaction boundary renders as a rule and a label while carrying the
    // retained summary on the wire, and a tool card renders as its image.
    for activity in [
        AgentActivityKind::Compaction,
        AgentActivityKind::Tool,
        AgentActivityKind::Reasoning,
        AgentActivityKind::Turn,
    ] {
        assert_eq!(
            narration_utterance(activity, "retained context nobody can read"),
            None,
            "{activity:?}",
        );
    }
}

#[test]
fn a_reply_that_is_only_quotation_never_voices_the_operator() {
    let borrowed = "> gate narration to visible cards\n> > and never quoted content\n";
    assert_eq!(
        narration_utterance(AgentActivityKind::Prose, borrowed),
        None
    );
    assert_eq!(prepared_utterance(borrowed), None);
}

#[test]
fn a_quote_between_two_runs_keeps_them_as_separate_edges() {
    let interleaved = "first run\n> borrowed line\nsecond run";
    assert_eq!(
        narration_utterance(AgentActivityKind::Prose, interleaved).as_deref(),
        Some("first run second run"),
    );
}

#[test]
fn narration_edges_skip_lists_outside_a_fence_and_reach_the_closing_paragraph() {
    let listed = "Shipped the fix.\n\n```\n- fenced, not a list\n```\n\nVerified.\n- **Change:** one `line`.\n- Checked.\n\nClosing.";
    assert_eq!(
        narration_utterance(AgentActivityKind::Prose, listed).as_deref(),
        Some("Shipped the fix. Closing."),
    );
    assert_eq!(
        prepared_utterance(listed).as_deref(),
        Some("Shipped the fix. Verified. Change: one line. Checked. Closing."),
        "playback the operator asks for still reads every segment",
    );
    assert_eq!(
        narration_utterance(AgentActivityKind::Prose, "Shipped the fix.\n\n- one\n- two")
            .as_deref(),
        Some("Shipped the fix."),
        "a closing list is never voiced",
    );
    assert_eq!(
        narration_utterance(
            AgentActivityKind::Prose,
            "* First item.\n* Second.\n\nAfterword."
        )
        .as_deref(),
        Some("First item. Afterword."),
        "an opening list speaks its first item",
    );
    assert_eq!(
        narration_utterance(AgentActivityKind::Prose, "* First item.\n* Second.").as_deref(),
        Some("First item."),
    );
}

#[test]
fn a_prepared_utterance_keeps_its_own_selection_under_the_card_contract() {
    assert_eq!(
        prepared_utterance("ACK applied first applied last").as_deref(),
        Some("ACK applied first applied last"),
        "the resident already chose these words; the contract only screens them",
    );
    assert_eq!(
        prepared_utterance("> borrowed directive\n\nacknowledged and applied").as_deref(),
        Some("acknowledged and applied"),
    );
    assert_eq!(prepared_utterance("> borrowed only"), None);
}

#[test]
fn a_link_speaks_its_label_and_an_image_speaks_nothing() {
    assert_eq!(
        narration_utterance(
            AgentActivityKind::Prose,
            "See [the badge rule](https://example.invalid/rule) ![shot](/tmp/after.png) now.",
        )
        .as_deref(),
        Some("See the badge rule now."),
    );
}
