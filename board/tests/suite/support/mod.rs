use board::{Entry, Snapshot, TaskError};
use journal::Journal;
use wire::TaskStamp;

/// Build one cursor-consistent snapshot from an explicit fixture task set.
pub fn bounded_snapshot(journal: &Journal, tasks: &[TaskStamp]) -> Result<Snapshot, TaskError> {
    let (cursor, heads) = journal
        .read(|read| {
            let heads = tasks
                .iter()
                .map(|task| {
                    read.task_heads_for(task)?
                        .ok_or_else(|| journal::GuardError::Invalid(format!("missing {task}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((read.cursor()?, heads))
        })
        .expect("fixture task generation");
    let entries = heads
        .into_iter()
        .map(|heads| {
            let bodies = heads.into_bodies();
            Entry::new(
                bodies.statement,
                bodies.workflow,
                bodies.completion,
                bodies.claims,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Snapshot::new(
        journal::SCHEMA_VERSION,
        cursor.map_or_else(wire::JournalCursor::empty, wire::JournalCursor::at),
        entries,
    )
    .expect("fixture snapshot schema"))
}

/// An unnamed team evaluated by the same ownership resolver as allocation.
pub fn allocation(team: wire::CanonicalUuid, drain: bool) -> board::Allocation {
    std::sync::Arc::new(board::OwnershipSnapshot::new(
        board::team_policy::Config::default(),
        vec![board::team_policy::Team {
            id: team,
            name: None,
            drive: true,
            drain,
            lanes: vec![team],
        }],
    ))
    .for_team(team)
}
