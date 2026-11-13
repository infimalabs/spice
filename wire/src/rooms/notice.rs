//! Resident records of session-targeted requests for open Designs.
//!
//! Each record durably chooses one request. It is not a room publication; it
//! keeps the asked lane listening to the Design's room until that lane leaves,
//! and adds no participation, steering, claim, assignment, or wake.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::{DecisionRef, DesignItemState, StatementItem};
use crate::{CanonicalUuid, Digest, Name, Seq, SequenceCode, TaskStamp};

/// Most bytes of completion validation retained for Design request matching.
pub const COMPLETION_VALIDATION_BYTES: usize = 4 * 1024;

/// Most matched terms one Design awareness record carries.
pub const DESIGN_NOTICE_TERMS: usize = 12;

/// Most matched terms any surface shows for one notice, however many its
/// record keeps.
pub const DESIGN_NOTICE_SHOWN_TERMS: usize = 6;

/// How widely one Design request was cast: across the whole Design, judged
/// from its maturity when selected, or at one quiet question item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesignReach {
    /// An unsteeped Design invites at the lower relevance floor.
    Wide,
    /// A steeped or OOPS Design invites only at the higher floor.
    Narrow,
    /// No whole Design invited the lane, and one quiet question item, read
    /// with its Design's pitch, cleared the item floor; the request asks
    /// only that item.
    Item,
}

impl fmt::Display for DesignReach {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Wide => "wide",
            Self::Narrow => "narrow",
            Self::Item => "item",
        })
    }
}

/// One exact finite cosine or admission floor.
///
/// The canonical decimal spelling round-trips to the original `f64`, including
/// harmless floating-point overshoot from a mathematically unit cosine, while
/// remaining readable in retained facts and JSON surfaces.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DesignSimilarity(String);

impl DesignSimilarity {
    /// Admit one finite value without rounding or clamping it.
    #[must_use]
    pub fn new(value: f64) -> Option<Self> {
        value.is_finite().then(|| Self(value.to_string()))
    }

    /// The exact floating-point value this spelling records.
    #[must_use]
    pub fn get(&self) -> f64 {
        self.0
            .parse()
            .expect("Design similarity constructors admit canonical decimals")
    }
}

impl TryFrom<String> for DesignSimilarity {
    type Error = &'static str;

    fn try_from(spelling: String) -> Result<Self, Self::Error> {
        let value = spelling
            .parse::<f64>()
            .map_err(|_| "Design similarity must be a canonical finite decimal")?;
        let admitted = Self::new(value).ok_or("Design similarity must be finite")?;
        if admitted.0 != spelling {
            return Err("Design similarity must use its canonical decimal spelling");
        }
        Ok(admitted)
    }
}

impl From<DesignSimilarity> for String {
    fn from(value: DesignSimilarity) -> Self {
        value.0
    }
}

/// Stable identity of the statement and conversation frequencies that weighted
/// one Design similarity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DesignFitGeneration(String);

impl DesignFitGeneration {
    /// Name an already-computed 32-byte fit identity.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(blake3::Hash::from_bytes(bytes).to_hex().to_string())
    }

    /// Canonical lower-case hexadecimal spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DesignFitGeneration {
    type Error = &'static str;

    fn try_from(spelling: String) -> Result<Self, Self::Error> {
        let hash = blake3::Hash::from_hex(&spelling)
            .map_err(|_| "Design fit generation must be a 32-byte hexadecimal digest")?;
        let admitted = Self::from_bytes(*hash.as_bytes());
        if admitted.0 != spelling {
            return Err("Design fit generation must use lower-case hexadecimal");
        }
        Ok(admitted)
    }
}

impl From<DesignFitGeneration> for String {
    fn from(value: DesignFitGeneration) -> Self {
        value.0
    }
}

