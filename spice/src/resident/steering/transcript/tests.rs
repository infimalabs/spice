use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{TranscriptEvidence, TranscriptRecognition};
use crate::resident::RuntimePaths;
use steer::Outcome as ScanOutcome;
use wire::{CanonicalUuid, Disposition};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

const KEY: &str = "1kKRRsDSx";
const SECOND_KEY: &str = "1kKRRsDTx";

const TRAILING_CALL: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"toolCall","id":"call-trailing","name":"bash","arguments":{"command":"git status"}}],"stopReason":"toolUse"}}
"#;
const LEADING_HEADER: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"ACK 1kKRRsDSx: revision accepted."}],"stopReason":"toolUse"}}
"#;
const TOOL_RESULT: &[u8] = br#"{"type":"message_end","message":{"role":"toolResult","toolCallId":"call-earlier","toolName":"bash","content":[{"type":"text","text":"ok"}],"isError":false}}
"#;
const OMP_ACK_UPDATE: &[u8] = br#"{"type":"message_update","assistantMessageEvent":{"type":"text_end","content":"ACK 1kKRRsDSx: revision accepted."},"message":{"role":"assistant","content":[{"type":"text","text":"ACK 1kKRRsDSx: revision accepted."}]}}
"#;
const OMP_PARAPHRASED_END: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Acknowledging the operator direction."},{"type":"toolCall","id":"call-1","name":"shell","arguments":{"command":"echo ACK 1kKRRsDSx"}}],"stopReason":"toolUse"}}
"#;
/// The shape the Anthropic API settles when it returns the prose before a tool
/// call as a paraphrased narration block: no text record exists, and the
/// header survives only in the call's declared intent, which is not evidence.
const OMP_NARRATED_INTENT: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Confirming the revision and checking the listener next.","thinkingSignature":"CAQS/AcKEQgSGAI4AUIJbmFycmF0aW9u"},{"type":"toolCall","id":"call-1","name":"shell","arguments":{"command":"git status","i":"ACK 1kKRRsDSx: revision accepted"},"intent":"ACK 1kKRRsDSx: revision accepted"}],"stopReason":"toolUse"}}
"#;
const TEXT_ACK_INTENT_NAK: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"ACK 1kKRRsDSx: accepted in text."},{"type":"toolCall","id":"call-1","name":"read","intent":"NAK 1kKRRsDSx: declined in intent.","arguments":{}}],"stopReason":"toolUse"}}
"#;
const TEXT_NAK_INTENT_ACK: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"NAK 1kKRRsDSx: declined in text."},{"type":"toolCall","id":"call-1","name":"read","intent":"ACK 1kKRRsDSx: accepted in intent.","arguments":{}}],"stopReason":"toolUse"}}
"#;
const MULTI_ACK_PARTIAL_LATER_NAK: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"ACK 1kKRRsDSx 1kKRRsDTx: both accepted."},{"type":"text","text":"NAK 1kKRRsDSx: declined later."}],"stopReason":"stop"}}
"#;

struct LiveKey;

impl steer::Standing for LiveKey {
    fn disposition(&self, key: &steer::Key) -> Option<Disposition> {
        matches!(key.as_str(), KEY | SECOND_KEY).then_some(Disposition::Live)
    }
}

#[test]
fn a_leading_header_older_than_a_trailing_tool_call_is_still_recognized() {
    let fixture = Fixture::new(&[TOOL_RESULT, LEADING_HEADER, TRAILING_CALL]);

    let recognition = transcript_recognition(&fixture.paths, lane(), &vendor(), KEY, &LiveKey)
        .unwrap_or_else(|outcome| panic!("the leading header is evidence: {outcome:?}"));

    assert!(matches!(
        recognition.outcome,
        ScanOutcome::Retired {
            disposition: Disposition::Acked,
            ..
        }
    ));
    assert_eq!(recognition.locus.member, 1);
    assert_eq!(recognition.locus.offset, TOOL_RESULT.len() as u64);
}

#[test]
fn omp_terminal_text_update_survives_a_header_stripped_settled_message() {
    let fixture = Fixture::new(&[OMP_ACK_UPDATE, OMP_PARAPHRASED_END]);

    let recognition = transcript_recognition(&fixture.paths, lane(), &vendor(), KEY, &LiveKey)
        .unwrap_or_else(|outcome| panic!("the streamed visible header is evidence: {outcome:?}"));

    assert!(matches!(
        recognition.outcome,
        ScanOutcome::Retired {
            disposition: Disposition::Acked,
            ..
        }
    ));
    assert_eq!(recognition.locus.offset, 0);
}

#[test]
fn omp_paraphrase_tool_payload_and_tool_intent_do_not_invent_acknowledgement_evidence() {
    for record in [OMP_PARAPHRASED_END, OMP_NARRATED_INTENT] {
        let fixture = Fixture::new(&[record]);

        assert!(matches!(
            transcript_recognition(&fixture.paths, lane(), &vendor(), KEY, &LiveKey),
            Err(wire::Outcome::Refusal(refusal))
                if refusal.reason() == "steer::transcript-evidence-absent"
        ));
    }
}

#[test]
fn a_text_answer_stands_against_a_later_contrary_tool_intent() {
    for (record, disposition, answer) in [
        (TEXT_ACK_INTENT_NAK, Disposition::Acked, "accepted in text."),
        (
            TEXT_NAK_INTENT_ACK,
            Disposition::Declined,
            "declined in text.",
        ),
    ] {
        let fixture = Fixture::new(&[record]);
        let recognition = transcript_recognition(&fixture.paths, lane(), &vendor(), KEY, &LiveKey)
            .unwrap_or_else(|outcome| panic!("the text answer is evidence: {outcome:?}"));
        assert!(matches!(
            recognition.outcome,
            ScanOutcome::Retired {
                disposition: selected,
                ..
            } if selected == disposition
        ));
        assert_eq!(
            recognition.answer.map(|clause| clause.text).as_deref(),
            Some(answer)
        );
        assert_eq!(recognition.locus.event, 0);
    }
}

