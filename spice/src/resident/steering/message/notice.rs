//! What a packet's author and the resident diagnostic hear about one
//! publication or refusal.

use super::*;

pub(in crate::resident::steering) fn refusal_detail(
    text: &str,
    refusal: &steer::ack::scan::MessageRefusal,
) -> String {
    use steer::ack::scan::MessageRefusal;
    match refusal {
        MessageRefusal::Unterminated => "the selector list is not closed by a colon".to_owned(),
        MessageRefusal::Recipient { span, error } => {
            format!(
                "`{}` is not an occupant address: {error}",
                &text[span.clone()],
            )
        }
        MessageRefusal::Context { span, .. } => {
            format!("`#{}` is not a task handle", &text[span.clone()])
        }
        MessageRefusal::DirectionAudience { span } => format!(
            "`{}` combines an occupant with an operator-direction pointer; use `#KEY` alone",
            &text[span.clone()]
        ),
        MessageRefusal::KeyRecipient { span } => key_recipient(&text[span.clone()]),
        MessageRefusal::Repeated { audience, .. } => {
            format!("{audience} is named more than once")
        }
        MessageRefusal::RepeatedContext { context, .. } => {
            format!("#{context} names a room another room-only route already names")
        }
        MessageRefusal::EmptyBody => "the packet has no body".to_owned(),
    }
}

/// The guidance for a steering key written where an occupant belongs.
pub(super) fn key_recipient(written: &str) -> String {
    format!(
        "`{written}` is spelled as a steering key and names no occupant; reach that input's \
         sender with `SYN #{written}: BODY`"
    )
}

/// Tell the recipients, or the author and the diagnostic, what one packet did.
pub(super) fn report(
    teller: &Teller<'_>,
    outcome: &Result<Published, Unpublished>,
    diagnostic: &mut dyn io::Write,
) {
    match outcome {
        Ok(Published::Sent {
            lanes,
            queued,
            resuming,
            redirected,
            unreached,
            spent,
            rooms,
            principals,
            held_room,
            lines,
        }) => {
            for lane in lanes {
                notify_inbox(teller.paths, *lane);
            }
            let beyond = !rooms.is_empty()
                || *principals != 0
                || !queued.is_empty()
                || !redirected.is_empty();
            if beyond || !lanes.is_empty() {
                teller.published(
                    &Recorded {
                        rooms,
                        queued,
                        resuming,
                        redirected,
                        reached: lanes.len() + principals,
                        held_room: *held_room,
                        lines: *lines,
                    },
                    diagnostic,
                );
            }
            if !unreached.is_empty() {
                teller.tell(UNAVAILABLE, &unreached.join("; "), lanes.len(), diagnostic);
            }
            if !spent.is_empty() {
                teller.tell(admission::SPENT, &spent.join("; "), lanes.len(), diagnostic);
            }
        }
        Ok(Published::Already) | Err(Unpublished::Failed(_)) => {}
        Err(Unpublished::Recorded { reason, detail }) => {
            teller.record_refusal(reason, detail, 0, diagnostic);
            if teller.body.is_some() {
                notify_inbox(teller.paths, teller.record.lane);
            }
        }
        Err(Unpublished::Withheld { .. }) => teller.correct(diagnostic),
        Err(Unpublished::Refused { reason, detail }) => {
            teller.tell(reason, detail, 0, diagnostic);
        }
    }
}

/// Where one packet's outcome is told.
pub(super) struct Teller<'a> {
    pub(super) bus: &'a Bus,
    pub(super) paths: &'a RuntimePaths,
    pub(super) epoch: CanonicalUuid,
    /// How long a correction this packet opens stays live.
    pub(super) lifetime: Duration,
    pub(super) record: &'a AuthoredRecord<'a>,
    pub(super) locus: &'a TranscriptLocus,
    pub(super) header: &'a str,
    /// The packet's body, when its grammar admitted one.
    pub(super) body: Option<&'a str>,
    pub(super) refusals: Refusals,
}

/// What one published packet did beyond its deliveries.
#[derive(Clone, Copy)]
struct Recorded<'a> {
    rooms: &'a [TaskHandle],
    queued: &'a [OccupantAddress],
    resuming: &'a [OccupantAddress],
    redirected: &'a [String],
    reached: usize,
    held_room: bool,
    lines: usize,
}

