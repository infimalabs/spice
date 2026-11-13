//! Light usage statistics kept beside each immutable receipt.
//!
//! A receipt's bytes are its identity, so its counters live in a mutable
//! sidecar at `receipt-stats/<2 hex>/<rest>`, keyed like the receipt itself.
//! Updates run under the store lock and only for receipts still present;
//! collection removes the sidecar before its receipt.

use serde::{Deserialize, Serialize};
use wire::Instant;

use super::{
    EvidenceStore, EvidenceStoreError, Layout, ReceiptId, decode_canonical, publish_json,
    read_optional_regular,
};

const STATS_VERSION: u64 = 1;
const STATS_LIMIT: u64 = 4 * 1024;

/// How often `gate prove` reused one receipt and committed admissions consumed it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptStats {
    version: u64,
    receipt: ReceiptId,
    uses: u64,
    last_used_at: Option<Instant>,
    admissions: u64,
    last_admitted_at: Option<Instant>,
}

impl ReceiptStats {
    /// Times `gate prove` reused this receipt instead of running its action.
    #[must_use]
    pub const fn uses(&self) -> u64 {
        self.uses
    }

    /// When `gate prove` last reused this receipt.
    #[must_use]
    pub const fn last_used_at(&self) -> Option<Instant> {
        self.last_used_at
    }

    /// Times a committed reference transaction consumed an admission naming it.
    #[must_use]
    pub const fn admissions(&self) -> u64 {
        self.admissions
    }

    /// When a committed admission last consumed this receipt.
    #[must_use]
    pub const fn last_admitted_at(&self) -> Option<Instant> {
        self.last_admitted_at
    }
}

impl EvidenceStore {
    /// Count one reuse at `at` of each listed receipt still stored.
    pub fn record_receipt_uses(
        &self,
        receipts: &[ReceiptId],
        at: Instant,
    ) -> Result<(), EvidenceStoreError> {
        self.with_lock(|layout| record(layout, receipts, Event::Use, at))
    }

    /// Read each receipt's usage statistics, `None` where nothing was counted.
    pub fn receipt_stats(
        &self,
        receipts: &[ReceiptId],
    ) -> Result<Vec<Option<ReceiptStats>>, EvidenceStoreError> {
        self.with_lock(|layout| {
            receipts
                .iter()
                .map(|receipt| read(layout, receipt))
                .collect()
        })
    }
}

#[derive(Clone, Copy)]
pub(super) enum Event {
    Use,
    Admission,
}

/// Count `event` once per listed receipt that is still stored.
pub(super) fn record(
    layout: &Layout,
    receipts: &[ReceiptId],
    event: Event,
    at: Instant,
) -> Result<(), EvidenceStoreError> {
    for receipt in receipts {
        if !layout.object_path(receipt).is_file() {
            continue;
        }
        // An unreadable sidecar restarts its counts rather than wedging every
        // later update; the receipt it describes is unaffected.
        let mut stats = read(layout, receipt)
            .ok()
            .flatten()
            .unwrap_or_else(|| ReceiptStats {
                version: STATS_VERSION,
                receipt: receipt.clone(),
                uses: 0,
                last_used_at: None,
                admissions: 0,
                last_admitted_at: None,
            });
        match event {
            Event::Use => {
                stats.uses = stats.uses.saturating_add(1);
                stats.last_used_at = Some(at);
            }
            Event::Admission => {
                stats.admissions = stats.admissions.saturating_add(1);
                stats.last_admitted_at = Some(at);
            }
        }
        publish_json(&layout.receipt_stats_path(receipt), &stats)?;
    }
    Ok(())
}

pub(super) fn read(
    layout: &Layout,
    receipt: &ReceiptId,
) -> Result<Option<ReceiptStats>, EvidenceStoreError> {
    let Some(bytes) = read_optional_regular(&layout.receipt_stats_path(receipt), STATS_LIMIT)?
    else {
        return Ok(None);
    };
    let stats: ReceiptStats = decode_canonical(&bytes, "gate-evidence::receipt-stats-invalid")?;
    if stats.version != STATS_VERSION || &stats.receipt != receipt {
        return Err(EvidenceStoreError::new(
            "gate-evidence::receipt-stats-invalid",
            format!("stats for {receipt} carry another version or receipt"),
        ));
    }
    Ok(Some(stats))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::evidence_store::derive_action_key;
    use crate::evidence_store::invariant_fixture::{Fixture, receipt};

    #[test]
    fn sharded_receipts_keep_counts_beside_them_until_collected() {
        let fixture = Fixture::new("receipt-stats");
        let store = fixture.store();
        let action = derive_action_key(b"stats action");
        let receipts = [receipt(&action, b"first"), receipt(&action, b"second")];
        for receipt in &receipts {
            store.publish(receipt).unwrap();
        }
        let selected = store.lookup(&action).unwrap().unwrap().id().unwrap();
        let (kept, dropped) = {
            let ids = receipts.each_ref().map(|receipt| receipt.id().unwrap());
            let [first, second] = ids;
            if first == selected {
                (first, second)
            } else {
                (second, first)
            }
        };
        let object = store.layout.object_path(&kept);
        assert_eq!(
            object.parent().unwrap().file_name().unwrap(),
            &kept.as_str()[..2]
        );
        assert!(!store.layout.objects.join(kept.as_str()).exists());
        let bytes = fs::read(&object).unwrap();

        let used: Instant = "2026-09-04T12:01:00.000Z".parse().unwrap();
        let admitted: Instant = "2026-09-04T12:02:00.000Z".parse().unwrap();
        let absent = receipt(&derive_action_key(b"never stored"), b"x")
            .id()
            .unwrap();
        store
            .record_receipt_uses(&[kept.clone(), kept.clone(), dropped.clone()], used)
            .unwrap();
        store
            .record_receipt_uses(std::slice::from_ref(&absent), used)
            .unwrap();
        store
            .with_lock(|layout| {
                record(
                    layout,
                    std::slice::from_ref(&kept),
                    Event::Admission,
                    admitted,
                )
            })
            .unwrap();

        let [kept_stats, dropped_stats, absent_stats] = store
            .receipt_stats(&[kept.clone(), dropped.clone(), absent])
            .unwrap()
            .try_into()
            .unwrap();
        let kept_stats = kept_stats.unwrap();
        assert_eq!(
            (kept_stats.uses(), kept_stats.last_used_at()),
            (2, Some(used))
        );
        assert_eq!(
            (kept_stats.admissions(), kept_stats.last_admitted_at()),
            (1, Some(admitted))
        );
        assert_eq!(dropped_stats.unwrap().uses(), 1);
        assert_eq!(absent_stats, None);
        assert_eq!(fs::read(&object).unwrap(), bytes);
        assert_eq!(store.lookup(&action).unwrap().unwrap().id().unwrap(), kept);

        fs::write(store.layout.objects.join("stray"), b"not sharded").unwrap();
        let report = store
            .collect(
                SystemTime::now() + Duration::from_secs(3600),
                Duration::ZERO,
                usize::MAX,
            )
            .unwrap();
        assert_eq!(report.removed_objects, 1);
        assert!(report.retained.iter().any(|line| line.contains("stray")));
        assert!(!store.layout.object_path(&dropped).exists());
        assert!(!store.layout.receipt_stats_path(&dropped).exists());
        assert_eq!(
            store.receipt_stats(std::slice::from_ref(&kept)).unwrap()[0],
            Some(kept_stats)
        );
    }
}
