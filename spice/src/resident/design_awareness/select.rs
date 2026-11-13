//! The one request a profile opportunity selects for its lane.
//!
//! Selection ranks questions by the feedback their Design still needs before
//! it forms a request. Two kinds of candidate compete. A whole Design the
//! lane was not yet asked about at its statement revision qualifies when its
//! whole statement clears its invitation floor; it stands for the neediest
//! item it would list. A current question of any open Design that the lane
//! never answered qualifies when its text, read with the Design's problem
//! sentence, clears the item floor against a retained card the lane has not
//! yet consumed for that question at that revision. Need decides
//! first: an unheard question, then one lacking agreement, then a concurred
//! one, and within one standing fewer independent contributing lanes. At
//! equal need a whole Design precedes a question, cosine ranks candidates of
//! one kind, and Design then statement order breaks the remaining ties. Every
//! card counted is independent of the Design it would invite.

use std::borrow::Cow;
use std::cmp::Ordering;

use wire::rooms::{DesignItemState, DesignItems};

use super::*;

/// How much feedback one question still needs. The derived order puts the
/// most needed first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Need {
    standing: Standing,
    /// Distinct lanes whose latest charge the item's series retains.
    lanes: usize,
}

/// Where one question stands, most needed first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Standing {
    /// No charge has weighed it.
    Unheard,
    /// Charges weighed it short of agreement: unopposed or contested.
    Unagreed,
    /// Its latest run concurs.
    Concurred,
}

impl Need {
    const UNHEARD: Self = Self {
        standing: Standing::Unheard,
        lanes: 0,
    };

    fn of(items: &DesignItems, ask: &DesignAsk) -> Self {
        let standing = match ask.state {
            DesignItemState::Quiet => Standing::Unheard,
            DesignItemState::UnopposedFor
            | DesignItemState::UnopposedAgainst
            | DesignItemState::Contested => Standing::Unagreed,
            DesignItemState::Concurred => Standing::Concurred,
        };
        Self {
            standing,
            lanes: items.series(&ask.item).map_or(0, DesignItemSeries::lanes),
        }
    }
}

/// Rank by need, then by higher cosine.
fn rank(need: Need, score: f64) -> impl Ord {
    (need, std::cmp::Reverse(Cosine(score)))
}

/// A finite cosine under total order.
#[derive(PartialEq)]
struct Cosine(f64);

impl Eq for Cosine {}

impl PartialOrd for Cosine {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Cosine {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// One candidate request, before its evidence is chosen.
enum Candidate<'e> {
    /// A Design asked whole, listing `asks`.
    Whole {
        prepared: &'e (&'e board::Entry, Arc<Prepared>),
        asks: Vec<DesignAsk>,
    },
    /// All independently qualified questions of `design`, most needed first.
    Question {
        design: &'e board::Entry,
        asks: Vec<DesignAsk>,
    },
}

/// One ranked candidate.
struct Ranked<'e> {
    candidate: Candidate<'e>,
    need: Need,
    score: f64,
}

impl Ranked<'_> {
    /// Need first; at equal need a whole Design keeps its precedence over a
    /// question, and cosine ranks candidates of one kind.
    fn outranks(&self, other: &Self) -> bool {
        let key = |ranked: &Self| {
            (
                ranked.need,
                matches!(ranked.candidate, Candidate::Question { .. }),
                std::cmp::Reverse(Cosine(ranked.score)),
            )
        };
        key(self) < key(other)
    }

    /// Keep `self` in `best` unless an earlier candidate ranks at least as
    /// high, so equal rank keeps the stable candidate order.
    fn offer(self, best: &mut Option<Self>) {
        if best.as_ref().is_none_or(|known| self.outranks(known)) {
            *best = Some(self);
        }
    }
}

