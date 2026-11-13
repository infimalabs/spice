//! Typed problem-space contributions to an open Design room.
//!
//! A contribution is one immutable, lane-authored record: a charge on one of
//! the Design's question items or an open question, its author, its text, and
//! the exact sources it cites. It is evidence for the operator's Design
//! judgment, never authority: recording or reading one moves no task, claim,
//! candidate, decision, or authorization.

use std::fmt::{self, Write as _};
use std::str::FromStr;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{RoomAnchor, RoomAuthorityStamp, RoomSource};
use crate::{CanonicalUuid, Digest, Domain};

/// Maximum bytes of one contribution's text.
pub const ROOM_CONTRIBUTION_TEXT_LIMIT: usize = 4_096;
/// Maximum sources one contribution may cite.
pub const ROOM_CONTRIBUTION_SOURCE_LIMIT: usize = 8;

/// A contribution's signed weight on one Design question item.
///
/// +1 and -1 are opinions and need no citation; 0 is a neutral observation and
/// +2 and -2 are strong charges, and each of those must cite a source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomCharge(i8);

impl RoomCharge {
    /// Every charge from strongest for to strongest against.
    pub const ALL: [Self; 5] = [Self(2), Self(1), Self(0), Self(-1), Self(-2)];

    /// The charge `value` names, if it lies in -2..=2.
    #[must_use]
    pub const fn new(value: i8) -> Option<Self> {
        if value >= -2 && value <= 2 {
            Some(Self(value))
        } else {
            None
        }
    }

    /// The signed weight.
    #[must_use]
    pub const fn get(self) -> i8 {
        self.0
    }

    /// Whether this charge is an opinion that needs no citation: +1 or -1.
    #[must_use]
    pub const fn is_opinion(self) -> bool {
        self.0 == 1 || self.0 == -1
    }
}

impl fmt::Display for RoomCharge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 > 0 {
            write!(formatter, "+{}", self.0)
        } else {
            write!(formatter, "{}", self.0)
        }
    }
}

/// The identity of one task statement item's text: a digest of the item
/// trimmed, with whitespace runs collapsed and case folded. Reordering items
/// keeps an item's identity; rewording it mints a new one.
///
/// Callers name an item by a prefix of its digest. Its handle is the prefix
/// surfaces render: [`Self::HANDLE_LEN`] hex digits, extended only while
/// another current item shares them.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StatementItem(Digest);

impl StatementItem {
    /// Hex digits a rendered handle carries unless another current item
    /// shares them.
    pub const HANDLE_LEN: usize = 6;

    /// The identity of item text `text`.
    #[must_use]
    pub fn of(text: &str) -> Self {
        let mut normalized = String::with_capacity(text.len());
        for word in text.split_whitespace() {
            if !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.extend(word.chars().flat_map(char::to_lowercase));
        }
        Self(Digest::compute(
            Domain::StatementItem,
            normalized.as_bytes(),
        ))
    }

