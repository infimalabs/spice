//! Reference-equivalent automatic Speak classification for app messages.
//!
//! Two rules voice one prose hunk, and the browser's narration shares them. A
//! hunk carrying a positively acknowledged steering key is a direct response,
//! so every visible paragraph is voiced with its protocol headers removed. Any
//! other final answer voices its edges: the first and last meaningful
//! paragraphs of the whole hunk, where list items are detail to read on the
//! card, a hunk that opens with a list speaks that list's first item, and a
//! closing list is never voiced. Commentary, NAK-only prose, tool activity,
//! history, and replay stay silent. This module makes that selection once at
//! the resident boundary so the browser never guesses from presentation
//! Markdown.

use super::{EventDirectives, PresentationDirectives, PresentationEvent};

use steer::ack::scan::{Acknowledgement, Miss, Outcome};
use wire::{AutomaticSpeech, AutomaticSpeechKind, Disposition};

pub(super) fn classify(
    prose: &listen::Prose,
    directives: Option<&PresentationDirectives>,
    event: PresentationEvent,
) -> Option<AutomaticSpeech> {
    let final_answer = prose.phase.as_deref() == Some("final_answer");
    let packets: Vec<(Acknowledgement, bool)> = directives.map_or_else(
        || {
            let standing = EmptyStanding;
            steer::ack::scan::scan_acknowledgements(&prose.text, &standing)
                .into_iter()
                .map(|packet| (packet, false))
                .collect()
        },
        |directives| {
            let standing = EventDirectives { directives, event };
            steer::ack::scan::scan_acknowledgements(&prose.text, &standing)
                .into_iter()
                .map(|packet| {
                    let acknowledged = packet.recognitions.iter().any(|recognition| {
                        matches!(
                            &recognition.outcome,
                            Outcome::Retired {
                                key,
                                disposition: Disposition::Acked,
                                ..
                            } if standing.recognizes_authored(
                                key,
                                Disposition::Acked,
                                recognition.locus,
                            )
                        )
                    });
                    (packet, acknowledged)
                })
                .collect()
        },
    );

    let acknowledged = packets.iter().any(|(_, eligible)| *eligible);
    if !acknowledged && !final_answer {
        return None;
    }
    let body = without_protocol_headers(&prose.text, &packets);
    if acknowledged {
        return prepared_all(&body).map(|text| AutomaticSpeech {
            kind: AutomaticSpeechKind::Acknowledgement,
            text,
        });
    }
    prepared_edges(&body).map(|text| AutomaticSpeech {
        kind: AutomaticSpeechKind::Final,
        text,
    })
}

struct EmptyStanding;

impl steer::Standing for EmptyStanding {
    fn disposition(&self, _key: &steer::Key) -> Option<Disposition> {
        None
    }
}

fn without_protocol_headers(text: &str, packets: &[(Acknowledgement, bool)]) -> String {
    let mut rendered = String::new();
    let mut cursor = 0;
    for (packet, _) in packets
        .iter()
        .filter(|(packet, _)| canonical_header(packet))
    {
        if let Some(source) = text.get(cursor..packet.start) {
            rendered.push_str(source);
        }
        cursor = packet.end;
        while text
            .as_bytes()
            .get(cursor)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b':' | b'.' | b'-' | b','))
        {
            cursor += 1;
        }
    }
    if let Some(source) = text.get(cursor..) {
        rendered.push_str(source);
    }
    rendered
}

fn canonical_header(packet: &Acknowledgement) -> bool {
    packet
        .recognitions
        .iter()
        .any(|recognition| match recognition.outcome {
            Outcome::Retired { .. } => true,
            Outcome::Missed {
                miss: Miss::Unknown | Miss::Historical,
                ..
            } => true,
            Outcome::Missed { .. } => false,
        })
}

/// The first and last meaningful paragraphs of a hunk.
///
/// The first says what the message is about and the last where it landed;
/// everything between is detail the operator can read on the card. List items
/// are detail too, except that a hunk opening with a list speaks that list's
/// first item; a closing list is never voiced, and a hunk with one meaningful
/// paragraph speaks it once.
fn prepared_edges(raw: &str) -> Option<String> {
    let segments = speech_segments(raw);
    let first = segments.first()?;
    let last = segments.iter().skip(1).rev().find(|segment| !segment.list);
    let selected = match last {
        Some(last) => format!("{} {}", first.text, last.text),
        None => first.text.clone(),
    };
    prepared_all(&selected)
}

