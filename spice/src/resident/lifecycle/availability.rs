use super::*;

/// One reconciliation's view of the board every vacant lane selects from;
/// `'e` is the projection the selected task borrows from.
pub(super) struct BoardView<'a, 'e> {
    pub(super) entries: &'e [Entry],
    pub(super) selection: &'a board::SelectionIndex,
    pub(super) ready_at: &'a BTreeMap<TaskStamp, Instant>,
    pub(super) allocation: &'a board::NextConfig,
    pub(super) environment: &'a wire::Environment,
    pub(super) owners: &'a std::sync::Arc<board::OwnershipSnapshot>,
    pub(super) now: Instant,
}

/// Select current work, with the task selected, or retain the exact deadline
/// of a temporarily blocked phase handoff so the resident reconciles when its
/// prior owner becomes valid.
pub(super) fn selected_ready<'e>(
    view: &BoardView<'_, 'e>,
    actor: &Actor,
    subscription: &board::Allocation,
    match_score: Option<&dyn Fn(&Entry) -> f64>,
) -> CommandResult<(Option<SelectedReady>, Option<&'e Entry>)> {
    let &BoardView {
        entries,
        selection,
        ready_at,
        allocation,
        environment,
        now,
        owners: _,
    } = view;
    let satisfied = selection.resolved();
    let selected = board::select_with_index_at(
        entries,
        board::Claimant {
            actor,
            subscription,
            environment,
            match_score,
        },
        *allocation,
        selection,
        now,
    );
    let Some(selected) = selected else {
        let wait =
            board::blocked_handoff_until(entries, satisfied, actor, subscription, environment, now)
                .map(|at| SelectedReady {
                    at,
                    eligible: 0,
                    immediate: false,
                    handoff_wait: true,
                    last_resort: false,
                });
        return Ok((wait, None));
    };
    let at = ready_instant(selected.entry(), ready_at)?;
    // A phase handoff, or a held row whose open seat someone asked for: no
    // existing occupant can take either instead, so neither waits for an age.
    let entry = selected.entry();
    let immediate = entry.phase_handoff().is_some() || (entry.is_held() && entry.has_capacity());
    let eligible = entries
        .iter()
        .filter(|entry| {
            board::selectable_entry_at(satisfied, entry, actor, subscription, environment, now)
        })
        .count();
    Ok((
        Some(SelectedReady {
            at,
            eligible,
            immediate,
            handoff_wait: false,
            last_resort: selected.last_resort(),
        }),
        Some(selected.entry()),
    ))
}

pub(super) fn ready_instant(
    entry: &Entry,
    ready_at: &BTreeMap<TaskStamp, Instant>,
) -> CommandResult<Instant> {
    ready_at.get(entry.born()).copied().ok_or_else(|| {
        store_fault(
            "lifecycle::task-ready-fact-missing",
            &format!(
                "task {} has no durable READY transition instant",
                entry.handle()
            ),
        )
    })
}
