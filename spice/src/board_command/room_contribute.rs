//! A lane's typed problem-space contribution to an open Design room.

use super::*;

use crate::room_command::{RoomSources, SourceOwners};
use wire::intent::{RoomContribute, RoomContributeAnswer};
use wire::rooms::{RoomContributed, RoomContributionError, RoomSource, StatementItem};

use super::room::mint_publication;

/// Record the commanding lane's contribution in `contribute.room`.
///
/// The anchor must be an open Design, a charge must name one of its current
/// question items, and every cited source must resolve through `owners` at the
/// same snapshot. A contribution authorizes nothing: it is one immutable room
/// record, never a task, workflow, claim, group, candidate, decision, or
/// authorization change.
pub(crate) fn room_contribute(
    bus: &Bus,
    command: &wire::AdmittedCommand,
    contribute: &RoomContribute,
    owners: Option<SourceOwners<'_>>,
    repository: Option<&Repository>,
) -> Outcome<CommandAnswer> {
    let Actor::Lane(lane) = command.actor().clone() else {
        return refusal(
            "room::contribution-lane-only",
            None,
            "contribute from a lane; the operator writes with `spice room post`",
        );
    };
    let text = contribute.text.trim();
    if let Err(error) = RoomContributed::admit(
        contribute.kind,
        contribute.question.as_ref(),
        text,
        &contribute.sources,
    ) {
        return malformed(error);
    }
    let expected = match contribution_witnesses(command, &contribute.sources) {
        Ok(expected) => expected,
        Err(refused) => return refused,
    };
    let snapshot = match validated_working_snapshot(
        bus,
        repository,
        command.actor().clone(),
        std::slice::from_ref(&contribute.room),
    ) {
        Ok(snapshot) => snapshot,
        Err(outcome) => return outcome,
    };
    let Some(anchor) = task_entry(&snapshot.entries, &contribute.room) else {
        return refusal(
            "room::anchor-unknown",
            Some(contribute.room.to_string()),
            "read the board and name one current task handle",
        );
    };
    if let Some(refused) = not_open_design(anchor).or_else(|| stale_question(anchor, contribute)) {
        return refused;
    }
    let room = anchor.born().clone();
    let Some(statement) = expected.iter().find_map(|witness| match witness {
        wire::Witness::Task { task, head } if task == &room => Some(*head),
        _ => None,
    }) else {
        return store_fault(
            "room::contribution-statement-missing",
            "the admitted contribution has no Design statement witness",
        );
    };
    let Some(before) = snapshot.cursor.get().checked_add(1).and_then(Seq::new) else {
        return store_fault(
            "room::cursor-exhausted",
            "the journal cursor has no successor",
        );
    };
    let mut sources = RoomSources::new(owners, &room);
    if let Some(refused) = unavailable_source(&mut sources, &contribute.sources, before) {
        return refused;
    }
    if let Some(refused) = unpublished_contribution(bus, &contribute.sources, before) {
        return refused;
    }
    let now = jiff::Timestamp::now();
    let at = match Instant::new(now) {
        Ok(at) => at,
        Err(error) => return environment_fault("room::clock-invalid", error.to_string()),
    };
    let publication = match mint_publication(bus, &room, now.as_microsecond()) {
        Ok(publication) => publication,
        Err(outcome) => return outcome,
    };
    let origin = match task_origin(&room) {
        Ok(origin) => origin,
        Err(outcome) => return outcome,
    };
    let answer = match RoomContributeAnswer::new(
        anchor.handle().clone(),
        publication.clone(),
        contribute.kind,
    ) {
        Ok(answer) => answer,
        Err(error) => return store_fault("room::contribution-answer", error.to_string()),
    };
    let mut write = JournalWrite::new(Actor::Lane(lane), origin, at, Producer::CommandDoor);
    guard(&mut write, expected);
    if let Err(error) = write.fact(
        RoomContributed {
            room,
            publication,
            lane,
            contribution: contribute.kind,
            item: contribute.question.clone(),
            statement,
            text: text.to_owned(),
            sources: contribute.sources.clone(),
        },
        FactCause::Root,
    ) {
        return store_fault("room::contribution-build", error.to_string());
    }
    if let Err(error) = write.command_result(
        request(command, contribute.clone()),
        command.epoch(),
        answer.clone(),
    ) {
        return store_fault("room::contribution-receipt", error.to_string());
    }
    committed(bus, write, CommandAnswer::RoomContribute(Box::new(answer)))
}