#[test]
fn a_later_event_reanswers_only_its_key_from_a_multi_key_header() {
    let fixture = Fixture::new(&[MULTI_ACK_PARTIAL_LATER_NAK]);
    let first = transcript_recognition(&fixture.paths, lane(), &vendor(), KEY, &LiveKey)
        .unwrap_or_else(|outcome| panic!("reanswered key is evidence: {outcome:?}"));
    assert!(matches!(
        first.outcome,
        ScanOutcome::Retired {
            disposition: Disposition::Declined,
            ..
        }
    ));
    assert_eq!(
        first.answer.map(|clause| clause.text).as_deref(),
        Some("declined later.")
    );
    assert_eq!(first.locus.event, 1);

    let second = transcript_recognition(&fixture.paths, lane(), &vendor(), SECOND_KEY, &LiveKey)
        .unwrap_or_else(|outcome| panic!("unrepeated key is evidence: {outcome:?}"));
    assert!(matches!(
        second.outcome,
        ScanOutcome::Retired {
            disposition: Disposition::Acked,
            ..
        }
    ));
    assert_eq!(
        second.answer.map(|clause| clause.text).as_deref(),
        Some("both accepted.")
    );
    assert_eq!(second.locus.event, 0);
}

#[test]
fn the_newest_placed_answer_wins_within_the_recent_window() {
    let declined = String::from_utf8(LEADING_HEADER.to_vec())
        .unwrap()
        .replace("ACK ", "NAK ");
    let fixture = Fixture::new(&[LEADING_HEADER, declined.as_bytes(), TRAILING_CALL]);
    let evidence = TranscriptEvidence::read(&fixture.paths, lane(), &vendor(), KEY)
        .unwrap_or_else(|error| panic!("{error:?}"));
    let recognition = evidence
        .recognize(KEY, &LiveKey)
        .unwrap_or_else(|error| panic!("{error:?}"));
    assert!(matches!(
        recognition.outcome,
        ScanOutcome::Retired {
            disposition: Disposition::Declined,
            ..
        }
    ));
    assert_eq!(recognition.locus.offset, LEADING_HEADER.len() as u64);
}

#[test]
fn an_answer_behind_records_wider_than_one_boundary_chunk_is_still_recognized() {
    let chunk =
        usize::try_from(listen::BOUNDARY_CHUNK_BYTES).expect("boundary chunk fits in memory");
    let mut oversized = vec![b' '; chunk];
    oversized.extend_from_slice(b"{}\n");
    let fixture = Fixture::new(&[
        TOOL_RESULT,
        LEADING_HEADER,
        &oversized,
        &oversized,
        TRAILING_CALL,
    ]);
    let recognition = transcript_recognition(&fixture.paths, lane(), &vendor(), KEY, &LiveKey)
        .unwrap_or_else(|error| panic!("{error:?}"));
    assert!(matches!(
        recognition.outcome,
        ScanOutcome::Retired {
            disposition: Disposition::Acked,
            ..
        }
    ));
    assert_eq!(recognition.locus.offset, TOOL_RESULT.len() as u64);
}

#[test]
fn an_unfinished_record_cannot_supply_evidence() {
    let unfinished = Fixture::new(&[&LEADING_HEADER[..LEADING_HEADER.len() - 1]]);
    assert!(
        matches!(transcript_recognition(&unfinished.paths, lane(), &vendor(), KEY, &LiveKey), Err(wire::Outcome::Refusal(refusal)) if refusal.reason() == "steer::transcript-evidence-absent")
    );
}

struct Fixture {
    root: PathBuf,
    paths: RuntimePaths,
}

impl Fixture {
    fn new(records: &[&[u8]]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "spice-transcript-recognition-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).expect("fixture root");
        let paths = runtime_paths(&root);
        let tape =
            tape::Tape::open(paths.lane_tape(lane()).join("omp")).expect("the lane tape opens");
        for record in records {
            tape.tee(record).expect("the transcript record is captured");
        }
        Self { root, paths }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove transcript fixture");
    }
}

/// Recognize `token` as occupant exit does: read the lane's evidence, then judge it.
fn transcript_recognition(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    vendor: &wire::Name,
    token: &str,
    standing: &impl steer::Standing,
) -> Result<TranscriptRecognition, wire::Outcome<wire::intent::CommandAnswer>> {
    TranscriptEvidence::read(paths, lane, vendor, token)?.recognize(token, standing)
}

fn lane() -> CanonicalUuid {
    "018f82d4-6f59-7d06-92e3-bce913116301"
        .parse()
        .expect("canonical fixture uuid")
}

fn vendor() -> wire::Name {
    "omp".parse().expect("vendor name")
}

fn runtime_paths(root: &Path) -> RuntimePaths {
    RuntimePaths {
        worktree: root.to_path_buf(),
        common: root.join("common"),
        runtime: root.join("runtime"),
        live: root.join("runtime").join("runtime"),
        data: root.join("data"),
        journal: root.join("journal"),
        resident: root.join("resident"),
        serve: root.join("serve"),
        lane_serve: root.join("lane-serve"),
        degraded: root.join("degraded"),
        tape: root.join("lanes"),
        supervisors: root.join("lanes"),
        steering_media: root.join("lanes"),
        releases: root.join("authority/releases"),
        stage: root.join("stage"),
    }
}
