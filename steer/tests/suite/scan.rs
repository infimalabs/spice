//! Reading acknowledgment outcomes out of recorded assistant turns.
//!
//! The fixtures are real-shaped turns kept beside this file and embedded at
//! compile time, so the scan these tests exercise performs no I/O and neither
//! does the test around it. Recognition therefore depends on the prose and the
//! standing it is read against, and on nothing else — the same property that
//! lets the same scan run at the command door and again during replay.

use std::collections::BTreeMap;

use steer::ack::keys::Key;
use steer::ack::scan::{
    Miss, Outcome, Recognition, Standing, answer, decisive, has_acknowledgement, scan,
    scan_acknowledgements,
};
use wire::Disposition;

const ACKNOWLEDGED: &str = include_str!("prose/acknowledged.md");
const REFUSED: &str = include_str!("prose/refused.md");
const MISSED: &str = include_str!("prose/missed.md");
const INDENTED_CODE: &str = include_str!("prose/indented-code.md");
const TILDE_FENCE: &str = include_str!("prose/tilde-fence.md");
const LONG_FENCE: &str = include_str!("prose/long-fence.md");

/// A header naming two live keys retires both, at the column each was written.
///
/// The turn also carries indented prose and a tab separating a marker from its
/// key. Neither changes what a header means, so refusing either would drop a
/// real acknowledgment over whitespace.
#[test]
fn a_leading_clause_retires_every_key_it_names() {
    let board = board();

    assert_eq!(
        report(&scan(ACKNOWLEDGED, &board)),
        vec![
            "1:5 1kJK6N3Rx acked".to_owned(),
            "1:15 1kJK6M0Vx acked".to_owned(),
            "5:7 1kJK7Sqyx acked".to_owned(),
            "7:5 1kJKB307x acked".to_owned(),
        ]
    );
}

/// Standing is loaded exactly when the canonical grammar sees a marker followed
/// by a key-shaped token. Authored prose before that structural header never
/// changes whether the packet speaks.
#[test]
fn acknowledgement_preflight_uses_the_canonical_clause_grammar() {
    for prose in [
        "ACK 1kJK6N3Rx: spaced.",
        "ACK\t1kJK6N3Rx: tabbed.",
        "NAK\t1kJK7Sqyx: declined.",
        "Progress follows: ACK 1kJK6N3Rx. applied.",
        "ACK\n1kJK6N3Rx\n: cross-line.",
        "**ACK** **1kJK6N3Rx**:** emphasized.",
        "ACK bcdfghjkl: lower-case key shape.",
        "As you said, ACK 1kJK6N3Rx: heard.",
        "If that is all, ACK 1kJK6N3Rx: heard.",
        "The form now validates, ACK 1kJK6N3Rx: heard.",
        "Fixed the token refresh, ACK 1kJK6N3Rx: heard.",
        "I do not ACK 1kJK6N3Rx: heard.",
        "If I ACK 1kJK6N3Rx: heard.",
        "The example ACK 1kJK6N3Rx: heard.",
        "I emitted ACK 1kJK6N3Rx: heard.",
        "The parser recognizes ACK 1kJK6N3Rx: as protocol.",
        "This sentence contains ACK 1kJK6N3Rx: as an example.",
    ] {
        assert!(has_acknowledgement(prose), "missed {prose:?}");
    }

    for prose in [
        "> ACK\t1kJK6N3Rx: quoted.",
        "    NAK\t1kJK7Sqyx: code.",
        "`ACK 1kJK6N3Rx: inline code.`",
        "src/ack.rs:12: ACK 1kJK6N3Rx: rendered source context.",
        "- ACK 1kJK6N3Rx: list example.",
        "1. NAK 1kJK7Sqyx: list example.",
        "ACK",
        "ACK the changes.",
        "ACK 1kJK6N3R: short non-key.",
    ] {
        assert!(!has_acknowledgement(prose), "accepted {prose:?}");
    }
}

