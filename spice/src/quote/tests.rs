use super::*;
use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
struct MemoryStore {
    tape: BTreeMap<String, SourceQuote>,
    steering: BTreeMap<String, SourceQuote>,
    room: BTreeMap<String, SourceQuote>,
    unreadable: bool,
}

impl QuoteStore for MemoryStore {
    fn tape(
        &self,
        lane: CanonicalUuid,
        source: CanonicalUuid,
        locus: wire::MessageLocus,
    ) -> Result<SourceQuote, SourceError> {
        if self.unreadable {
            return Err(SourceError::Unreadable);
        }
        self.tape
            .get(&format!(
                "{lane}/{source}/{}/{}/{}/{}",
                locus.member, locus.offset, locus.length, locus.event
            ))
            .cloned()
            .ok_or(SourceError::Unknown)
    }

    fn steering(&self, key: &str) -> Result<SourceQuote, SourceError> {
        if self.unreadable {
            return Err(SourceError::Unreadable);
        }
        self.steering.get(key).cloned().ok_or(SourceError::Unknown)
    }

    fn room(
        &self,
        room: &wire::TaskStamp,
        publication: &wire::rooms::RoomAuthorityStamp,
    ) -> Result<SourceQuote, SourceError> {
        if self.unreadable {
            return Err(SourceError::Unreadable);
        }
        self.room
            .get(&format!("{room}/{publication}"))
            .cloned()
            .ok_or(SourceError::Unknown)
    }
}

fn uuid(last: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02x}")
        .parse()
        .expect("canonical UUID")
}

const PREDECESSOR_TEXT: &str = "answer from predecessor";
const SUCCESSOR_TEXT: &str = "answer from successor";
const FAILURE_TEXT: &str = "provider request failed";
const FAILURE_RECORD: &str = r#"{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"error","sessionSettled":true,"error":{"message":"provider request failed","provider":"anthropic","model":"claude-opus-5-5","httpStatus":400,"retryable":false}}"#;
const INTENT_TEXT: &str = "ACK 1kQwPZtKx: intent-backed reply";
const INTENT_RECORD: &str = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"toolCall","id":"call-1","name":"read","intent":"ACK 1kQwPZtKx: intent-backed reply","arguments":{}}],"stopReason":"toolUse"}}"#;

struct DurableQuoteFixture {
    root: PathBuf,
    tape_root: PathBuf,
    tape: tape::Tape,
    bus: bus::Bus,
    lane: CanonicalUuid,
    source: CanonicalUuid,
    predecessor_locus: tape::Locus,
    successor_locus: tape::Locus,
    failure_locus: tape::Locus,
    intent_locus: tape::Locus,
    end_offset: u64,
}

impl DurableQuoteFixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "spice-quote-source-session-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let lane = uuid(1);
        let vendor: wire::Name = "omp".parse().expect("fixture vendor");
        let tape_root = root.join("tape");
        let tape_path = crate::repository::lane_tape(&tape_root, lane).join(vendor.as_str());
        fs::create_dir_all(&tape_path).expect("fixture tape directory exists");
        let tape = tape::Tape::open(&tape_path).expect("fixture tape opens");
        for text in [PREDECESSOR_TEXT, SUCCESSOR_TEXT] {
            tape.tee(
                    format!(
                        "{{\"type\":\"message_end\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}],\"stopReason\":\"stop\"}}}}\n"
                    )
                    .as_bytes(),
                )
                .expect("fixture prose is durable");
        }
        tape.tee(format!("{FAILURE_RECORD}\n").as_bytes())
            .expect("fixture provider failure is durable");
        tape.tee(format!("{INTENT_RECORD}\n").as_bytes())
            .expect("fixture tool intent is durable");
        let scan =
            listen::scan(&tape_path, listen::Dialect::Omp, None).expect("fixture tape scans");
        let predecessor_locus = scan.records[0].locus;
        let successor_locus = scan.records[1].locus;
        let failure_locus = scan.records[2].locus;
        let intent_locus = scan.records[3].locus;
        let source = scan
            .checkpoint
            .source
            .as_str()
            .parse::<CanonicalUuid>()
            .expect("fixture tape source is canonical");
        let bus = session_bus(
            lane,
            source,
            vendor,
            predecessor_locus,
            successor_locus,
            scan.checkpoint.next,
        );
        Self {
            root,
            tape_root,
            tape,
            bus,
            lane,
            source,
            predecessor_locus,
            successor_locus,
            failure_locus,
            intent_locus,
            end_offset: scan.checkpoint.next.offset,
        }
    }

    fn target(&self, source: CanonicalUuid, locus: tape::Locus, text: &str) -> QuoteTarget {
        QuoteTarget::Tape {
            lane: self.lane,
            source,
            locus: wire::MessageLocus {
                member: locus.member,
                offset: locus.offset,
                length: locus.length,
                event: 0,
            },
            digest: QuoteTarget::text_digest(text),
        }
    }

    fn finish(self) {
        self.bus.shutdown().expect("fixture bus stops");
        drop(self.tape);
        fs::remove_dir_all(self.root).expect("fixture cleanup succeeds");
    }
}

