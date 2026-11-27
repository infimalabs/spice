use std::fmt::Write as _;

use wire::{StatementField, TasteHold, TasteMatch};

use super::*;

/// One read of a statement against the complete current taste policy.
#[derive(Clone, Debug)]
pub(super) struct TasteReview {
    matches: Vec<TasteMatch>,
}

impl TasteReview {
    pub(super) fn required(&self) -> bool {
        !self.matches.is_empty()
    }

    pub(super) fn refusal(&self, observed: String, remedy: &str) -> Outcome<CommandAnswer> {
        taste_matches_refusal(observed, &self.matches, remedy)
    }
}

/// Refuse an authored statement while naming every taste match needed to fix it.
pub(super) fn taste_matches_refusal(
    observed: String,
    matches: &[TasteMatch],
    remedy: &str,
) -> Outcome<CommandAnswer> {
    let mut detail = String::with_capacity(remedy.len() + matches.len().saturating_mul(96));
    detail.push_str(remedy);
    for found in matches {
        let field = match found.field {
            StatementField::Title => "title",
            StatementField::Pitch => "pitch",
            StatementField::Description => "description",
            StatementField::Term => "term",
        };
        match &found.finding {
            Some(finding) => write!(
                detail,
                "\n- taste {} matched word {:?} in {field} line {}, column {}: {}",
                found.taste, finding.trigger, found.line, finding.column, finding.message,
            )
            .expect("writing a taste refusal into a String cannot fail"),
            None => write!(
                detail,
                "\n- taste {} matched {field} line {}; match details are unavailable",
                found.taste, found.line,
            )
            .expect("writing a taste refusal into a String cannot fail"),
        }
    }
    refusal("task::taste-review-required", Some(observed), &detail)
}

pub(super) fn validate_term(term: &[String]) -> Result<(), Outcome<CommandAnswer>> {
    if term.iter().any(|criterion| criterion.trim().is_empty()) {
        return Err(refusal(
            "task::term-empty",
            None,
            "remove blank term values or replace each with a substantive completion criterion",
        ));
    }
    Ok(())
}

/// Characters of refused statement text a refusal echoes back.
const REFUSED_EXCERPT_CHARS: usize = 80;

/// Admit an authored pitch against the three mechanical rules.
pub(super) fn validate_pitch(pitch: &str) -> Result<(), Outcome<CommandAnswer>> {
    wire::pitch::admit(pitch).map_err(|refused| {
        let observed = match refused {
            wire::pitch::PitchRefusal::Empty => None,
            wire::pitch::PitchRefusal::Multiline | wire::pitch::PitchRefusal::TooLong { .. } => {
                Some(wire::rooms::one_line(pitch, REFUSED_EXCERPT_CHARS))
            }
        };
        refusal(refused.reason(), observed, refused.remedy())
    })
}

/// Admit an authored title against its length bound.
pub(super) fn validate_title(title: &str) -> Result<(), Outcome<CommandAnswer>> {
    wire::title::admit(title).map_err(|refused| {
        refusal(
            wire::title::TitleTooLong::REASON,
            Some(wire::rooms::one_line(title, REFUSED_EXCERPT_CHARS)),
            refused.remedy(),
        )
    })
}

/// Record or update a statement's taste hold from one policy read.
///
/// A statement not yet held opens a fresh hold; a standing hold records the
/// new matches and one more recheck. The hold is a statement marker only: it
/// never touches the task's flow, phase, or claim.
pub(super) fn keep_taste_hold(taste: &mut Option<TasteHold>, review: &TasteReview) {
    match taste {
        Some(hold) => {
            hold.matches.clone_from(&review.matches);
            hold.rechecks = hold.rechecks.saturating_add(1);
        }
        None => {
            *taste = Some(TasteHold {
                matches: review.matches.clone(),
                rechecks: 0,
                opened: None,
            });
        }
    }
}

/// The statement a policy-reading write publishes at `fact`.
///
/// A hold this write opened records `fact` as its opening, so the lane told
/// about the hold is the one whose wording it found, never a later writer that
/// only carried it through.
pub(super) fn published_statement(mut statement: TaskHead, fact: wire::Seq) -> TaskHead {
    if let Some(hold) = &mut statement.taste {
        hold.record_opening(fact);
    }
    statement
}

/// Carry a statement's taste hold through one policy read of its rewrite.
///
/// A match records or updates the hold; a clear read clears it. A statement
/// that held before needs repository policy authority to clear, so an
/// unavailable policy refuses rather than silently dropping the hold.
pub(super) fn resync_taste_hold(
    taste: &mut Option<TasteHold>,
    review: Option<&TasteReview>,
) -> Result<(), Outcome<CommandAnswer>> {
    match review {
        Some(review) if review.required() => keep_taste_hold(taste, review),
        Some(_) => *taste = None,
        None if taste.is_some() => {
            return Err(environment_fault(
                "task::taste-policy-unavailable",
                "an existing taste review cannot clear without repository policy authority",
            ));
        }
        None => {}
    }
    Ok(())
}

