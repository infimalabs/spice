use super::*;

// Refusal and closed-vocabulary checks that complete the identity-law sweep.

#[test]
fn a_seq_cannot_encode_a_zero_position() {
    // Seq wraps NonZeroU64, so the law above can never sample a zero. The
    // refusal belongs to the decoder, not to a caller-side check.
    let refused = wire::decode::<Seq>(b"0".as_slice());
    assert!(refused.is_err(), "zero is not a position");

    let admitted: Seq = wire::decode(b"1".as_slice()).expect("one is a position");
    assert_eq!(admitted.get(), NonZeroU64::new(1).expect("non-zero").get());
}

/// The closed sets a frame names rather than spells out.
///
/// Each is a fixed vocabulary, so the law that matters is that one variant has
/// exactly one spelling and two variants never share one. Every set here also
/// derives `Ord` and `Hash`, so each owes a stated order: `Law` holds
/// `WitnessKind` in static slices and asks `contains`, and any of them can key
/// a map or sort a rendered column.
///
/// The enums go through [`ranked_law`] because a derived `Ord` ranks by the
/// arm's position, and these arms are declared in the order the domain reads
/// in — a producer chain, a lifecycle, a retirement path — which no vocabulary
/// here spells alphabetically. [`Name`] and [`Digest`] go through
/// [`spelling_law`] instead: both wrap the text itself, so the text is the
/// rank.
///
/// The sweep is over the whole variant set rather than a sample, because a
/// closed set is exactly the case where the whole set is nameable: `ALL` and
/// the enumerated arms cost the same to write as two values and catch the
/// variant a sample would skip. Listing them in declared order is what
/// `ranked_law` wants anyway, so the sweep and the order assertion are the same
/// list.
pub(super) fn cover_vocabularies() {
    ranked_law("CommandKind", CommandKind::ALL);
    ranked_law("FactKind", FactKind::ALL);
    ranked_law("Producer", Producer::ALL);
    ranked_law("WitnessKind", WitnessKind::ALL);
    ranked_law(
        "Disposition",
        &[
            Disposition::Live,
            Disposition::Acked,
            Disposition::Declined,
            Disposition::Lapsed,
            Disposition::DeadLettered,
            Disposition::Canceled,
        ],
    );
    ranked_law(
        "Answer",
        &[
            Answer::Asked,
            Answer::Granted,
            Answer::Denied,
            Answer::Queued,
        ],
    );
    ranked_law(
        "TrustState",
        &[TrustState::Pended, TrustState::Granted, TrustState::Revoked],
    );
    ranked_law(
        "ResidentState",
        &[
            ResidentState::Seated,
            ResidentState::Quiescing,
            ResidentState::Draining,
            ResidentState::Terminating,
            ResidentState::Killing,
            ResidentState::Verifying,
            ResidentState::Dark,
        ],
    );
    spelling_law("Name", &[name("polling"), name("aliases"), name("legacy")]);
    spelling_law(
        "Digest",
        &[
            DIGEST.parse::<Digest>().expect("digest"),
            "0000000000000000000000000000000000000000000000000000000000000000"
                .parse::<Digest>()
                .expect("digest"),
        ],
    );
}
