use super::*;
/// Score one caller-supplied document against the exact current session
/// profile addressed by `request`, without moving any profile cursor or
/// derived profile state.
pub(in crate::resident) fn score(
    bus: &Bus,
    profiles: &std::sync::Mutex<message_surface::MessageState>,
    request: &wire::intent::RoomScore,
) -> Outcome<wire::intent::CommandAnswer> {
    let planning = match Planning::load(bus) {
        Ok(planning) => planning,
        Err(outcome) => return typed_outcome(outcome),
    };
    let lane = match planning
        .snapshot
        .facts()
        .select_lane(&planning.residency.lanes, request.address)
    {
        Ok(selection) => selection.lane,
        Err(refused) => {
            return score_refusal(
                "room::score-lane-unavailable",
                Some(request.address.to_string()),
                &refused.to_string(),
            );
        }
    };
    let Some(occupant) = planning
        .residency
        .lanes
        .iter()
        .find(|row| row.lane == lane)
        .and_then(|row| row.occupant)
    else {
        return score_refusal(
            "room::score-lane-unavailable",
            Some(request.address.to_string()),
            "the addressed lane is vacant",
        );
    };
    let Some(session) = planning
        .snapshot
        .facts()
        .lane_session(lane, occupant)
        .cloned()
    else {
        return score_refusal(
            "room::score-session-missing",
            Some(request.address.to_string()),
            "the addressed current seat has no declared vendor session",
        );
    };
    let state = match message_surface::lock_state(profiles) {
        Ok(state) => state,
        Err(outcome) => return typed_outcome(outcome),
    };
    let Some(profile) = state.profiles.profile(lane) else {
        return score_refusal(
            "room::score-profile-unavailable",
            Some(request.address.to_string()),
            "the addressed lane has no retained resident profile",
        );
    };
    if !profile.warm_state().is_ready() {
        return score_refusal(
            "room::score-profile-incomplete",
            Some(request.address.to_string()),
            "the addressed lane profile has not completed retained history acquisition",
        );
    }
    let matcher = board::similarity::MatchCache::shared();
    let frequency = matcher.fit(planning.entries.iter().map(board::Entry::statement), || {
        state.profiles.conversation_background(&planning.residency)
    });
    let Some(vector) = frequency.prepare(&TermCounts::from_texts([request.text.as_str()])) else {
        return score_refusal(
            "room::score-text-unweightable",
            None,
            "TEXT has no terms weighted by the current Design-awareness corpus",
        );
    };
    let stale = match stale_profile_evidence(bus, lane, &session, profile) {
        Ok(stale) => stale,
        Err(outcome) => return typed_outcome(outcome),
    };
    let matched = match profile
        .evidence()
        .measure_where(&frequency, &vector, |evidence| {
            stale.binary_search(&evidence.digest()).is_err()
        }) {
        Ok(Some(matched)) => matched,
        Ok(None) => {
            return score_refusal(
                "room::score-profile-empty",
                Some(request.address.to_string()),
                "the addressed ready profile has no weightable current evidence",
            );
        }
        Err(error) => {
            return store_fault(
                "room::score-profile-generation-mismatch",
                &error.to_string(),
            );
        }
    };
    score_answer(
        request,
        lane,
        session,
        planning.horizon,
        &frequency,
        matched,
    )
}
fn score_answer(
    request: &wire::intent::RoomScore,
    lane: CanonicalUuid,
    session: DesignSession,
    position: Option<Seq>,
    frequency: &FrequencyProfile,
    matched: spice::rooms::evidence::EvidenceMatch<'_, EvidenceKey>,
) -> Outcome<wire::intent::CommandAnswer> {
    let Some(score) = wire::rooms::DesignSimilarity::new(matched.score) else {
        return store_fault(
            "room::score-match-invalid",
            "the strongest profile cosine was not finite",
        );
    };
    let generation =
        wire::rooms::DesignFitGeneration::from_bytes(frequency.generation().into_bytes());
    let evidence = durable_evidence(matched.evidence, matched.digest);
    let view = score_view(matched.view);
    let qualifications = [DesignReach::Wide, DesignReach::Narrow, DesignReach::Item]
        .into_iter()
        .map(|reach| {
            let floor = match_score_floor(reach);
            wire::intent::RoomScoreQualification {
                reach,
                floor: wire::rooms::DesignSimilarity::new(floor)
                    .expect("Design-awareness floors are finite"),
                qualifies: matched.score >= floor,
            }
        })
        .collect::<Vec<_>>();
    let text = render_score(ScoreRender {
        address: request.address,
        lane,
        session: &session,
        score: matched.score,
        generation: &generation,
        evidence: &evidence,
        view,
        terms: &matched.terms,
        qualifications: &qualifications,
    });
    let Some(position) = position else {
        return score_refusal(
            "room::score-journal-empty",
            None,
            "start the resident before scoring a session profile",
        );
    };
    Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(position),
        body: wire::intent::CommandAnswer::RoomScore(Box::new(wire::intent::RoomScoreAnswer {
            text,
            address: request.address,
            lane,
            session,
            score,
            generation,
            evidence,
            view,
            terms: matched.terms,
            qualifications,
        })),
    })
}

