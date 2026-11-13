//! The peer lines one inbox snapshot shows: where each pending input stands
//! and which lines each lane still has open.

use super::*;
use crate::peer_line;

/// The address that reaches `lane`: its newest occupant's, seated or not.
pub(super) fn lane_address(
    bus: &Bus,
    lane: CanonicalUuid,
) -> Result<wire::OccupantAddress, Outcome<CommandAnswer>> {
    bus.task_snapshot()
        .map_err(|error| store_fault("steer::residency-unreadable", &error.to_string()))?
        .facts()
        .lane_code(lane)
        .ok_or_else(|| {
            protocol_fault(
                "steer::line-recipient-unproved",
                "a peer line lane never held an occupant",
            )
        })
}

/// Every open peer line `lane` takes part in, oldest effect first, with the
/// other lane named by the address `addresses` proves for it.
fn open(
    read: &journal::JournalRead<'_>,
    lane: CanonicalUuid,
    addresses: &BTreeMap<CanonicalUuid, wire::OccupantAddress>,
) -> Result<Vec<wire::intent::PeerLine>, GuardError> {
    let mut lines = Vec::new();
    for root in read.open_peer_lines(Some(lane))? {
        let (Some(state), Some(opening)) = (read.peer_line_head(root)?, read.steering_head(root)?)
        else {
            continue;
        };
        let mut answers = peer_line::answers(read, &state)?;
        let mut inputs = Vec::with_capacity(state.inputs().len());
        for input in state.inputs() {
            let Some(head) = read.steering_head(*input)? else {
                continue;
            };
            let head = head.body();
            let Some((_, body, _)) = head
                .control
                .as_deref()
                .and_then(wire::parse_peer_line_control)
            else {
                continue;
            };
            inputs.push((
                head.created,
                wire::intent::PeerLineInput {
                    key: head.key.clone(),
                    held: head.recipient == wire::Actor::Lane(lane),
                    body,
                    answer: answers.remove(input),
                    disposition: head.disposition,
                    admission: peer_line::admission(read, head.created)?,
                },
            ));
        }
        inputs.sort_by_key(|(created, _)| *created);
        lines.push((
            state.newest_effect(),
            wire::intent::PeerLine {
                root: opening.body().key.clone(),
                parent: match state.line().parent {
                    Some(parent) => peer_line::key(read, parent)?,
                    None => None,
                },
                counterpart: state
                    .participants()
                    .iter()
                    .filter_map(|actor| actor.lane())
                    .find(|other| *other != lane)
                    .and_then(|other| addresses.get(&other).copied()),
                inputs: inputs.into_iter().map(|(_, input)| input).collect(),
                returned: state.opening_covered(),
                checkpoint: super::reply::clean(read, root)?,
            },
        ));
    }
    lines.sort_by_key(|(effect, _)| *effect);
    Ok(lines.into_iter().map(|(_, line)| line).collect())
}

/// What one inbox snapshot shows of peer lines.
#[derive(Default)]
pub(super) struct Lines {
    /// Line standing of each pending peer line input, by directive.
    pub(super) pending: BTreeMap<CanonicalUuid, wire::intent::LineStanding>,
    /// Open lines of each requested lane.
    pub(super) open: BTreeMap<CanonicalUuid, Vec<wire::intent::PeerLine>>,
}

/// Line standing of every pending head, and the open lines of each seated
/// lane `selected` admits, in the journal snapshot at `position`; `None` when
/// the journal has moved past it.
pub(super) fn snapshot(
    bus: &Bus,
    position: Seq,
    heads: &[journal::JournalHead<wire::SteeringHead>],
    seated: impl Iterator<Item = CanonicalUuid>,
    selected: Option<CanonicalUuid>,
    authority: &bus::TaskSnapshot,
) -> Result<Option<Lines>, Outcome<CommandAnswer>> {
    let heads = heads
        .iter()
        .map(|head| head.body().clone())
        .collect::<Vec<_>>();
    let seated = seated.collect::<Vec<_>>();
    let addresses = seated
        .iter()
        .filter_map(|lane| Some((*lane, authority.facts().lane_code(*lane)?)))
        .collect::<BTreeMap<_, _>>();
    bus.read(move |read| {
        if read.cursor()? != Some(position) {
            return Ok(None);
        }
        let mut lines = Lines::default();
        for head in &heads {
            if let Some(line) = peer_line::standing(read, head)? {
                lines.pending.insert(head.directive, line);
            }
        }
        for lane in seated
            .into_iter()
            .filter(|lane| selected.is_none_or(|selected| selected == *lane))
        {
            let open = open(read, lane, &addresses)?;
            if !open.is_empty() {
                lines.open.insert(lane, open);
            }
        }
        Ok(Some(lines))
    })
    .map_err(|error| store_fault("steer::line-unreadable", &error.to_string()))
}
