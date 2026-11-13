//! Admitted host writes join the existing tape-backed authored-message feed.

use super::*;
mod activity;
pub(super) use activity::{Activity, Authored};

pub(in crate::resident) fn advance(
    bus: &bus::BusReader,
    paths: &RuntimePaths,
    publisher: &impl AgentMessagePublisher,
    state: &Mutex<MessageState>,
) -> CommandResult<()> {
    let mut state = lock_state(state)?;
    state.outbox_activity.bind(bus, &paths.tape);
    loop {
        let page = bus
            .selected_fact_page(
                vec![wire::FactKind::OutboxAccepted],
                state.outbox_cursor,
                journal::FactReadBudget::RETAINED,
            )
            .map_err(|error| store_fault("outbox::projection-unreadable", &format!("{error:?}")))?;
        for fact in &page.rows {
            let Some(accepted) = fact.fact.read::<wire::outbox::Accepted>() else {
                continue;
            };
            let decoded = spice::outbox_source::read(&paths.tape, &accepted)
                .map_err(|error| store_fault("outbox::source-unreadable", &error))?;
            let steering = directive_context::steering_ledger(bus, &decoded)?;
            let Some(context) = decoded.context() else {
                continue;
            };
            let events = decoded
                .events()
                .iter()
                .enumerate()
                .map(|(position, event)| (position as u32, event))
                .collect::<Vec<_>>();
            let locus = accepted.hunk.locus;
            publish_agent_record(
                publisher,
                AgentRecord {
                    lane: accepted.lane,
                    author: accepted.occupant,
                    source: Some(accepted.hunk.head.source),
                    locus: listen::Locus::new(locus.member, locus.offset, locus.length),
                    context,
                    events: &events,
                    // The host record carries no vendor time; its admission
                    // is when it was observed, exactly as replay presents it.
                    observed: Some(fact.at),
                    media_source: None,
                    invitation: accepted.invitation.as_ref(),
                },
                &mut state,
                steering.as_ref(),
            )?;
            state.outbox_cursor = Some(fact.seq);
        }
        state.outbox_cursor = page.continuation.or(page.horizon);
        if page.continuation.is_none() {
            return Ok(());
        }
    }
}
