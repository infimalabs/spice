//! Replay the durable notice obligation, not the membership operation.

use std::fmt::Write as _;
use std::time::Duration;

use super::*;
use crate::resident::steering::{
    InternalDirective, append_internal_directive, control_recorded, internal_directive_identity,
    internal_write, notify_inbox,
};

pub(in crate::resident) fn reconcile(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    snapshot: &bus::TaskSnapshot,
    cursor: Option<Seq>,
    lifetime: Duration,
) -> CommandResult<bool> {
    let Some(residency) = snapshot.residency() else {
        return Ok(false);
    };
    let mut changed = false;
    for envelope in snapshot.facts().after(cursor) {
        let Some(notice) = envelope.fact.read::<wire::body::TeamDepartureNoticed>() else {
            continue;
        };
        if !residency
            .body()
            .lanes
            .iter()
            .any(|row| row.lane == notice.lane)
        {
            continue;
        }
        let control = format!("team-departure:{}", envelope.seq);
        let directive = internal_directive_identity(&format!("lane:{}:{control}", notice.lane));
        if control_recorded(bus, directive, notice.lane, &control)? {
            continue;
        }
        let mut text = String::from(
            "Your team changed. Your new team's drive policy does not admit this held work:\n",
        );
        for task in &notice.tasks {
            writeln!(text, "- {task}").expect("formatting into a string cannot fail");
        }
        text.push_str(
            "Free each named task unless you are about to finish or pass it cleanly. Your claims remain held; no task was moved or released for you.",
        );
        let mut write = internal_write(epoch)?;
        crate::resident::shutdown::guard_residency(&mut write, residency);
        append_internal_directive(
            bus,
            &mut write,
            InternalDirective {
                directive,
                lane: notice.lane,
                text,
                control,
                lifetime,
                cause: FactCause::Existing(envelope.seq),
            },
        )?;
        commit(bus, write)?;
        notify_inbox(paths, notice.lane);
        changed = true;
    }
    Ok(changed)
}