fn session_bus(
    lane: CanonicalUuid,
    source: CanonicalUuid,
    vendor: wire::Name,
    predecessor: tape::Locus,
    successor: tape::Locus,
    end: listen::Position,
) -> bus::Bus {
    let bus = bus::Bus::start(journal::Journal::in_memory().expect("fixture journal opens"))
        .expect("fixture bus starts");
    let mut write = journal::JournalWrite::new(
        wire::Actor::Spice,
        format!("lane:{lane}").parse().expect("fixture origin"),
        wire::Instant::new(jiff::Timestamp::now()).expect("fixture instant"),
        wire::Producer::ResidentRecognition,
    );
    for (occupant, worktree, identity, locus) in [
        (uuid(3), "/worktrees/predecessor", "session-a", predecessor),
        (uuid(4), "/worktrees/successor", "session-b", successor),
    ] {
        write
            .fact(
                wire::body::LaneSessionDeclared {
                    lane,
                    occupant,
                    worktree: worktree.to_owned(),
                    vendor: vendor.clone(),
                    launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
                    epoch: uuid(5),
                    stream: "session".to_owned(),
                    source,
                    identity: identity.to_owned(),
                    member: locus.member,
                    offset: locus.offset,
                    length: locus.length,
                },
                journal::FactCause::Root,
            )
            .expect("fixture session declaration builds");
    }
    let next = wire::body::TranscriptPosition {
        member: end.member,
        offset: end.offset,
    };
    let ingested = write
        .fact(
            wire::body::JournalIngested {
                lane,
                vendor: vendor.clone(),
                stream: "session".to_owned(),
                source,
                supersedes: None,
                from: wire::body::TranscriptPosition {
                    member: predecessor.member,
                    offset: predecessor.offset,
                },
                to: next,
                usage: Vec::new(),
                non_usage_records: 4,
                residue_records: 0,
            },
            journal::FactCause::Root,
        )
        .expect("fixture ingestion builds");
    write.transcript_source_head(ingested, move |_| wire::TranscriptSourceHead {
        lane,
        vendor,
        stream: "session".to_owned(),
        source,
        next,
    });
    assert!(matches!(
        bus.commit(write).expect("fixture facts commit"),
        bus::CommitOutcome::Committed(_)
    ));
    bus
}

#[test]
fn durable_source_loci_resolve_each_session_author_after_lane_renewal() {
    let fixture = DurableQuoteFixture::new();
    let reader = fixture.bus.reader();
    let resolver = Resolver::from_tape(&fixture.tape_root, &reader);
    for (locus, text, occupant) in [
        (fixture.predecessor_locus, PREDECESSOR_TEXT, uuid(3)),
        (fixture.successor_locus, SUCCESSOR_TEXT, uuid(4)),
        (fixture.failure_locus, FAILURE_TEXT, uuid(4)),
    ] {
        assert_eq!(
            resolver.resolve(&fixture.target(fixture.source, locus, text).to_string(), 0),
            QuoteResolution::Resolved(ResolvedQuote {
                text: text.to_owned(),
                author: QuoteAuthor::Agent {
                    lane: fixture.lane,
                    occupant,
                },
                disposition: None,
            })
        );
    }
    for unavailable in [
        fixture.target(uuid(9), fixture.predecessor_locus, PREDECESSOR_TEXT),
        fixture.target(fixture.source, fixture.intent_locus, INTENT_TEXT),
        fixture.target(
            fixture.source,
            tape::Locus::new(fixture.successor_locus.member, fixture.end_offset + 1, 1),
            SUCCESSOR_TEXT,
        ),
    ] {
        assert!(matches!(
            resolver.resolve(&unavailable.to_string(), 0),
            QuoteResolution::Unavailable {
                reason: QuoteUnavailable::Unknown,
                ..
            }
        ));
    }
    drop(reader);
    fixture.finish();
}