    /// The identity of each item in `term`, in statement order.
    pub fn each(term: &[String]) -> impl Iterator<Item = Self> + '_ {
        term.iter().map(|item| Self::of(item))
    }

    /// Every item of `term` as one `- HANDLE text` line per item, in
    /// statement order; empty when the statement names none.
    #[must_use]
    pub fn bullets(term: &[String]) -> String {
        let current = Self::each(term).collect::<Vec<_>>();
        let mut bullets = String::new();
        for (item, text) in current.iter().zip(term) {
            if !bullets.is_empty() {
                bullets.push('\n');
            }
            write!(bullets, "- {} {text}", item.handle(&current))
                .expect("writing to a String succeeds");
        }
        bullets
    }

    /// The text of this item in `term`, if the list holds it.
    #[must_use]
    pub fn text_in<'a>(&self, term: &'a [String]) -> Option<&'a str> {
        term.iter()
            .find(|item| Self::of(item) == *self)
            .map(String::as_str)
    }

    /// The handle surfaces render for this item among `among`, the current
    /// items: the shortest prefix of at least [`Self::HANDLE_LEN`] digits no
    /// other distinct item shares.
    #[must_use]
    pub fn handle(&self, among: &[Self]) -> &str {
        let digest = self.0.as_str();
        let mut length = Self::HANDLE_LEN;
        while length < digest.len()
            && among
                .iter()
                .any(|other| other != self && other.0.as_str().starts_with(&digest[..length]))
        {
            length += 1;
        }
        &digest[..length]
    }

    /// The one item of `among` whose digest begins with `prefix`, case
    /// folded. Identical items are one identity and never make a prefix
    /// ambiguous.
    ///
    /// A prefix carries at least [`Self::HANDLE_LEN`] digits, as every
    /// rendered handle does, so a short number written as a one-based
    /// position never resolves to whichever item happens to begin with it.
    ///
    /// # Errors
    ///
    /// Refuses a short or non-hex prefix, one no current item begins with,
    /// and one several distinct items begin with.
    pub fn resolve<'a>(
        prefix: &str,
        among: &'a [Self],
    ) -> Result<&'a Self, StatementItemPrefixError> {
        let prefix = prefix.trim().to_ascii_lowercase();
        if prefix.len() < Self::HANDLE_LEN || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StatementItemPrefixError::Malformed);
        }
        let mut matches = among
            .iter()
            .filter(|item| item.0.as_str().starts_with(&prefix));
        let Some(first) = matches.next() else {
            return Err(StatementItemPrefixError::Unknown);
        };
        if matches.any(|item| item != first) {
            return Err(StatementItemPrefixError::Ambiguous);
        }
        Ok(first)
    }
}

impl fmt::Display for StatementItem {
    /// The full digest hex.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.as_str())
    }
}

/// Why an item prefix names no single current statement item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StatementItemPrefixError {
    /// The prefix is shorter than a handle or holds a non-hex character.
    #[error("a statement-item handle is at least six hex digits")]
    Malformed,
    /// No current item's digest begins with the prefix.
    #[error("no current statement item begins with this handle")]
    Unknown,
    /// Several distinct current items' digests begin with the prefix.
    #[error("several current statement items begin with this handle")]
    Ambiguous,
}

/// What a contribution offers the Design: a charge on one question item, or
/// an open question.
///
/// On the wire a charge is its integer and a question is the word `question`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RoomContributionKind {
    /// A signed weight on one question item.
    Charge(RoomCharge),
    /// An open question the Design has not answered.
    Question,
}

impl RoomContributionKind {
    /// Every kind, in the order reads group them.
    pub const ALL: [Self; 6] = [
        Self::Charge(RoomCharge::ALL[0]),
        Self::Charge(RoomCharge::ALL[1]),
        Self::Charge(RoomCharge::ALL[2]),
        Self::Charge(RoomCharge::ALL[3]),
        Self::Charge(RoomCharge::ALL[4]),
        Self::Question,
    ];

    /// The charge `value` names.
    ///
    /// # Panics
    ///
    /// When `value` lies outside -2..=2.
    #[must_use]
    pub const fn charge(value: i8) -> Self {
        match RoomCharge::new(value) {
            Some(charge) => Self::Charge(charge),
            None => panic!("a room charge lies in -2..=2"),
        }
    }

    /// The kind as reads and the command line spell it: a signed charge or
    /// `question`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Charge(RoomCharge(2)) => "+2",
            Self::Charge(RoomCharge(1)) => "+1",
            Self::Charge(RoomCharge(0)) => "0",
            Self::Charge(RoomCharge(-1)) => "-1",
            Self::Charge(_) => "-2",
            Self::Question => "question",
        }
    }
}

impl fmt::Display for RoomContributionKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for RoomContributionKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Charge(charge) => serializer.serialize_i8(charge.0),
            Self::Question => serializer.serialize_str("question"),
        }
    }
}

