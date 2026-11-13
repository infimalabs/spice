use std::collections::BTreeSet;

use super::*;
use crate::board_command::Joining;

/// One durable accounting range, the wall-clock instant that observed it, and
/// every complete record the pass handed over in tape order.
#[derive(Clone, Debug)]
pub(super) struct TranscriptObservation {
    pub(super) batch: wire::body::JournalIngested,
    /// Where the handed records begin: the batch start, or earlier when the
    /// pass took over a hold another pass overtook and so hands over records
    /// that pass accounted ahead of this batch.
    pub(super) from: wire::body::TranscriptPosition,
    pub(super) at: Instant,
    pub(super) records: Vec<listen::Record>,
}

impl TranscriptObservation {
    fn new(batch: wire::body::JournalIngested, at: Instant, records: Vec<listen::Record>) -> Self {
        let from = records
            .first()
            .map(|first| wire::body::TranscriptPosition {
                member: first.locus.member,
                offset: first.locus.offset,
            })
            .filter(|first| (first.member, first.offset) < (batch.from.member, batch.from.offset))
            .unwrap_or(batch.from);
        Self {
            batch,
            from,
            at,
            records,
        }
    }
}

/// Results of one accounting sweep and the checkpoint edges it committed.
struct TranscriptSweep {
    refusals: Vec<String>,
    observations: Vec<TranscriptObservation>,
    coalescing: lane::ingest::Coalescing,
    checkpointed: BTreeSet<CanonicalUuid>,
}

/// Declared value owned by one vendor, absent when the fixed table has none.
///
/// The map is closed and shared: a vendor that can be launched is a vendor
/// whose transcript can be read, so the launch door and the ingestion door must
/// never disagree about which spellings those are.
pub(super) fn ingest_vendor(vendor: &wire::Name) -> Option<&'static spice::extension::Vendor> {
    spice::extension::VENDORS
        .iter()
        .find(|entry| entry.vendor.name() == vendor.as_str())
        .map(|entry| entry.vendor)
}

