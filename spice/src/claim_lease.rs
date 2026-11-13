//! Shared lease identity for claims minted by the resident lifecycle.

/// Short expiry granted before an automatically launched agent proves health.
pub const AUTOMATIC_PROVISIONAL_SECONDS: i64 = 60;

/// Horizon available after an automatically launched agent proves health.
pub const AUTOMATIC_MAXIMUM_SECONDS: i64 = 60 * 60;

/// Whether a lease is represented by the canonical indefinite horizon.
#[must_use]
pub fn is_indefinite(lease: &wire::LeaseHead) -> bool {
    lease.expiry.is_indefinite() && lease.maximum.is_indefinite()
}

/// The same lease iced on the canonical indefinite horizon, keeping its start,
/// renewal, and epoch.
#[must_use]
pub fn iced(lease: &wire::LeaseHead) -> wire::LeaseHead {
    let horizon = wire::Instant::indefinite();
    wire::LeaseHead {
        expiry: horizon,
        maximum: horizon,
        ..lease.clone()
    }
}

/// Whether one durable claim fact identifies a resident-minted automatic claim.
#[must_use]
pub fn is_automatic_claim_fact(
    claim: &wire::ClaimHead,
    envelope: &wire::Envelope<wire::AnyFact>,
) -> bool {
    if envelope.seq != claim.claimed {
        return false;
    }
    let Some(won) = envelope.fact.read::<wire::body::ClaimWon>() else {
        return false;
    };
    envelope
        .at
        .get()
        .checked_add(jiff::SignedDuration::from_secs(
            AUTOMATIC_PROVISIONAL_SECONDS,
        ))
        .is_ok_and(|expiry| won.until.get() == expiry)
}