/// The request `kind` admits for the lane `asked`, if any. While the lane
/// owes a request for `owed`, only a whole Design strictly outranking the
/// owed Design's current whole-statement match may replace it; an owed
/// Design that no longer clears its floor bars nothing.
pub(super) fn select(
    scoring: &Scoring<'_>,
    profile: &mut spice::rooms::profile::LaneProfile,
    asked: CanonicalUuid,
    kind: &ProfileOpportunityKind,
    owed: Option<&TaskStamp>,
) -> CommandResult<Option<Selection>> {
    let mut best: Option<Ranked<'_>> = None;
    for prepared in scoring.prepared {
        if !opportunity_design_matches(kind, prepared.0) {
            continue;
        }
        if let Some(found) = whole(scoring, profile, asked, prepared)? {
            // Its qualifying questions contain every possible item candidate
            // at this reach, so none can outrank this Design's whole request.
            found.offer(&mut best);
        } else if owed.is_none()
            && let Some(found) = questions(scoring, profile, asked, prepared.0)?
        {
            found.offer(&mut best);
        }
    }
    let bar = match owed.and_then(|owed| {
        scoring
            .prepared
            .iter()
            .find(|(design, _)| design.born() == owed)
    }) {
        Some(prepared) => owed_bar(scoring, profile, asked, prepared)?,
        None => None,
    };
    let Some(best) = best else {
        return Ok(None);
    };
    if bar.as_ref().is_some_and(|bar| !best.outranks(bar)) {
        return Ok(None);
    }
    match best.candidate {
        Candidate::Whole { prepared, asks } => whole_request(scoring, profile, prepared, asks),
        Candidate::Question { design, asks } => {
            question_request(scoring, profile, asked, design, asks)
        }
    }
    .map(Some)
}

/// The owed Design's current standing as a bar, when its whole statement
/// still clears its floor.
fn owed_bar<'e>(
    scoring: &Scoring<'_>,
    profile: &mut spice::rooms::profile::LaneProfile,
    asked: CanonicalUuid,
    prepared: &'e (&'e board::Entry, Arc<Prepared>),
) -> CommandResult<Option<Ranked<'e>>> {
    let (design, vector) = prepared;
    let Some(score) = whole_score(scoring, profile, design, vector)? else {
        return Ok(None);
    };
    let items = design_items(scoring.facts, design);
    let Some((_, notice)) = scoring.facts.latest_design_notice(asked) else {
        return Ok(None);
    };
    let mut asks = items.current(&design.statement().term);
    asks.retain(|ask| notice.asks.iter().any(|offered| offered.item == ask.item));
    Ok(Some(Ranked {
        need: whole_need(&items, &asks),
        score,
        candidate: Candidate::Whole { prepared, asks },
    }))
}

/// `design`'s whole-statement candidate, when the lane was not yet asked
/// whole at its statement revision and an independent card clears its
/// invitation floor.
fn whole<'e>(
    scoring: &Scoring<'_>,
    profile: &mut spice::rooms::profile::LaneProfile,
    asked: CanonicalUuid,
    prepared: &'e (&'e board::Entry, Arc<Prepared>),
) -> CommandResult<Option<Ranked<'e>>> {
    let (design, vector) = prepared;
    let revision = design.statement().revision;
    if scoring
        .facts
        .design_was_asked_whole(asked, design.born(), revision)
        || design_items(scoring.facts, design).all_concurred(&design.statement().term)
    {
        return Ok(None);
    }
    let Some(score) = whole_score(scoring, profile, design, vector)? else {
        return Ok(None);
    };
    let asks = qualified_asks(
        scoring,
        profile,
        asked,
        design,
        admitting_reach(scoring.facts, design),
    )?;
    // Questions were filtered individually; a questionless Design is retired
    // by any publication from this lane at its statement revision.
    if asks.is_empty()
        && (!design.statement().term.is_empty()
            || scoring
                .facts
                .design_room_answered_after(asked, design.born(), revision))
    {
        return Ok(None);
    }
    let items = design_items(scoring.facts, design);
    Ok(Some(Ranked {
        need: whole_need(&items, &asks),
        score,
        candidate: Candidate::Whole { prepared, asks },
    }))
}

/// The best independent cosine of `design`'s whole statement, when it clears
/// the Design's invitation floor.
fn whole_score(
    scoring: &Scoring<'_>,
    profile: &mut spice::rooms::profile::LaneProfile,
    design: &board::Entry,
    vector: &Prepared,
) -> CommandResult<Option<f64>> {
    let born = design.born();
    let score = profile
        .evidence_mut()
        .best_score_where(scoring.frequency, vector, |card| {
            card.key().independent_of(born)
        })
        .map_err(generation_mismatch)?;
    Ok(score.filter(|score| *score >= invitation_floor(scoring.facts, design)))
}

