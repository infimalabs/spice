use super::*;

// Generic identity, hash, spelling, and rank laws shared by this fixture.

/// Row used by the packed frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Row {
    pub(super) task: TaskHandle,
}

pub(super) fn uuid(text: &str) -> CanonicalUuid {
    text.parse().expect("canonical uuid")
}

pub(super) fn seq(value: u64) -> Seq {
    Seq::new(value).expect("non-zero position")
}

pub(super) fn handle(text: &str) -> TaskHandle {
    text.parse().expect("task handle")
}

pub(super) fn stamp(text: &str) -> TaskStamp {
    text.parse().expect("task stamp")
}

pub(super) fn digest(nibble: &str) -> Digest {
    nibble.repeat(64).parse().expect("digest spelling")
}

fn hash_of<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Assert the law for one type's samples.
///
/// Checks, per sample: encoding then decoding returns an equal value, and
/// re-encoding the decoded value returns identical bytes. Checks, per ordered
/// pair: byte equality holds exactly when value equality holds.
pub(super) fn law<T>(name: &str, samples: &[T])
where
    T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    assert_identity_law(name, samples, encoded_by_wire);
}

/// The encoder a type's identity binds, for types where that is `wire::encode`.
///
/// Named rather than written inline at each call so the call states which
/// encoding it checks. A type whose identity binds a different encoding names
/// that one instead; see [`canonical_command_identity`].
fn encoded_by_wire<T: Serialize>(value: &T) -> Vec<u8> {
    wire::encode(value).expect("value encodes")
}

/// The encoder `Command` identity binds.
///
/// A command's identity is its canonical rendering, not `wire::encode`'s.
/// The two differ exactly where the difference matters: canonicalization sorts
/// `expected`, so a client that lists the same predicates in another order
/// spells the same request. Checking `Command` under the generic encoder would
/// report the type covered while testing bytes nothing takes an identity over.
pub(super) fn canonical_command_identity<B: Serialize>(command: &Command<B>) -> Vec<u8> {
    command
        .canonical_json()
        .expect("command canonicalizes")
        .into_bytes()
}

/// Assert the law for one type's samples under the encoder that type's
/// identity actually binds.
///
/// The encoder is a parameter rather than always [`encoded_by_wire`] because a
/// type is free to bind a narrower rendering than the generic one, and only the
/// bound rendering is the one anything takes an identity over. Each call names
/// which it checked.
pub(super) fn assert_identity_law<T, E>(name: &str, samples: &[T], encode: E)
where
    T: serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    E: Fn(&T) -> Vec<u8>,
{
    assert!(
        samples.len() >= 2,
        "{name} needs at least two samples for the pairwise check to say anything",
    );

    let encoded: Vec<Vec<u8>> = samples.iter().map(&encode).collect();

    for (sample, bytes) in samples.iter().zip(&encoded) {
        let decoded: T = wire::decode(bytes).unwrap_or_else(|_| panic!("{name} decodes"));
        assert_eq!(
            &decoded, sample,
            "{name} lost a distinction its bytes do not carry",
        );

        let again = encode(&decoded);
        assert_eq!(&again, bytes, "{name} does not encode stably");
    }

    for (left, left_bytes) in samples.iter().zip(&encoded) {
        for (right, right_bytes) in samples.iter().zip(&encoded) {
            assert_eq!(
                left_bytes == right_bytes,
                left == right,
                "{name}: byte equality and value equality disagree for {left:?} and {right:?}",
            );
        }
    }
}

/// Assert the hash agreement every covered `Hash` type owes.
///
/// Stated so it says something for every such type rather than only for the
/// types that happen to carry two samples encoding alike. A decoded round trip
/// is a second value equal to the first, so this exercises the `Eq`-`Hash`
/// contract across the wire once per sample; the pairwise arm then catches a
/// type whose distinct-looking samples spell the same bytes.
fn hash_law<T>(name: &str, samples: &[T], encoded: &[Vec<u8>])
where
    T: serde::de::DeserializeOwned + Hash,
{
    for (sample, bytes) in samples.iter().zip(encoded) {
        let decoded: T = wire::decode(bytes).unwrap_or_else(|_| panic!("{name} decodes"));
        assert_eq!(
            hash_of(sample),
            hash_of(&decoded),
            "{name}: a value and its round trip hash differently, so a keyed \
             lookup would miss what the bytes say is the same value",
        );
    }

    for (left, left_bytes) in samples.iter().zip(encoded) {
        for (right, right_bytes) in samples.iter().zip(encoded) {
            if left_bytes == right_bytes {
                assert_eq!(
                    hash_of(left),
                    hash_of(right),
                    "{name}: identical bytes must hash identically",
                );
            }
        }
    }
}

