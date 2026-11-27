//! A lane's armed set: the open peer lines with its round partners that it
//! names as waiting on in its claimed task phase round.
//!
//! Admission reads only current authority: each root's steering head and
//! peer-line head, the caller's active claim and its task heads, and the
//! caller's newest arm fact. A call that changes the set commits one
//! lane-authored [`wire::body::LineArmed`] fact guarded on that same read; an
//! unchanged call writes nothing. Nothing here wakes any lane.

use super::*;

/// Most roots one armed set may name.
const MAX_ROOTS: usize = 16;
const MOVED: &str = "line::arm-moved";

/// The claim one arm binds: its task, phase and round opening, never its lease.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClaimBinding {
    task: wire::TaskStamp,
    phase: wire::Phase,
    round: Seq,
}

/// What one admission read proved.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Admitted {
    binding: Option<ClaimBinding>,
    /// Requested roots, by line root, with the key that named each.
    roots: BTreeMap<CanonicalUuid, String>,
    /// The caller's newest arm fact, when any, and its position.
    latest: Option<(Seq, wire::body::LineArmed)>,
}

impl Admitted {
    /// The set the caller's arm holds for its current claim; an arm bound to
    /// any other claim, phase or round holds nothing.
    fn current(&self) -> &[CanonicalUuid] {
        match (&self.binding, &self.latest) {
            (Some(binding), Some((_, armed)))
                if armed.task == binding.task
                    && armed.phase == binding.phase
                    && armed.round == binding.round =>
            {
                &armed.roots
            }
            _ => &[],
        }
    }

    fn changes(&self) -> bool {
        !self
            .current()
            .iter()
            .copied()
            .eq(self.roots.keys().copied())
    }

    fn answer(&self) -> wire::intent::LineArmAnswer {
        let mut roots = self.roots.values().cloned().collect::<Vec<_>>();
        roots.sort();
        wire::intent::LineArmAnswer { roots }
    }
}

/// Replace the calling lane's armed set with `request`'s roots.
pub(in crate::resident) fn arm(
    bus: &Bus,
    epoch: CanonicalUuid,
    command: &wire::AdmittedCommand,
    request: &wire::intent::LineArm,
) -> Outcome<CommandAnswer> {
    let Actor::Lane(lane) = *command.actor() else {
        return refusal(
            "line::lane-only",
            Some("a lane".to_owned()),
            Some(command.actor().to_string()),
            "only a lane arms the peer lines it waits on",
        );
    };
    let keys = request.roots.clone();
    let admission = bus.read(move |read| Ok((admit(read, lane, &keys)?, read.cursor()?)));
    let (observed, cursor) = match admission {
        Ok(admitted) => admitted,
        Err(bus::BusReadError::Head(GuardError::Refused(refused))) => {
            let (diagnostic, _) = refused.into_diagnostic();
            return Outcome::Refusal(Refusal::bare(diagnostic));
        }
        Err(error) => return store_fault("line::arm-unreadable", &error.to_string()),
    };
    let answer = observed.answer();
    let Some(binding) = observed.binding.clone().filter(|_| observed.changes()) else {
        return match cursor {
            Some(cursor) => Outcome::Reply(Reply {
                position: ReplyPosition::Snapshot(cursor),
                body: CommandAnswer::LineArm(answer),
            }),
            None => store_fault("line::journal-empty", "a running resident has no fact"),
        };
    };
    let actor = command.actor().clone();
    let mut proposal = match crate::resident::write(actor, epoch, Producer::CommandDoor) {
        Ok(proposal) => proposal,
        Err(outcome) => return typed_outcome(outcome),
    };
    let keys = request.roots.clone();
    let expected = observed.clone();
    proposal.guard(JournalGuard::new(move |read| {
        if admit(read, lane, &keys)? == expected {
            Ok(())
        } else {
            Err(GuardError::refused(MOVED))
        }
    }));
    if let Err(error) = proposal.fact(
        wire::body::LineArmed {
            lane,
            task: binding.task,
            phase: binding.phase,
            round: binding.round,
            roots: observed.roots.keys().copied().collect(),
        },
        FactCause::Root,
    ) {
        return store_fault("line::fact-invalid", &error.to_string());
    }
    if let Err(error) = proposal.command_result(
        typed_command(command, request.clone()),
        epoch,
        answer.clone(),
    ) {
        return store_fault("line::receipt-invalid", &error.to_string());
    }
    match commit(bus, proposal) {
        Ok(committed) => Outcome::Reply(Reply {
            position: ReplyPosition::Committed(SeqRange {
                first: seq(committed.first()),
                last: seq(committed.last()),
            }),
            body: CommandAnswer::LineArm(answer),
        }),
        Err(Outcome::Refusal(refused)) if refused.reason() == MOVED => {
            arm(bus, epoch, command, request)
        }
        Err(outcome) => typed_outcome(outcome),
    }
}

