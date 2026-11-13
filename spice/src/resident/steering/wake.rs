//! Which pending steering may wake its addressed lane.

use wire::CanonicalUuid;

/// The directive a send opened when its authority is allowed to wake a lane.
///
/// Sends by an actor whose principal holds the operator role retain universal
/// wake authority; resident sends have none. The lifecycle resolver and the
/// inbox's `wakes` flag both answer from this one predicate.
pub(in crate::resident) fn operator_sent(
    roles: &wire::principal::PrincipalRoles,
    envelope: &wire::Envelope<wire::AnyFact>,
) -> Option<CanonicalUuid> {
    let sent = envelope.fact.read::<wire::body::SteerSent>()?;
    roles
        .holds(&envelope.actor, wire::principal::PrincipalRole::Operator)
        .then_some(sent.directive)
}

/// The directive a send opened when it may wake its addressed lane.
///
/// Operator authority remains universal. A peer body keeps its committed
/// admission; typed invitations and their outcomes retain domain authority.
pub(in crate::resident) fn sent_wakes(
    roles: &wire::principal::PrincipalRoles,
    snapshot: &bus::TaskSnapshot,
    envelope: &wire::Envelope<wire::AnyFact>,
) -> Option<CanonicalUuid> {
    operator_sent(roles, envelope).or_else(|| {
        let (sent, wake) = peer_wake(envelope)?;
        let wakes = match wake {
            PeerWake::Committed => true,
            PeerWake::Domain(published) => peer_wakes(
                snapshot,
                sent.sender.lane()?,
                sent.recipient.lane()?,
                sent.room.as_ref(),
                sent.control.as_deref(),
                Some(published),
            ),
        };
        wakes.then_some(sent.directive)
    })
}

/// How one authentic peer directive may wake its lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::resident) enum PeerWake {
    /// Its admission recorded the delivery or start it committed, which no
    /// later round, claim or occupant change withdraws.
    Committed,
    /// A typed invitation or invitation outcome, governed by current domain
    /// authority rather than a conversational admission.
    Domain(wire::Seq),
}

/// The peer directive `envelope` opened and how it may wake its lane.
pub(in crate::resident) fn peer_wake(
    envelope: &wire::Envelope<wire::AnyFact>,
) -> Option<(wire::body::SteerSent, PeerWake)> {
    let (sent, admission) = peer_body(envelope)?;
    let wake = match admission {
        Some(_) => PeerWake::Committed,
        None => PeerWake::Domain(envelope.seq),
    };
    Some((sent, wake))
}

/// The admission cause a peer body's creating fact recorded.
pub(in crate::resident) fn recorded_admission(
    fact: &wire::AnyFact,
) -> Option<wire::body::PeerAdmission> {
    fact.read::<wire::body::SteerMessaged>()
        .map(|body| body.admission)
        .or_else(|| {
            fact.read::<wire::body::SteerQueued>()
                .map(|body| body.admission)
        })
        .flatten()
}

/// A current peer body or typed invitation, authenticated to its sending lane.
fn peer_body(
    envelope: &wire::Envelope<wire::AnyFact>,
) -> Option<(wire::body::SteerSent, Option<wire::body::PeerAdmission>)> {
    let fact = &envelope.fact;
    let (sent, admission) = fact
        .read::<wire::body::SteerSent>()
        .filter(|body| {
            body.control
                .as_deref()
                .is_some_and(|control| control.parse::<wire::TaskInviteControl>().is_ok())
        })
        .map(|body| (body, None))
        .or_else(|| {
            fact.read::<wire::body::SteerMessaged>()
                .and_then(|body| body.admission.map(|cause| (body.as_sent(), Some(cause))))
        })
        .or_else(|| {
            fact.read::<wire::body::SteerQueued>()
                .and_then(|body| body.admission.map(|cause| (body.as_sent(), Some(cause))))
        })
        .or_else(|| {
            fact.read::<wire::body::SteerReplied>()
                .filter(|body| {
                    matches!(
                        body.answer,
                        wire::PeerAnswer::Joined
                            | wire::PeerAnswer::Declined
                            | wire::PeerAnswer::Expired
                    )
                })
                .map(|body| (body.as_sent(), None))
        })?;
    let sender = sent.sender.lane()?;
    (envelope.actor == wire::Actor::Lane(sender)).then_some((sent, admission))
}