/// Assert the law plus the agreements a type ranked by its spelling owes.
///
/// These are the narrowing-hazard types: each crosses as one admitted text,
/// and each ranks by that text and nothing else. The order assertion is
/// directional. Asserting only that `cmp` is `Equal` exactly when the bytes
/// match leaves `Less` and `Greater` free to point the opposite way from the
/// spelling, which is what a ranked list actually reads, and `Actor` did point
/// the opposite way: `Operator` is declared before `Lane(..)` while `lane:`
/// spells before `operator`.
///
/// A type whose rank is a declared sequence rather than its text belongs in
/// [`ranked_law`] instead.
pub(super) fn spelling_law<T>(name: &str, samples: &[T])
where
    T: Serialize + serde::de::DeserializeOwned + Ord + Hash + std::fmt::Debug,
{
    law(name, samples);

    let encoded: Vec<Vec<u8>> = samples.iter().map(encoded_by_wire).collect();
    hash_law(name, samples, &encoded);

    for bytes in &encoded {
        assert_eq!(
            bytes.first(),
            Some(&b'"'),
            "{name}: claims to rank by its spelling but does not cross as text, \
             so comparing encoded bytes would not be comparing that spelling",
        );
    }

    for (left, left_bytes) in samples.iter().zip(&encoded) {
        for (right, right_bytes) in samples.iter().zip(&encoded) {
            assert_eq!(
                left.cmp(right),
                left_bytes.cmp(right_bytes),
                "{name}: rank and spelling disagree for {left:?} and {right:?}; \
                 the value ranks {:?} while its bytes rank {:?}",
                left.cmp(right),
                left_bytes.cmp(right_bytes),
            );
        }
    }
}

/// Assert the law plus the agreements a type ranked by a declared sequence
/// owes, given its samples in ascending order.
///
/// Not every ordered type ranks by its spelling, and asserting that it does
/// would be false rather than strict. [`Seq`] ranks numerically while spelling
/// decimal digits, so `9` ranks before `10` and spells after it; a closed
/// vocabulary like [`ResidentState`] ranks by how far a shutdown has
/// progressed, which is the order the arms are declared in. For these the
/// ascending order is the thing worth pinning, so it is written out at the call
/// site and compared against `cmp` directly.
///
/// The fixture then requires at least one adjacent pair the spelling ranks the
/// other way. A declared order that agrees with its spelling at every step is
/// a spelling order wearing a different name, and belongs in [`spelling_law`]
/// where the whole sequence is checked rather than the sequence someone
/// remembered to list.
pub(super) fn ranked_law<T>(name: &str, ascending: &[T])
where
    T: Serialize + serde::de::DeserializeOwned + Ord + Hash + std::fmt::Debug,
{
    law(name, ascending);

    let encoded: Vec<Vec<u8>> = ascending.iter().map(encoded_by_wire).collect();
    hash_law(name, ascending, &encoded);

    for (left, right) in ascending.iter().zip(ascending.iter().skip(1)) {
        assert!(
            left < right,
            "{name}: samples are given ascending but {left:?} does not rank \
             before {right:?}",
        );
    }

    let contradicted = encoded
        .iter()
        .zip(encoded.iter().skip(1))
        .any(|(left, right)| left > right);
    assert!(
        contradicted,
        "{name}: every step of the declared order also ascends by spelling, so \
         this proves nothing the stricter spelling_law would not prove; either \
         move it there or extend the samples to the pair that disagrees",
    );
}

/// The body every crossing type generic over one carries in these samples.
///
/// This is a registered fact body rather than a shape declared here, because
/// [`Fact`] admits only bodies the vocabulary binds. It serves the command
/// samples too, whose bodies are unconstrained.
pub(super) fn body(validation: &str) -> body::TaskDone {
    body::TaskDone {
        task: stamp("1kKRRsDPx"),
        validation: validation.to_owned(),
        disposition: None,
        terminal: None,
        git: None,
        source: body::SourceBoundary::Proven { heads: Vec::new() },
    }
}

/// A digest fixture, spelled the one way a digest is spelled.
pub(super) const DIGEST: &str = "9f2b4c7d1e6a08b35c4f9d2e7a1b8c05d6e3f4a291b7c8d0e5f6a3b2c1d4e7f8";