impl<'de> Deserialize<'de> for RoomContributionKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Kind;

        impl Visitor<'_> for Kind {
            type Value = RoomContributionKind;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an integer charge in -2..=2 or \"question\"")
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                i8::try_from(value)
                    .ok()
                    .and_then(RoomCharge::new)
                    .map(RoomContributionKind::Charge)
                    .ok_or_else(|| E::invalid_value(de::Unexpected::Signed(value), &self))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                i8::try_from(value)
                    .ok()
                    .and_then(RoomCharge::new)
                    .map(RoomContributionKind::Charge)
                    .ok_or_else(|| E::invalid_value(de::Unexpected::Unsigned(value), &self))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                if value == "question" {
                    Ok(RoomContributionKind::Question)
                } else {
                    Err(E::invalid_value(de::Unexpected::Str(value), &self))
                }
            }
        }

        deserializer.deserialize_any(Kind)
    }
}

/// A spelling outside the closed charge vocabulary.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid contribution charge {0:?}; expected -2, -1, 0, +1, or +2")]
pub struct RoomChargeError(String);

/// Canonical immutable authority behind one room-source link. A transcript's
/// moving accounted boundary is deliberately absent.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum RoomSourceAuthority<'a> {
    Transcript {
        lane: &'a CanonicalUuid,
        vendor: &'a crate::Name,
        stream: &'a str,
        source: &'a CanonicalUuid,
        locus: &'a crate::MessageLocus,
        text: &'a super::TextRange,
    },
    Journal {
        fact: &'a crate::SequenceCode,
    },
    Artifact {
        digest: &'a Digest,
    },
    Contribution {
        room: &'a RoomAnchor,
        publication: &'a RoomAuthorityStamp,
    },
    Git {
        commit: &'a super::GitObjectId,
        path: &'a str,
        lines: &'a Option<super::LineRange>,
    },
}

impl FromStr for RoomCharge {
    type Err = RoomChargeError;

    /// A signed or bare integer in -2..=2: `+2`, `1`, `0`, `-1`.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse::<i8>()
            .ok()
            .and_then(Self::new)
            .ok_or_else(|| RoomChargeError(value.to_owned()))
    }
}

impl RoomSource {
    /// Whether two links name the same immutable source authority.
    ///
    /// A transcript's accounted boundary may advance without changing the
    /// event bytes its locus and text range identify.
    #[must_use]
    pub fn same_authority(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Transcript(left), Self::Transcript(right)) => left.same_bytes(right),
            (Self::Journal { fact: left }, Self::Journal { fact: right }) => left == right,
            (Self::Artifact { digest: left }, Self::Artifact { digest: right }) => left == right,
            (Self::Contribution { .. }, Self::Contribution { .. }) => self == other,
            (Self::Git(left), Self::Git(right)) => {
                left.commit == right.commit && left.path == right.path && left.lines == right.lines
            }
            _ => false,
        }
    }

    /// Digest of the exact immutable authority this link names.
    #[must_use]
    pub fn authority_digest(&self) -> Digest {
        let authority = match self {
            Self::Transcript(hunk) => RoomSourceAuthority::Transcript {
                lane: &hunk.head.lane,
                vendor: &hunk.head.vendor,
                stream: &hunk.head.stream,
                source: &hunk.head.source,
                locus: &hunk.locus,
                text: &hunk.text,
            },
            Self::Journal { fact } => RoomSourceAuthority::Journal { fact },
            Self::Artifact { digest } => RoomSourceAuthority::Artifact { digest },
            Self::Contribution { room, publication } => {
                RoomSourceAuthority::Contribution { room, publication }
            }
            Self::Git(source) => RoomSourceAuthority::Git {
                commit: &source.commit,
                path: &source.path,
                lines: &source.lines,
            },
        };
        let canonical = crate::encode(&authority).expect("a typed room source authority encodes");
        Digest::compute(Domain::RoomSource, &canonical)
    }
}