impl Teller<'_> {
    /// Stable identity of the packet's source locus.
    fn packet(&self) -> String {
        let locus = self.locus;
        format!(
            "{}:{}:{}:{}:{}",
            locus.member, locus.offset, locus.event, locus.line, locus.column
        )
    }

    /// The private notice telling the packet's authoring occupant how many
    /// recipients it reached and why the rest received nothing.
    fn refusal_notice(&self, reason: &str, detail: &str, reached: usize) -> MessageRefusalNotice {
        MessageRefusalNotice {
            occupant: self.record.author,
            packet: self.packet(),
            header: bounded(self.header),
            line: self.locus.line,
            column: self.locus.column,
            reached,
            reason: reason.to_owned(),
            detail: bounded(detail),
            closed_line: None,
        }
    }

    /// Record the refusal and, under [`Refusals::Notify`], tell the author
    /// privately, with how many recipients the packet did reach.
    fn tell(&self, reason: &str, detail: &str, reached: usize, diagnostic: &mut dyn io::Write) {
        self.record_refusal(reason, detail, reached, diagnostic);
        if self.refusals == Refusals::Notify {
            crate::resident::supervisor::command::message_refused(
                self.paths,
                self.record.lane,
                self.refusal_notice(reason, detail, reached),
            );
        }
    }

    /// Open the one correction a wholly withheld packet owes its authoring
    /// lane and tell that lane its inbox changed. The correction's identity
    /// is the packet's source, so a reread opens nothing.
    fn correct(&self, diagnostic: &mut dyn io::Write) {
        let Some(body) = self.body else {
            return;
        };
        let lane = self.record.lane;
        let refused = correction::Refused {
            lane,
            author: self.record.author,
            locus: self.locus,
            header: self.header,
            body,
            keyed: false,
        };
        match correction::correct_withheld(self.bus, self.epoch, self.lifetime, &refused) {
            Ok(true) => notify_inbox(self.paths, lane),
            Ok(false) => {}
            Err(outcome) => {
                let _ = writeln!(
                    diagnostic,
                    "lane {lane} packet `{}` correction is not open: {}",
                    self.header,
                    describe(&typed_outcome(outcome)).1,
                );
            }
        }
    }

    fn record_refusal(
        &self,
        reason: &str,
        detail: &str,
        reached: usize,
        diagnostic: &mut dyn io::Write,
    ) {
        let (record, locus) = (self.record, self.locus);
        let _ = writeln!(
            diagnostic,
            "lane {} packet `{}` at record {}:{} event {} line {}:{} reached {reached} and \
             refused: {reason}: {detail}",
            record.lane,
            self.header,
            locus.member,
            locus.offset,
            locus.event,
            locus.line,
            locus.column,
        );
    }

    /// Tell the author privately, under [`Refusals::Notify`], how many
    /// recipients the packet reached and what it published beyond those
    /// deliveries, so a packet that sent always earns a receipt; no participant
    /// acknowledgment is implied. Only what goes beyond plain delivery reaches
    /// the diagnostic.
    fn published(&self, outcome: &Recorded<'_>, diagnostic: &mut dyn io::Write) {
        let (record, locus) = (self.record, self.locus);
        let Recorded {
            rooms,
            queued,
            resuming,
            redirected,
            reached,
            held_room,
            lines,
        } = *outcome;
        let rooms = rooms.iter().map(ToString::to_string).collect::<Vec<_>>();
        let (resuming, queued): (Vec<&OccupantAddress>, Vec<&OccupantAddress>) = queued
            .iter()
            .partition(|address| resuming.contains(address));
        let resuming = resuming.iter().map(ToString::to_string).collect::<Vec<_>>();
        let queued = queued.iter().map(ToString::to_string).collect::<Vec<_>>();
        let mut outcome = String::new();
        if !queued.is_empty() {
            let _ = write!(outcome, " queued for [{}]", queued.join(", "));
        }
        if !resuming.is_empty() {
            let _ = write!(outcome, " resuming [{}]", resuming.join(", "));
        }
        if !redirected.is_empty() {
            let _ = write!(outcome, " redirected [{}]", redirected.join("; "));
        }
        if !rooms.is_empty() || !outcome.is_empty() {
            let _ = writeln!(
                diagnostic,
                "lane {} packet `{}` at record {}:{} event {} line {}:{} recorded in [{}]{outcome}",
                record.lane,
                self.header,
                locus.member,
                locus.offset,
                locus.event,
                locus.line,
                locus.column,
                rooms.join(", "),
            );
        }
        if self.refusals == Refusals::Notify {
            crate::resident::supervisor::command::message_recorded(
                self.paths,
                record.lane,
                MessageRecordedNotice {
                    occupant: record.author,
                    packet: self.packet(),
                    header: bounded(self.header),
                    line: locus.line,
                    column: locus.column,
                    rooms,
                    queued,
                    resuming,
                    redirected: redirected.to_vec(),
                    reached,
                    held_room,
                    lines,
                },
            );
        }
    }
}

