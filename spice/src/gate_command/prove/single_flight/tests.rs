use std::fs;
use std::thread;
use std::time::{Duration, SystemTime};

use gate::{
    EvidenceProducerRecord, LeaseAcquisition, PassReceipt, PassReceiptFields, derive_action_key,
};
use wire::{Digest, Domain};

use super::{Claim, LeaseGuard, Timing, acquired, claim, claim_with_timing};

/// How long a fixture producer holds its lease: long enough that a loaded
/// harness never sees it expire behind a waiter, which would let the waiter
/// recover the lease instead of observing the producer's outcome.
const PRODUCER_LEASE: Duration = Duration::from_secs(60 * 60);

struct Fixture {
    root: std::path::PathBuf,
    store: gate::EvidenceStore,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "spice-proof-flight-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple(),
        ));
        fs::create_dir(&root).expect("create single-flight fixture");
        let store = gate::EvidenceStore::open(&root).expect("open single-flight evidence store");
        Self { root, store }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove single-flight fixture");
    }
}

#[test]
fn explicit_claim_reuses_an_equivalent_indexed_receipt_before_execution() {
    let fixture = Fixture::new("equivalent-receipt");
    let action = derive_action_key(b"canonical action closure");
    let published = receipt(&action);
    fixture.store.publish(&published).unwrap();

    let Ok(Claim::Present(report)) = claim(&fixture.store, "explicit", "formatters", &action)
    else {
        panic!("explicit proof consumes the equivalent receipt");
    };
    assert_eq!(report.action_key, action);
    assert_eq!(report.receipt, published.id().unwrap());
    assert!(
        fs::read_dir(fixture.store.root().join("leases"))
            .unwrap()
            .next()
            .is_none(),
        "equivalent receipt reuse acquires no execution lease",
    );
}

#[test]
fn waiter_consumes_the_producers_receipt() {
    let fixture = Fixture::new("success");
    let action = derive_action_key(b"shared success");
    let LeaseAcquisition::Acquired { lease, .. } = fixture
        .store
        .acquire_lease(&action, "producer", SystemTime::now(), PRODUCER_LEASE)
        .unwrap()
    else {
        panic!("producer acquires action");
    };
    let waiting_store = fixture.store.clone();
    let waiting_action = action.clone();
    // The receipt is durable store state, so the waiter consumes it whether it
    // finds the producer still working or arrives after publication.
    let waiter = thread::spawn(move || claim(&waiting_store, "waiter", "shared", &waiting_action));
    let receipt = receipt(&action);
    fixture
        .store
        .publish_batch_leased([(&receipt, &lease)], SystemTime::now())
        .unwrap();
    fixture.store.release_lease(&lease).unwrap();

    let Ok(Claim::Present(report)) = waiter.join().expect("waiter thread completes") else {
        panic!("waiter consumes published receipt");
    };
    assert_eq!(report.action_key, action);
}

#[test]
fn waiter_receives_the_producers_terminal_failure() {
    let fixture = Fixture::new("failure");
    let action = derive_action_key(b"shared failure");
    let LeaseAcquisition::Acquired { lease, .. } = fixture
        .store
        .acquire_lease(&action, "producer", SystemTime::now(), PRODUCER_LEASE)
        .unwrap()
    else {
        panic!("producer acquires action");
    };
    let waiting_store = fixture.store.clone();
    let waiting_action = action.clone();
    // The terminal failure is durable lease state, so the waiter receives it
    // whether it is already waiting or arrives after the producer failed.
    let waiter = thread::spawn(move || claim(&waiting_store, "waiter", "shared", &waiting_action));
    fixture
        .store
        .fail_lease(&lease, "gate::shared-failure", "producer failed", true)
        .unwrap();

    let Err(error) = waiter.join().expect("waiter thread completes") else {
        panic!("waiter receives failure");
    };
    assert_eq!(error.reason(), "gate::shared-failure");
    assert_eq!(error.detail(), "producer failed");
    assert!(error.is_fault());
}

#[test]
fn acquired_lease_rechecks_a_racing_receipt_before_execution() {
    let fixture = Fixture::new("acquired-recheck");
    let action = derive_action_key(b"receipt after acquisition");
    let LeaseAcquisition::Acquired { lease, .. } = fixture
        .store
        .acquire_lease(&action, "producer", SystemTime::now(), PRODUCER_LEASE)
        .unwrap()
    else {
        panic!("producer acquires action");
    };
    let published = receipt(&action);
    fixture.store.publish(&published).unwrap();

    let Ok(Claim::Present(report)) = acquired(
        &fixture.store,
        "shared",
        &action,
        lease,
        false,
        test_timing(),
    ) else {
        panic!("racing receipt becomes present");
    };
    assert_eq!(report.receipt, published.id().unwrap());
    let LeaseAcquisition::Acquired { lease, .. } = fixture
        .store
        .acquire_lease(&action, "next", SystemTime::now(), Duration::from_secs(1))
        .unwrap()
    else {
        panic!("receipt recheck releases acquired lease");
    };
    fixture.store.release_lease(&lease).unwrap();
}

