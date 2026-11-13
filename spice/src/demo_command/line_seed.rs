//! Conversation lines between the demo's transcript lanes.
//!
//! Iris and Theo write each packet and answer on their own omp tapes, and the
//! resident's live admission publishes, delivers and answers it: one line
//! opened, continued and closed, one declined, one packet whose recipients
//! fare differently, and one room record. Every key an answer heads is the key
//! admission minted for the input it answers.

use std::path::Path;

use journal::{FactCause, JournalWrite};
use wire::{Actor, CanonicalUuid, DemoAnswer, Producer, body};

use super::{
    DEMO_EPOCH, DEMO_IRIS_SEAT, DEMO_JUNO_SEAT, DEMO_RESIDENT_AT, DEMO_TASK, DEMO_THEO_SEAT,
    DemoMessageLane, DemoResult, build_failure, commit_failure, environment_fault, inbox_seed,
    instant, uuid,
};
use crate::resident::message_surface::tape_admission::TapeAdmission;

/// What the resident's admission answers: its refusals and faults.
type Admitted<T> = Result<T, wire::Outcome<()>>;

/// Vacate Juno's seat and remove its lane, so its occupant's address names a
/// lane that left the residency.
pub(super) fn retire_absent_seat(journal: &mut journal::Journal) -> DemoResult<()> {
    inbox_seed::vacate_seat(journal, DEMO_JUNO_SEAT)?;
    let current = journal
        .read(|read| read.residency_head())
        .map_err(|error| environment_fault("demo::journal-invalid", error.to_string()))?
        .expect("demo lane seeding publishes residency");
    let mut residency = current.body().clone();
    let lane = uuid(DEMO_JUNO_SEAT.lane);
    let row = residency
        .lanes
        .iter()
        .find(|row| row.lane == lane)
        .cloned()
        .expect("Juno's vacant seat remains until removed");
    residency.lanes.retain(|row| row.lane != lane);
    residency.topology_revision += 1;
    let epoch = uuid(DEMO_EPOCH);
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("resident:{epoch}")
            .parse()
            .expect("demo resident origin is valid"),
        instant(DEMO_RESIDENT_AT),
        Producer::ResidentRecognition,
    );
    let removed = write
        .fact(
            body::LaneRemoved {
                lane,
                team: row.team,
                worktree: row.worktree,
            },
            FactCause::Existing(current.position()),
        )
        .map_err(build_failure)?;
    write.residency_head(removed, move |_| residency);
    journal.apply(write).map_err(commit_failure)?;
    Ok(())
}

/// Admit the demo's peer lines, then vacate both speakers as their exits
/// would, so no input they were sent waits on a seat.
pub(super) fn seed(root: &Path, state: &spice::repository::StatePaths) -> DemoResult<()> {
    let admission = TapeAdmission::open(root, uuid(DEMO_EPOCH))
        .map_err(crate::typed_outcome::<_, DemoAnswer>)?;
    let played = converse(&admission);
    let closed = admission.close();
    played.map_err(crate::typed_outcome::<_, DemoAnswer>)?;
    closed.map_err(crate::typed_outcome::<_, DemoAnswer>)?;
    let mut journal = journal::Journal::open(state.journal())
        .map_err(|error| environment_fault("demo::journal-unavailable", error.to_string()))?;
    for seat in [DEMO_IRIS_SEAT, DEMO_THEO_SEAT] {
        inbox_seed::vacate_seat(&mut journal, seat)?;
    }
    journal
        .verify()
        .map_err(|error| environment_fault("demo::journal-invalid", error.to_string()))?;
    Ok(())
}

/// One seated transcript speaker.
struct Speaker<'a> {
    admission: &'a TapeAdmission,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
}

impl<'a> Speaker<'a> {
    fn new(admission: &'a TapeAdmission, seat: DemoMessageLane) -> Self {
        Self {
            admission,
            lane: uuid(seat.lane),
            occupant: uuid(seat.occupant),
        }
    }

    fn address(&self) -> Admitted<wire::OccupantAddress> {
        self.admission.address(self.lane, self.occupant)
    }

    /// Write `text` at `minute` past five and admit it.
    fn say(&self, minute: u8, text: &str) -> Admitted<()> {
        let at = format!("2099-07-11T05:{minute:02}:00.000Z");
        self.admission
            .say(self.lane, self.occupant, &at, text, &mut std::io::sink())
    }

    /// The one input pending here that `peer` sent.
    fn heard(&self, peer: &Self) -> Admitted<String> {
        let mut keys = self.admission.peer_keys(self.lane, peer.lane)?;
        match (keys.pop(), keys.is_empty()) {
            (Some(key), true) => Ok(key),
            (key, _) => Err(environment_fault(
                "demo::line-moved",
                format!(
                    "lane {} holds {} inputs from lane {}, not one",
                    self.lane,
                    keys.len() + usize::from(key.is_some()),
                    peer.lane
                ),
            )),
        }
    }
}

fn converse(admission: &TapeAdmission) -> Admitted<()> {
    let iris = Speaker::new(admission, DEMO_IRIS_SEAT);
    let theo = Speaker::new(admission, DEMO_THEO_SEAT);
    let juno = admission.address(uuid(DEMO_JUNO_SEAT.lane), uuid(DEMO_JUNO_SEAT.occupant))?;
    let (iris_address, theo_address) = (iris.address()?, theo.address()?);

    // A line opened, continued with a body, and closed.
    iris.say(
        5,
        &format!(
            "The porcelain rows are drafted.\n\nSYN {theo_address}: Should `spice status \
             --porcelain` keep one row per lane, even for a seat that never started?"
        ),
    )?;
    let opened = theo.heard(&iris)?;
    theo.say(
        6,
        &format!(
            "ACK {opened}: Yes. Keep one row per lane, with `state: \"vacant\"` for a seat that \
             never started, so a consumer can count seats."
        ),
    )?;
    let continued = iris.heard(&theo)?;
    iris.say(
        7,
        &format!("FIN {continued}: Agreed. Every seat now has a row."),
    )?;
    let closed = theo.heard(&iris)?;
    theo.say(8, &format!("ACK {closed}"))?;

    // A line declined.
    theo.say(
        9,
        &format!("SYN {iris_address}: Could porcelain mode drop the table header as well?"),
    )?;
    let asked = iris.heard(&theo)?;
    iris.say(
        10,
        &format!("NAK {asked}: The header belongs to the table; porcelain is a separate mode."),
    )?;
    let declined = theo.heard(&iris)?;
    theo.say(11, &format!("ACK {declined}"))?;

    // One packet to a seated lane and to a lane that left the residency.
    iris.say(
        12,
        &format!(
            "SYN {theo_address} {juno}: The porcelain rows are settled; check that your callers \
             read the `state` field."
        ),
    )?;
    let told = theo.heard(&iris)?;
    theo.say(13, &format!("FIN {told}: My callers already read `state`."))?;
    let answered = iris.heard(&theo)?;
    iris.say(14, &format!("ACK {answered}"))?;

    // A record in the task's room that reaches nobody.
    iris.say(
        15,
        &format!(
            "SYN #{DEMO_TASK}: Porcelain rows settled with Theo: one row per seat, with `state` \
             naming each vacant seat."
        ),
    )
}