/// One private receipt for a packet that sent: how many actor endpoints it
/// reached, how many body lines it carried, and what it published beyond those
/// deliveries, scoped to the packet's authoring occupant.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resident) struct MessageRecordedNotice {
    pub(in crate::resident) occupant: CanonicalUuid,
    /// Stable identity of the recorded packet's source locus.
    pub(in crate::resident) packet: String,
    /// The packet header as authored, bounded.
    pub(in crate::resident) header: String,
    pub(in crate::resident) line: u32,
    pub(in crate::resident) column: u32,
    /// Each room the packet recorded in, as its header spelled the task.
    pub(in crate::resident) rooms: Vec<String>,
    /// Historical addresses whose vacant logical lanes hold queued delivery
    /// that wakes no one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(in crate::resident) queued: Vec<String>,
    /// Historical addresses whose vacant logical lanes hold queued delivery
    /// the route's room relationship resumes them to hear.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(in crate::resident) resuming: Vec<String>,
    /// Each route that named an earlier occupant and reached its successor:
    /// the named address, the address that heard it, and the interval between
    /// their births.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(in crate::resident) redirected: Vec<String>,
    /// How many named actor endpoints the packet reached.
    pub(in crate::resident) reached: usize,
    /// Some `#TASK` the packet named is the task its author's lane holds.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(in crate::resident) held_room: bool,
    /// Lines of the delivered body.
    pub(in crate::resident) lines: usize,
}

impl MessageRecordedNotice {
    /// The notice its author hears.
    pub(in crate::resident) fn text(&self) -> String {
        let mut clauses = Vec::new();
        if !self.redirected.is_empty() {
            clauses.push(format!(
                "reached a later occupant of a lane you addressed by an earlier code: {}; its \
                 delivery header tells the receiver the same",
                self.redirected.join("; ")
            ));
        }
        if !self.queued.is_empty() {
            clauses.push(format!(
                "queued for {}; unheard until each lane's next occupant starts; nothing woke anyone",
                self.queued.join(", ")
            ));
        }
        if !self.resuming.is_empty() {
            clauses.push(format!(
                "queued for {}; each lane arrived in the round your claim holds, so it resumes \
                 to hear this while you hold that claim and it takes no other work",
                self.resuming.join(", ")
            ));
        }
        if !self.rooms.is_empty() {
            clauses.push(format!("was recorded in room {}", self.rooms.join(", ")));
        }
        match self.reached {
            0 => {}
            1 => clauses.push("reached 1 recipient".to_owned()),
            reached => clauses.push(format!("reached {reached} recipients")),
        }
        clauses.push(match self.lines {
            1 => "carried 1 body line".to_owned(),
            lines => format!("carried {lines} body lines"),
        });
        let recorded = match (self.rooms.is_empty(), self.resuming.is_empty()) {
            (true, _) => "",
            (false, true) => {
                " Recorded means the room holds it; no participant has received or acknowledged \
                 it, and nothing woke anyone."
            }
            (false, false) => {
                " Recorded means the room holds it; no participant has received or acknowledged \
                 it."
            }
        };
        let held = if self.held_room {
            " It pointed at your own held task. Ordinary output needs no packet header. \
             `SYN ADDRESS:` delivers to a peer, while `#TASK` only records that room. To \
             raise a concern with the principal who assigned the work, use \
             `SYN #DIRECTION: BODY`, replacing `DIRECTION` with that direction's exact key."
        } else {
            ""
        };
        format!(
            "<system-notice>\nYour packet `{}` at line {}, column {} of its assistant text \
             block {}.{recorded}{held} This is a private notice; no acknowledgment key or \
             visible ACK/NAK is required.\n</system-notice>",
            self.header,
            self.line,
            self.column,
            clauses.join("; "),
        )
    }
}