/// Separator punctuation, decoration, and emphasis share one key grammar.
#[test]
fn current_header_forms_share_one_packet_grammar() {
    for (prose, expected, separator) in [
        ("ACK 1kJK6N3Rx: applied.", "1:5 1kJK6N3Rx acked", Some(':')),
        ("ACK 1kJK6N3Rx. applied.", "1:5 1kJK6N3Rx acked", Some('.')),
        ("ACK 1kJK6N3Rx applied.", "1:5 1kJK6N3Rx acked", None),
        (
            "NAK #1kJK7Sqyx: declined.",
            "1:6 1kJK7Sqyx declined",
            Some(':'),
        ),
        (
            "**ACK 1kJK6N3Rx:** applied.",
            "1:7 1kJK6N3Rx acked",
            Some(':'),
        ),
        (
            "**ACK** **1kJK6N3Rx**:** applied.",
            "1:11 1kJK6N3Rx acked",
            Some(':'),
        ),
    ] {
        let packets = scan_acknowledgements(prose, &board());
        assert_eq!(packets.len(), 1, "missed {prose:?}");
        assert_eq!(
            packets[0].separator, separator,
            "wrong separator for {prose:?}"
        );
        assert_eq!(report(&packets[0].recognitions), [expected]);
        assert!(!prose[packets[0].start..packets[0].end].ends_with(' '));
    }
}

/// Current standing ends a multi-key run even when the next prose token is
/// syntactically key-shaped. That token begins the optional body, whose
/// subsequent lines remain part of the same packet.
#[test]
fn current_standing_delimits_a_colonless_multiline_body() {
    let prose = "ACK 1kJK6N3Rx begins the body\nand keeps its second line.";
    let packets = scan_acknowledgements(prose, &board());

    assert_eq!(packets.len(), 1);
    assert_eq!(report(&packets[0].recognitions), ["1:5 1kJK6N3Rx acked"]);
    assert_eq!(&prose[packets[0].start..packets[0].end], "ACK 1kJK6N3Rx");
    assert_eq!(
        answer(prose, "1kJK6N3Rx", &board()).map(|range| &prose[range]),
        Some("begins the body\nand keeps its second line.")
    );
}

/// Marker, key list, and separator line breaks remain part of one packet. The
/// packet ranges are what presentation uses to attribute each multiline body.
#[test]
fn multiline_headers_preserve_exact_loci_and_body_boundaries() {
    let prose = concat!(
        "Before.\nACK\n1kJK6N3Rx\n1kJK6M0Vx\n:\nApplied together.\n",
        "NAK\n1kJK7Sqyx\n\u{2014}\nUnsafe.",
    );
    let packets = scan_acknowledgements(prose, &board());

    assert_eq!(packets.len(), 2);
    assert_eq!(
        &prose[packets[0].start..packets[0].end],
        "ACK\n1kJK6N3Rx\n1kJK6M0Vx\n:"
    );
    assert_eq!(
        report(&packets[0].recognitions),
        ["3:1 1kJK6N3Rx acked", "4:1 1kJK6M0Vx acked"]
    );
    assert_eq!(
        &prose[packets[0].end..packets[1].start],
        "\nApplied together.\n"
    );
    assert_eq!(
        &prose[packets[1].start..packets[1].end],
        "NAK\n1kJK7Sqyx\n\u{2014}"
    );
    assert_eq!(report(&packets[1].recognitions), ["8:1 1kJK7Sqyx declined"]);
    assert_eq!(&prose[packets[1].end..], "\nUnsafe.");
}

/// Every prose boundary ends the key list for both dispositions. The Unicode
/// dashes are multi-byte separators, while a bare space remains an unconsumed
/// boundary rather than punctuation owned by the packet.
#[test]
fn ack_and_nak_key_lists_stop_at_every_prose_delimiter() {
    for (delimiter, separator) in [
        (":", Some(':')),
        (".", Some('.')),
        (";", Some(';')),
        (",", Some(',')),
        ("!", Some('!')),
        ("?", Some('?')),
        ("\u{2013}", Some('\u{2013}')),
        ("\u{2014}", Some('\u{2014}')),
        (" ", None),
    ] {
        for (marker, key, column, disposition) in [
            ("ACK", "1kJK6N3Rx", 5, "acked"),
            ("NAK", "1kJK7Sqyx", 5, "declined"),
        ] {
            let prose = format!("{marker} {key}{delimiter}ordinary response prose");
            let packets = scan_acknowledgements(&prose, &board());

            assert_eq!(packets.len(), 1, "missed {prose:?}");
            assert_eq!(
                packets[0].separator, separator,
                "wrong separator for {prose:?}"
            );
            assert_eq!(
                report(&packets[0].recognitions),
                [format!("1:{column} {key} {disposition}")]
            );
            assert!(
                packets[0].end <= prose.find("ordinary").expect("fixture has response prose"),
                "response prose was consumed by {prose:?}"
            );
        }
    }
}