#[test]
fn busy_waiter_stops_at_its_bound_without_disturbing_the_producer() {
    let fixture = Fixture::new("wait-limit");
    let action = derive_action_key(b"bounded waiter");
    let LeaseAcquisition::Acquired { lease, .. } = fixture
        .store
        .acquire_lease(&action, "producer", SystemTime::now(), PRODUCER_LEASE)
        .unwrap()
    else {
        panic!("producer acquires action");
    };
    let Err(error) = claim_with_timing(
        &fixture.store,
        "waiter",
        "shared",
        &action,
        Timing {
            // An exhausted bound refuses at the first busy observation.
            wait_limit: Duration::ZERO,
            ..test_timing()
        },
    ) else {
        panic!("busy waiter reaches its bound");
    };
    assert_eq!(error.reason(), "gate::prove-peer-action-timeout");
    assert!(matches!(
        fixture.store.acquire_lease(
            &action,
            "contender",
            SystemTime::now(),
            Duration::from_secs(1),
        ).unwrap(),
        LeaseAcquisition::Busy { owner, .. } if owner == "producer"
    ));
    fixture.store.release_lease(&lease).unwrap();
}

#[test]
fn dropping_a_guard_releases_only_its_exact_token() {
    let fixture = Fixture::new("guard-drop");
    let action = derive_action_key(b"dropped guard");
    let LeaseAcquisition::Acquired { lease, .. } = fixture
        .store
        .acquire_lease(&action, "producer", SystemTime::now(), PRODUCER_LEASE)
        .unwrap()
    else {
        panic!("producer acquires action");
    };
    let obsolete = lease.clone();
    drop(LeaseGuard::new(fixture.store.clone(), lease, test_timing()));
    let LeaseAcquisition::Acquired { lease, recovered } = fixture
        .store
        .acquire_lease(&action, "next", SystemTime::now(), Duration::from_secs(1))
        .unwrap()
    else {
        panic!("guard drop releases action");
    };
    assert!(!recovered);
    assert!(fixture.store.release_lease(&obsolete).is_err());
    fixture.store.release_lease(&lease).unwrap();
}

#[test]
fn renewal_extends_a_live_producer_past_its_original_expiry() {
    let fixture = Fixture::new("renewal");
    let action = derive_action_key(b"renewed action");
    let acquired_at = SystemTime::now();
    let original = Duration::from_millis(500);
    let LeaseAcquisition::Acquired { lease, .. } = fixture
        .store
        .acquire_lease(&action, "producer", acquired_at, original)
        .unwrap()
    else {
        panic!("producer acquires action");
    };
    // The background worker stays idle; the producer renews explicitly, as it
    // does before publishing.
    let guard = LeaseGuard::new(
        fixture.store.clone(),
        lease,
        Timing {
            lease_lifetime: Duration::from_secs(60 * 60),
            renew_interval: Duration::from_secs(60 * 60),
            ..test_timing()
        },
    );
    assert!(guard.renew_now().is_ok(), "live lease renews");
    let past_original = acquired_at + original + Duration::from_secs(60);
    assert!(matches!(
        fixture.store.acquire_lease(
            &action,
            "contender",
            past_original,
            Duration::from_millis(20),
        ).unwrap(),
        LeaseAcquisition::Busy { owner, .. } if owner == "producer"
    ));
}

#[test]
fn an_already_expired_producer_admits_one_waiter_takeover() {
    let fixture = Fixture::new("takeover");
    let action = derive_action_key(b"expired action");
    // The departed producer's lease ended long ago; contention inside its
    // lifetime still names it as owner.
    let acquired_at = SystemTime::now() - Duration::from_secs(60 * 60);
    let LeaseAcquisition::Acquired { lease, .. } = fixture
        .store
        .acquire_lease(&action, "departed", acquired_at, Duration::from_millis(200))
        .unwrap()
    else {
        panic!("departed producer acquires action");
    };
    assert!(matches!(
        fixture.store.acquire_lease(
            &action,
            "early",
            acquired_at + Duration::from_millis(100),
            Duration::from_secs(1),
        ).unwrap(),
        LeaseAcquisition::Busy { owner, .. } if owner == "departed"
    ));
    let Ok(Claim::Lease(guard)) = claim(&fixture.store, "waiter", "shared", &action) else {
        panic!("expired producer admits waiter takeover");
    };
    assert_eq!(guard.current().owner(), "waiter");
    assert!(matches!(
        fixture
            .store
            .acquire_lease(
                &action,
                "contender",
                SystemTime::now(),
                Duration::from_secs(1),
            )
            .unwrap(),
        LeaseAcquisition::Busy { owner, .. } if owner == "waiter"
    ));
    assert!(fixture.store.release_lease(&lease).is_err());
    assert!(
        fixture
            .store
            .publish_batch_leased([(&receipt(&action), &lease)], SystemTime::now())
            .is_err()
    );
}

fn test_timing() -> Timing {
    Timing {
        lease_lifetime: Duration::from_secs(2),
        renew_interval: Duration::from_secs(1),
        wait_limit: Duration::from_secs(1),
    }
}

fn receipt(action: &gate::ActionKey) -> PassReceipt {
    PassReceipt::new(PassReceiptFields {
        action_key: action.clone(),
        action_spec: Digest::compute(Domain::GatePolicy, b"spec"),
        input_manifest: Digest::compute(Domain::GateInventory, b"inputs"),
        typed_result: Digest::compute(Domain::GateMeasurement, b"passed"),
        started_at: "2026-09-05T20:00:00.000Z".parse().unwrap(),
        finished_at: "2026-09-05T20:00:01.000Z".parse().unwrap(),
        duration_millis: 1_000,
        artifacts: Vec::new(),
        producer: EvidenceProducerRecord::default(),
        diagnostics: None,
    })
    .unwrap()
}