/// Whether source witnesses name each cited immutable authority exactly once.
///
/// Non-source witnesses are ignored. The bounded comparison stays on the
/// stack because every admitted contribution has at most eight citations.
#[must_use]
pub fn source_witnesses_match(sources: &[RoomSource], expected: &[crate::Witness]) -> bool {
    if sources.len() > ROOM_CONTRIBUTION_SOURCE_LIMIT {
        return false;
    }
    let mut cited: [Option<Digest>; ROOM_CONTRIBUTION_SOURCE_LIMIT] = std::array::from_fn(|_| None);
    for (slot, source) in cited.iter_mut().zip(sources) {
        *slot = Some(source.authority_digest());
    }
    let mut matched = [false; ROOM_CONTRIBUTION_SOURCE_LIMIT];
    let mut source_count = 0_usize;
    for witness in expected {
        let crate::Witness::Source { source } = witness else {
            continue;
        };
        source_count += 1;
        let Some(index) = cited[..sources.len()]
            .iter()
            .position(|cited| cited.as_ref() == Some(source))
        else {
            return false;
        };
        if std::mem::replace(&mut matched[index], true) {
            return false;
        }
    }
    source_count == sources.len() && matched[..sources.len()].iter().all(|matched| *matched)
}

/// A lane contributed one typed record to an open Design room.
///
/// The fact owns the text; each source stays a link to the authority that
/// owns its bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomContributed {
    /// The Design anchor; this is the room identity.
    pub room: RoomAnchor,
    /// Native moment identity of this publication.
    pub publication: RoomAuthorityStamp,
    /// The logical lane that authored the contribution.
    pub lane: CanonicalUuid,
    /// What the contribution offers: a charge or a question.
    pub contribution: RoomContributionKind,
    /// The text identity of the question item a charge answers at
    /// [`Self::statement`]; the item keeps one charge series across statement
    /// reorders. Every charge names one; a question may.
    #[serde(deserialize_with = "crate::optional::required")]
    pub item: Option<StatementItem>,
    /// Design statement head already guarded by contribution admission.
    pub statement: crate::Seq,
    /// The lane's complete contributed text.
    pub text: String,
    /// Exact sources cited, in the author's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<RoomSource>,
}

/// Why a contribution's own shape is inadmissible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RoomContributionError {
    /// The text is empty or only whitespace.
    #[error("contribution text is empty")]
    Empty,
    /// The stored text retains leading or trailing whitespace.
    #[error("contribution text is not trimmed")]
    Untrimmed,
    /// The text exceeds [`ROOM_CONTRIBUTION_TEXT_LIMIT`] bytes.
    #[error("contribution text exceeds {ROOM_CONTRIBUTION_TEXT_LIMIT} bytes")]
    TextTooLong,
    /// More than [`ROOM_CONTRIBUTION_SOURCE_LIMIT`] sources are cited.
    #[error("contribution cites more than {ROOM_CONTRIBUTION_SOURCE_LIMIT} sources")]
    TooManySources,
    /// One source is cited twice.
    #[error("contribution cites one source twice")]
    DuplicateSource,
    /// A new charge names no question item.
    #[error("a charge names no question item")]
    Unplaced,
    /// A charge of 0, +2, or -2 on a question item cites nothing.
    #[error("a charge of 0, +2, or -2 cites no source")]
    Uncited,
}

impl RoomContributed {
    /// Refuse a contribution whose text, sources, item, or citation duty is
    /// out of bounds: every charge names the question item it answers.
    ///
    /// # Errors
    ///
    /// Returns the first bound the contribution violates.
    pub fn admit(
        kind: RoomContributionKind,
        item: Option<&StatementItem>,
        text: &str,
        sources: &[RoomSource],
    ) -> Result<(), RoomContributionError> {
        if matches!(kind, RoomContributionKind::Charge(_)) && item.is_none() {
            return Err(RoomContributionError::Unplaced);
        }
        if text.trim().is_empty() {
            return Err(RoomContributionError::Empty);
        }
        if text != text.trim() {
            return Err(RoomContributionError::Untrimmed);
        }
        if text.len() > ROOM_CONTRIBUTION_TEXT_LIMIT {
            return Err(RoomContributionError::TextTooLong);
        }
        if sources.len() > ROOM_CONTRIBUTION_SOURCE_LIMIT {
            return Err(RoomContributionError::TooManySources);
        }
        if sources.iter().enumerate().any(|(index, source)| {
            sources[..index]
                .iter()
                .any(|earlier| source.same_authority(earlier))
        }) {
            return Err(RoomContributionError::DuplicateSource);
        }
        if let RoomContributionKind::Charge(charge) = kind
            && !charge.is_opinion()
            && sources.is_empty()
        {
            return Err(RoomContributionError::Uncited);
        }
        Ok(())
    }

