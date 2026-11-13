//! Typed wire-response fixtures shared by the app integration suites.
//!
//! Tests still feed bytes through [`app::Session`], but valid response bytes
//! originate here from [`wire::Response`]. A required response or answer field
//! therefore breaks the fixture at compile time instead of failing later as an
//! unrelated `frame::undecodable` assertion.

use serde::Serialize;
use wire::{CanonicalUuid, Outcome, Response, WireVersion};

pub fn response<B: Serialize>(
    version: WireVersion,
    id: CanonicalUuid,
    epoch: CanonicalUuid,
    outcome: Outcome<B>,
) -> Vec<u8> {
    wire::encode(&Response {
        v: version,
        id,
        epoch,
        outcome,
    })
    .expect("typed wire response fixture encodes")
}