/// The standing hold's refusal of a phase completion: every match, how many
/// rereads the statement has already survived, and the rewording remedy.
pub(super) fn taste_hold_refusal(entry: &Entry, hold: &TasteHold) -> Outcome<CommandAnswer> {
    // A lane that rewords and refuses again can only tell progress from
    // stasis if the refusal says how many reads the statement has already
    // survived.
    let standing = match hold.rechecks {
        0 => "the statement has not been rechecked since the hold began".to_owned(),
        1 => "1 recheck since the hold began still matched".to_owned(),
        count => format!("{count} rechecks since the hold began still matched"),
    };
    taste_matches_refusal(
        entry.handle().to_string(),
        &hold.matches,
        &format!(
            "{standing}; change what the statement says in a statement field (--title, --pitch, --description, or --term), then spice task modify {} --title \"...\" lets the complete current taste policy recheck it",
            entry.handle(),
        ),
    )
}

/// Read one statement against the repository taste policy and the built-in
/// pitch shape, which applies even where no repository policy exists.
pub(super) fn task_taste_review(
    repository: Option<Host<'_>>,
    snapshot: &ValidatedSnapshot,
    actor: &Actor,
    title: &str,
    pitch: &str,
    description: Option<&str>,
    term: &[String],
) -> Result<Option<TasteReview>, Outcome<CommandAnswer>> {
    let mut matches = pitch_matches(pitch);
    let Some(repository) = repository else {
        return Ok((!matches.is_empty()).then_some(TasteReview { matches }));
    };
    let loaded = spice::config::load_repository(&repository, &[])
        .map_err(|error| environment_fault("task::taste-policy-unreadable", error.to_string()))?;
    let bag = loaded.root().taste().bag();
    let seat = match actor {
        Actor::Lane(lane) => snapshot
            .answer
            .residency
            .body
            .lanes
            .iter()
            .find(|seat| seat.lane == *lane),
        Actor::Spice | Actor::Principal(_) => None,
    };
    let vendor = seat.map(|seat| seat.vendor.as_str());
    let model = seat
        .and_then(|seat| seat.session.as_ref())
        .map(|session| session.launch.model.as_str());
    let role = if matches!(actor, Actor::Principal(_)) {
        listen::ProseRole::Operator
    } else {
        listen::ProseRole::Assistant
    };
    let fields = std::iter::once((StatementField::Title, title))
        .chain((!pitch.is_empty()).then_some((StatementField::Pitch, pitch)))
        .chain(description.map(|text| (StatementField::Description, text)))
        .chain(
            term.iter()
                .map(|text| (StatementField::Term, text.as_str())),
        )
        .collect::<Vec<_>>();
    // Task prose has no repository path, extension, or gate phase. A taste
    // constrained to one of those axes is inapplicable here, not unresolved.
    let applicable = bag
        .tastes()
        .iter()
        .filter(|taste| {
            let scopes = taste.scopes();
            scopes.paths().is_empty()
                && scopes.extensions().is_empty()
                && scopes.phases().is_empty()
                && (scopes.vendors().is_empty() || vendor.is_some())
                && (scopes.models().is_empty() || model.is_some())
        })
        .cloned()
        .collect::<Vec<_>>();
    let applicable = taste::Bag::new(applicable).map_err(|error| {
        protocol_fault(
            "task::taste-policy-invalid",
            format!("resolved taste bag could not be reconstructed: {error}"),
        )
    })?;
    let mut context = gate::PolicyContext::new();
    if let Some(vendor) = vendor {
        context = context.with_vendor(vendor);
    }
    if let Some(model) = model {
        context = context.with_model(model);
    }
    for (field, text) in &fields {
        let event = listen::Event::Prose(listen::Prose {
            role,
            text: (*text).to_owned(),
            phase: None,
            media: Vec::new(),
            external_media: Vec::new(),
            locus: listen::Locus::new(0, 0, text.len() as u64),
        });
        let hits = applicable.hits(&event, &context).map_err(|error| {
            protocol_fault(
                "task::taste-policy-invalid",
                format!("resolved taste policy refused task text: {error}"),
            )
        })?;
        for hit in hits {
            let found = TasteMatch {
                field: *field,
                line: hit.line(),
                taste: hit.taste().as_str().to_owned(),
                finding: Some(wire::TasteFinding {
                    trigger: hit.trigger().to_owned(),
                    column: hit.column(),
                    message: hit.message().to_owned(),
                }),
            };
            if !matches.contains(&found) {
                matches.push(found);
            }
        }
    }
    Ok(Some(TasteReview { matches }))
}

/// The built-in pitch findings as taste matches: an identifier or a second
/// sentence holds the statement naming the matched text, as a taste word does.
fn pitch_matches(pitch: &str) -> Vec<TasteMatch> {
    wire::pitch::findings(pitch)
        .into_iter()
        .map(|finding| TasteMatch {
            field: StatementField::Pitch,
            line: 1,
            taste: finding.taste.to_owned(),
            finding: Some(wire::TasteFinding {
                trigger: finding.trigger,
                column: finding.column,
                message: finding.message.to_owned(),
            }),
        })
        .collect()
}

/// Withhold what each taste match found from a lane's read of one hold.
///
/// The lane still reads which field, line, and taste held its statement; the
/// matched text and the taste's message stay with the operator, because an
/// author told the word swaps the word instead of rewriting the statement.
pub(super) fn withhold_taste_findings(principal: &Actor, taste: &mut Option<TasteHold>) {
    if matches!(principal, Actor::Lane(_))
        && let Some(hold) = taste
    {
        hold.withhold_findings();
    }
}

/// The taste hold a command answer leads with, read as `principal` may read it.
pub(super) fn answered_taste(principal: &Actor, taste: Option<&TasteHold>) -> Option<TasteHold> {
    let mut taste = taste.cloned();
    withhold_taste_findings(principal, &mut taste);
    taste
}
