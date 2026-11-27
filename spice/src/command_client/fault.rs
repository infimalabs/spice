//! How the client names what ended a command it could not complete.

use std::io;
use std::time::{Duration, Instant};

use wire::intent::CommandAnswer;
use wire::{Fault, FaultClass, Outcome};

use super::deadline_stream::{DeadlineStream, IoDirection, IoFailure};
use crate::cli::Contract;

/// Name a transport failure by what ended it.
///
/// The bound elapsing is not a peer that failed: the resident may still be
/// composing the answer, and a reader deciding whether to retry or to
/// investigate the resident needs those two apart. The direction is only
/// consulted once the bound is ruled out, because a socket implementation
/// writes control frames underneath a read the caller asked for.
pub(super) fn transport_fault(
    stream: &DeadlineStream,
    detail: impl Into<String>,
    assumed: IoDirection,
) -> Outcome<CommandAnswer> {
    let failure = stream.last_failure.unwrap_or(IoFailure {
        direction: assumed,
        deadline_elapsed: false,
    });
    // An elapsed bound is the whole story: the detail it arrives with is only
    // the socket's own timeout errno.
    if failure.deadline_elapsed {
        return environment_fault(
            "serve::deadline-elapsed",
            format!(
                "resident did not answer within the {:?} client bound",
                stream.bound
            ),
        );
    }
    match failure.direction {
        IoDirection::Read => environment_fault("serve::receive-failed", detail),
        IoDirection::Write => environment_fault("serve::send-failed", detail),
    }
}

/// Name the unknown effect of a delivered command whose answer never came.
///
/// The deadline is terminal, unlike a dropped socket that the caller replays
/// under the same identity, and a rerun is a new command: a mutation the
/// resident still records would then land twice. Only commands that write
/// facts carry the warning; a read is safe to repeat.
pub(super) fn delivered_unanswered(
    mut outcome: Outcome<CommandAnswer>,
    contract: &Contract,
) -> Outcome<CommandAnswer> {
    if let Outcome::Fault(fault) = &mut outcome
        && fault.reason == "serve::deadline-elapsed"
    {
        let command = contract.path.join(" ");
        fault.detail = format!(
            "`spice {command}` waited for resident authority and its result: {}",
            fault.detail
        );
        if !contract.facts.is_empty() {
            fault.detail.push_str(
                "; the command was delivered and the resident may still record it, so check its effect before running it again",
            );
        }
    }
    outcome
}

/// What the opening handshake was bounded by, kept past the stream it came from.
#[derive(Clone, Copy)]
pub(super) struct OpeningBound {
    pub(super) bound: Duration,
    pub(super) deadline: Instant,
}

/// Name a failed opening by whether the client's own bound is what ended it.
///
/// The handshake consumes the stream, so the direction `transport_fault` reads
/// is out of reach by the time the failure surfaces; the deadline the stream
/// was configured from outlives it. A resident under load accepts the socket
/// long before it answers the upgrade, so this is the first place the bound can
/// elapse, and tungstenite spells that arrival as an interrupted handshake --
/// the same would-block shape a reader must not mistake for a peer that failed.
pub(super) fn opening_fault(
    opening: OpeningBound,
    detail: impl Into<String>,
) -> Outcome<CommandAnswer> {
    if Instant::now() >= opening.deadline {
        return environment_fault(
            "serve::deadline-elapsed",
            format!(
                "resident did not open a socket within the {:?} client bound",
                opening.bound
            ),
        );
    }
    environment_fault("serve::opening-failed", detail)
}

pub(super) fn discovery_unavailable(error: io::Error) -> Fault {
    environment_fault_value("serve::discovery-unavailable", error.to_string())
}

pub(super) fn discovery_replaced() -> Fault {
    environment_fault_value(
        "serve::discovery-replaced",
        "resident discovery changed identity while the current generation was acquired",
    )
}

pub(super) fn capability_disclosed() -> Outcome<CommandAnswer> {
    protocol_fault(
        "serve::capability-disclosed",
        "resident response attempted to disclose the opening capability",
    )
}

pub(super) fn protocol_fault(
    reason: impl Into<String>,
    detail: impl Into<String>,
) -> Outcome<CommandAnswer> {
    Outcome::Fault(protocol_fault_value(reason, detail))
}

pub(super) fn protocol_fault_value(reason: impl Into<String>, detail: impl Into<String>) -> Fault {
    Fault {
        class: FaultClass::Protocol,
        reason: reason.into(),
        detail: detail.into(),
    }
}

pub(super) fn environment_fault(
    reason: impl Into<String>,
    detail: impl Into<String>,
) -> Outcome<CommandAnswer> {
    Outcome::Fault(environment_fault_value(reason, detail))
}

pub(super) fn environment_fault_value(
    reason: impl Into<String>,
    detail: impl Into<String>,
) -> Fault {
    Fault {
        class: FaultClass::Environment,
        reason: reason.into(),
        detail: detail.into(),
    }
}