/// The exact match that admitted one Design notice.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesignNoticeMatch {
    /// Reach whose floor admitted the match.
    reach: DesignReach,
    /// Best-view cosine selected from the retained profile.
    score: DesignSimilarity,
    /// Candidate-owned floor in force when selected.
    floor: DesignSimilarity,
    /// Frequency fit that weighted both selected vectors.
    generation: DesignFitGeneration,
}

impl DesignNoticeMatch {
    /// Build an admitted match, refusing a score below its recorded floor.
    #[must_use]
    pub fn new(reach: DesignReach, score: f64, floor: f64, generation: [u8; 32]) -> Option<Self> {
        let score = DesignSimilarity::new(score)?;
        let floor = DesignSimilarity::new(floor)?;
        (score.get() >= floor.get()).then(|| Self {
            reach,
            score,
            floor,
            generation: DesignFitGeneration::from_bytes(generation),
        })
    }

    /// Reach whose floor admitted the match.
    #[must_use]
    pub const fn reach(&self) -> DesignReach {
        self.reach
    }

    /// Best-view cosine selected from the retained profile.
    #[must_use]
    pub fn score(&self) -> f64 {
        self.score.get()
    }

    /// Candidate-owned floor in force when selected.
    #[must_use]
    pub fn floor(&self) -> f64 {
        self.floor.get()
    }

    /// Frequency fit that weighted both selected vectors.
    #[must_use]
    pub fn generation(&self) -> &DesignFitGeneration {
        &self.generation
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DesignNoticeMatchWire {
    reach: DesignReach,
    score: DesignSimilarity,
    floor: DesignSimilarity,
    generation: DesignFitGeneration,
}

impl<'de> Deserialize<'de> for DesignNoticeMatch {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = DesignNoticeMatchWire::deserialize(deserializer)?;
        if wire.score.get() < wire.floor.get() {
            return Err(serde::de::Error::custom(
                "Design notice score must clear its recorded floor",
            ));
        }
        Ok(Self {
            reach: wire.reach,
            score: wire.score,
            floor: wire.floor,
            generation: wire.generation,
        })
    }
}

/// Most characters of a Design request's supporting-work excerpt.
pub const DESIGN_EXCERPT_CHARS: usize = 240;

/// Supporting-work `text` as one line of at most [`DESIGN_EXCERPT_CHARS`]
/// characters. An admitted excerpt is non-empty and its own excerpt.
#[must_use]
pub fn design_excerpt(text: &str) -> String {
    one_line(text, DESIGN_EXCERPT_CHARS)
}

/// `text` with whitespace runs collapsed, cut to `chars` characters with an
/// ellipsis when longer.
#[must_use]
pub fn one_line(text: &str, chars: usize) -> String {
    let mut line = String::with_capacity(text.len().min(chars));
    for word in text.split_whitespace() {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if line.chars().count() <= chars {
        return line;
    }
    let mut cut = line
        .chars()
        .take(chars.saturating_sub(1))
        .collect::<String>();
    cut.push('…');
    cut
}

/// What the task work supporting a Design request did, named from the phase
/// its claim held so the request does not overstate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DesignSupport {
    /// An implementing phase changed the product.
    Implemented,
    /// A plan or Design phase diagnosed or planned the work.
    Planned,
    /// A review phase inspected another lane's work.
    Reviewed,
}

impl DesignSupport {
    /// The kind of the supporting work, from the phase its claim held.
    #[must_use]
    pub fn for_phase(phase: &crate::Phase) -> Self {
        match phase.as_str() {
            "review" => Self::Reviewed,
            "plan" | "design" => Self::Planned,
            _ => Self::Implemented,
        }
    }

    /// The past-tense clause a request uses for this work.
    #[must_use]
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Implemented => "implemented",
            Self::Planned => "diagnosed or planned",
            Self::Reviewed => "reviewed",
        }
    }
}