/// Whether `from` and `lane` share qualifying current round authority.
/// New body admission judges it once with `published: None`; typed invitation
/// domain authority can additionally bind the original publication position.
///
/// A directive that names a room checks that room. A direct message or reply
/// checks the current rounds shared by its sender and recipient:
/// - a typed task invitation wakes its recipient while the exact claim that
///   issued it is still active, so an occupant that ends before answering
///   hands the invitation to its successor;
/// - a recipient holding an active claim in the round wakes for a sender that
///   holds a claim or arrived there;
/// - a recipient that arrived in the round wakes while the sender still holds
///   its claim there and the recipient took no other work or vendor session
///   since: it holds no active claim anywhere, has won none since that
///   arrival's claim, and every occupant generation after the arrival
///   resumed the arrived session rather than beginning a fresh one. Phase
///   advance clears the round's arrivals and ends this.
///
/// A retained directive may use only a round, claims, and an occupant
/// generation that already existed when it was published, so a generation
/// started after publication permanently revokes that directive's wake while
/// later traffic in the same formation may still wake its resumed successor.
/// `published: None` judges a new publication against current authority
/// inside its guarded write.
pub(in crate::resident) fn peer_wakes(
    snapshot: &bus::TaskSnapshot,
    from: CanonicalUuid,
    lane: CanonicalUuid,
    room: Option<&wire::rooms::RoomAnchor>,
    control: Option<&str>,
    published: Option<wire::Seq>,
) -> bool {
    let recipient = || Ok::<_, std::convert::Infallible>(snapshot_recipient(snapshot, lane));
    if let Some(room) = room {
        return snapshot.task_heads(room).is_some_and(|heads| {
            round_wakes(heads, room, from, lane, control, published, recipient)
                .unwrap_or_else(|never| match never {})
        });
    }
    let recipient = snapshot_recipient(snapshot, lane);
    snapshot.tasks().iter().any(|heads| {
        let room = &heads.statement().body().born;
        round_wakes(heads, room, from, lane, control, published, || {
            Ok::<_, std::convert::Infallible>(recipient)
        })
        .unwrap_or_else(|never| match never {})
    })
}

/// [`peer_wakes`] read from the journal authority a guarded write commits
/// against, so a write that states or acts on a peer wake proves it then.
pub(in crate::resident) fn peer_wakes_now(
    read: &journal::JournalRead<'_>,
    from: CanonicalUuid,
    lane: CanonicalUuid,
    room: Option<&wire::rooms::RoomAnchor>,
    control: Option<&str>,
    published: Option<wire::Seq>,
) -> Result<bool, journal::GuardError> {
    if let Some(room) = room {
        let Some(heads) = read.task_heads_for(room)? else {
            return Ok(false);
        };
        return round_wakes(&heads, room, from, lane, control, published, || {
            journal_recipient(read, lane)
        });
    }
    let recipient = journal_recipient(read, lane)?;
    let mut after = None;
    loop {
        let page = read.task_head_page(after.as_ref(), journal::FactReadBudget::RETAINED)?;
        for heads in &page.rows {
            let room = &heads.statement().body().born;
            if round_wakes(heads, room, from, lane, control, published, || {
                Ok(recipient)
            })? {
                return Ok(true);
            }
        }
        match page.continuation {
            Some(next) => after = Some(next),
            None => return Ok(false),
        }
    }
}

#[derive(Clone, Copy)]
struct Recipient {
    busy: bool,
    latest_claim: Option<wire::Seq>,
    generations: Generations,
}

/// A lane's newest occupant generation and its newest generation that began
/// a vendor session rather than resuming the retained one.
#[derive(Clone, Copy)]
struct Generations {
    latest: Option<wire::Seq>,
    fresh: Option<wire::Seq>,
}