/// One assistant message may distribute acknowledgements across separate
/// paragraphs. Each header remains an independent disposition rather than the
/// first header consuming the rest of the message as its body.
#[test]
fn one_distributed_message_extracts_every_acknowledgement_once() {
    let prose = concat!(
        "ACK 1kQwPZtKx: The current completion path is singular and visible.\n\n",
        "ACK 1kQwPZtLx: Historical TaskDone records are compatibility evidence only.",
    );

    assert_eq!(
        report(&scan(prose, &board())),
        vec![
            "1:5 1kQwPZtKx acked".to_owned(),
            "3:5 1kQwPZtLx acked".to_owned(),
        ]
    );
}

/// A copied SYN hash decorates a key without changing a mixed key list; a
/// comma that nothing keyed follows remains the header separator.
#[test]
fn a_comma_between_keys_lists_them_and_a_comma_after_the_last_key_separates() {
    let board = board();

    assert_eq!(
        report(&scan("ACK #1kJK6N3Rx, 1kJK6M0Vx: both handled.", &board)),
        vec![
            "1:6 1kJK6N3Rx acked".to_owned(),
            "1:17 1kJK6M0Vx acked".to_owned(),
        ],
    );
    let packets = scan_acknowledgements("ACK 1kJK6N3Rx, then the rest.", &board);
    assert_eq!(packets.len(), 1);
    assert_eq!(report(&packets[0].recognitions), ["1:5 1kJK6N3Rx acked"]);
    assert_eq!(packets[0].separator, Some(','));
}

/// Historical standing changes disposition, not packet membership: a later
/// key in the same header must still be parsed and judged independently.
#[test]
fn a_mixed_historical_and_live_header_parses_every_key_in_source_order() {
    let standing = Board(
        [
            ("1kR5x8yRx", Disposition::Acked),
            ("1kR5xJDDx", Disposition::Live),
        ]
        .into_iter()
        .map(|(key, disposition)| (key.parse::<Key>().expect("fixture key"), disposition))
        .collect(),
    );

    assert_eq!(
        report(&scan(
            "ACK 1kR5x8yRx 1kR5xJDDx: One response covers both keys.",
            &standing,
        )),
        vec![
            "1:5 1kR5x8yRx historical".to_owned(),
            "1:15 1kR5xJDDx acked".to_owned(),
        ],
    );
}

/// A refusal is a disposition, not an absence of one.
#[test]
fn a_refusal_retires_the_directive_as_declined() {
    let board = board();

    assert_eq!(
        report(&scan(REFUSED, &board)),
        vec!["1:5 1kJK7Sqyx declined".to_owned()]
    );
}

/// Unknown, quoted, and historical keys alter nothing and are reported by
/// name. A malformed non-key never establishes a packet.
#[test]
fn every_way_a_clause_fails_to_land_is_reported_by_name() {
    let board = board();

    assert_eq!(
        report(&scan(MISSED, &board)),
        vec![
            "5:5 1kJKZZZZx unknown".to_owned(),
            "7:5 1kJK8KcWx historical".to_owned(),
            "9:7 1kJK6N3Rx quoted".to_owned(),
            "14:5 1kJK6N3Rx quoted".to_owned(),
            "17:6 1kJK6N3Rx quoted".to_owned(),
            "20:5 1kJK6N3Rx quoted".to_owned(),
        ]
    );
}

/// Nothing in the missed turn touches the live keys it quotes.
#[test]
fn a_turn_that_only_quotes_keys_retires_none_of_them() {
    let board = board();

    for recognition in scan(MISSED, &board) {
        assert!(
            matches!(recognition.outcome, Outcome::Missed { .. }),
            "a quoted or misplaced clause retired something at {}",
            recognition.locus
        );
    }
}

#[test]
fn a_four_space_indented_code_clause_is_quoted() {
    assert_eq!(
        report(&scan(INDENTED_CODE, &board())),
        vec!["1:9 1kJK6N3Rx quoted".to_owned()],
    );
}