fn tape_target(text: &str) -> QuoteTarget {
    QuoteTarget::Tape {
        lane: uuid(1),
        source: uuid(2),
        locus: wire::MessageLocus {
            member: 3,
            offset: 5,
            length: 8,
            event: 13,
        },
        digest: QuoteTarget::text_digest(text),
    }
}

fn steering_target(key: &str, text: &str) -> QuoteTarget {
    QuoteTarget::Steering {
        key: key.to_owned(),
        digest: QuoteTarget::text_digest(text),
    }
}

#[test]
fn message_parse_render_preserves_text_without_a_quote_uri_byte_for_byte() {
    let source = "  prose\\r\\n[ordinary](https://example.test/a%20b) and `spice://other/v1`  ";
    let parsed = QuoteMessage::parse(source);

    assert!(parsed.quotes().is_empty());
    assert_eq!(parsed.render().as_bytes(), source.as_bytes());
}

#[test]
fn message_parser_names_only_structural_fixed_label_quote_links() {
    let target = steering_target("dTVLR0qL0", "operator text");
    let link = target.markdown();
    let source = format!("before {link} after `[quote]({target})`");
    let parsed = QuoteMessage::parse(&source);

    assert_eq!(
        parsed.quotes(),
        &[MessageQuote {
            range: 7..7 + link.len(),
            target: target.to_string(),
        }]
    );
    assert_eq!(parsed.render(), source);
}

#[test]
fn every_authority_kind_resolves_with_recorded_authorship() {
    let mut store = MemoryStore::default();
    let tape = tape_target("agent text");
    store.tape.insert(
        format!("{}/{}/3/5/8/13", uuid(1), uuid(2)),
        SourceQuote {
            identity: TextIdentity::Presented,
            text: "agent text".to_owned(),
            attribution: authored_by(QuoteAuthor::Agent {
                lane: uuid(1),
                occupant: uuid(3),
            }),
            disposition: None,
            media: Vec::new(),
            body: None,
        },
    );
    let steering = steering_target("dTVLR0qL0", "operator text");
    store.steering.insert(
        "dTVLR0qL0".to_owned(),
        SourceQuote {
            identity: TextIdentity::Presented,
            text: "operator text".to_owned(),
            attribution: authored_by(QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL,
            }),
            disposition: Some(wire::Disposition::Acked),
            media: Vec::new(),
            body: None,
        },
    );

    assert_eq!(
        resolve_with(&store, &tape.to_string(), 0),
        QuoteResolution::Resolved(ResolvedQuote {
            text: "agent text".to_owned(),
            author: QuoteAuthor::Agent {
                lane: uuid(1),
                occupant: uuid(3),
            },
            disposition: None,
        })
    );
    assert_eq!(
        resolve_with(&store, &steering.to_string(), 0),
        QuoteResolution::Resolved(ResolvedQuote {
            text: "operator text".to_owned(),
            author: QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL,
            },
            disposition: Some(wire::Disposition::Acked),
        })
    );

    let room: wire::TaskStamp = "dVrD59Y8N".parse().expect("fixture room");
    let publication: wire::rooms::RoomAuthorityStamp =
        "dVsYdDj7d".parse().expect("fixture publication");
    store.room.insert(
        format!("{room}/{publication}"),
        SourceQuote {
            identity: TextIdentity::Presented,
            text: "room post".to_owned(),
            attribution: authored_by(QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL,
            }),
            disposition: None,
            media: Vec::new(),
            body: None,
        },
    );
    let target = |text| QuoteTarget::Room {
        room: room.clone(),
        publication: publication.clone(),
        digest: QuoteTarget::text_digest(text),
    };
    assert_eq!(
        resolve_with(&store, &target("room post").to_string(), 0),
        QuoteResolution::Resolved(ResolvedQuote {
            text: "room post".to_owned(),
            author: QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL,
            },
            disposition: None,
        })
    );
    let changed = target("other words").to_string();
    assert_eq!(
        resolve_with(&store, &changed, 0),
        unavailable(&changed, QuoteUnavailable::DigestMismatch)
    );
}

