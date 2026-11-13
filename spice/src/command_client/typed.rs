//! Already-typed board and task family command mapping.
//!
//! This seam keeps family mapping growth out of the transport client while
//! preserving its one authenticated request path.

use std::ops::Deref;

use spice::repository::Repository;

use super::*;

/// The repository a resident command addresses, paired with the process
/// environment its client and Git reads see.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Host<'a> {
    pub(crate) repository: &'a Repository,
    pub(crate) ambient: &'a Ambient,
}

impl<'a> Host<'a> {
    pub(crate) const fn new(repository: &'a Repository, ambient: &'a Ambient) -> Self {
        Self {
            repository,
            ambient,
        }
    }
}

impl Deref for Host<'_> {
    type Target = Repository;

    fn deref(&self) -> &Repository {
        self.repository
    }
}

/// Submit one already-typed family request through the sole resident client.
pub(crate) fn execute_typed<B: Serialize>(
    host: Host<'_>,
    contract: &Contract,
    kind: CommandKind,
    expected: Vec<Witness>,
    body: &B,
) -> Outcome<CommandAnswer> {
    let Some(mapped_kind) = kind_for(contract.path) else {
        return protocol_fault(
            "command::mapping-unsupported",
            "selected CLI contract has no resident command mapping",
        );
    };
    if mapped_kind != kind
        && !matches!(
            (contract.path, kind),
            (["task", "next"], CommandKind::ClaimRefresh)
                | (["task", "join"], CommandKind::TaskInvite)
        )
    {
        return protocol_fault(
            "command::mapping-mismatch",
            "selected CLI contract does not own the requested wire kind",
        );
    }
    let mapped = match MappedCommand::new(contract, contract.path, kind, expected, body) {
        Ok(mapped) => mapped,
        Err(outcome) => return outcome,
    };
    let state = match host.admit_state() {
        Ok(state) => state,
        Err(error) => return crate::state_failure(&error).outcome(),
    };
    let connection = match ambient_lane_identity(host.ambient) {
        Ok(Some(lane)) => Connection::Lane(lane),
        Ok(None) => Connection::Operator,
        Err(outcome) => return outcome,
    };
    execute_mapped(
        state.serve(),
        contract,
        connection,
        Ok(mapped),
        host.ambient,
    )
}

pub(super) fn kind_for(path: &[&str]) -> Option<CommandKind> {
    Some(match path {
        ["board"] => CommandKind::BoardRead,
        ["lane", "list"] => CommandKind::BoardRead,
        ["board", "graph"] => CommandKind::BoardGraph,
        ["board", "handout"] => CommandKind::BoardHandout,
        ["room", "post"] => CommandKind::RoomPost,
        ["room", "score"] => CommandKind::RoomScore,
        ["room", "leave"] => CommandKind::RoomLeave,
        ["room", "contribute"] => CommandKind::RoomContribute,
        ["room", "ack" | "nak" | "fin"] => CommandKind::SteerLineControl,
        ["design", "decide"] => CommandKind::PreparationDecide,
        ["principal", verb @ ..] => return principal_kind_for(verb),
        ["task", verb] => return task_kind_for(verb),
        ["taste", "add"] | ["taste", "drop"] => CommandKind::TaskAdd,
        ["doc", "ledger"] => CommandKind::DocLedger,
        ["sweep"] => CommandKind::TaskSweep,
        _ => return None,
    })
}

fn task_kind_for(verb: &str) -> Option<CommandKind> {
    Some(match verb {
        "list" | "status" | "show" => CommandKind::BoardRead,
        "adopt" => CommandKind::OutputAdopt,
        "next" | "recover" | "capture" => CommandKind::ClaimWin,
        "add" | "plan" | "design" => CommandKind::TaskAdd,
        "oops" => CommandKind::TaskOopsAdd,
        "done" => CommandKind::TaskDone,
        "review" => CommandKind::TaskReview,
        "note" => CommandKind::TaskNote,
        "defer" => CommandKind::TaskDefer,
        "wake" => CommandKind::TaskWake,
        "target" => CommandKind::TaskTarget,
        "file" => CommandKind::TaskFile,
        "place" => CommandKind::TaskPlace,
        "free" => CommandKind::ClaimFree,
        "join" => CommandKind::TaskJoin,
        "depends" => CommandKind::TaskDepends,
        "modify" | "require" => CommandKind::TaskEdit,
        "delete" => CommandKind::TaskDelete,
        "close" => CommandKind::TaskClose,
        "conclude" => CommandKind::TaskConclude,
        _ => return None,
    })
}

fn principal_kind_for(verb: &[&str]) -> Option<CommandKind> {
    Some(match verb {
        ["list"] => CommandKind::PrincipalRead,
        ["admit"] => CommandKind::PrincipalAdmit,
        ["profile"] => CommandKind::PrincipalEdit,
        ["role", "grant"] => CommandKind::PrincipalGrant,
        ["role", "revoke"] => CommandKind::PrincipalRevoke,
        ["deauthorize"] => CommandKind::PrincipalDeauthorize,
        _ => return None,
    })
}
