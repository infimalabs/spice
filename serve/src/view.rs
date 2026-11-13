//! Packing helpers for direct app authority reads.
//!
//! App axes are assembled by the live surface from their owning authorities
//! and are never stored as a second durable projection.

/// Pack already-ranked rows without hiding overflow.
#[must_use]
pub fn pack_ranked<T>(
    mut rows: Vec<T>,
    limit: usize,
    horizon: wire::Seq,
    position: impl Fn(&T) -> wire::Seq,
) -> wire::Packed<T> {
    let overflow = (rows.len() > limit).then(|| wire::Overflow {
        omitted: u64::try_from(rows.len() - limit).expect("a vector length fits in u64"),
        continuation: position(&rows[limit]),
    });
    rows.truncate(limit);
    wire::Packed {
        rows,
        overflow,
        horizon,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(value: u64) -> wire::Seq {
        wire::Seq::new(value).expect("non-zero fixture position")
    }

    #[test]
    fn ranked_packing_names_omission_continuation_and_horizon() {
        let packed = pack_ranked(vec![seq(1), seq(2), seq(3)], 2, seq(9), |row| *row);

        assert_eq!(packed.rows, vec![seq(1), seq(2)]);
        assert_eq!(
            packed.overflow,
            Some(wire::Overflow {
                omitted: 1,
                continuation: seq(3),
            })
        );
        assert_eq!(packed.horizon, seq(9));
    }
}
