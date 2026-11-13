//! Private Design input due after authority or lane-evidence changes:
//! profile-selected requests are recorded before allocation and each is told
//! once to its occupant when that occupant is seated, while a vacant lane's
//! request waits for `task next`; then the newest return of a lane's own
//! active Design is delivered and durably consumed.

use serde::{Deserialize, Serialize};
use wire::CanonicalUuid;

use super::supervisor::command::{KICK_BYTES, Kick};
use super::*;
use crate::board_command::{ReturnOwed, owed_returns, record_return};

/// Return reason length a notice keeps whole while fitting one datagram.
const SHORTEST_REASON_BYTES: usize = 16;
/// Design title length a request notice keeps whole while fitting one datagram.
const SHORTEST_TITLE_BYTES: usize = 16;
/// Asked question handles a request notice names before counting the rest.
const NAMED_QUESTIONS: usize = 3;

/// One private background notice for an exact occupant, replacing any queued
/// notice of the same purpose.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resident) struct BackgroundNotice {
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) purpose: String,
    pub(in crate::resident) text: String,
}

impl BackgroundNotice {
    /// Whether this notice and a maximum-size confirmation route fit one kick
    /// datagram.
    pub(in crate::resident) fn confirmed_fits(&self) -> bool {
        serde_json::to_vec(&Kick::BackgroundConfirmed {
            notice: self.clone(),
            owner: None,
            request: "00000000-0000-0000-0000-000000000000".to_owned(),
            reply:
                "/tmp/spice-b-0000000000000000000000000000000000000000000000000000000000000000.sock"
                    .into(),
        })
        .is_ok_and(|encoded| encoded.len() <= KICK_BYTES)
    }
}

/// Record lane-profile Design requests and tell each one once to its
/// occupant when that occupant is seated as it is recorded, then tell every
/// occupied claimant the newest return of an active Design it holds. A request
/// notice is never retried: `task next` keeps showing a request its lane has
/// not answered, including one recorded while the lane was vacant.
/// Return delivery is confirmed before its durable consumption record; either
/// failure is retried on the next reconciliation pass. `changed` names the
/// tasks the settled commit touched; `None` stands for a pass that knows no
/// commit.
pub(in crate::resident) fn reconcile(
    bus: &Bus,
    paths: &RuntimePaths,
    seats: &Seats,
    completions: &mut super::design_awareness::CompletionCursor,
    profiles: &std::sync::Mutex<message_surface::MessageState>,
    changed: Option<&std::collections::BTreeSet<wire::TaskStamp>>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    for recorded in
        super::design_awareness::reconcile(bus, seats, completions, profiles, changed, diagnostic)?
    {
        let _told = super::supervisor::command::confirmed_background_notice(
            paths,
            recorded.lane,
            request_notice(&recorded),
            None,
        );
    }
    let occupied = super::design_awareness::occupied(seats);
    if occupied.is_empty() {
        return Ok(());
    }
    let owed = match owed_returns(bus, &occupied) {
        Ok(owed) => owed,
        Err(outcome) => {
            let _ = writeln!(diagnostic, "Design returns were not read: {outcome:?}");
            return Ok(());
        }
    };
    for owed in owed {
        let notice = return_notice(&owed);
        match super::supervisor::command::confirmed_background_notice(
            paths, owed.lane, notice, None,
        ) {
            Ok(true) => {
                if let Err(outcome) = record_return(bus, &owed) {
                    let _ = writeln!(
                        diagnostic,
                        "lane {} return of Design {} was delivered but not recorded: {outcome:?}",
                        owed.lane, owed.design
                    );
                }
            }
            Ok(false) => {
                let _ = writeln!(
                    diagnostic,
                    "lane {} return of Design {} was refused by its current supervisor",
                    owed.lane, owed.design
                );
            }
            Err(error) => {
                let _ = writeln!(
                    diagnostic,
                    "lane {} return of Design {} was not delivered: {error}",
                    owed.lane, owed.design
                );
            }
        }
    }
    Ok(())
}

/// The private notice telling a seated occupant its lane now owes one Design
/// request. It names the request rather than carrying it, with the title
/// clipped until it fits one datagram: `task next` shows the request whole.
/// Its opening line is what marks the turn it lands in as caused by that
/// request ([`spice::rooms::profile::design_request_causes`]).
pub(in crate::resident) fn request_notice(
    recorded: &super::design_awareness::Recorded,
) -> BackgroundNotice {
    let request = &recorded.request;
    let design = &request.design;
    let asked = match request.asks.as_slice() {
        [] => "the Design as a whole".to_owned(),
        [ask] => format!("question {}", ask.handle),
        asks => {
            let named = asks
                .iter()
                .take(NAMED_QUESTIONS)
                .map(|ask| ask.handle.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            match asks.len().saturating_sub(NAMED_QUESTIONS) {
                0 => format!("questions {named}"),
                more => format!("questions {named} and {more} more"),
            }
        }
    };
    let mut title = request.title.clone();
    loop {
        let notice = BackgroundNotice {
            occupant: recorded.occupant,
            purpose: format!("design-request:{design}"),
            text: format!(
                "<system-notice>\nDesign request {design} \"{title}\" asks for your view on \
                 {asked}, matched to your earlier work. This grants no claim and starts no work. Read it \
                 with `spice task show {design} --full`; answer once with `spice room contribute \
                 {design} --charge=C --question HEX TEXT` or decline with `spice room leave \
                 {design} --clause TEXT`. Holding no claim, `spice task next` shows the whole \
                 request until you answer. This is a private background notice: it needs no \
                 acknowledgment key or visible ACK/NAK.\n</system-notice>",
            ),
        };
        if notice.confirmed_fits() || title.len() <= SHORTEST_TITLE_BYTES {
            return notice;
        }
        title = clip(&title, title.len() / 2);
    }
}

/// The private notice carrying one decision's feedback to the Design holder,
/// clipped until it fits one datagram; room history keeps the text whole.
pub(in crate::resident) fn return_notice(owed: &ReturnOwed) -> BackgroundNotice {
    let mut reason = owed.reason.clone();
    loop {
        let notice = BackgroundNotice {
            occupant: owed.occupant,
            purpose: format!("design-return:{}", owed.design),
            text: format!(
                "<system-notice>\nThe operator recorded feedback on your Design {} \
                 (decision {}):\n{reason}\nThis feedback is the Design's newest input: carry it \
                 into the next pass. `spice room history {}` lists every decision. This is a \
                 private background notice: it needs no acknowledgment key or visible ACK/NAK.\n\
                 </system-notice>",
                owed.design, owed.decision.decision, owed.design,
            ),
        };
        if notice.confirmed_fits() || reason.len() <= SHORTEST_REASON_BYTES {
            return notice;
        }
        reason = clip(&reason, reason.len() / 2);
    }
}

/// `text` cut at a character boundary to at most `bytes` bytes.
pub(in crate::resident) fn clip(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.to_owned();
    }
    let mut end = bytes - '…'.len_utf8();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
