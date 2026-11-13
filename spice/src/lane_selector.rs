//! Lane selectors written on the command line.
//!
//! Every occupant code a lane has shown, current or historical, names that
//! lane: the resident resolves it against one residency snapshot, so seated
//! and vacant lanes resolve alike. Display names are never selectors. A
//! canonical lane UUID still passes through unchanged for tooling that holds
//! one.

use wire::intent::{BoardRead, CommandAnswer};
use wire::{CanonicalUuid, LaneSelection, OccupantAddress, Outcome, Refusal};

use crate::{command_client::Host, protocol_fault};

/// What one written selector names before the resident resolves it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Written {
    Code(OccupantAddress),
    Lane(CanonicalUuid),
}

fn parse(written: &str) -> Option<Written> {
    written
        .parse()
        .map(Written::Code)
        .or_else(|_| written.parse().map(Written::Lane))
        .ok()
}

/// The canonical lane `written` selects, or the refusal naming why none.
///
/// A historical code is answered with one notice naming how many occupants
/// ago it was current and the code the lane shows now.
pub(crate) fn resolve(
    host: Host<'_>,
    written: &str,
    reason: &str,
) -> Result<CanonicalUuid, Outcome<CommandAnswer>> {
    let code = match parse(written) {
        Some(Written::Code(code)) => code,
        Some(Written::Lane(lane)) => return Ok(lane),
        None => {
            return Err(Outcome::Refusal(Refusal::bare(
                wire::RefusalDiagnostic::new(
                    reason.to_owned(),
                    Some("lane".to_owned()),
                    Some("an occupant code the lane has shown".to_owned()),
                    Some(written.to_owned()),
                )
                .with_detail("`spice lane list` shows each lane's current code"),
            )));
        }
    };
    let board = crate::board_command::current_board(host, BoardRead::Lane { code })?;
    let selection = board.selected.ok_or_else(|| {
        protocol_fault(
            "lane::selection-missing",
            "the resident answered a lane selection read without its selection",
        )
    })?;
    if let Some(notice) = staleness(code, &selection) {
        eprintln!("{notice}");
    }
    Ok(selection.lane)
}

/// The notice a superseded code earns, or none for the lane's current code.
pub(crate) fn staleness(code: OccupantAddress, selection: &LaneSelection) -> Option<String> {
    match selection.since {
        0 => None,
        1 => Some(format!(
            "spice: {code} was this lane's code one occupant ago; it now shows {}",
            selection.code
        )),
        since => Some(format!(
            "spice: {code} was this lane's code {since} occupants ago; it now shows {}",
            selection.code
        )),
    }
}