#[test]
fn agent_quote_presentation_carries_authenticated_directive_context() {
    let mut store = MemoryStore::default();
    let authored = "ACK dTVLR0qL0: completed";
    let operator = "inspect the quote chain";
    let directive = steering_target("dTVLR0qL0", operator);
    let presented = format!("{}\n\ncompleted", directive.markdown());
    let tape = tape_target(authored);
    store.tape.insert(
        format!("{}/{}/3/5/8/13", uuid(1), uuid(2)),
        SourceQuote {
            identity: TextIdentity::Authored(authored.to_owned()),
            text: presented.clone(),
            attribution: authored_by(QuoteAuthor::Agent {
                lane: uuid(1),
                occupant: uuid(3),
            }),
            disposition: None,
            media: Vec::new(),
            body: None,
        },
    );
    store.steering.insert(
        "dTVLR0qL0".to_owned(),
        SourceQuote {
            identity: TextIdentity::Presented,
            text: operator.to_owned(),
            attribution: authored_by(QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL,
            }),
            disposition: Some(wire::Disposition::Acked),
            media: Vec::new(),
            body: None,
        },
    );

    let source = tape.markdown();
    assert_eq!(
        expand_with(&store, &source, 0),
        format!("{operator}\n\ncompleted")
    );
    let presentation = present_with(&store, &source, 0);
    let [QuotePresentation::Resolved { text, quotes, .. }] = presentation.as_slice() else {
        panic!("agent quote resolves");
    };
    assert_eq!(text, &presented);
    assert!(matches!(
        quotes.as_slice(),
        [QuotePresentation::Resolved {
            attribution: wire::QuoteAttribution {
                author: QuoteAuthor::Directive {
                    actor: wire::Actor::TEST_PRINCIPAL,
                },
                ..
            },
            disposition: Some(wire::Disposition::Acked),
            ..
        }]
    ));
}

#[test]
fn nested_quote_presentation_inlines_each_sources_own_media() {
    let image =
        wire::Media::from_base64("image.png", "image/png", "iVBORw0KGgo=").expect("fixture media");
    let inner_text = format!("look\n\n{}", image.reference_markdown().expect("reference"));
    let inner = steering_target("dTVLR0qL0", &inner_text);
    let outer_text = format!("see {}", inner.markdown());
    let outer = steering_target("dTVLR0qL1", &outer_text);
    let mut store = MemoryStore::default();
    for (key, text, media) in [
        ("dTVLR0qL0", &inner_text, vec![image.clone()]),
        ("dTVLR0qL1", &outer_text, Vec::new()),
    ] {
        store.steering.insert(
            key.to_owned(),
            SourceQuote {
                identity: TextIdentity::Presented,
                text: text.clone(),
                attribution: authored_by(QuoteAuthor::Directive {
                    actor: wire::Actor::TEST_PRINCIPAL,
                }),
                disposition: Some(wire::Disposition::Acked),
                media,
                body: None,
            },
        );
    }
    let message = outer.markdown();

    let presented = present_with(&store, &message, 0);
    let [QuotePresentation::Resolved { text, quotes, .. }] = presented.as_slice() else {
        panic!("outer quote resolves");
    };
    assert_eq!(text, &outer_text);
    let [QuotePresentation::Resolved { text, quotes, .. }] = quotes.as_slice() else {
        panic!("nested quote resolves against its raw digest");
    };
    assert_eq!(
        text,
        &format!("look\n\n{}", image.markdown().expect("markdown"))
    );
    assert!(quotes.is_empty());
    assert_eq!(
        expand_with(&store, &message, 0),
        format!("see {inner_text}")
    );
}
#[test]
fn media_targets_select_one_authenticated_ordinal_and_present_it_as_media() {
    let first =
        wire::Media::from_base64("first.png", "image/png", "iVBORw0KGgo=").expect("fixture media");
    let second =
        wire::Media::from_base64("second.png", "image/png", "iVBORw0KGgo=").expect("fixture media");
    let mut store = MemoryStore::default();
    store.steering.insert(
        "dTVLR0qL0".to_owned(),
        SourceQuote {
            identity: TextIdentity::Presented,
            text: "source prose is not selected".to_owned(),
            attribution: authored_by(QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL,
            }),
            disposition: Some(wire::Disposition::Acked),
            media: vec![first, second.clone()],
            body: None,
        },
    );
    let target = QuoteTarget::SteeringMedia {
        key: "dTVLR0qL0".to_owned(),
        ordinal: 1,
        digest: second.identity().expect("media identity"),
    };

    assert!(matches!(
        resolve_with(&store, &target.to_string(), 0),
        QuoteResolution::Media(ResolvedMediaQuote {
            media,
            author: QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL
            },
            disposition: Some(wire::Disposition::Acked),
        }) if media == second
    ));
    assert!(matches!(
        present_with(&store, &target.markdown(), 0).as_slice(),
        [QuotePresentation::Media { media, .. }] if media == &second
    ));

    let missing = QuoteTarget::SteeringMedia {
        key: "dTVLR0qL0".to_owned(),
        ordinal: 2,
        digest: second.identity().expect("media identity"),
    };
    assert_eq!(
        resolve_with(&store, &missing.to_string(), 0),
        unavailable(&missing.to_string(), QuoteUnavailable::Unknown)
    );
    let mismatched = QuoteTarget::SteeringMedia {
        key: "dTVLR0qL0".to_owned(),
        ordinal: 1,
        digest: QuoteTarget::text_digest("not image bytes"),
    };
    assert_eq!(
        resolve_with(&store, &mismatched.to_string(), 0),
        unavailable(&mismatched.to_string(), QuoteUnavailable::DigestMismatch)
    );
}

