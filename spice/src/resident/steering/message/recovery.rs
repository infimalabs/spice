use super::*;
/// Reread each lane's recent tail for packets a crash left unpublished.
///
/// Live publication commits after the sweep that accounted its record, so a
/// resident that stopped between the two leaves a packet on the tape with no
/// message. Every record after a lane's current session declaration was
/// written by the occupant that declared it; rereading them publishes what is
/// missing, finds the rest already published, and leaves every packet an
/// earlier judgment withheld unpublished. Each wholly withheld packet, newly
/// judged or not, opens the correction its judgment owes when a crash left
/// that unopened, and finds the one already opened otherwise, so a restart
/// neither loses nor repeats a correction. Grammar refusals and partial
/// misses remain diagnostics only, since a live read may already have told
/// them.
///
/// Recovery runs before this resident seats any new occupant, so a packet a
/// crash left unjudged resolves against no occupant born after the previous
/// resident stopped. Each lane owns a fresh wall-time allowance; work on one
/// tape never spends another tape's scan budget.
pub(in crate::resident) fn publish_at_startup(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    config: &steer::Config,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let residency = current_residency(bus, epoch)?;
    let view = bus
        .task_snapshot()
        .map_err(|error| store_fault("steer::residency-unreadable", &error.to_string()))?;
    for row in &residency.body().lanes {
        if let Err(detail) = recover_lane(bus, paths, epoch, config, &view, row, diagnostic) {
            let _ = writeln!(
                diagnostic,
                "lane {} SYN recovery incomplete: {detail}",
                row.lane
            );
        }
    }
    Ok(())
}

fn recover_lane(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    config: &steer::Config,
    view: &bus::TaskSnapshot,
    row: &wire::LaneResidency,
    diagnostic: &mut dyn io::Write,
) -> Result<(), String> {
    let Some(session) = row
        .session
        .as_ref()
        .filter(|session| session.lane == row.lane)
    else {
        return Ok(());
    };
    // An occupant with no birth on its lane authors nothing publishable.
    let Ok(birth) = view.facts().occupant_address(row.lane, session.occupant) else {
        return Ok(());
    };
    let withheld = withheld_packets(bus, row.lane, session.occupant, Some(birth.birth()))?;
    let path = paths.lane_tape(row.lane).join(row.vendor.as_str());
    if !path.is_dir() {
        return Ok(());
    }
    let Some(vendor) = spice::extension::VENDORS
        .iter()
        .find(|entry| entry.vendor.name() == row.vendor.as_str())
        .map(|entry| entry.vendor)
    else {
        return Ok(());
    };
    let budget = listen::TailBudget::new(
        RECOVERY_WINDOW_BYTES,
        RECOVERY_SOURCE_BYTES,
        RECOVERY_RECORDS,
        RECOVERY_PASSES,
        RECOVERY_DURATION,
    )
    .map_err(|error| error.to_string())?;
    let declared = (session.locus.member, session.locus.offset);
    let mut newest_first = Vec::new();
    let search = listen::search_tail(
        &path,
        vendor.dialect(),
        &[steer::ack::scan::message::MARKER.as_bytes()],
        budget,
        |record| {
            if (record.locus.member, record.locus.offset) <= declared {
                return ControlFlow::Break(());
            }
            if let listen::RecordPayload::Events(events) = record.payload {
                newest_first.push((
                    record.locus,
                    events
                        .into_iter()
                        .map(|sourced| sourced.event)
                        .collect::<Vec<_>>(),
                ));
            }
            ControlFlow::Continue(())
        },
    )
    .map_err(|error| error.to_string())?;
    let source = search
        .source
        .as_str()
        .parse::<CanonicalUuid>()
        .map_err(|error| error.to_string())?;
    for (locus, events) in newest_first.iter().rev() {
        publish_record(
            bus,
            paths,
            epoch,
            config,
            &AuthoredRecord {
                lane: row.lane,
                author: session.occupant,
                source,
                locus: *locus,
                events,
            },
            Pass::Recovery(&withheld),
            diagnostic,
        );
    }
    Ok(())
}