/// Recover transcript accounting, spoken interjection answers, and authored
/// SYN packets, then park peer directives whose lane a removal left behind.
///
/// Retirement and publication are replayed independently: a prior resident
/// may have committed the accounting checkpoint before it could retire the
/// captured answer or publish the captured packet.
pub(super) fn recover_transcripts(
    bus: &Bus,
    paths: &RuntimePaths,
    (epoch, joining): (CanonicalUuid, Joining<'_>),
    config: &steer::Config,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    ingest_transcripts(
        bus,
        paths,
        None,
        lane::ingest::Bound::IMMEDIATE,
        lane::ingest::Coalescing::new(),
        diagnostic,
    )?;
    steering::outbox::recover(bus, paths, (epoch, joining), config)?;
    steering::dispose_at_startup(bus, paths, config, (epoch, joining), diagnostic)?;
    steering::publish_at_startup(bus, paths, epoch, config, diagnostic)?;
    steering::dead_letter_removed_lanes(bus, paths, epoch, diagnostic)
}

/// Account every complete record the named lanes' tapes have gained.
///
/// Live capture passes the one lane whose record just landed; startup catch-up
/// passes nothing and sweeps them all. Both arrive here, so there is exactly
/// one accounting resume rule over the transcript checkpoint. Live display
/// may read ahead of that checkpoint, but only this guarded sweep advances it.
///
/// `bound` decides how much a stream may hold before the sweep accounts it,
/// and the returned coalescing carries whatever is still held to the next
/// sweep. It is passed by value because the writer runs the sweep on its own
/// thread, which cannot borrow the caller's state for the length of a
/// transaction.
///
/// The residency read and every accounting share one writer transaction, so a
/// lane cannot be seated into or out of the sweep midway through it.
pub(super) fn ingest_transcripts(
    bus: &Bus,
    paths: &RuntimePaths,
    only: Option<CanonicalUuid>,
    bound: lane::ingest::Bound,
    coalescing: lane::ingest::Coalescing,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<(Vec<TranscriptObservation>, lane::ingest::Coalescing)> {
    let at = current_instant()?;
    let tapes = paths.tape.clone();
    let TranscriptSweep {
        refusals,
        observations,
        coalescing,
        checkpointed,
    } = bus
        .transact(move |journal| sweep(journal, &tapes, only, at, bound, coalescing))
        .map_err(|error| store_fault("resident::ingest-bus-closed", &error.to_string()))?;
    for refusal in refusals {
        let _ = writeln!(diagnostic, "{refusal}");
    }
    // Closing settled lines rides on this pass but never decides it: the
    // observations above are already committed and still owed to the caller.
    let mut reconcile = |lane| {
        if let Err(outcome) = super::steering::reconcile_lines(bus, lane) {
            let _ = writeln!(
                diagnostic,
                "steer::line-reconcile-failed {}",
                serde_json::to_string(&outcome).unwrap_or_else(|error| error.to_string())
            );
        }
    };
    if only.is_none() {
        reconcile(None);
    } else {
        for lane in checkpointed {
            reconcile(Some(lane));
        }
    }
    Ok((observations, coalescing))
}

/// Ingest each selected lane, keeping the journal range the sweep committed.
///
/// One refused lane does not end the sweep. A tape that drifted is a fact about
/// that lane alone, and stopping would leave every later lane unaccounted for a
/// reason that has nothing to do with it.
fn sweep(
    journal: &mut journal::Journal,
    tapes: &Path,
    only: Option<CanonicalUuid>,
    at: Instant,
    bound: lane::ingest::Bound,
    mut coalescing: lane::ingest::Coalescing,
) -> bus::JournalTransaction<TranscriptSweep> {
    let mut refusals = Vec::new();
    let mut observations = Vec::new();
    let mut range: Option<(wire::Seq, wire::Seq)> = None;
    let mut checkpointed = BTreeSet::new();
    for row in resident_lanes(journal, &mut refusals) {
        if only.is_some_and(|lane| lane != row.lane) {
            continue;
        }
        match ingest_lane_transcript(journal, tapes, &row, at, bound, &mut coalescing) {
            Ok(Some((applied, observation))) => {
                if let Some(applied) = applied {
                    let first = range.map_or(applied.first(), |(first, _)| first);
                    range = Some((first, applied.last()));
                    checkpointed.insert(row.lane);
                }
                observations.push(observation);
            }
            Ok(None) => {}
            Err(refusal) => refusals.push(refusal),
        }
    }
    let result = TranscriptSweep {
        refusals,
        observations,
        coalescing,
        checkpointed,
    };
    match range {
        Some((first, last)) => bus::JournalTransaction::committed(result, first, last),
        None => bus::JournalTransaction::settled(result),
    }
}

/// Lanes this resident currently holds, empty when residency cannot be read.
fn resident_lanes(
    journal: &journal::Journal,
    refusals: &mut Vec<String>,
) -> Vec<wire::LaneResidency> {
    match journal.read(|read| read.residency_head()) {
        Ok(Some(head)) => head.into_body().lanes,
        Ok(None) => Vec::new(),
        Err(error) => {
            refusals.push(format!("resident lanes unreadable: {error}"));
            Vec::new()
        }
    }
}

/// Account one lane's transcript, naming any refusal instead of raising it.
///
/// A lane with no tape yet has nothing to account rather than something wrong:
/// a seat that has never launched an occupant is the ordinary state at startup,
/// so its absent directory is skipped silently instead of reported every sweep.
fn ingest_lane_transcript(
    journal: &mut journal::Journal,
    tapes: &Path,
    row: &wire::LaneResidency,
    at: Instant,
    bound: lane::ingest::Bound,
    coalescing: &mut lane::ingest::Coalescing,
) -> Result<Option<(Option<journal::Applied>, TranscriptObservation)>, String> {
    let Some(vendor) = ingest_vendor(&row.vendor) else {
        return Err(format!(
            "lane {} transcript not accounted: vendor {} has no decoder dialect",
            row.lane,
            row.vendor.as_str()
        ));
    };
    let root = spice::repository::lane_tape(tapes, row.lane).join(row.vendor.as_str());
    if !root.exists() {
        return Ok(None);
    }
    let origin = format!("lane:{}", row.lane)
        .parse::<wire::Origin>()
        .map_err(|error| format!("lane {} origin is not addressable: {error}", row.lane))?;
    // Only the seated occupant's own records close its boundaries: its session
    // declaration is the first record it wrote, and a row whose session still
    // belongs to a predecessor has nothing of the current occupant to close.
    let occupancy = row.occupant.and_then(|occupant| {
        row.session
            .as_ref()
            .filter(|session| session.occupant == occupant && session.vendor == row.vendor)
            .map(|session| lane::ingest::Occupancy {
                occupant,
                from: wire::body::TranscriptPosition {
                    member: session.locus.member,
                    offset: session.locus.offset,
                },
            })
    });
    let ingestion = lane::ingest::Ingestion {
        lane: row.lane,
        vendor: row.vendor.clone(),
        stream: wire::body::TASK_CREATION_CONTEXT_STREAM.to_owned(),
        root,
        dialect: vendor.dialect(),
        actor: Actor::Spice,
        origin,
        at,
        occupancy,
    };
    match lane::ingest::ingest_bounded(journal, ingestion.clone(), coalescing, bound) {
        Ok(lane::ingest::Ingested::Accounted {
            applied,
            batch,
            records,
        }) => Ok(Some((
            Some(applied),
            TranscriptObservation::new(batch, at, records),
        ))),
        // A packet, like the closing line of a room page the lane read, cites
        // its accounted hunk, so a pass that hands one over accounts its hold
        // at once rather than leaving it unpublished or unrecognized.
        Ok(lane::ingest::Ingested::Deferred { batch, records })
            if records.iter().any(|record| {
                super::steering::carries_packet(record)
                    || super::room_catch_up::carries_delivery(record, row.lane)
            }) =>
        {
            let applied = lane::ingest::settle_held(journal, &ingestion, coalescing)
                .map_err(|error| format!("lane {} transcript not accounted: {error}", row.lane))?;
            Ok(Some((
                applied,
                TranscriptObservation::new(batch, at, records),
            )))
        }
        Ok(lane::ingest::Ingested::Deferred { batch, records }) => {
            Ok(Some((None, TranscriptObservation::new(batch, at, records))))
        }
        Ok(lane::ingest::Ingested::Unchanged) => Ok(None),
        Err(error) => Err(format!(
            "lane {} transcript not accounted: {error}",
            row.lane
        )),
    }
}