#[test]
fn a_tilde_fenced_clause_is_quoted() {
    assert_eq!(
        report(&scan(TILDE_FENCE, &board())),
        vec!["2:5 1kJK6M0Vx quoted".to_owned()],
    );
}

#[test]
fn a_shorter_inner_backtick_run_does_not_close_a_long_fence() {
    assert_eq!(
        report(&scan(LONG_FENCE, &board())),
        vec!["3:5 1kJK7Sqyx quoted".to_owned()],
    );
}

/// Prose that uses the word without naming a key is not an attempt at all, so
/// ordinary sentences do not each produce a diagnostic.
#[test]
fn prose_about_acknowledging_is_silent() {
    let board = board();

    for quiet in [
        "I will ACK the following changes: the first and the second.",
        "The ACK protocol: a marker, a key, a colon.",
        "ACK",
        "ACK: nothing named",
        "acking is not ACKing 1kJK6N3Rx without a colon",
        "I will ACK something after checking the result.",
    ] {
        assert!(
            scan(quiet, &board).is_empty(),
            "{quiet:?} was read as an acknowledgment attempt"
        );
    }
}

/// A key run ends when response prose begins, regardless of separator choice.
#[test]
fn a_leading_clause_stops_before_non_key_response_prose() {
    let board = board();

    assert_eq!(
        report(&scan("ACK 1kJK6N3Rx nope: partly named.", &board)),
        vec!["1:5 1kJK6N3Rx acked".to_owned()]
    );
}

/// Every structurally valid marker starts another authentic packet, even when
/// ordinary response prose precedes it on the same line.
#[test]
fn an_unseparated_second_clause_on_a_line_also_speaks() {
    let board = board();

    assert_eq!(
        report(&scan(
            "ACK 1kJK6N3Rx: and then ACK 1kJK6M0Vx: again.",
            &board
        )),
        vec![
            "1:5 1kJK6N3Rx acked".to_owned(),
            "1:29 1kJK6M0Vx acked".to_owned(),
        ]
    );
}

/// Sentence punctuation starts another authentic packet on the same line.
#[test]
fn sentence_delimited_packets_on_one_line_each_speak() {
    let board = board();

    assert_eq!(
        report(&scan(
            "ACK 1kJK6N3Rx: first handled. ACK 1kJK6M0Vx: second handled.",
            &board,
        )),
        vec![
            "1:5 1kJK6N3Rx acked".to_owned(),
            "1:35 1kJK6M0Vx acked".to_owned(),
        ]
    );
}

/// The header an operator reads decides the key, however the prose goes on
/// to talk about it.
///
/// A lane that declines and then explains itself by quoting the header it
/// wrote earlier has still declined. The scan reports quoted mentions by name,
/// so diagnostics retain context while the authored header remains decisive.
#[test]
fn a_placed_header_decides_its_key_over_a_later_quoted_mention() {
    let board = board();
    let prose = concat!(
        "NAK 1kJK7Sqyx: Verified on the tape; the direction names another lane.\n\n",
        "I emitted `ACK 1kJK7Sqyx: Carrying it forward now` as prose before checking.\n",
        "`ACK 1kJK7Sqyx: Carrying it forward now` is the header the operator saw.",
    );

    assert_eq!(
        report(&scan(prose, &board)),
        vec![
            "1:5 1kJK7Sqyx declined".to_owned(),
            "3:16 1kJK7Sqyx quoted".to_owned(),
            "4:6 1kJK7Sqyx quoted".to_owned(),
        ]
    );
    assert_eq!(
        decided(prose, "1kJK7Sqyx", &board),
        Some("1:5 1kJK7Sqyx declined".to_owned())
    );
}

/// Authored clauses outrank quoted mentions, and among authored clauses the
/// later one speaks.
#[test]
fn the_decisive_recognition_is_the_last_of_the_highest_rank() {
    let board = board();

    for (prose, expected) in [
        (
            "Earlier I wrote ACK 1kJK6N3Rx: no.\nACK 1kJK6N3Rx: yes.",
            Some("2:5 1kJK6N3Rx acked"),
        ),
        (
            "ACK 1kJK6N3Rx: first thought.\nNAK 1kJK6N3Rx: on reflection, no.",
            Some("2:5 1kJK6N3Rx declined"),
        ),
        (
            "> ACK 1kJK6N3Rx: quoted\nI said ACK 1kJK6N3Rx: authored",
            Some("2:12 1kJK6N3Rx acked"),
        ),
        ("ACK 1kJK6M0Vx: another key entirely.", None),
    ] {
        assert_eq!(
            decided(prose, "1kJK6N3Rx", &board),
            expected.map(str::to_owned),
            "{prose:?}"
        );
    }
    assert_eq!(
        decided(
            "ACK 1kJK8KcWx: already retired.\n`ACK 1kJK8KcWx: shown`",
            "1kJK8KcWx",
            &board
        ),
        Some("1:5 1kJK8KcWx historical".to_owned()),
        "a miss judged on the key itself is still placed"
    );
}

