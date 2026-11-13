//! What the journal records about one peer line input, read for presentation.
//!
//! Every surface that names a line input, whether a pending notice, the inbox
//! or a room's route receipt, reads its standing here, so each one names the
//! same root, covered inputs, admission cause, answer and terminal action.

use std::collections::BTreeMap;

use journal::GuardError;
use wire::body::PeerAdmission;
use wire::intent::{LineAnswer, LineStanding};
use wire::{CanonicalUuid, Seq, SteeringHead};

/// The admission cause recorded by the fact that created one peer body.
///
/// One primary-key fact read. A body admitted before causes were recorded, and
/// a directive no peer body created, has none.
pub(crate) fn admission(
    read: &journal::JournalRead<'_>,
    created: Seq,
) -> Result<Option<PeerAdmission>, GuardError> {
    let budget = journal::FactReadBudget::new(1, journal::MAX_RETAINED_FACT_PAGE_BYTES)?;
    let page = read.fact_page(journal::FactPageStart::At(created), created, budget)?;
    Ok(page.rows.first().and_then(|envelope| {
        envelope
            .fact
            .read::<wire::body::SteerMessaged>()
            .map(|messaged| messaged.admission)
            .or_else(|| {
                envelope
                    .fact
                    .read::<wire::body::SteerQueued>()
                    .map(|queued| queued.admission)
            })
            .flatten()
    }))
}

/// The key of one directive, when the steering index still holds its head.
pub(crate) fn key(
    read: &journal::JournalRead<'_>,
    directive: CanonicalUuid,
) -> Result<Option<String>, GuardError> {
    Ok(read
        .steering_head(directive)?
        .map(|head| head.body().key.clone()))
}

/// What a control did with each covered input of the line `state` describes.
///
/// An input a later input's header covered was answered by that input; a
/// covered input no admitted body names was only seen.
pub(crate) fn answers(
    read: &journal::JournalRead<'_>,
    state: &journal::PeerLineHead,
) -> Result<BTreeMap<CanonicalUuid, LineAnswer>, GuardError> {
    let mut answers = state
        .covered()
        .iter()
        .map(|input| (*input, LineAnswer::Seen {}))
        .collect::<BTreeMap<_, _>>();
    for input in state.inputs() {
        let Some(head) = read.steering_head(*input)? else {
            continue;
        };
        let head = head.body();
        let Some((_, body, covered)) = head
            .control
            .as_deref()
            .and_then(wire::parse_peer_line_control)
        else {
            continue;
        };
        for answered in covered {
            if let Some(answer) = answers.get_mut(&answered) {
                *answer = LineAnswer::Answered {
                    key: head.key.clone(),
                    body,
                };
            }
        }
    }
    Ok(answers)
}

/// Where the peer input `head` stands on its line, pending or retired; `None`
/// for every directive that is not a line input.
pub(crate) fn standing(
    read: &journal::JournalRead<'_>,
    head: &SteeringHead,
) -> Result<Option<LineStanding>, GuardError> {
    let Some((line, body, covered)) = head
        .control
        .as_deref()
        .and_then(wire::parse_peer_line_control)
    else {
        return Ok(None);
    };
    let root = if line.root == head.directive {
        Some(head.key.clone())
    } else {
        key(read, line.root)?
    };
    let Some(root) = root else {
        return Ok(None);
    };
    let mut keys = Vec::with_capacity(covered.len());
    for input in covered {
        keys.extend(key(read, input)?);
    }
    let state = read.peer_line_head(line.root)?;
    Ok(Some(LineStanding {
        root,
        parent: match line.parent {
            Some(parent) => key(read, parent)?,
            None => None,
        },
        body,
        covered: keys,
        admission: admission(read, head.created)?,
        answer: match &state {
            Some(state) => answers(read, state)?.remove(&head.directive),
            None => None,
        },
        closed: state
            .and_then(|state| state.terminal())
            .map(|terminal| terminal.action),
    }))
}
