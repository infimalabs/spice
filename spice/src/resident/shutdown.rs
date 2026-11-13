use bus::Bus;
use journal::{FactCause, JournalGuard, JournalWrite};
use lane::occupant::Seats;
use wire::{
    Actor, CanonicalUuid, LaneRenewalRecord, LaneResidency, Outcome, Producer, Refusal,
    ResidencyHead, ResidentState, TeamSubscription,
};

use super::{CommandResult, RuntimePaths, commit, committed_reply, store_fault, write};

/// Release one resident generation without ending supervisor-owned work.
///
/// A resident is replaceable coordination authority. Lane supervisors own the
/// long-lived agent process groups and their transcript tapes, so stopping the
/// resident must neither wait for them nor signal them. The next resident
/// generation adopts the same supervisors from the occupied durable rows.
///
/// The guarded commit is the proof that the dark fact is sound. Auditing the
/// whole journal here would grow with the journal, and `spice journal verify`
/// already owns that audit.
pub(super) fn drive_shutdown(
    bus: &Bus,
    _paths: &RuntimePaths,
    epoch: CanonicalUuid,
    seats: &mut Seats,
    author: Actor,
) -> Outcome<()> {
    let observed = match current_residency(bus, epoch) {
        Ok(observed) => observed,
        Err(outcome) => return outcome,
    };
    if observed.body().state == ResidentState::Dark {
        let position = observed.position().get();
        seats.release_all();
        return committed_reply(position, position);
    }

    let mut next = observed.body().clone();
    next.state = ResidentState::Dark;
    let mut ending = match write(author, epoch, Producer::CommandDoor) {
        Ok(write) => write,
        Err(outcome) => return outcome,
    };
    guard_residency(&mut ending, &observed);
    let dark = match ending.fact(
        wire::body::UpDark { epoch },
        FactCause::Existing(observed.position()),
    ) {
        Ok(dark) => dark,
        Err(error) => return store_fault("journal::write-invalid", &error.to_string()),
    };
    ending.residency_head(dark, move |_| next);
    let committed = match commit(bus, ending) {
        Ok(committed) => committed,
        Err(outcome) => return outcome,
    };
    seats.release_all();
    committed_reply(committed.first(), committed.last())
}

pub(super) fn current_residency(
    bus: &Bus,
    epoch: CanonicalUuid,
) -> CommandResult<journal::JournalHead<ResidencyHead>> {
    let head = bus
        .residency_head()
        .map_err(|error| store_fault("resident::head-unreadable", &error.to_string()))?
        .ok_or_else(|| store_fault("resident::head-missing", "residency head is absent"))?;
    if head.body().epoch != epoch {
        return Err(store_fault(
            "resident::epoch-moved",
            &format!("expected {epoch}, observed {}", head.body().epoch),
        ));
    }
    Ok(head)
}

/// The current head a launch proven off the resident loop commits against.
///
/// Session proof runs while unrelated commits proceed, so a proven occupant
/// revalidates only its own lane's authority: the resident generation and life,
/// the lane row, its team's subscription, and its renewal records. When that
/// authority is unchanged the start folds onto the current head; otherwise it
/// refuses exactly as a moved head would.
pub(super) fn rebase_lane(
    bus: &Bus,
    epoch: CanonicalUuid,
    admitted: &journal::JournalHead<ResidencyHead>,
    lane: CanonicalUuid,
) -> CommandResult<journal::JournalHead<ResidencyHead>> {
    let current = current_residency(bus, epoch)?;
    if current.position() == admitted.position()
        || lane_authority(current.body(), lane) == lane_authority(admitted.body(), lane)
    {
        return Ok(current);
    }
    Err(Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            "resident::head-moved".to_owned(),
            Some("lane".to_owned()),
            Some(lane.to_string()),
            None,
        )
        .with_detail("the lane's own residency changed while its occupant proved its session"),
    )))
}

type LaneAuthority<'h> = (
    &'h ResidentState,
    Option<&'h LaneResidency>,
    Option<&'h TeamSubscription>,
    Vec<&'h LaneRenewalRecord>,
);

fn lane_authority(head: &ResidencyHead, lane: CanonicalUuid) -> LaneAuthority<'_> {
    let row = head.lanes.iter().find(|row| row.lane == lane);
    let subscription = row.and_then(|row| {
        head.subscriptions
            .iter()
            .find(|subscription| subscription.team == row.team)
    });
    let renewals = head
        .renewals
        .iter()
        .filter(|renewal| renewal.lane == lane)
        .collect();
    (&head.state, row, subscription, renewals)
}

pub(crate) fn guard_residency(
    write: &mut JournalWrite,
    observed: &journal::JournalHead<ResidencyHead>,
) {
    let expected = observed.clone();
    write.guard(JournalGuard::new(move |read| {
        let Some(current) = read.residency_head()? else {
            return Err(journal::GuardError::refused("resident::head-missing"));
        };
        if current != expected {
            return Err(journal::GuardError::refused("resident::head-moved"));
        }
        Ok(())
    }));
}