    /// Whether one successful command receipt proves this exact record.
    ///
    /// The receipt binds the anchor heads, every cited source authority, the
    /// submitted body, and the returned publication. A contribution fact
    /// without this correspondence did not cross resident admission.
    #[must_use]
    pub fn matches_command(
        &self,
        actor: &crate::Actor,
        request: &crate::intent::RoomContribute,
        expected: &[crate::Witness],
        answer: &crate::intent::RoomContributeAnswer,
    ) -> bool {
        request.question == self.item && self.matches_receipt(actor, request, expected, answer)
    }

    /// Every correspondence but the question item's.
    fn matches_receipt(
        &self,
        actor: &crate::Actor,
        request: &crate::intent::RoomContribute,
        expected: &[crate::Witness],
        answer: &crate::intent::RoomContributeAnswer,
    ) -> bool {
        let mut tasks = expected.iter().filter_map(|witness| match witness {
            crate::Witness::Task { task, head } => Some((task, *head)),
            _ => None,
        });
        let task_matches = tasks
            .next()
            .is_some_and(|(task, head)| task == &self.room && self.statement == head)
            && tasks.next().is_none();
        let mut workflows = expected.iter().filter_map(|witness| match witness {
            crate::Witness::Workflow { task, .. } => Some(task),
            _ => None,
        });
        let workflow_matches =
            workflows.next().is_some_and(|task| task == &self.room) && workflows.next().is_none();
        request.room.born().is_ok_and(|room| room == self.room)
            && *actor == crate::Actor::Lane(self.lane)
            && task_matches
            && workflow_matches
            && source_witnesses_match(&self.sources, expected)
            && request.kind == self.contribution
            && request.text.trim() == self.text
            && request.sources == self.sources
            && answer.room == request.room
            && answer.publication == self.publication
            && answer.kind == self.contribution
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::TranscriptPosition;
    use crate::rooms::{TextRange, TranscriptHunk};
    use crate::{MessageLocus, TranscriptSourceHead};

    fn transcript(accounted_offset: u64) -> RoomSource {
        RoomSource::Transcript(TranscriptHunk {
            head: TranscriptSourceHead {
                lane: "018f82d4-6f59-7d06-92e3-bce913116302"
                    .parse()
                    .expect("fixture lane"),
                vendor: "omp".parse().expect("fixture vendor"),
                stream: "session".to_owned(),
                source: "018f82d4-6f59-7d06-92e3-bce913116303"
                    .parse()
                    .expect("fixture source"),
                next: TranscriptPosition {
                    member: 1,
                    offset: accounted_offset,
                },
            },
            locus: MessageLocus {
                member: 1,
                offset: 20,
                length: 30,
                event: 2,
            },
            text: TextRange { start: 4, end: 12 },
        })
    }

    /// A handle addresses the item it was read from however the statement is
    /// reordered or extended, a reworded item's old handle refuses as stale,
    /// and a prefix shared by several distinct items refuses as ambiguous.
    #[test]
    fn a_handle_follows_its_item_across_statement_edits() {
        let term = |texts: &[&str]| {
            texts
                .iter()
                .map(|text| (*text).to_owned())
                .collect::<Vec<_>>()
        };
        let read = term(&["Who defines the input?", "Who owns the state?"]);
        let items = StatementItem::each(&read).collect::<Vec<_>>();
        let target = &items[1];
        let handle = target.handle(&items).to_owned();
        assert_eq!(handle.len(), StatementItem::HANDLE_LEN);
        assert_eq!(StatementItem::resolve(&handle, &items), Ok(target));
        assert_eq!(
            StatementItem::resolve(&handle.to_ascii_uppercase(), &items),
            Ok(target),
            "a handle is case-insensitive"
        );

        let inserted = term(&[
            "Who reviews it?",
            "Who defines the input?",
            "  who OWNS the state? ",
        ]);
        let current = StatementItem::each(&inserted).collect::<Vec<_>>();
        let landed = StatementItem::resolve(&handle, &current).expect("the item is current");
        assert_eq!(landed, target, "an insert ahead moves no charge");
        assert_eq!(landed.text_in(&inserted), Some("  who OWNS the state? "));

        let reworded = term(&["Who defines the input?", "Who owns the state now?"]);
        let current = StatementItem::each(&reworded).collect::<Vec<_>>();
        assert_eq!(
            StatementItem::resolve(&handle, &current),
            Err(StatementItemPrefixError::Unknown),
            "a reworded item is a new item"
        );

        let twice = term(&["Who owns the state?", "who owns the state?"]);
        let current = StatementItem::each(&twice).collect::<Vec<_>>();
        assert_eq!(
            StatementItem::resolve(&handle, &current),
            Ok(target),
            "identical items are one identity"
        );
        for malformed in ["", "xyz", "2", &handle[..StatementItem::HANDLE_LEN - 1]] {
            assert_eq!(
                StatementItem::resolve(malformed, &items),
                Err(StatementItemPrefixError::Malformed),
                "{malformed:?} is no handle, whatever item begins with it"
            );
        }

        // Two distinct items sharing their first seven digits: the shared
        // prefix is ambiguous while each rendered handle stays unique.
        let digest = |tail: &str| {
            StatementItem(
                format!("abcdef0{}", tail.repeat(57))
                    .parse()
                    .expect("canonical digest"),
            )
        };
        let current = [digest("1"), digest("2")];
        assert_eq!(
            StatementItem::resolve("abcdef", &current),
            Err(StatementItemPrefixError::Ambiguous)
        );
        for item in &current {
            let handle = item.handle(&current);
            assert_eq!(handle.len(), 8, "the handle grows past the shared digits");
            assert_eq!(StatementItem::resolve(handle, &current), Ok(item));
        }
    }

    #[test]
    fn canonical_contributions_use_exact_source_authority() {
        let question = RoomContributionKind::Question;
        assert_eq!(
            RoomContributed::admit(question, None, " evidence ", &[]),
            Err(RoomContributionError::Untrimmed)
        );
        let first = transcript(50);
        let advanced = transcript(90);
        assert!(first.same_authority(&advanced));
        assert_eq!(first.authority_digest(), advanced.authority_digest());
        assert_eq!(
            RoomContributed::admit(question, None, "Evidence.", &[first, advanced]),
            Err(RoomContributionError::DuplicateSource)
        );
    }

    fn git(lines: Option<(u32, u32)>) -> RoomSource {
        RoomSource::Git(crate::rooms::GitSource {
            commit: "c".repeat(40).parse().expect("fixture commit"),
            path: "spice/src/x.rs".to_owned(),
            blob: "b".repeat(40).parse().expect("fixture blob"),
            lines: lines.map(|(first, last)| {
                crate::rooms::LineRange::new(first, last).expect("fixture range")
            }),
        })
    }

    /// Repository source is one authority per commit, path, and line range;
    /// other lines of the same file are another, and only a forward 1-based
    /// range of a full object id crosses the wire.
    #[test]
    fn repository_source_authority_is_commit_path_and_lines() {
        let question = RoomContributionKind::Question;
        let cited = git(Some((2, 3)));
        assert!(cited.same_authority(&git(Some((2, 3)))));
        assert_eq!(
            RoomContributed::admit(question, None, "Evidence.", &[cited.clone(), cited.clone()]),
            Err(RoomContributionError::DuplicateSource)
        );
        for other in [git(Some((2, 4))), git(None)] {
            assert!(!cited.same_authority(&other));
            assert_ne!(cited.authority_digest(), other.authority_digest());
            assert_eq!(
                RoomContributed::admit(question, None, "Evidence.", &[cited.clone(), other]),
                Ok(())
            );
        }
        let encoded = serde_json::to_value(&cited).expect("source encodes");
        assert_eq!(
            serde_json::from_value::<RoomSource>(encoded.clone()).ok(),
            Some(cited)
        );
        for (field, value) in [
            ("lines", serde_json::json!({"first": 0, "last": 3})),
            ("lines", serde_json::json!({"first": 3, "last": 2})),
            ("commit", serde_json::json!("C".repeat(40))),
            ("blob", serde_json::json!("b".repeat(39))),
        ] {
            let mut forged = encoded.clone();
            forged[field] = value;
            assert!(
                serde_json::from_value::<RoomSource>(forged).is_err(),
                "{field} must refuse"
            );
        }
    }

    /// Only +1 and -1 are anecdotal: a 0, +2, or -2 must cite, and every
    /// charge names its item.
    #[test]
    fn a_question_bound_charge_cites_unless_it_is_an_opinion() {
        let bound = StatementItem::of("Who answers it?");
        let item = Some(&bound);
        let cited = [transcript(50)];
        for charge in RoomCharge::ALL {
            let kind = RoomContributionKind::Charge(charge);
            let uncited = RoomContributed::admit(kind, item, "Text.", &[]);
            if charge.is_opinion() {
                assert_eq!(uncited, Ok(()), "{charge} needs no citation");
            } else {
                assert_eq!(uncited, Err(RoomContributionError::Uncited), "{charge}");
            }
            assert_eq!(RoomContributed::admit(kind, item, "Text.", &cited), Ok(()));
            assert_eq!(
                RoomContributed::admit(kind, None, "Text.", &cited),
                Err(RoomContributionError::Unplaced),
                "a {charge} names its item"
            );
        }
        for question in [None, item] {
            assert_eq!(
                RoomContributed::admit(RoomContributionKind::Question, question, "Why?", &[]),
                Ok(()),
                "a question carries no charge and needs no item or source"
            );
        }
    }

    /// A charge travels as its integer and a question as its word; nothing
    /// else, including the retired label vocabulary, decodes.
    #[test]
    fn a_contribution_kind_has_one_wire_spelling() {
        for (kind, wire) in [
            (RoomContributionKind::charge(2), serde_json::json!(2)),
            (RoomContributionKind::charge(0), serde_json::json!(0)),
            (RoomContributionKind::charge(-1), serde_json::json!(-1)),
            (
                RoomContributionKind::Question,
                serde_json::json!("question"),
            ),
        ] {
            assert_eq!(serde_json::to_value(kind).expect("encodes"), wire);
            assert_eq!(
                serde_json::from_value::<RoomContributionKind>(wire).expect("decodes"),
                kind
            );
        }
        for refused in [
            serde_json::json!(3),
            serde_json::json!(-3),
            serde_json::json!("reason-for"),
            serde_json::json!("evidence"),
            serde_json::json!("+1"),
        ] {
            assert!(serde_json::from_value::<RoomContributionKind>(refused).is_err());
        }
        assert_eq!("+2".parse(), Ok(RoomCharge::ALL[0]));
        assert_eq!("-2".parse(), Ok(RoomCharge::ALL[4]));
        assert_eq!("1".parse(), Ok(RoomCharge::ALL[1]));
        assert!("3".parse::<RoomCharge>().is_err());
        assert!("question".parse::<RoomCharge>().is_err());
    }

    #[test]
    fn a_contribution_answer_carries_the_source_that_cites_it() {
        let stamp = "1kKRRsDR0".parse().expect("room stamp");
        let room = crate::TaskHandle::for_topic(
            &"/rooms/test"
                .parse::<crate::Topic>()
                .expect("fixture topic"),
            &stamp,
        );
        let publication: RoomAuthorityStamp = "1kKRRsDR2".parse().expect("publication");
        let answer = crate::intent::RoomContributeAnswer::new(
            room,
            publication.clone(),
            RoomContributionKind::charge(0),
        )
        .expect("answer");
        let own = RoomSource::Contribution {
            room: stamp,
            publication,
        };
        assert_eq!(answer.source, own);
        let encoded = serde_json::to_value(&answer).expect("answer encodes");
        assert_eq!(
            serde_json::from_value::<RoomSource>(encoded["source"].clone()).expect("source"),
            own
        );
        let mut forged = encoded;
        forged["source"]["publication"] = "1kKRRsDR3".into();
        assert!(serde_json::from_value::<crate::intent::RoomContributeAnswer>(forged).is_err());
    }
}
