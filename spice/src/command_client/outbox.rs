//! Native resource transport; no CLI action or model-authored author field.

use super::*;

const PATH: &[&str] = &["spice://outbox"];

fn contract(read: bool) -> Contract {
    Contract {
        path: PATH,
        syntax: "spice://outbox",
        principal: Principal::Lane,
        journal: if read {
            crate::cli::JournalAccess::Read
        } else {
            crate::cli::JournalAccess::Command
        },
        reply_position: if read {
            ReplyPositionLaw::Snapshot
        } else {
            ReplyPositionLaw::SnapshotOrCommitted
        },
        facts: &[wire::FactKind::OutboxAccepted, wire::FactKind::OutboxJudged],
        guards: &[Guard::Resident],
        tools: &[],
        result: crate::cli::ResultClass::Reply,
        outcomes: crate::cli::ExitOutcome::ALL,
        battery: "native authored-outbox resource",
    }
}

pub(crate) fn execute_outbox<B: Serialize>(
    host: Host<'_>,
    lane: CanonicalUuid,
    kind: CommandKind,
    body: &B,
) -> Outcome<CommandAnswer> {
    let contract = contract(kind.is_read());
    let state = match host.admit_state() {
        Ok(state) => state,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    execute_mapped(
        state.serve(),
        &contract,
        Connection::Lane(lane),
        MappedCommand::new(&contract, PATH, kind, Vec::new(), body),
        host.ambient,
    )
}