fn prepared_all(raw: &str) -> Option<String> {
    let joined = speech_segments(raw)
        .into_iter()
        .map(|segment| segment.text)
        .collect::<Vec<_>>()
        .join(" ");
    let markdown = replace_markdown(&joined);
    let dates = speak_utc_datetimes(&markdown);
    let hashes = speak_hashes(&dates);
    let collapsed = hashes.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}

/// One visible paragraph, or one list item, in reading order.
struct Segment {
    list: bool,
    text: String,
}

/// Visible paragraphs in reading order, each list item its own segment.
/// Fenced code is shown to be read, never heard: a fence ends the paragraph
/// around it and its contents are skipped. A quoted line is another speaker's
/// words and is skipped the same way.
fn speech_segments(raw: &str) -> Vec<Segment> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut segments = Vec::new();
    let mut lines = Vec::new();
    let mut fenced = false;
    for line in normalized.split('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            push_segment(&mut segments, &mut lines, false);
        } else if fenced {
            continue;
        } else if trimmed.is_empty() || trimmed.starts_with('>') {
            push_segment(&mut segments, &mut lines, false);
        } else if AutomaticSpeech::list_item(trimmed).is_some() {
            push_segment(&mut segments, &mut lines, false);
            lines.push(line);
            push_segment(&mut segments, &mut lines, true);
        } else {
            lines.push(line);
        }
    }
    push_segment(&mut segments, &mut lines, false);
    segments
}

fn push_segment(segments: &mut Vec<Segment>, raw_lines: &mut Vec<&str>, list: bool) {
    let lines = raw_lines
        .drain(..)
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed.starts_with("::") && trimmed.ends_with('}')) && !trimmed.starts_with("TASK ")
        })
        .map(AutomaticSpeech::plain_line)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if !lines.is_empty() {
        segments.push(Segment {
            list,
            text: lines.join("\n"),
        });
    }
}

fn replace_markdown(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut rendered = String::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let image = bytes.get(cursor) == Some(&b'!') && bytes.get(cursor + 1) == Some(&b'[');
        let link = bytes.get(cursor) == Some(&b'[');
        if image || link {
            let label_start = cursor + usize::from(image) + 1;
            if let Some(label_end) = raw[label_start..].find(']').map(|at| label_start + at)
                && bytes.get(label_end + 1) == Some(&b'(')
                && let Some(target_end) =
                    raw[label_end + 2..].find(')').map(|at| label_end + 2 + at)
            {
                let label = raw[label_start..label_end].trim();
                if image {
                    if label.is_empty() {
                        rendered.push_str("An image.");
                    } else {
                        rendered.push_str("Image: ");
                        rendered.push_str(label);
                        rendered.push('.');
                    }
                } else {
                    rendered.push_str(label);
                }
                cursor = target_end + 1;
                continue;
            }
        }
        let character = raw[cursor..].chars().next().expect("cursor is in text");
        rendered.push(character);
        cursor += character.len_utf8();
    }
    rendered
}

fn speak_hashes(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut rendered = String::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor].is_ascii_hexdigit() && (cursor == 0 || !is_ascii_word(bytes[cursor - 1])) {
            let end = (cursor..bytes.len())
                .find(|index| !bytes[*index].is_ascii_hexdigit())
                .unwrap_or(bytes.len());
            if end - cursor >= 7 && (end == bytes.len() || !is_ascii_word(bytes[end])) {
                if !hash_has_label(&raw[..cursor]) {
                    rendered.push_str("commit ");
                }
                for (index, byte) in bytes[cursor..cursor + 7].iter().enumerate() {
                    if index > 0 {
                        rendered.push(' ');
                    }
                    rendered.push((*byte as char).to_ascii_lowercase());
                }
                cursor = end;
                continue;
            }
        }
        let character = raw[cursor..].chars().next().expect("cursor is in text");
        rendered.push(character);
        cursor += character.len_utf8();
    }
    rendered
}

