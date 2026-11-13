use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::lock::replacement_record;

use super::owner::{GuardianState, LockOwnerEvidence, owner_record, read_owner, resolve_refusal};
use super::{
    Contention, LockAttempt, LockAvailability, LockSurfaceError, LockTargetKind, LockTopology,
    ProtectedGroup,
};
use crate::LockConfig;

const BUSY_EXIT: u64 = 71;
const TAKEN_EXIT: u64 = 72;
const FULL_EXIT: u64 = 73;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "sill-surface-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("exclusive scratch root is created");
        Self(fs::canonicalize(path).expect("scratch root canonicalizes"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn topology(root: &Path) -> LockTopology {
    let config: LockConfig = toml::from_str(&format!(
        concat!(
            "busy = {}\n",
            "taken = {}\n",
            "full = {}\n",
            "[single]\npath = \"single.lock\"\n",
            "[pool]\ndirectory = \"pool\"\nshards = 2\n",
        ),
        BUSY_EXIT, TAKEN_EXIT, FULL_EXIT,
    ))
    .expect("fixture configuration is admitted");
    LockTopology::new(root, &config)
}

#[test]
fn report_is_stable_and_does_not_materialize_the_topology() {
    let scratch = Scratch::new("report");
    let root = scratch.path().join("state/locks");
    let report = topology(&root).report().expect("topology is observable");

    assert_eq!(report.root, root.display().to_string());
    assert_eq!(report.exits.busy, BUSY_EXIT as i32);
    assert_eq!(report.exits.taken, TAKEN_EXIT as i32);
    assert_eq!(report.exits.full, FULL_EXIT as i32);
    assert_eq!(
        report
            .pools
            .iter()
            .map(|pool| (pool.name.as_str(), pool.kind, pool.shards))
            .collect::<Vec<_>>(),
        [
            ("pool", LockTargetKind::Pool, 2),
            ("single", LockTargetKind::File, 1),
        ]
    );
    assert_eq!(
        report.pools[0]
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>(),
        [
            root.join("pool/0.lock").display().to_string(),
            root.join("pool/1.lock").display().to_string(),
        ]
    );
    assert!(
        report
            .pools
            .iter()
            .flat_map(|pool| &pool.entries)
            .all(|entry| entry.availability == LockAvailability::Available)
    );
    assert!(!root.exists(), "reporting created lock state");
}

#[test]
fn report_observes_held_and_released_shards_without_creating_missing_ones() {
    let scratch = Scratch::new("availability");
    let root = scratch.path().join("locks");
    let topology = topology(&root);
    let mut held = match topology
        .try_acquire("pool", Some(0))
        .expect("selected shard is observable")
    {
        LockAttempt::Acquired(held) => held,
        LockAttempt::Contended { .. } => panic!("fresh shard was contended"),
    };
    held.set_context(Some("claim-context".to_owned()))
        .expect("owner context publishes");

    let report = topology.report().expect("held topology is observable");
    let pool = report
        .pools
        .iter()
        .find(|pool| pool.name == "pool")
        .expect("pool is reported");
    assert_eq!(pool.entries[0].availability, LockAvailability::Held);
    assert_eq!(pool.entries[1].availability, LockAvailability::Available);
    assert!(root.join("pool/0.lock").is_file());
    assert!(
        !root.join("pool/1.lock").exists(),
        "observing an absent shard created it"
    );
    assert_eq!(
        pool.entries[0].owner.as_ref().map(|owner| owner.pid),
        Some(std::process::id())
    );
    assert_eq!(
        pool.entries[0]
            .owner
            .as_ref()
            .and_then(|owner| owner.context.as_deref()),
        Some("claim-context")
    );
    assert!(!pool.entries[0].owner_stale);

    drop(held);
    let released = topology.report().expect("released topology is observable");
    let pool = released
        .pools
        .iter()
        .find(|pool| pool.name == "pool")
        .expect("pool is reported");
    assert_eq!(pool.entries[0].availability, LockAvailability::Available);
    assert_eq!(pool.entries[0].owner, None);
    assert!(!pool.entries[0].owner_stale);
}

/// A departed guardian must send the question back to the kernel, and only
/// a departed guardian may. The difference is otherwise visible only inside
/// the moment between collecting a refusal and reading the record against
/// it, so what each state does is asserted here rather than raced for.
#[test]
fn only_a_departed_guardian_puts_a_collected_refusal_back_to_the_kernel() {
    fn resolved(state: GuardianState) -> (&'static str, u32, u32) {
        let waited = Cell::new(0);
        let reasked = Cell::new(0);
        let answer = resolve_refusal(
            state,
            "collected",
            || {
                waited.set(waited.get() + 1);
                Ok("waited")
            },
            || {
                reasked.set(reasked.get() + 1);
                Ok("reasked")
            },
        );
        (
            answer.expect("every state answers"),
            waited.get(),
            reasked.get(),
        )
    }

    assert_eq!(resolved(GuardianState::Departed), ("reasked", 0, 1));
    assert_eq!(resolved(GuardianState::Pending), ("waited", 1, 0));
    assert_eq!(resolved(GuardianState::Unbounded), ("collected", 0, 0));
}

/// Bytes that are neither a whole owner record nor empty are not a release.
///
/// The direct reading pins the distinction at the parser boundary. The public
/// acquisition then proves the distinction reaches the caller that acts on it:
/// collapsing the parse error back into `None` makes this call acquire the lock
/// and turns the fixture red.
#[test]
fn a_torn_owner_record_refuses_acquisition_instead_of_spelling_release() {
    let scratch = Scratch::new("torn-owner");
    let root = scratch.path().join("locks");
    fs::create_dir_all(&root).expect("lock root creates");
    let path = root.join("single.lock");
    let released = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .expect("released owner file creates");
    assert!(
        read_owner(&released)
            .expect("an empty owner file reads")
            .is_none(),
        "an empty file no longer spells release"
    );
    fs::write(&path, br#"{"pid":7,"cwd":"/torn""#).expect("torn owner record writes");

    let torn = read_owner(&released).expect_err("a torn owner record must not spell release");
    assert_eq!(torn.kind(), std::io::ErrorKind::InvalidData);

    let error = topology(&root)
        .try_acquire("single", None)
        .expect_err("a torn owner record must stop acquisition");
    match error {
        LockSurfaceError::Acquire {
            path: refused,
            source,
        } => {
            assert_eq!(refused, path);
            assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
        }
        other => panic!("a torn owner record reached the wrong refusal: {other}"),
    }
}

#[test]
fn a_replacement_record_supersedes_every_byte_the_previous_one_spanned() {
    let record = replacement_record(br#"{"pid":7}"#.to_vec(), 40);
    assert_eq!(record.len(), 40, "the write covers the whole span");
    assert!(record.starts_with(b"{\"pid\":7}\n"));
    assert!(
        record[10..].iter().all(|byte| *byte == b' '),
        "the tail is whitespace a reader can ignore"
    );
    let padded: serde_json::Value = serde_json::from_slice(&record).expect("padded record reads");
    assert_eq!(padded["pid"], 7);

    // A record longer than the span it replaces is written whole, and a
    // fresh file spans nothing at all; neither case may clip the record.
    assert_eq!(
        replacement_record(br#"{"pid":7}"#.to_vec(), 4),
        b"{\"pid\":7}\n"
    );
    assert_eq!(
        replacement_record(br#"{"pid":7}"#.to_vec(), 0),
        b"{\"pid\":7}\n"
    );
}

#[test]
fn a_reserved_slot_is_filled_in_place_and_never_moves_under_the_guardian() {
    use crate::protected::{GROUP_SLOT_BLANK, GROUP_SLOT_BYTES, GROUP_SLOT_PREFIX, group_slot};

    let guardian = std::process::id();
    let reserved = serde_json::to_vec(
        &owner_record(Some((ProtectedGroup::Blank, guardian)), false, None).expect("owner reads"),
    )
    .expect("a reserved record serializes");
    let start = reserved
        .windows(GROUP_SLOT_PREFIX.len())
        .position(|window| window == GROUP_SLOT_PREFIX)
        .expect("a reserved record carries the slot the guardian seeks")
        + GROUP_SLOT_PREFIX.len();
    assert!(
        reserved[start..start + GROUP_SLOT_BYTES]
            .iter()
            .all(|byte| *byte == GROUP_SLOT_BLANK),
        "the slot is reserved blank, at its full width"
    );

    // Every later record this owner writes puts the slot in the same place,
    // which is the whole of why the guardian may write into an offset it
    // read a moment ago. `owner_released` is the field that would break it
    // by appearing between the two, so it is the one asked here.
    for record in [
        owner_record(
            Some((ProtectedGroup::Published(4242), guardian)),
            false,
            None,
        ),
        owner_record(
            Some((ProtectedGroup::Published(4242), guardian)),
            true,
            None,
        ),
    ] {
        let record = serde_json::to_vec(&record.expect("owner reads")).expect("serializes");
        assert_eq!(
            &record[start - GROUP_SLOT_PREFIX.len()..start],
            GROUP_SLOT_PREFIX,
            "the slot moved: {}",
            String::from_utf8_lossy(&record)
        );
    }

    // What the guardian does is exactly this: the reserved bytes, replaced
    // where they lie, by a record it never re-serializes.
    let mut published = reserved.clone();
    published[start..start + GROUP_SLOT_BYTES].copy_from_slice(&group_slot(4242));
    let published: LockOwnerEvidence =
        serde_json::from_slice(&published).expect("a filled record reads");
    assert_eq!(
        published.protected_group,
        Some(ProtectedGroup::Published(4242))
    );
    let reserved: LockOwnerEvidence =
        serde_json::from_slice(&reserved).expect("a reserved record reads");
    assert_eq!(reserved.protected_group, Some(ProtectedGroup::Blank));
    assert_eq!(published.protected_guardian, reserved.protected_guardian);
}

#[test]
fn stable_selection_and_each_contention_class_use_configured_codes() {
    let scratch = Scratch::new("contention");
    let topology = topology(&scratch.path().join("locks"));

    let first = match topology.try_acquire("pool", None).expect("first shard") {
        LockAttempt::Acquired(held) => held,
        LockAttempt::Contended { .. } => panic!("fresh pool was contended"),
    };
    assert_eq!(first.shard(), 0);
    let second = match topology.try_acquire("pool", None).expect("second shard") {
        LockAttempt::Acquired(held) => held,
        LockAttempt::Contended { .. } => panic!("second shard was contended"),
    };
    assert_eq!(second.shard(), 1);

    assert!(matches!(
        topology.try_acquire("pool", Some(0)).expect("chosen contention"),
        LockAttempt::Contended {
            kind: Contention::Taken,
            code
        } if code.get() == TAKEN_EXIT as i32
    ));
    assert!(matches!(
        topology.try_acquire("pool", None).expect("pool exhaustion"),
        LockAttempt::Contended {
            kind: Contention::Full,
            code
        } if code.get() == FULL_EXIT as i32
    ));

    let single = match topology.try_acquire("single", None).expect("single lock") {
        LockAttempt::Acquired(held) => held,
        LockAttempt::Contended { .. } => panic!("fresh single lock was contended"),
    };
    assert!(matches!(
        topology.try_acquire("single", None).expect("single contention"),
        LockAttempt::Contended {
            kind: Contention::Busy,
            code
        } if code.get() == BUSY_EXIT as i32
    ));

    drop(first);
    assert!(matches!(
        topology.try_acquire("pool", Some(0)).expect("released shard"),
        LockAttempt::Acquired(held) if held.shard() == 0
    ));
    drop(second);
    drop(single);
}

#[test]
fn invalid_unknown_and_out_of_range_requests_do_not_create_state() {
    let scratch = Scratch::new("selection");
    let root = scratch.path().join("locks");
    let topology = topology(&root);

    assert!(matches!(
        topology.try_acquire("../outside", None),
        Err(LockSurfaceError::InvalidName(_))
    ));
    assert!(matches!(
        topology.try_acquire("missing", None),
        Err(LockSurfaceError::UnknownName(_))
    ));
    assert!(matches!(
        topology.try_acquire("pool", Some(2)),
        Err(LockSurfaceError::ShardOutOfRange { .. })
    ));
    assert!(matches!(
        topology.try_acquire("single", Some(0)),
        Err(LockSurfaceError::ShardOnFile { .. })
    ));
    assert!(!root.exists(), "a refused selection created lock state");
}
