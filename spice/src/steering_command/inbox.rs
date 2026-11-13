//! Bounded `spice inbox`: every pending key, then a page of detail.
//!
//! The resident answers the complete visible inbox. This view always shows
//! every lane's pending keys in a compact index, so no authority item can sit
//! behind a page boundary unseen, and admits the directive detail most urgent
//! first through [`crate::read_page::Page`] with an exact continuation.

use std::ffi::OsString;
use std::fmt;

use clap::ArgMatches;
use serde::Serialize;
use wire::body::SteeringStage;
use wire::intent::{CommandAnswer, InboxAnswer, PendingDirective};
use wire::output::{Bounded, ClippedText};
use wire::{CanonicalUuid, Instant, Outcome, Reply};

use crate::read_page::Page;

#[cfg(test)]
mod tests;

/// One `spice inbox` answer.
#[derive(Debug, Serialize)]
pub(crate) struct InboxView {
    epoch: CanonicalUuid,
    /// Every visible lane and every key pending on it, in canonical lane order.
    lanes: Vec<LaneIndex>,
    /// Directive detail, most urgent first.
    pending: Bounded<InboxRow>,
}

#[derive(Debug, Serialize)]
struct LaneIndex {
    lane: CanonicalUuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    occupant: Option<CanonicalUuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compaction: Option<u64>,
    keys: Vec<String>,
    /// Open peer lines this lane takes part in.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    lines: Vec<wire::intent::PeerLine>,
}

/// One pending directive with its prose clipped to the page's text limit.
#[derive(Debug, Serialize)]
struct InboxRow {
    lane: CanonicalUuid,
    directive: CanonicalUuid,
    key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<CanonicalUuid>,
    /// Address of the exact occupant that sent a peer input.
    #[serde(skip_serializing_if = "Option::is_none")]
    sender: Option<wire::OccupantAddress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    room: Option<wire::TaskHandle>,
    stage: SteeringStage,
    authored: Instant,
    expires: Instant,
    text: ClippedText,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<ClippedText>,
    /// Steering control, omitted for a peer line input, whose `line` names
    /// the same identity by key.
    #[serde(skip_serializing_if = "Option::is_none")]
    control: Option<ClippedText>,
    /// Where a peer input stands on its conversation line.
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<wire::intent::LineStanding>,
    /// Retained attachments; `spice session` and the delivery hook carry them.
    #[serde(skip_serializing_if = "is_zero")]
    attachments: usize,
}

const fn is_zero(count: &usize) -> bool {
    *count == 0
}

impl InboxRow {
    fn of(lane: CanonicalUuid, pending: PendingDirective, text: usize) -> Self {
        let clip = |value: &str| wire::output::clip(value, text);
        Self {
            lane,
            directive: pending.directive,
            key: pending.key,
            from: pending.from,
            sender: pending.sender.and_then(|sender| sender.address),
            room: pending.room,
            stage: pending.stage,
            authored: pending.authored,
            expires: pending.expires,
            text: clip(&pending.text),
            preview: pending.preview.as_deref().map(clip),
            control: match pending.line {
                Some(_) => None,
                None => pending.control.as_deref().map(clip),
            },
            line: pending.line,
            attachments: pending.attachments.len(),
        }
    }
}

/// Read the visible inbox and project one bounded page of it.
pub(crate) fn run(
    matches: &ArgMatches,
    contract: &crate::cli::Contract,
    ambient: &spice::ambient::Ambient,
) -> Outcome<InboxView> {
    paged(matches, || super::run(matches, contract, ambient))
}

/// [`run`] against `repository`.
pub(crate) fn run_at(
    repository: &spice::repository::Repository,
    matches: &ArgMatches,
    contract: &crate::cli::Contract,
    lane_environment: Option<OsString>,
    ambient: &spice::ambient::Ambient,
) -> Outcome<InboxView> {
    paged(matches, || {
        super::run_at(repository, matches, contract, lane_environment, ambient)
    })
}

fn paged(
    matches: &ArgMatches,
    read: impl FnOnce() -> Outcome<CommandAnswer>,
) -> Outcome<InboxView> {
    let page = match Page::from_matches(matches, &["inbox"]) {
        Ok(page) => page,
        Err(refusal) => return Outcome::Refusal(refusal),
    };
    match read() {
        Outcome::Reply(Reply {
            position,
            body: CommandAnswer::Inbox(inbox),
        }) => match project(inbox, page) {
            Ok(body) => Outcome::Reply(Reply { position, body }),
            Err(refusal) => Outcome::Refusal(refusal),
        },
        Outcome::Reply(_) => Outcome::Fault(wire::Fault {
            class: wire::FaultClass::Protocol,
            reason: "steer::inbox-answer".to_owned(),
            detail: "an inbox read returned another answer shape".to_owned(),
        }),
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

fn project(inbox: InboxAnswer, page: Page) -> Result<InboxView, wire::Refusal> {
    let text = page.limits().text;
    let mut lanes = Vec::with_capacity(inbox.lanes.len());
    let mut rows = Vec::new();
    for lane in inbox.lanes {
        lanes.push(LaneIndex {
            lane: lane.lane,
            occupant: lane.occupant,
            compaction: lane.compaction,
            keys: lane
                .pending
                .iter()
                .map(|pending| pending.key.clone())
                .collect(),
            lines: lane.lines,
        });
        rows.extend(
            lane.pending
                .into_iter()
                .map(|pending| InboxRow::of(lane.lane, pending, text)),
        );
    }
    // Most urgent presentation first, then the oldest direction; the
    // directive identity breaks the last tie so pages never shift.
    rows.sort_by(|left, right| {
        right
            .stage
            .cmp(&left.stage)
            .then(left.authored.cmp(&right.authored))
            .then(left.directive.cmp(&right.directive))
    });
    let pending = page.listed(
        rows,
        |row| row.directive.to_string(),
        "ID of a pending directive",
    )?;
    Ok(InboxView {
        epoch: inbox.epoch,
        lanes,
        pending,
    })
}

impl fmt::Display for InboxView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "inbox epoch {}", self.epoch)?;
        for lane in &self.lanes {
            write!(
                formatter,
                "\nlane {} pending {}",
                lane.lane,
                lane.keys.len()
            )?;
            for key in &lane.keys {
                write!(formatter, " {key}")?;
            }
            for line in &lane.lines {
                write!(formatter, "\n{line}")?;
            }
        }
        for row in &self.pending.rows {
            write!(
                formatter,
                "\n{} {} {} lane={} expires={}",
                row.key,
                row.stage.as_str(),
                row.directive,
                row.lane,
                row.expires
            )?;
            match (row.sender, row.from) {
                (Some(sender), _) => write!(formatter, " from={sender}")?,
                (None, Some(from)) => write!(formatter, " from={from}")?,
                (None, None) => {}
            }
            if let Some(line) = &row.line {
                write!(formatter, " {}", line.standing())?;
            }
            if let Some(room) = &row.room {
                write!(formatter, " room={room}")?;
            }
            if row.attachments > 0 {
                write!(formatter, " attachments={}", row.attachments)?;
            }
            write!(formatter, "\n  {}", row.text)?;
        }
        match &self.pending.omission {
            Some(omission) => write!(formatter, "\n{omission}"),
            None => Ok(()),
        }
    }
}