/// Parse a phase fixture, which is one lower-case word by construction.
pub(super) fn phase(text: &str) -> Phase {
    text.parse().expect("phase")
}

/// Parse a name fixture, which is one lower-case word by construction.
pub(super) fn name(text: &str) -> Name {
    text.parse().expect("name")
}

pub(super) fn task_witness() -> Witness {
    Witness::Task {
        task: stamp("1kKRRsDPx"),
        head: seq(48190),
    }
}

pub(super) fn workflow_witness() -> Witness {
    Witness::Workflow {
        task: stamp("1kKRRsDPx"),
        head: seq(48190),
        phase: "todo".parse().expect("phase"),
    }
}

/// A command differing from its siblings only in the predicates it names.
pub(super) fn command(kind: &str, expected: Vec<Witness>) -> Command<body::TaskDone> {
    Command {
        v: WireVersion(1),
        id: uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"),
        epoch: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
        kind: kind.parse::<CommandKind>().expect("command kind"),
        expected,
        body: body("checked"),
    }
}

/// Witness order is not part of what a command asked for.
///
/// The two identities a command could have — its canonical bytes and its
/// value equality — have to agree, or a caller that de-duplicates by value and
/// a journal that de-duplicates by bytes would disagree about whether they had
/// already seen a request.
#[test]
fn a_command_is_the_predicates_it_names_not_the_order_it_named_them() {
    let asked = command("task::done", vec![task_witness(), workflow_witness()]);
    let reordered = command("task::done", vec![workflow_witness(), task_witness()]);

    assert_eq!(
        canonical_command_identity(&asked),
        canonical_command_identity(&reordered),
        "canonical identity must sort witness order away",
    );
    assert_eq!(
        asked, reordered,
        "two commands the wire cannot tell apart must not compare unequal",
    );
}

/// Naming a predicate twice is not the same as naming it once.
///
/// Canonicalization sorts and deliberately does not deduplicate, so
/// multiplicity survives into the bytes. Equality has to see what the bytes
/// see, or the collision the sort closes would reopen one field over.
#[test]
fn a_command_keeps_the_multiplicity_its_canonical_bytes_keep() {
    let once = command("task::done", vec![task_witness()]);
    let twice = command("task::done", vec![task_witness(), task_witness()]);

    assert_ne!(
        canonical_command_identity(&once),
        canonical_command_identity(&twice),
        "a repeated predicate must survive canonicalization",
    );
    assert_ne!(
        once, twice,
        "equality must keep the multiplicity the bytes keep",
    );
}
/// Parsing does not repair another valid timestamp spelling into WIRE's one spelling.
#[test]
fn an_instant_admits_only_utc_millisecond_text() {
    let canonical = "2030-04-12T18:44:09.271Z";
    assert_eq!(
        canonical
            .parse::<Instant>()
            .expect("canonical instant")
            .to_string(),
        canonical,
    );

    for text in [
        "2030-04-12T18:44:09.271+00:00",
        "2030-04-12T18:44:09Z",
        "2030-04-12T18:44:09.271828Z",
        "2030-04-12T18:44:09.271z",
        "2030-04-12 18:44:09.271Z",
        "2030-04-12T18:44:09.271Z ",
    ] {
        assert_eq!(
            text.parse::<Instant>(),
            Err(wire::IdentError::Instant(text.to_owned())),
            "{text:?} must be refused rather than repaired",
        );
    }
}

/// Construction narrows clock precision before the generic identity law sees a value.
#[test]
fn instant_construction_floors_to_the_wire_millisecond() {
    for (reading, spelling) in [
        ("2030-04-12T18:44:09.271828Z", "2030-04-12T18:44:09.271Z"),
        ("1969-12-31T23:59:59.999999Z", "1969-12-31T23:59:59.999Z"),
    ] {
        let timestamp = reading.parse::<jiff::Timestamp>().expect("timestamp");
        let narrowed = Instant::new(timestamp).expect("timestamp is in the wire year range");

        assert_eq!(narrowed.to_string(), spelling);
        assert!(
            narrowed.get() <= timestamp,
            "flooring must not move {reading} forward",
        );
    }

    assert!(
        matches!(
            Instant::new(jiff::Timestamp::MIN),
            Err(wire::IdentError::Instant(spelling)) if spelling.starts_with('-')
        ),
        "an extended negative year has no admitted wire spelling",
    );
}