fn snapshot_recipient(snapshot: &bus::TaskSnapshot, lane: CanonicalUuid) -> Recipient {
    Recipient {
        busy: snapshot
            .tasks()
            .iter()
            .any(|task| task.claims().iter().any(|held| active(held, lane))),
        latest_claim: snapshot
            .facts()
            .latest_claim(&wire::Actor::Lane(lane))
            .map(|(claimed, _)| claimed),
        generations: Generations {
            latest: snapshot.facts().latest_lane_generation(lane),
            fresh: snapshot.facts().latest_fresh_lane_generation(lane),
        },
    }
}

fn journal_recipient(
    read: &journal::JournalRead<'_>,
    lane: CanonicalUuid,
) -> Result<Recipient, journal::GuardError> {
    Ok(Recipient {
        busy: read.lane_holds_active_claim(lane)?,
        latest_claim: read.latest_claim_won_by(&wire::Actor::Lane(lane))?,
        generations: journal_generations(read, lane)?,
    })
}

fn journal_generations(
    read: &journal::JournalRead<'_>,
    lane: CanonicalUuid,
) -> Result<Generations, journal::GuardError> {
    let mut generations = Generations {
        latest: None,
        fresh: None,
    };
    let mut before = None;
    loop {
        let page = read.recent_fact_page(
            wire::FactKind::LaneStarted,
            before,
            journal::FactReadBudget::RETAINED,
        )?;
        for row in &page.rows {
            let Some(started) = row
                .fact
                .read::<wire::body::LaneStarted>()
                .filter(|started| started.lane == lane)
            else {
                continue;
            };
            generations.latest.get_or_insert(row.seq);
            if started.fresh_session {
                generations.fresh = Some(row.seq);
                return Ok(generations);
            }
        }
        match page.continuation {
            Some(next) => before = Some(next),
            None => return Ok(generations),
        }
    }
}

/// The peer wake rule over one room's heads. `recipient` answers, only when
/// the arrival rule needs it, whether `lane` holds any active claim, the
/// newest claim its actor won, and its newest and newest fresh generations.
fn round_wakes<E>(
    heads: &journal::TaskHeads,
    room: &wire::rooms::RoomAnchor,
    from: CanonicalUuid,
    lane: CanonicalUuid,
    control: Option<&str>,
    published: Option<wire::Seq>,
    recipient: impl FnOnce() -> Result<Recipient, E>,
) -> Result<bool, E> {
    let existed = |position| published.is_none_or(|sent| position <= sent);
    if !existed(heads.completion().body().round) {
        return Ok(false);
    }
    let holds = |lane| {
        heads
            .claims()
            .iter()
            .any(|held| active(held, lane) && existed(held.claim().body().claimed))
    };
    let arrival = |lane| {
        heads
            .completion()
            .body()
            .arrivals
            .iter()
            .find(|arrival| arrival.lane == lane && existed(arrival.claimed))
    };
    let invited = control
        .and_then(|control| control.parse::<wire::TaskInviteControl>().ok())
        .is_some_and(|control| {
            control.task() == room
                && heads.claims().iter().any(|held| {
                    held.claim().body().claimed == control.claim()
                        && existed(control.claim())
                        && active(held, from)
                })
        });
    if invited {
        return Ok(true);
    }
    if holds(lane) {
        return Ok(holds(from) || arrival(from).is_some());
    }
    let Some(arrival) = arrival(lane).filter(|_| holds(from)) else {
        return Ok(false);
    };
    let recipient = recipient()?;
    Ok(!recipient.busy
        && recipient
            .latest_claim
            .is_none_or(|claimed| claimed <= arrival.claimed)
        && recipient.generations.latest.is_some_and(existed)
        && recipient
            .generations
            .fresh
            .is_none_or(|fresh| fresh < arrival.fact))
}

fn active(held: &journal::ClaimHeads, lane: CanonicalUuid) -> bool {
    let claim = held.claim().body();
    claim.disposition == wire::ClaimDisposition::Active && claim.lane == lane
}