/// The matched terms a surface shows: each distinct term once, in record
/// order, at most [`DESIGN_NOTICE_SHOWN_TERMS`].
#[must_use]
pub fn shown_notice_terms(terms: &[String]) -> Vec<&str> {
    let mut shown: Vec<&str> = Vec::with_capacity(DESIGN_NOTICE_SHOWN_TERMS);
    for term in terms {
        if shown.len() == DESIGN_NOTICE_SHOWN_TERMS {
            break;
        }
        if !shown.contains(&term.as_str()) {
            shown.push(term);
        }
    }
    shown
}

/// One exact continuing vendor session invited to an open Design.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignSession {
    /// Vendor whose declaration admitted the identity.
    pub vendor: Name,
    /// Exact resumable vendor session identity.
    pub identity: String,
}

/// Exact settled conversation source used by an opportunity or as support.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignConversationEvidence {
    /// Durable transcript record containing the assistant-authored prose.
    pub locus: crate::body::TranscriptLocus,
    /// Identity of the exact cleaned prose retained by the session profile.
    pub digest: Digest,
}

/// Event at which one session profile was evaluated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DesignNoticeOpportunity {
    /// A successful task completion settled.
    TaskCompletion { task: TaskStamp, completed: Seq },
    /// A retained task selection under this exact claim. `terminal` names the
    /// terminal task operation when one preceded the selection record; its
    /// absence does not pretend that the task completed.
    TaskClaim {
        task: TaskStamp,
        claim: SequenceCode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<Seq>,
    },
    /// One assistant-authored conversation message settled.
    Conversation { source: DesignConversationEvidence },
    /// The selected Design was filed or revised.
    DesignStatement,
    /// A bounded profile warm finished, optionally through its newest journal
    /// fact when the profile has task history.
    ProfileReady { horizon: Option<Seq> },
    /// The same continuing session resumed at this declaration.
    SessionResumed { declaration: Seq },
}

/// Exact retained card that supported one Design match.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DesignNoticeEvidence {
    /// Successful task evidence at the statement observed with its completion,
    /// classified by the phase the completed claim held.
    Task {
        task: TaskStamp,
        statement: Seq,
        completed: Seq,
        support: DesignSupport,
    },
    /// Task evidence retained under an exact claim, classified by the phase
    /// that claim held. `terminal` is present only when a terminal task
    /// operation preceded the selection record; the claim remains the witness
    /// when no completion did.
    TaskClaim {
        task: TaskStamp,
        statement: Seq,
        claim: SequenceCode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal: Option<Seq>,
        support: DesignSupport,
    },
    /// Assistant-authored conversation evidence.
    Conversation { source: DesignConversationEvidence },
}

/// The independent retained card that qualified one offered question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignQuestionSupport {
    /// Immutable support; consumed only by a question-specific offer.
    pub evidence: DesignNoticeEvidence,
    /// The card's bounded authored excerpt.
    pub excerpt: String,
    /// This question's own score, floor and frequency generation.
    pub matching: DesignNoticeMatch,
}

/// One question item a Design request asks a lane about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignAsk {
    /// The item's text identity, which keys its charge series.
    pub item: StatementItem,
    /// Where the item stood when asked. A contested item asks a lane already
    /// in its series for a strength-2 re-charge and any other lane for its
    /// charge.
    pub state: DesignItemState,
    /// Per-question qualification, absent on an unselected item.
    #[serde(deserialize_with = "crate::optional::required")]
    pub support: Option<DesignQuestionSupport>,
}