/// Parse a timestamp fixture and narrow it the way the wire admits one.
fn instant(text: &str) -> Instant {
    Instant::new(text.parse().expect("timestamp")).expect("instant is in the wire year range")
}

/// A complete taste reminder, every field spelled rather than defaulted in.
///
/// `compaction` is `0` because that is what the field means before the occupant
/// has compacted, not because zero is what a `u64` falls back to; the variant
/// below carries the post-compaction case.
///
/// Built as one base the variants differ from in exactly one place, so what
/// each sample proves is stated by which field it moved rather than by reading
/// two unrelated values side by side.
fn reminder() -> body::TasteReminded {
    body::TasteReminded {
        directive: uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"),
        key: "taste-hedging-1".to_owned(),
        lane: uuid("018f82d4-6f59-7d06-92e3-bce9131163c3"),
        taste: "hedging".to_owned(),
        content: Digest::compute(Domain::TasteProse, b"it might possibly be the case that"),
        occupant: uuid("018f82d4-6f59-7d06-92e3-bce9131163c5"),
        compaction: 0,
        locus: body::TranscriptLocus {
            source: None,
            member: 12,
            offset: 4096,
            length: 512,
            event: 3,
            line: 7,
            column: 19,
        },
        text: "say it plainly or do not say it".to_owned(),
        expires: instant("2030-04-12T18:44:09.271Z"),
    }
}

/// A fact is its body's bytes, and the reminder body is ten fields wide.
///
/// [`Fact`] is sampled in the crossing fixture over one body, `TaskDone`,
/// which carries a handle and two strings. That says nothing about a body
/// composed of a [`Digest`], an [`Instant`], a bare `u64` compaction epoch, and
/// a six-field locus: each is a place a field can be dropped at encode or
/// narrowed at render while derived equality keeps the distinction the bytes
/// lost. `TranscriptLocus` is reached through this reminder body.
///
/// The samples are a base reminder plus one variant per field, each differing
/// from the base in exactly that field. A field the encoding drops therefore
/// makes its own variant encode to the base's bytes while comparing unequal,
/// and the pairwise arm names which one. Two merely different reminders would
/// not say that: they differ everywhere at once, so the bytes differ for
/// whichever fields do survive and the dropped field hides behind them.
#[test]
fn a_taste_reminder_fact_carries_no_more_than_its_bytes() {
    let base = reminder();
    let other = uuid("018f82d4-6f59-7d06-92e3-bce9131163c4");
    let samples = [
        base.clone(),
        body::TasteReminded {
            directive: other,
            ..base.clone()
        },
        body::TasteReminded {
            key: "taste-hedging-2".to_owned(),
            ..base.clone()
        },
        body::TasteReminded {
            lane: other,
            ..base.clone()
        },
        body::TasteReminded {
            taste: "padding".to_owned(),
            ..base.clone()
        },
        body::TasteReminded {
            content: Digest::compute(Domain::TasteProse, b"it is the case that"),
            ..base.clone()
        },
        body::TasteReminded {
            occupant: other,
            ..base.clone()
        },
        body::TasteReminded {
            compaction: 7,
            ..base.clone()
        },
        body::TasteReminded {
            locus: body::TranscriptLocus {
                column: 20,
                ..base.locus
            },
            ..base.clone()
        },
        body::TasteReminded {
            text: "cut the hedge".to_owned(),
            ..base.clone()
        },
        body::TasteReminded {
            expires: instant("2030-04-12T18:44:09.272Z"),
            ..base.clone()
        },
    ]
    .map(Fact::new);

    assert_identity_law("Fact<TasteReminded>", &samples, encoded_by_wire);
}

/// An actor ranks by the text it crosses as, not by where it was declared.
///
/// This is the pair that proves the fixture's order assertion reads direction
/// rather than only equality. `Operator` is declared before `Lane(..)` while
/// `lane:` spells before `operator`, so a derived order and the bytes point
/// opposite ways here and an `Equal`-only assertion passes over it.
#[test]
fn a_lane_ranks_before_the_operator_because_its_spelling_does() {
    let lane = Actor::Lane(uuid("018f82d4-6f59-7d06-92e3-bce9131163c2"));
    let operator = Actor::TEST_PRINCIPAL;

    assert!(
        lane.to_string() < operator.to_string(),
        "the spelling this regression rests on changed",
    );
    assert!(
        lane < operator,
        "a lane must rank before the operator, as their spellings do",
    );
}