#[test]
fn a_media_only_tape_source_answers_only_its_media_targets() {
    let image =
        wire::Media::from_base64("shot.png", "image/png", "iVBORw0KGgo=").expect("fixture media");
    let mut store = MemoryStore::default();
    store.tape.insert(
        format!("{}/{}/3/5/8/13", uuid(1), uuid(2)),
        SourceQuote {
            identity: TextIdentity::Absent,
            text: String::new(),
            attribution: authored_by(QuoteAuthor::Agent {
                lane: uuid(1),
                occupant: uuid(3),
            }),
            disposition: None,
            media: vec![image.clone()],
            body: None,
        },
    );

    // The digest of the empty text a media-only event presents still names
    // no prose: resolution, presentation, and delivery all refuse it.
    let text = tape_target("");
    let spelled = text.to_string();
    assert_eq!(
        resolve_with(&store, &spelled, 0),
        unavailable(&spelled, QuoteUnavailable::Unknown)
    );
    assert_eq!(
        present_with(&store, &text.markdown(), 0),
        [QuotePresentation::Unavailable {
            target: spelled.clone(),
            reason: QuoteUnavailable::Unknown,
        }]
    );
    let mut stub = String::new();
    render_unavailable(&mut stub, &spelled, QuoteUnavailable::Unknown);
    assert_eq!(expand_with(&store, &text.markdown(), 0), stub);

    let media = QuoteTarget::TapeMedia {
        lane: uuid(1),
        source: uuid(2),
        locus: wire::MessageLocus {
            member: 3,
            offset: 5,
            length: 8,
            event: 13,
        },
        ordinal: 0,
        digest: image.identity().expect("media identity"),
    };
    assert!(matches!(
        resolve_with(&store, &media.to_string(), 0),
        QuoteResolution::Media(ResolvedMediaQuote { media, .. }) if media == image
    ));
}

#[test]
fn refusals_name_the_original_target_without_synthesized_values() {
    let store = MemoryStore::default();
    let unknown = tape_target("missing").to_string();
    assert_eq!(
        resolve_with(&store, &unknown, 0),
        unavailable(&unknown, QuoteUnavailable::Unknown)
    );

    let unknown_key = steering_target("dTVLR0qL0", "missing").to_string();
    assert_eq!(
        resolve_with(&store, &unknown_key, 0),
        unavailable(&unknown_key, QuoteUnavailable::Unknown)
    );

    let unreadable_store = MemoryStore {
        unreadable: true,
        ..MemoryStore::default()
    };
    assert_eq!(
        resolve_with(&unreadable_store, &unknown, 0),
        unavailable(&unknown, QuoteUnavailable::Unreadable)
    );

    let malformed = unknown.replacen("/3/", "/03/", 1);
    assert_eq!(
        resolve_with(&store, &malformed, 0),
        unavailable(&malformed, QuoteUnavailable::Malformed)
    );

    let mut store = MemoryStore::default();
    let target = steering_target("dTVLR0qL0", "expected");
    store.steering.insert(
        "dTVLR0qL0".to_owned(),
        SourceQuote {
            identity: TextIdentity::Presented,
            text: "forged".to_owned(),
            attribution: authored_by(QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL,
            }),
            disposition: Some(wire::Disposition::Live),
            media: Vec::new(),
            body: None,
        },
    );
    assert_eq!(
        resolve_with(&store, &target.to_string(), 0),
        unavailable(&target.to_string(), QuoteUnavailable::DigestMismatch)
    );
}