/// Only listed questions lend their need to a request. A questionless Design
/// has no question history to weigh.
fn whole_need(items: &DesignItems, asks: &[DesignAsk]) -> Need {
    asks.iter()
        .map(|ask| Need::of(items, ask))
        .min()
        .unwrap_or(Need::UNHEARD)
}

/// `design`'s item series, empty when no item was charged.
fn design_items<'f>(facts: &'f bus::TaskFacts, design: &board::Entry) -> Cow<'f, DesignItems> {
    facts
        .design_items(design.born())
        .map_or_else(|| Cow::Owned(DesignItems::default()), Cow::Borrowed)
}

/// The weighted text of one question: its item read with the Design's problem
/// sentence.
fn question_vector(
    frequency: &FrequencyProfile,
    design: &board::Entry,
    ask: &DesignAsk,
) -> Option<Prepared> {
    let statement = design.statement();
    let text = ask.item.text_in(&statement.term)?;
    frequency.prepare(&TermCounts::from_texts([text, statement.pitch.as_str()]))
}

/// The durable identity of every retained card, keyed by its digest.
fn card_identities(
    profile: &spice::rooms::profile::LaneProfile,
) -> BTreeMap<ProfileDigest, DesignNoticeEvidence> {
    profile
        .evidence()
        .evidence()
        .map(|card| (card.digest(), durable_evidence(card.key(), card.digest())))
        .collect()
}

/// Whether `card` may support offering `ask` of `design` to the lane: it is
/// independent of the Design and the lane has not consumed it for the
/// question at the Design's statement revision.
fn offerable(
    facts: &bus::TaskFacts,
    asked: CanonicalUuid,
    identities: &BTreeMap<ProfileDigest, DesignNoticeEvidence>,
    design: &board::Entry,
    ask: &DesignAsk,
    card: &spice::rooms::evidence::Evidence<EvidenceKey>,
) -> bool {
    card.key().independent_of(design.born())
        && identities.get(&card.digest()).is_some_and(|identity| {
            !facts.design_offer_consumed(
                asked,
                design.born(),
                design.statement().revision,
                &ask.item,
                identity,
            )
        })
}

/// Every question independently qualified by an unconsumed retained card.
/// Both whole and question requests use this same inclusion and need order.
fn qualified_asks(
    scoring: &Scoring<'_>,
    profile: &mut spice::rooms::profile::LaneProfile,
    asked: CanonicalUuid,
    design: &board::Entry,
    reach: DesignReach,
) -> CommandResult<Vec<DesignAsk>> {
    let identities = card_identities(profile);
    let items = design_items(scoring.facts, design);
    let floor = request_floor(design, reach);
    let mut found = Vec::new();
    for mut ask in items.current(&design.statement().term) {
        if scoring
            .facts
            .design_item_answered(asked, design.born(), &ask.item)
        {
            continue;
        }
        let Some(vector) = question_vector(scoring.frequency, design, &ask) else {
            continue;
        };
        let Some(matched) = profile
            .evidence_mut()
            .select_where(
                scoring.frequency,
                [DesignCandidate::new(design.born(), &(), &vector, floor)],
                |card, _| offerable(scoring.facts, asked, &identities, design, &ask, card),
            )
            .map_err(generation_mismatch)?
        else {
            continue;
        };
        ask.support = Some(wire::rooms::DesignQuestionSupport {
            evidence: durable_evidence(matched.evidence, matched.digest),
            excerpt: matched.excerpt.to_owned(),
            matching: DesignNoticeMatch::new(
                reach,
                matched.score,
                floor,
                scoring.frequency.generation().into_bytes(),
            )
            .ok_or_else(vanished)?,
        });
        found.push(ask);
    }
    found.sort_by(|ask, other| {
        rank(
            Need::of(&items, ask),
            ask.support.as_ref().expect("qualified").matching.score(),
        )
        .cmp(&rank(
            Need::of(&items, other),
            other.support.as_ref().expect("qualified").matching.score(),
        ))
    });
    Ok(found)
}