fn hash_has_label(prefix: &str) -> bool {
    let context = prefix
        .chars()
        .rev()
        .take(16)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .to_ascii_lowercase();
    let trimmed = context.trim_end();
    ["commit", "sha"].into_iter().any(|label| {
        trimmed.strip_suffix(label).is_some_and(|before| {
            before
                .as_bytes()
                .last()
                .is_none_or(|byte| !is_ascii_word(*byte))
        })
    })
}

fn is_ascii_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn speak_utc_datetimes(raw: &str) -> String {
    let mut rendered = String::new();
    let mut cursor = 0;
    while cursor < raw.len() {
        if let Some((end, spoken)) = utc_datetime_at(raw, cursor) {
            rendered.push_str(&spoken);
            cursor = end;
            continue;
        }
        let character = raw[cursor..].chars().next().expect("cursor is in text");
        rendered.push(character);
        cursor += character.len_utf8();
    }
    rendered
}

fn utc_datetime_at(raw: &str, start: usize) -> Option<(usize, String)> {
    let bytes = raw.as_bytes();
    if start > 0 && is_ascii_word(*bytes.get(start - 1)?) {
        return None;
    }
    if !has_utc_datetime_prefix(bytes, start) {
        return None;
    }
    let (end, second) = utc_datetime_suffix(raw, start + 16)?;
    if bytes.get(end).is_some_and(|byte| is_ascii_word(*byte)) {
        return None;
    }

    let year = raw.get(start..start + 4)?;
    let month = raw.get(start + 5..start + 7)?.parse::<usize>().ok()?;
    let day = raw.get(start + 8..start + 10)?.parse::<u8>().ok()?;
    let hour = raw.get(start + 11..start + 13)?.parse::<u8>().ok()?;
    let minute = raw.get(start + 14..start + 16)?;
    let month = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ]
    .get(month.checked_sub(1)?)?;
    let hour12 = hour % 12;
    let hour12 = if hour12 == 0 { 12 } else { hour12 };
    let seconds = second.map_or_else(String::new, |value| format!(":{value}"));
    let meridiem = if hour < 12 { "AM" } else { "PM" };
    Some((
        end,
        format!("{month} {day}, {year} at {hour12}:{minute}{seconds} {meridiem} UTC"),
    ))
}

fn has_utc_datetime_prefix(bytes: &[u8], start: usize) -> bool {
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
    };
    digits(start..start + 4)
        && bytes.get(start + 4) == Some(&b'-')
        && digits(start + 5..start + 7)
        && bytes.get(start + 7) == Some(&b'-')
        && digits(start + 8..start + 10)
        && matches!(bytes.get(start + 10), Some(b' ' | b'T'))
        && digits(start + 11..start + 13)
        && bytes.get(start + 13) == Some(&b':')
        && digits(start + 14..start + 16)
}