#[test]
fn presentation_expands_nested_quotes_and_preserves_the_depth_boundary_target() {
    let mut store = MemoryStore::default();
    let mut returned_text = "terminal text".to_owned();
    let mut targets = Vec::new();
    for index in 0..9 {
        let key = format!("0000000{index}x");
        let target = steering_target(&key, &returned_text);
        store.steering.insert(
            key,
            SourceQuote {
                identity: TextIdentity::Presented,
                text: returned_text,
                attribution: authored_by(QuoteAuthor::Directive {
                    actor: wire::Actor::TEST_PRINCIPAL,
                }),
                disposition: Some(if index == 4 {
                    wire::Disposition::Acked
                } else {
                    wire::Disposition::Live
                }),
                media: Vec::new(),
                body: None,
            },
        );
        returned_text = target.markdown();
        targets.push(target);
    }
    let unknown = steering_target("dTVLR0qL1", "missing");
    let presented = present_with(
        &store,
        &format!(
            "{} and {}",
            targets
                .last()
                .expect("chain has an outer target")
                .markdown(),
            unknown.markdown()
        ),
        0,
    );

    assert_eq!(presented.len(), 2);
    let mut node = &presented[0];
    for depth in 0..usize::from(QUOTE_DEPTH_LIMIT) {
        let QuotePresentation::Resolved {
            target,
            attribution,
            disposition,
            quotes,
            ..
        } = node
        else {
            panic!("depth {depth} should resolve");
        };
        let source_index = 8 - depth;
        assert_eq!(target, &targets[source_index].to_string());
        assert_eq!(
            &attribution.author,
            &QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL,
            }
        );
        assert_eq!(
            *disposition,
            Some(if source_index == 4 {
                wire::Disposition::Acked
            } else {
                wire::Disposition::Live
            })
        );
        assert_eq!(quotes.len(), 1);
        node = &quotes[0];
    }
    assert_eq!(
        node,
        &QuotePresentation::Unavailable {
            target: targets[0].to_string(),
            reason: QuoteUnavailable::DepthExceeded,
        }
    );
    assert_eq!(
        presented[1],
        QuotePresentation::Unavailable {
            target: unknown.to_string(),
            reason: QuoteUnavailable::Unknown,
        }
    );
}

#[test]
fn presentation_reloads_current_directive_disposition() {
    let key = "dTVLR0qL0";
    let text = "operator text";
    let target = steering_target(key, text);
    let mut store = MemoryStore::default();
    store.steering.insert(
        key.to_owned(),
        SourceQuote {
            identity: TextIdentity::Presented,
            text: text.to_owned(),
            attribution: authored_by(QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL,
            }),
            disposition: Some(wire::Disposition::Live),
            media: Vec::new(),
            body: None,
        },
    );
    let message = target.markdown();
    let live = present_with(&store, &message, 0);
    store
        .steering
        .get_mut(key)
        .expect("directive fixture exists")
        .disposition = Some(wire::Disposition::Acked);
    let refreshed = present_with(&store, &message, 0);

    assert!(matches!(
        &live[0],
        QuotePresentation::Resolved {
            disposition: Some(wire::Disposition::Live),
            ..
        }
    ));
    assert!(matches!(
        &refreshed[0],
        QuotePresentation::Resolved {
            disposition: Some(wire::Disposition::Acked),
            ..
        }
    ));
}