/// Admit `keys` as `lane`'s whole armed set against current authority, or
/// refuse the whole set naming the first root that fails.
fn admit(
    read: &journal::JournalRead<'_>,
    lane: CanonicalUuid,
    keys: &[String],
) -> Result<Admitted, GuardError> {
    if keys.len() > MAX_ROOTS {
        return Err(refused(
            "line::roots-over-bound",
            format!("at most {MAX_ROOTS} roots"),
            keys.len().to_string(),
            "arm at most 16 lines at once",
        ));
    }
    let caller = Actor::Lane(lane);
    let heads = read.active_claim_for_actor(&caller)?;
    let binding = heads.as_ref().map(|heads| ClaimBinding {
        task: heads.statement().body().born.clone(),
        phase: heads.workflow().body().phase,
        round: heads.completion().body().round,
    });
    let mut roots = BTreeMap::new();
    for key in keys {
        let Some(binding) = &binding else {
            return Err(refused(
                "line::claim-missing",
                "an active claim".to_owned(),
                key.clone(),
                "arm lines only while holding the claim whose round they serve",
            ));
        };
        let root = round_line(read, lane, binding, key)?;
        if roots.insert(root, key.clone()).is_some() {
            return Err(refused(
                "line::root-duplicate",
                "distinct roots".to_owned(),
                key.clone(),
                "name each line once",
            ));
        }
    }
    Ok(Admitted {
        binding,
        roots,
        latest: latest_arm(read, lane)?,
    })
}

/// The root of the open line `key` opens between `lane` and one of its
/// partners in `binding`'s round.
fn round_line(
    read: &journal::JournalRead<'_>,
    lane: CanonicalUuid,
    binding: &ClaimBinding,
    key: &str,
) -> Result<CanonicalUuid, GuardError> {
    let refuse = |reason: &str, expected: &str, detail: &str| {
        refused(reason, expected.to_owned(), key.to_owned(), detail)
    };
    let Some(head) = read.steering_key(key)? else {
        return Err(refuse(
            "line::root-unknown",
            "a peer line's opening key",
            "name a SYN key this lane sent or received",
        ));
    };
    let head = head.body();
    let Some((line, body, _)) = head
        .control
        .as_deref()
        .and_then(wire::parse_peer_line_control)
    else {
        return Err(refuse(
            "line::root-not-peer",
            "a peer line's opening key",
            "operator and Spice steering opens no peer line",
        ));
    };
    if line.root != head.directive || body != wire::body::PeerBodyKind::Open {
        return Err(refuse(
            "line::root-continuation",
            "the line's opening SYN key",
            "name the key of the SYN that opened the line",
        ));
    }
    let Some(state) = read.peer_line_head(line.root)? else {
        return Err(refuse(
            "line::root-unknown",
            "a peer line's opening key",
            "name a SYN key this lane sent or received",
        ));
    };
    if state.terminal().is_some() {
        return Err(refuse(
            "line::root-closed",
            "an open line",
            "the line already closed",
        ));
    }
    let participants = state.participants();
    if participants.len() != 2 || !participants.contains(&Actor::Lane(lane)) {
        return Err(refuse(
            "line::root-not-own",
            "a line between this lane and one other lane",
            "arm only lines this lane takes part in",
        ));
    }
    let partner = participants
        .iter()
        .find(|actor| **actor != Actor::Lane(lane))
        .expect("two distinct participants include another");
    let partner = match partner {
        Actor::Lane(partner) => *partner,
        Actor::Principal(_) => {
            return Err(refuse(
                "line::root-principal",
                "a line with another lane",
                "a principal's line is not a round partner's",
            ));
        }
        Actor::Spice => {
            return Err(refuse(
                "line::root-spice",
                "a line with another lane",
                "Spice is not a round partner",
            ));
        }
    };
    if !peer_wakes_now(read, partner, lane, Some(&binding.task), None, None)? {
        return Err(refuse(
            "line::root-foreign-round",
            "a line with a partner in this claim's task phase round",
            "arm only lines with lanes sharing this claim's round",
        ));
    }
    Ok(line.root)
}

/// `lane`'s newest arm fact and its position.
fn latest_arm(
    read: &journal::JournalRead<'_>,
    lane: CanonicalUuid,
) -> Result<Option<(Seq, wire::body::LineArmed)>, GuardError> {
    let mut before = None;
    loop {
        let page = read.recent_fact_page(
            wire::FactKind::LineArmed,
            before,
            journal::FactReadBudget::RETAINED,
        )?;
        for row in &page.rows {
            if let Some(armed) = row
                .fact
                .read::<wire::body::LineArmed>()
                .filter(|armed| armed.lane == lane)
            {
                return Ok(Some((row.seq, armed)));
            }
        }
        match page.continuation {
            Some(next) => before = Some(next),
            None => return Ok(None),
        }
    }
}

fn refused(reason: &str, expected: String, observed: String, detail: &str) -> GuardError {
    GuardError::Refused(
        journal::GuardRefusal::new(reason)
            .with_evidence("line-arm", Some(expected), Some(observed))
            .with_detail(detail),
    )
}