fn score_refusal(
    reason: &str,
    observed: Option<String>,
    detail: &str,
) -> Outcome<wire::intent::CommandAnswer> {
    Outcome::Refusal(wire::Refusal::bare(
        wire::RefusalDiagnostic::new(reason.to_owned(), None, None, observed)
            .with_detail(detail.to_owned()),
    ))
}

fn score_view(view: spice::rooms::evidence::View) -> wire::intent::RoomScoreView {
    match view {
        spice::rooms::evidence::View::Statement => wire::intent::RoomScoreView::Statement,
        spice::rooms::evidence::View::Completion => wire::intent::RoomScoreView::Completion,
        spice::rooms::evidence::View::Message => wire::intent::RoomScoreView::Message,
        spice::rooms::evidence::View::Passage(index) => {
            wire::intent::RoomScoreView::Passage { index }
        }
    }
}

struct ScoreRender<'a> {
    address: wire::OccupantAddress,
    lane: CanonicalUuid,
    session: &'a DesignSession,
    score: f64,
    generation: &'a wire::rooms::DesignFitGeneration,
    evidence: &'a DesignNoticeEvidence,
    view: wire::intent::RoomScoreView,
    terms: &'a [String],
    qualifications: &'a [wire::intent::RoomScoreQualification],
}

fn render_score(report: ScoreRender<'_>) -> String {
    let ScoreRender {
        address,
        lane,
        session,
        score,
        generation,
        evidence,
        view,
        terms,
        qualifications,
    } = report;
    let evidence = match evidence {
        DesignNoticeEvidence::Task {
            task,
            statement,
            completed,
            support,
        } => format!("task {task} statement {statement} completion {completed} ({support:?})"),
        DesignNoticeEvidence::TaskClaim {
            task,
            statement,
            claim,
            terminal,
            support,
        } => format!(
            "task {task} statement {statement} claim {claim} terminal {terminal:?} ({support:?})"
        ),
        DesignNoticeEvidence::Conversation { source } => format!(
            "conversation source {} member {} bytes {}+{}",
            source
                .locus
                .source
                .map_or_else(|| "unknown".to_owned(), |source| source.to_string()),
            source.locus.member,
            source.locus.offset,
            source.locus.length
        ),
    };
    let view = match view {
        wire::intent::RoomScoreView::Statement => "statement".to_owned(),
        wire::intent::RoomScoreView::Completion => "completion".to_owned(),
        wire::intent::RoomScoreView::Message => "message".to_owned(),
        wire::intent::RoomScoreView::Passage { index } => format!("passage {index}"),
    };
    let terms = if terms.is_empty() {
        "(none)".to_owned()
    } else {
        terms.join(", ")
    };
    let mut text = format!(
        "lane {address} resolves to {lane}\nsession {} {}\nscore {score}\nfit generation \
         {}\nevidence {evidence}\nview {view}\nexplanation terms {terms}\nqualifications only; \
         these do not predict admission:",
        session.vendor,
        session.identity,
        generation.as_str(),
    );
    for qualification in qualifications {
        let clears = if qualification.qualifies {
            "clears"
        } else {
            "does not clear"
        };
        text.push_str(&format!(
            "\n  {}: {clears} {}",
            qualification.reach,
            qualification.floor.get()
        ));
    }
    text
}