#[test]
fn delivery_expands_nested_quotes_and_names_unavailable_sources() {
    let mut store = MemoryStore::default();
    let inner_text = "the authenticated inner words";
    let inner = steering_target("dTVLR0qL0", inner_text);
    store.steering.insert(
        "dTVLR0qL0".to_owned(),
        SourceQuote {
            identity: TextIdentity::Presented,
            text: inner_text.to_owned(),
            attribution: authored_by(QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL,
            }),
            disposition: Some(wire::Disposition::Live),
            media: Vec::new(),
            body: None,
        },
    );
    let outer_text = format!("before {} after", inner.markdown());
    let outer = steering_target("dTVLR0qL1", &outer_text);
    store.steering.insert(
        "dTVLR0qL1".to_owned(),
        SourceQuote {
            identity: TextIdentity::Presented,
            text: outer_text,
            attribution: authored_by(QuoteAuthor::Directive {
                actor: wire::Actor::TEST_PRINCIPAL,
            }),
            disposition: Some(wire::Disposition::Live),
            media: Vec::new(),
            body: None,
        },
    );
    let missing = steering_target("dTVLR0qL2", "missing");
    let source = format!("say {} then {}", outer.markdown(), missing.markdown());

    assert_eq!(
        expand_with(&store, &source, 0),
        format!(
            "say before {inner_text} after then \
                 [quote unavailable: reason=unknown source={:?}]",
            missing.to_string(),
        )
    );
}

#[test]
fn delivery_depth_limit_leaves_one_explicit_source_stub() {
    let mut store = MemoryStore::default();
    let mut returned_text = "terminal text".to_owned();
    let mut outer = None;
    for index in 0..9 {
        let key = format!("0000000{index}x");
        let target = steering_target(&key, &returned_text);
        store.steering.insert(
            key,
            SourceQuote {
                identity: TextIdentity::Presented,
                text: returned_text,
                attribution: authored_by(QuoteAuthor::Directive {
                    actor: wire::Actor::TEST_PRINCIPAL,
                }),
                disposition: Some(wire::Disposition::Live),
                media: Vec::new(),
                body: None,
            },
        );
        returned_text = target.markdown();
        outer = Some(target);
    }

    let expanded = expand_with(
        &store,
        &outer.expect("chain has an outer target").markdown(),
        0,
    );
    assert!(!expanded.contains("[quote]("), "{expanded}");
    assert!(expanded.contains("[quote unavailable: reason=depth-exceeded source="));
    assert_eq!(expanded.lines().count(), 1);
}

/// One settled omp assistant message whose visible prose the provider
/// folded into reasoning, leaving only call metadata beside it.
const NARRATED_SYN: &str = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"SYN #dVwg6Zqlf: paraphrased question"},{"type":"toolCall","id":"call-1","name":"shell","arguments":{"command":"true","i":"SYN #dVwg6Zqlf: arguments are not authored words"},"intent":"SYN #dVwg6Zqlf: Which gesture activates a design?"}],"stopReason":"toolUse"}}"#;

fn narrated_events() -> Vec<listen::Event> {
    let decoded = listen::decode(
        listen::Dialect::Omp,
        NARRATED_SYN.as_bytes(),
        tape::Locus::new(1, 0, NARRATED_SYN.len() as u64),
    )
    .expect("fixture record decodes");
    decoded.events().to_vec()
}

fn range(start: u64, end: u64) -> wire::rooms::TextRange {
    wire::rooms::TextRange { start, end }
}

#[test]
fn a_hunk_naming_reasoning_a_call_intent_or_an_out_of_range_span_reads_as_changed() {
    let events = narrated_events();
    let reasoning = events
        .iter()
        .position(|event| matches!(event, listen::Event::Reasoning(_)))
        .expect("the record carries reasoning");
    let call = events
        .iter()
        .position(|event| matches!(event, listen::Event::Tool(_)))
        .expect("the record carries the call");
    let changed = Err(wire::rooms::RoomSourceUnavailable::Changed {});

    assert_eq!(hunk_text(&events, reasoning as u32, range(0, 3)), changed);
    let header = "SYN #dVwg6Zqlf: ".len() as u64;
    let end = "SYN #dVwg6Zqlf: Which gesture activates a design?".len() as u64;
    assert_eq!(hunk_text(&events, call as u32, range(header, end)), changed);
    assert_eq!(hunk_text(&events, u32::MAX, range(0, 3)), changed);
}

#[test]
fn operator_prose_is_never_packet_text() {
    let operator = listen::Event::StreamedProse(listen::Prose {
        role: listen::ProseRole::Operator,
        text: "SYN #dVwg6Zqlf: not authored by the lane".to_owned(),
        phase: None,
        media: Vec::new(),
        external_media: Vec::new(),
        locus: tape::Locus::new(1, 0, 1),
    });

    assert_eq!(packet_text(&operator), None);
}