/// One lane was invited to contribute to an open Design at one statement
/// revision. The lane owes the request under any occupant until it answers.
///
/// The opportunity and supporting evidence are independent: a new completion
/// can select a Design because of an older task or conversation card. The
/// record is request state only. It grants no participation, steering, claim,
/// assignment, permission to work, or wake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignNoticed {
    /// Open Design whose room accepts the answer.
    pub design: TaskStamp,
    /// Exact Design statement head evaluated for this request.
    pub statement: Seq,
    /// Logical lane that owes the request.
    pub lane: CanonicalUuid,
    /// Occupant carrying the continuing session when selected.
    pub occupant: CanonicalUuid,
    /// Continuing vendor session whose evidence selected the Design.
    pub session: DesignSession,
    /// Newest compaction boundary of this session before selection. Admission
    /// refuses if it moves before recording; it does not reset request history.
    pub compaction: Option<Seq>,
    /// Event that gave the resident an opportunity to evaluate the profile.
    pub opportunity: DesignNoticeOpportunity,
    /// Retained card whose view selected the Design.
    pub evidence: DesignNoticeEvidence,
    /// The card's authored text as a [`design_excerpt`]: the completion
    /// validation, the cleaned assistant prose, or the task title when the
    /// supporting claim reached no terminal operation.
    pub excerpt: String,
    /// Matched terms in ascending order.
    pub terms: Vec<String>,
    /// The exact match selection computed for this request.
    pub admission: DesignNoticeMatch,
    /// Every independently qualified question, most needed first, then by its
    /// own score and statement order. Empty asks about a questionless whole
    /// Design.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asks: Vec<DesignAsk>,
}

/// The resident handed an active Design's holder the operator's newest return
/// decision as its next input.
///
/// One record consumes that decision: no later claimant is told it again, and
/// only a newer return becomes input. It authorizes and mutates nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignReturnDelivered {
    /// The return decision delivered; its controller is the Design.
    pub decision: DecisionRef,
    /// Logical lane holding the Design.
    pub lane: CanonicalUuid,
    /// Occupant told.
    pub occupant: CanonicalUuid,
    /// Fact that won the claim the delivery served.
    pub claim: SequenceCode,
}

#[cfg(test)]
mod tests {
    use super::{DESIGN_NOTICE_SHOWN_TERMS, DesignNoticeMatch, DesignReach, shown_notice_terms};

    #[test]
    fn shown_terms_are_distinct_and_capped_in_record_order() {
        let terms = [
            "beta", "alpha", "beta", "gamma", "alpha", "delta", "eps", "zeta", "eta",
        ]
        .map(str::to_owned);
        assert_eq!(
            shown_notice_terms(&terms),
            ["beta", "alpha", "gamma", "delta", "eps", "zeta"]
        );
        let many = (0..20).map(|n| format!("term{n:02}")).collect::<Vec<_>>();
        assert_eq!(shown_notice_terms(&many).len(), DESIGN_NOTICE_SHOWN_TERMS);
        assert!(shown_notice_terms(&[]).is_empty());
    }

    #[test]
    fn notice_match_round_trips_exact_values_and_refuses_a_missed_floor() {
        let score = f64::from_bits(0.3_f64.to_bits() + 1);
        let floor = 0.16;
        let generation = [0x5a; 32];
        let matching = DesignNoticeMatch::new(DesignReach::Narrow, score, floor, generation)
            .expect("score clears floor");
        let encoded = serde_json::to_value(&matching).expect("match serializes");
        let decoded: DesignNoticeMatch =
            serde_json::from_value(encoded).expect("match deserializes");
        assert_eq!(decoded.reach(), DesignReach::Narrow);
        assert_eq!(decoded.score().to_bits(), score.to_bits());
        assert_eq!(decoded.floor().to_bits(), floor.to_bits());
        assert_eq!(
            decoded.generation(),
            &super::DesignFitGeneration::from_bytes(generation)
        );
        assert!(
            DesignNoticeMatch::new(DesignReach::Narrow, floor.next_down(), floor, generation)
                .is_none()
        );
        let invalid = serde_json::json!({
            "reach": "narrow",
            "score": "0.1",
            "floor": "0.2",
            "generation": super::DesignFitGeneration::from_bytes(generation),
        });
        assert!(serde_json::from_value::<DesignNoticeMatch>(invalid).is_err());
    }
}
