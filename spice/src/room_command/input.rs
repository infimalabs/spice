//! The exact principal input a steering key names.
//!
//! A principal input is an ordinary steering head addressed to that principal,
//! so its directive and recipient are the exact route a control names. A head
//! addressed to a lane has none: the lane controls it with keyed prose.

use spice::repository::Repository;
use wire::rooms::PrincipalInputRef;
use wire::{Actor, Outcome, SteeringHead};

use super::{Failure, open, store};

/// The principal input `head` stands for; `None` when a lane receives it.
pub(super) const fn of_head(head: &SteeringHead) -> Option<PrincipalInputRef> {
    match head.recipient {
        Actor::Principal(principal) => Some(PrincipalInputRef {
            directive: head.directive,
            principal,
        }),
        _ => None,
    }
}

/// The principal input whose steering key is `key`, read from `repository`.
///
/// A key no head carries, or one a lane receives, refuses before anything is
/// submitted.
pub(crate) fn resolve<T>(
    repository: &Repository,
    key: &str,
) -> Result<PrincipalInputRef, Outcome<T>> {
    let (_, journal) = open(repository).map_err(Failure::outcome)?;
    let head = journal.read_with(|read| read.steering_key(key)).map_err(
        |(journal::ReadError::Journal(error) | journal::ReadError::Answer(error))| {
            store(error).outcome()
        },
    )?;
    let Some(head) = head else {
        return Err(refusal(
            "room::input-unknown",
            key,
            "name an input by the key `spice room inbox` lists",
        ));
    };
    of_head(head.body()).ok_or_else(|| {
        refusal(
            "room::input-not-principal",
            key,
            "a lane answers its own input with keyed ACK, NAK, or FIN prose",
        )
    })
}

fn refusal<T>(reason: &str, key: &str, detail: &str) -> Outcome<T> {
    Outcome::Refusal(wire::Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason,
            Some("room-input".to_owned()),
            Some("the key of one principal input".to_owned()),
            Some(key.to_owned()),
        )
        .with_detail(detail),
    ))
}