/// One private authored-source refusal, scoped to its exact occupant.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resident) struct MessageRefusalNotice {
    pub(in crate::resident) occupant: CanonicalUuid,
    /// Stable identity of the refused packet's source locus.
    pub(in crate::resident) packet: String,
    /// The packet header as authored, bounded.
    pub(in crate::resident) header: String,
    pub(in crate::resident) line: u32,
    pub(in crate::resident) column: u32,
    /// How many named recipients the packet did reach.
    pub(in crate::resident) reached: usize,
    pub(in crate::resident) reason: String,
    /// Which addresses the packet missed, or why it was refused, bounded.
    pub(in crate::resident) detail: String,
    /// Answered input whose already closed line refused a transcript reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::resident) closed_line: Option<String>,
}

impl MessageRefusalNotice {
    /// Build the keyless, non-waking notice for a transcript reply whose line
    /// had already closed.
    pub(in crate::resident::steering) fn closed_line(
        occupant: CanonicalUuid,
        locus: TranscriptLocus,
        key: &str,
    ) -> Self {
        Self {
            occupant,
            packet: format!(
                "{}:{}:{}:{}:{}",
                locus.member, locus.offset, locus.event, locus.line, locus.column
            ),
            header: String::new(),
            line: locus.line,
            column: locus.column,
            reached: 0,
            reason: super::super::reply::LINE_CLOSED.to_owned(),
            detail: String::new(),
            closed_line: Some(key.to_owned()),
        }
    }

    pub(in crate::resident) fn purpose(&self) -> String {
        format!(
            "{}:{}",
            if self.closed_line.is_some() {
                "line"
            } else {
                "syn"
            },
            self.packet
        )
    }

    /// The notice its author hears. It names the packet's stable reference,
    /// so two packets refused alike stay two recognizable notices.
    pub(in crate::resident) fn text(&self) -> String {
        if let Some(key) = &self.closed_line {
            return format!(
                "<system-notice>\nA reply on this closed line reaches nobody. Build on this \
                 message in a new linked line with `SYN #{key}: BODY`.\n</system-notice>"
            );
        }
        let (outcome, missed) = match self.reached {
            0 => ("published nothing".to_owned(), "No recipient received it"),
            reached => (
                format!(
                    "reached {reached} recipient{}, but not every address it named",
                    if reached == 1 { "" } else { "s" }
                ),
                "Only the addresses named here did not receive it",
            ),
        };
        let advice = if self.reason == admission::SPENT {
            "Your lane may start another idle lane outside your shared rounds 5 times; one \
             start returns each minute. Anything sent to a running lane or within a shared \
             round is never counted. Send again once a start returns, or record what can wait \
             in a room with `#TASK`, which interrupts nobody."
        } else {
            "Name exact addresses of other seated occupants (`spice lane list` shows each \
             lane's), or point at one concern the assigning principal already sees: \
             `#DIRECTION` with the exact key of a principal direction sent to this lane. \
             `#TASK` only records the task or room and reaches nobody by itself. Qualify an \
             occupant with `#TASK` only for an existing task's handle, close the list with a \
             colon, and author a packet to the routes that still need it in new visible text."
        };
        format!(
            "<system-notice>\nYour packet `{}` at line {}, column {} of its assistant text \
             block {outcome}: {}: {}. {missed}. {advice} This is a private refusal notice for \
             packet {}; no acknowledgment key or visible ACK/NAK is required.\n</system-notice>",
            self.header, self.line, self.column, self.reason, self.detail, self.packet,
        )
    }
}

/// At most [`NOTICE_TEXT_BYTES`] of `text`, cut at a character boundary.
pub(in crate::resident::steering) fn bounded(text: &str) -> String {
    if text.len() <= NOTICE_TEXT_BYTES {
        return text.to_owned();
    }
    let mut end = NOTICE_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
