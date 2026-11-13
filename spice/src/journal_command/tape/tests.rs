use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    root: PathBuf,
    tape: ::tape::Tape,
    sessions: bus::TranscriptSessions,
    lane: CanonicalUuid,
    sender: CanonicalUuid,
    source: CanonicalUuid,
}

fn uuid(value: u64) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{value:012x}")
        .parse()
        .unwrap()
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "spice-tape-peer-check-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let lane = uuid(1);
        let sender = uuid(2);
        let tape =
            ::tape::Tape::open(spice::repository::lane_tape(&root, lane).join("omp")).unwrap();
        let source = tape.source_id().unwrap().as_str().parse().unwrap();
        let mut journal = journal::Journal::in_memory().unwrap();
        let mut write = journal::JournalWrite::new(
            wire::Actor::Spice,
            format!("lane:{lane}").parse().unwrap(),
            wire::Instant::new("2030-04-12T18:44:09Z".parse().unwrap()).unwrap(),
            wire::Producer::ResidentRecognition,
        );
        write
            .fact(
                wire::body::LaneSessionDeclared {
                    lane,
                    occupant: sender,
                    worktree: "/worktrees/author".to_owned(),
                    vendor: "omp".parse().unwrap(),
                    launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
                    epoch: uuid(3),
                    stream: "session".to_owned(),
                    source,
                    identity: "author-session".to_owned(),
                    member: 0,
                    offset: 0,
                    length: 1,
                },
                journal::FactCause::Root,
            )
            .unwrap();
        journal.apply(write).unwrap();
        let sessions = journal.read(super::sessions).unwrap();
        Self {
            root,
            tape,
            sessions,
            lane,
            sender,
            source,
        }
    }

    fn message(&self, record: &[u8], text: &str, start: usize) -> Message {
        let locus = self.tape.tee(record).unwrap();
        Message {
            key: "dYRPWShxJ".to_owned(),
            lane: self.lane,
            sender: self.sender,
            text: text.to_owned(),
            locus: wire::body::TranscriptLocus {
                source: Some(self.source),
                member: locus.member,
                offset: locus.offset,
                length: locus.length,
                event: 0,
                line: 1,
                column: 1,
            },
            span: BodySpan::new(start..start + text.len(), text),
        }
    }

    fn prose(&self, event: &str, text: &str, start: usize) -> Message {
        let record = serde_json::json!({
            "type": "message_end",
            "message": {"role": "assistant", "content": [{"type": "text", "text": event}], "stopReason": "stop"},
        });
        self.message(format!("{record}\n").as_bytes(), text, start)
    }

    fn verify(&self, message: &Message) -> Verdict {
        verify_message(&self.root, &self.sessions, message)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn peer_body_comparison_checks_decoded_bytes_digest_and_utf8_boundaries() {
    let fixture = Fixture::new();
    let prefix = "π: ";
    let mut message = fixture.prose("π: café λ tail", "café λ", prefix.len());
    assert_eq!(fixture.verify(&message), Verdict::Agrees);

    let different = fixture.prose("π: café μ tail", "café λ", prefix.len());
    assert_eq!(fixture.verify(&different), Verdict::Differs);

    message.span.digest = QuoteTarget::text_digest("other");
    assert_eq!(fixture.verify(&message), Verdict::Differs);
    message.span.digest = QuoteTarget::text_digest(&message.text);
    message.span.range.start = 1;
    assert_eq!(fixture.verify(&message), Verdict::Differs);
}

#[test]
fn peer_body_comparison_distinguishes_source_generation_extent_and_decode_failures() {
    let fixture = Fixture::new();
    let mut message = fixture.prose("body", "body", 0);
    message.locus.source = Some(uuid(99));
    assert_eq!(
        fixture.verify(&message),
        Verdict::Unavailable {
            reason: Unavailable::AuthorSessionMissing
        }
    );
    message.locus.source = Some(fixture.source);
    let source_file = fixture.tape.root().join("source");
    let original_source = fs::read(&source_file).unwrap();
    fs::write(&source_file, format!("tape-source 1\n{}\n", uuid(99))).unwrap();
    assert_eq!(
        fixture.verify(&message),
        Verdict::Unavailable {
            reason: Unavailable::Replaced
        }
    );
    fs::write(&source_file, original_source).unwrap();

    message.locus.length += 1;
    assert_eq!(
        fixture.verify(&message),
        Verdict::Unavailable {
            reason: Unavailable::Truncated
        }
    );
    let invalid = fixture.message(b"{invalid\n", "body", 0);
    assert_eq!(
        fixture.verify(&invalid),
        Verdict::Unavailable {
            reason: Unavailable::Undecodable
        }
    );
    fs::remove_file(&source_file).unwrap();
    assert_eq!(
        fixture.verify(&invalid),
        Verdict::Unavailable {
            reason: Unavailable::Missing
        }
    );
}

#[test]
fn captured_peer_bodies_compare_the_host_write_without_interpreting_embedded_packets() {
    let fixture = Fixture::new();
    let body = "SYN literal: café λ";
    let record = serde_json::json!({
        "type": "host_uri_request", "id": "check-body",
        "operation": "write", "url": "spice://outbox", "content": body,
    });
    let message = fixture.message(format!("{record}\n").as_bytes(), body, 0);
    assert_eq!(fixture.verify(&message), Verdict::Agrees);
}