fn utc_datetime_suffix(raw: &str, mut end: usize) -> Option<(usize, Option<&str>)> {
    let bytes = raw.as_bytes();
    let mut second = None;
    if bytes.get(end) == Some(&b':') {
        let candidate = bytes.get(end + 1..end + 3)?;
        if !candidate.iter().all(u8::is_ascii_digit) {
            return None;
        }
        second = raw.get(end + 1..end + 3);
        end += 3;
        if bytes.get(end) == Some(&b'.') {
            let fraction_start = end + 1;
            end = fraction_start;
            while end < bytes.len() && bytes[end].is_ascii_digit() && end - fraction_start < 6 {
                end += 1;
            }
            if end == fraction_start || bytes.get(end).is_some_and(u8::is_ascii_digit) {
                return None;
            }
        }
    }
    if bytes.get(end) == Some(&b'Z') {
        Some((end + 1, second))
    } else if bytes.get(end..end + 6) == Some(b"+00:00") {
        Some((end + 6, second))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use crate::resident::message_surface::{PresentationDirective, PresentationDirectives};

    fn lane() -> wire::CanonicalUuid {
        "018f82d4-6f59-7d06-92e3-bce9131163e5"
            .parse()
            .expect("fixture lane")
    }

    fn published() -> wire::Instant {
        "2030-04-12T18:44:09.001Z"
            .parse()
            .expect("fixture publication instant")
    }

    fn event() -> PresentationEvent<'static> {
        PresentationEvent {
            lane: lane(),
            author: None,
            source: None,
            locus: listen::Locus {
                member: 1,
                offset: 2,
                length: 3,
            },
            ordinal: 0,
            superseded_answers: &[],
        }
    }

    fn prose(text: &str, phase: Option<&str>) -> listen::Prose {
        listen::Prose {
            role: listen::ProseRole::Assistant,
            text: text.to_owned(),
            phase: phase.map(str::to_owned),
            media: Vec::new(),
            external_media: Vec::new(),
            locus: event().locus,
        }
    }

    fn directives(keys: &[&str]) -> PresentationDirectives {
        PresentationDirectives::new(BTreeMap::from_iter(keys.iter().map(|key| {
            let text = format!("direction for {key}");
            let quote_target = wire::QuoteTarget::Steering {
                key: (*key).to_owned(),
                digest: wire::QuoteTarget::text_digest(&text),
            };
            (
                key.parse().expect("fixture steering key"),
                PresentationDirective {
                    actor: wire::Actor::TEST_PRINCIPAL,
                    attribution: wire::QuoteAttribution {
                        author: wire::QuoteAuthor::Directive {
                            actor: wire::Actor::TEST_PRINCIPAL,
                        },
                        recipients: vec![wire::QuoteRecipient {
                            lane: lane(),
                            occupant: None,
                        }],
                        packet: false,
                    },
                    published: published(),
                    fanout: None,
                    lane: lane(),
                    text,
                    authored: true,
                    taste: false,
                    review: None,
                    disposition: Disposition::Live,
                    quote_target,
                    media_quotes: Vec::new(),
                    retirement: None,
                },
            )
        })))
    }

    #[test]
    fn system_acknowledgment_is_not_speech_evidence() {
        let key = "dRyzkGX8b";
        let mut directives = directives(&[key]);
        directives
            .directives
            .get_mut(&key.parse().expect("current system key parses"))
            .expect("fixture directive exists")
            .authored = false;

        assert_eq!(
            classify(
                &prose("ACK dRyzkGX8b: Accepted revision 44.", None),
                Some(&directives),
                event(),
            ),
            None,
        );
    }

    #[test]
    fn final_edges_drop_middle_paragraphs_and_clean_markdown() {
        assert_eq!(
            prepared_edges(concat!(
                "Opened [the change](https://example.test).\n\n",
                "middle detail\n\n",
                "Landed 7A12bc99 and ![proof](proof.png).",
            )),
            Some("Opened the change. Landed commit 7 a 1 2 b c 9 and Image: proof..".to_owned()),
        );
    }

    #[test]
    fn task_directives_and_empty_image_alt_follow_reference_cleanup() {
        assert_eq!(
            prepared_all("TASK title=hidden | topic=app.test\nVisible ![](proof.png)"),
            Some("Visible An image.".to_owned()),
        );
    }

    #[test]
    fn utc_dates_whitespace_boundaries_and_hash_context_follow_reference_cleanup() {
        assert_eq!(
            prepared_edges(concat!(
                "Started 2026-09-02T00:04:05.123456Z.\n \t\n",
                "middle paragraph\n\n",
                "Finished 2026-09-02 13:14+00:00 at sha 7A12bc99; keep x_7a12bc99 literal.",
            )),
            Some(
                concat!(
                    "Started September 2, 2026 at 12:04:05 AM UTC. ",
                    "Finished September 2, 2026 at 1:14 PM UTC at sha 7 a 1 2 b c 9; ",
                    "keep x_7a12bc99 literal.",
                )
                .to_owned()
            ),
        );
    }

    #[test]
    fn final_edges_skip_lists_and_reach_the_closing_paragraph() {
        assert_eq!(
            prepared_edges(concat!(
                "Shipped the fix.\n\n",
                "```\n- fenced, not a list\n```\n\n",
                "+1 and 1.5 hours on 2026-10-05 stay prose.\n",
                "- **Change:** one line.\n",
                "- Checked both themes.\n\n",
                "Closing paragraph after the list.",
            )),
            Some("Shipped the fix. Closing paragraph after the list.".to_owned()),
        );
        // A closing list is never voiced; an opening list speaks its first item.
        assert_eq!(
            prepared_edges("Shipped the fix.\n\n- one\n- two"),
            Some("Shipped the fix.".to_owned()),
        );
        assert_eq!(
            prepared_edges("1. First item.\n2) Second item.\n\nAfterword."),
            Some("First item. Afterword.".to_owned()),
        );
        assert_eq!(
            prepared_edges("1. First item.\n2) Second item."),
            Some("First item.".to_owned()),
        );
    }

    #[test]
    fn fenced_code_and_quoted_lines_are_never_spoken() {
        assert_eq!(
            prepared_all("Applied the patch.\n```sh\nrm -rf target\n```\nDone."),
            Some("Applied the patch. Done.".to_owned()),
        );
        assert_eq!(
            prepared_edges("Opened the change.\n\n~~~\nlast words are code\n~~~"),
            Some("Opened the change.".to_owned()),
        );
        assert_eq!(
            prepared_all("Started.\n```\nunterminated code"),
            Some("Started.".to_owned())
        );
        // A quote is another speaker's words: it ends the paragraph around it,
        // as the browser's narration segments do.
        assert_eq!(
            prepared_all("Replied.\n> the operator's words\nDone."),
            Some("Replied. Done.".to_owned()),
        );
        assert_eq!(
            prepared_edges("Opened the change.\n\n> quoted last words"),
            Some("Opened the change.".to_owned()),
        );
    }

    #[test]
    fn an_unverified_ack_header_leaves_a_final_answer_on_its_edges() {
        let speech = classify(
            &prose(
                "ACK 1kJK6N3Rx: first paragraph\n\nmiddle paragraph\n\nlast paragraph",
                Some("final_answer"),
            ),
            None,
            event(),
        )
        .expect("final answer is eligible");
        assert_eq!(speech.kind, AutomaticSpeechKind::Final);
        assert_eq!(speech.text, "first paragraph last paragraph");
    }

    #[test]
    fn an_acknowledged_hunk_is_voiced_in_full_without_its_headers_final_or_not() {
        let text = concat!(
            "ordinary preamble\n\n",
            "ACK 1kJK6N3Rx: applied [first](https://example.test)\n\n",
            "- a list item in the middle\n\n",
            "NAK 1kJK7Sqyx: unsafe\n\n",
            "ACK 1kJK8Bbcx: applied last",
        );
        let known = directives(&["1kJK6N3Rx", "1kJK7Sqyx", "1kJK8Bbcx"]);
        for phase in [None, Some("final_answer")] {
            let speech = classify(&prose(text, phase), Some(&known), event())
                .expect("an acknowledged hunk is eligible");
            assert_eq!(speech.kind, AutomaticSpeechKind::Acknowledgement);
            assert_eq!(
                speech.text,
                "ordinary preamble applied first a list item in the middle unsafe applied last"
            );
        }
    }

    #[test]
    fn a_final_answer_with_only_a_nak_speaks_its_edges() {
        let speech = classify(
            &prose(
                "NAK 1kJK6N3Rx: declined\n\nmiddle\n\n- item\n\nlast words",
                Some("final_answer"),
            ),
            Some(&directives(&["1kJK6N3Rx"])),
            event(),
        )
        .expect("final answer is eligible");
        assert_eq!(speech.kind, AutomaticSpeechKind::Final);
        assert_eq!(speech.text, "declined last words");
    }

    #[test]
    fn commentary_malformed_ack_and_nak_only_prose_stay_silent() {
        let known = directives(&["1kJK6N3Rx"]);
        for text in [
            "ordinary progress",
            "ACK not-a-key: discussed",
            "NAK 1kJK6N3Rx: unsafe",
        ] {
            assert_eq!(classify(&prose(text, None), Some(&known), event()), None);
        }
    }
}