fn malformed(error: RoomContributionError) -> Outcome<CommandAnswer> {
    let (reason, hint) = match error {
        RoomContributionError::Empty => {
            ("room::contribution-empty", "state the contribution as TEXT")
        }
        RoomContributionError::Untrimmed => (
            "room::contribution-untrimmed",
            "remove leading and trailing whitespace",
        ),
        RoomContributionError::TextTooLong => (
            "room::contribution-too-long",
            "shorten the contribution and cite longer material as a source",
        ),
        RoomContributionError::TooManySources => (
            "room::contribution-sources-exceeded",
            "cite only the sources this contribution rests on",
        ),
        RoomContributionError::DuplicateSource => (
            "room::contribution-source-duplicate",
            "cite each source once",
        ),
        RoomContributionError::Unplaced => (
            "room::contribution-question-missing",
            "name the question item this charge weighs with --question HEX, its handle",
        ),
        RoomContributionError::Uncited => (
            "room::contribution-uncited",
            "only +1 and -1 are opinions; cite what a 0, +2, or -2 rests on with --cite KEY",
        ),
    };
    refusal(reason, Some(error.to_string()), hint)
}
/// The journal-backed witnesses and the refusal when source witnesses do not
/// name exactly the request's citations. Source availability is evaluated
/// separately through its owning authority before these witnesses are stored
/// in the successful command receipt.
fn contribution_witnesses(
    command: &wire::AdmittedCommand,
    cited: &[RoomSource],
) -> Result<Vec<Witness>, Outcome<CommandAnswer>> {
    if !wire::rooms::source_witnesses_match(cited, command.expected()) {
        return Err(refusal(
            "room::contribution-source-witness",
            Some(format!(
                "expected {:?}; cited sources {cited:?}",
                command.expected()
            )),
            "read the Design again and retry with one witness for each exact source",
        ));
    }
    Ok(command
        .expected()
        .iter()
        .filter(|witness| !matches!(witness, Witness::Source { .. }))
        .cloned()
        .collect())
}

/// Why `anchor` is not an open Design at its current workflow head.
fn not_open_design(anchor: &Entry) -> Option<Outcome<CommandAnswer>> {
    if !anchor
        .statement()
        .intent
        .is_some_and(wire::TaskIntent::is_design)
    {
        return Some(refusal(
            "room::contribution-not-design",
            Some(anchor.handle().to_string()),
            "contribute to the room of an open Design",
        ));
    }
    (!wire::intent::is_open_design(anchor.statement().intent, anchor.workflow())).then(|| {
        refusal(
            "room::contribution-design-concluded",
            Some(format!(
                "{} {} phase {}",
                anchor.handle(),
                anchor.workflow().status,
                anchor.workflow().phase
            )),
            "contribute only after the Design reaches its open design phase",
        )
    })
}

/// The refusal when `contribute` names a question item outside the anchor's
/// current statement: the statement changed between the lane's read and the
/// command.
fn stale_question(anchor: &Entry, contribute: &RoomContribute) -> Option<Outcome<CommandAnswer>> {
    let item = contribute.question.as_ref()?;
    let term = &anchor.statement().term;
    (item.text_in(term).is_none()).then(|| {
        let hint = match term.as_slice() {
            [] => "the Design's questions changed; it now names none".to_owned(),
            _ => format!(
                "the Design's questions changed; current questions:\n{}",
                StatementItem::bullets(term)
            ),
        };
        refusal(
            "room::contribution-question-stale",
            Some(format!("{} in {}", item.handle(&[]), anchor.handle())),
            &hint,
        )
    })
}

/// The refusal naming the first cited source unavailable before `before`.
fn unavailable_source(
    sources: &mut RoomSources<'_>,
    cited: &[RoomSource],
    before: Seq,
) -> Option<Outcome<CommandAnswer>> {
    cited.iter().find_map(|source| {
        let unavailable = sources.check(source, before).err()?;
        Some(refusal(
            "room::contribution-source-unavailable",
            Some(format!(
                "{}: {}",
                serde_json::to_string(source).unwrap_or_default(),
                serde_json::to_string(&unavailable).unwrap_or_default()
            )),
            "cite only journal facts, transcript hunks, Design artifacts, or repository source \
             that exist now",
        ))
    })
}

/// The refusal naming the first cited contribution that no room contribution
/// before `before` answers.
fn unpublished_contribution(
    bus: &Bus,
    cited: &[RoomSource],
    before: Seq,
) -> Option<Outcome<CommandAnswer>> {
    let contributions = cited
        .iter()
        .filter_map(|source| match source {
            RoomSource::Contribution { room, publication } => {
                Some((source.clone(), room.clone(), publication.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if contributions.is_empty() {
        return None;
    }
    let missing = bus.read(move |read| {
        for (source, room, publication) in contributions {
            let contributed = match read.room_publication_seq(&room, &publication)? {
                Some(seq) if seq < before => read
                    .room_fact(seq)?
                    .and_then(|envelope| envelope.fact.read::<RoomContributed>())
                    .is_some_and(|contributed| contributed.publication == publication),
                _ => false,
            };
            if !contributed {
                return Ok::<_, GuardError>(Some(source));
            }
        }
        Ok(None)
    });
    match missing {
        Ok(None) => None,
        Ok(Some(source)) => Some(refusal(
            "room::contribution-source-unavailable",
            Some(serde_json::to_string(&source).unwrap_or_default()),
            "cite a contribution in this room by its publication stamp",
        )),
        Err(error) => Some(store_fault("room::journal-unreadable", error.to_string())),
    }
}