/// Every independently qualifying question competes for its Design.
fn questions<'e>(
    scoring: &Scoring<'_>,
    profile: &mut spice::rooms::profile::LaneProfile,
    asked: CanonicalUuid,
    design: &'e board::Entry,
) -> CommandResult<Option<Ranked<'e>>> {
    let items = design_items(scoring.facts, design);
    let asks = qualified_asks(scoring, profile, asked, design, DesignReach::Item)?;
    let Some(lead) = asks.first() else {
        return Ok(None);
    };
    Ok(Some(Ranked {
        need: Need::of(&items, lead),
        score: lead.support.as_ref().expect("qualified").matching.score(),
        candidate: Candidate::Question { design, asks },
    }))
}

/// Form a whole request using only its independently qualified questions.
fn whole_request(
    scoring: &Scoring<'_>,
    profile: &mut spice::rooms::profile::LaneProfile,
    prepared: &(&board::Entry, Arc<Prepared>),
    asks: Vec<DesignAsk>,
) -> CommandResult<Selection> {
    let design = prepared.0;
    let reach = admitting_reach(scoring.facts, design);
    let matched = profile
        .evidence_mut()
        .select_where(
            scoring.frequency,
            [candidate(scoring.facts, prepared)],
            |card, design| card.key().independent_of(design),
        )
        .map_err(generation_mismatch)?
        .ok_or_else(vanished)?;
    selected(
        design,
        reach,
        invitation_floor(scoring.facts, design),
        scoring.frequency,
        matched,
        asks,
    )
}

/// Form a question request from every independently qualified card, not just
/// the leading question's card.
fn question_request(
    scoring: &Scoring<'_>,
    profile: &mut spice::rooms::profile::LaneProfile,
    asked: CanonicalUuid,
    design: &board::Entry,
    asks: Vec<DesignAsk>,
) -> CommandResult<Selection> {
    let lead = asks.first().ok_or_else(vanished)?;
    let identities = card_identities(profile);
    let vector = question_vector(scoring.frequency, design, lead).ok_or_else(vanished)?;
    let matched = profile
        .evidence_mut()
        .select_where(
            scoring.frequency,
            [DesignCandidate::new(
                design.born(),
                design,
                &vector,
                request_floor(design, DesignReach::Item),
            )],
            |card, _| offerable(scoring.facts, asked, &identities, design, lead, card),
        )
        .map_err(generation_mismatch)?
        .ok_or_else(vanished)?;
    selected(
        design,
        DesignReach::Item,
        request_floor(design, DesignReach::Item),
        scoring.frequency,
        matched,
        asks,
    )
}

/// The selected candidate no longer matched when its request was formed
/// from the same profile and fit, which one planning pass cannot cause.
fn vanished() -> Outcome<()> {
    store_fault(
        "room::awareness-selection-vanished",
        "the selected candidate no longer matched its own evidence",
    )
}

/// One prepared open Design weighted under its invitation floor.
fn candidate<'a>(
    facts: &bus::TaskFacts,
    (design, vector): &'a (&'a board::Entry, Arc<Prepared>),
) -> DesignCandidate<'a, board::Entry> {
    DesignCandidate::new(
        design.born(),
        *design,
        vector,
        invitation_floor(facts, design),
    )
}

/// The selection `matched` makes for `design` at `reach` and its state-owned
/// `floor`, asking `asks`.
fn selected<T>(
    design: &board::Entry,
    reach: DesignReach,
    floor: f64,
    frequency: &FrequencyProfile,
    matched: ProfileMatch<'_, '_, EvidenceKey, T>,
    asks: Vec<DesignAsk>,
) -> CommandResult<Selection> {
    let matching = DesignNoticeMatch::new(
        reach,
        matched.score,
        floor,
        frequency.generation().into_bytes(),
    )
    .ok_or_else(|| {
        store_fault(
            "room::awareness-match-invalid",
            "the selected finite cosine did not clear its recorded floor",
        )
    })?;
    Ok(Selection {
        design: design.born().clone(),
        statement: design.statement().revision,
        evidence: durable_evidence(matched.evidence, matched.digest),
        excerpt: matched.excerpt.to_owned(),
        terms: matched.terms,
        asks,
        matching,
    })
}
