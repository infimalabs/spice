//! Statement witnesses over every task a dependency edit can reach.

use super::*;

pub(super) fn append_dependency_reachability_witnesses(
    board: &BoardAnswer,
    starts: &[TaskStamp],
    expected: &mut Vec<Witness>,
) -> Result<(), Outcome<CommandAnswer>> {
    let mut pending = starts.to_vec();
    let mut visited = Vec::new();
    while let Some(task) = pending.pop() {
        if visited.contains(&task) {
            continue;
        }
        visited.push(task.clone());
        let heads = board
            .tasks
            .iter()
            .find(|heads| heads.statement.body.born == task)
            .ok_or_else(|| {
                protocol_fault(
                    "task::board-incoherent",
                    "a stored dependency has no visible board task",
                )
            })?;
        let statement = statement_witness(heads);
        if !expected.contains(&statement) {
            expected.push(statement);
        }
        pending.extend(heads.statement.body.dependencies.iter().cloned());
    }
    Ok(())
}