/// Renders the decisive recognition for one token the way [`report`] does.
fn decided(prose: &str, token: &str, standing: &impl Standing) -> Option<String> {
    decisive(prose, token, standing).map(|recognition| report(&[recognition]).remove(0))
}

/// Renders each recognition as `locus token outcome`, which is exactly the
/// diagnostic an operator needs to see why a key did or did not land.
fn report(recognitions: &[Recognition]) -> Vec<String> {
    recognitions
        .iter()
        .map(|recognition| match &recognition.outcome {
            Outcome::Retired {
                key, disposition, ..
            } => {
                let word = steer::ack::keys::DISPOSITIONS
                    .iter()
                    .find(|(_, admitted)| admitted == disposition)
                    .map_or("live", |(word, _)| word);
                format!("{} {key} {word}", recognition.locus)
            }
            Outcome::Missed { token, miss } => {
                format!("{} {token} {}", recognition.locus, miss.as_str())
            }
        })
        .collect()
}

/// The standing the fixtures are read against.
struct Board(BTreeMap<Key, Disposition>);

impl Standing for Board {
    fn disposition(&self, key: &Key) -> Option<Disposition> {
        self.0.get(key).copied()
    }
}

fn board() -> Board {
    Board(
        [
            ("1kJK6N3Rx", Disposition::Live),
            ("1kJK6M0Vx", Disposition::Live),
            ("1kJK7Sqyx", Disposition::Live),
            ("1kJKB307x", Disposition::Live),
            ("1kJK8KcWx", Disposition::Acked),
            ("1kQwPZtKx", Disposition::Live),
            ("1kQwPZtLx", Disposition::Live),
        ]
        .into_iter()
        .map(|(key, disposition)| (key.parse::<Key>().expect("fixture key"), disposition))
        .collect(),
    )
}

/// The scanner reads the same outcomes out of prose whichever ledger reduced
/// the facts, so the ledger itself answers as standing.
#[test]
fn the_ledger_answers_as_standing() {
    use steer::directives::{Ledger, Steering};
    use wire::body::SteerSent;

    let mut ledger = Ledger::new();
    ledger
        .absorb(Steering::Sent(&SteerSent {
            sender: wire::Actor::TEST_PRINCIPAL,
            room: None,
            directive: "00000000-0000-4000-8000-000000000001"
                .parse()
                .expect("fixture directive UUID"),
            fanout: None,
            recipients: Vec::new(),
            key: "1kJK6N3Rx".to_owned(),
            recipient: wire::Actor::Lane(
                "018f82d4-6f59-7d06-92e3-bce913116302"
                    .parse()
                    .expect("lane"),
            ),
            text: "hold the line".to_owned(),
            control: None,
            attachments: Vec::new(),
            expires: "2026-08-01T07:07:19.000Z".parse().expect("instant"),
        }))
        .expect("sent");

    assert_eq!(
        report(&scan("ACK 1kJK6N3Rx: heard.", &ledger)),
        vec!["1:5 1kJK6N3Rx acked".to_owned()]
    );
    assert_eq!(
        report(&scan("ACK 1kJK6M0Vx: never sent.", &ledger)),
        vec!["1:5 1kJK6M0Vx unknown".to_owned()]
    );
}

/// Miss words are the ones the diagnostics are written in.
#[test]
fn every_miss_is_reported_under_one_word() {
    for (miss, word) in [
        (Miss::Quoted, "quoted"),
        (Miss::Unknown, "unknown"),
        (Miss::Historical, "historical"),
    ] {
        assert_eq!(miss.as_str(), word);
        assert_eq!(miss.to_string(), word);
    }
}
