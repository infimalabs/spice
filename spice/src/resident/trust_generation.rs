//! Generation-race coverage for resident startup and trust rulings.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bus::Bus;
use spice::repository::Repository;
use wire::intent::TrustRef;
use wire::{Actor, CanonicalUuid, CommandKind, Outcome, Producer, TrustHead, TrustState, Witness};

use super::policy_surface::{trust_pend, trust_pend_with, trust_rule, trust_rule_with};
use super::{RuntimePaths, commit_startup, commit_startup_with};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

#[test]
fn startup_runtime_policy_and_trust_share_one_capture_generation() {
    const FIRST: &str = concat!(
        "[steer]\nexpiry = \"12h\"\nresend = [\"2m\", \"4m\", \"6m\"]\n",
        "[consent]\nposture = \"deny\"\n",
        "[taste.polling]\nwords = [\"polling\"]\nmessage = \"Wait for completion.\"\n",
    );
    const SECOND: &str = concat!(
        "[steer]\nexpiry = \"48h\"\nresend = [\"10m\", \"20m\", \"30m\"]\n",
        "[consent]\nposture = \"allow\"\n",
        "[taste.fallbacks]\nwords = [\"fallback\"]\nmessage = \"Keep one path.\"\n",
    );
    let (root, paths) = resident_fixture("startup-single-capture");
    let tracked = root.join(wire::TRACKED_POLICY_FILE);
    fs::write(&tracked, FIRST).expect("write first startup policy");
    let repository = Repository::discover(&root).expect("fixture repository resolves");

    let first = super::load_startup_surface_with(&repository, &spice::ambient::capture(), || {
        replace_policy(&tracked, SECOND);
    })
    .expect("captured startup surface remains admitted");
    assert_eq!(first.config.steering.expiry().to_string(), "12h");
    assert_eq!(first.config.consent.posture(), consent::Answer::Deny);
    assert_eq!(
        first
            .config
            .taste
            .tastes()
            .iter()
            .map(|taste| taste.name().as_str())
            .collect::<Vec<_>>(),
        ["polling"],
        "taste and steering policy come from the first captured generation"
    );
    let named_current =
        super::load_startup_surface(&repository).expect("replacement startup surface loads");
    assert_ne!(first.trust, named_current.trust);

    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    let before = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("empty authority reads");
    let refused = commit_startup(&bus, &repository, &first.trust, test_uuid(9), None)
        .expect_err("moved single capture must refuse startup");
    assert!(matches!(
        refused,
        Outcome::Refusal(ref refusal) if refusal.reason() == "trust::surface-moved"
    ));
    assert_eq!(
        bus.transact(|journal| bus::JournalTransaction::settled(
            journal.fact_snapshot().expect("journal snapshot")
        ))
        .expect("refused authority reads"),
        before,
        "stale startup capture changed journal authority"
    );

    let second = super::load_startup_surface(&repository).expect("replacement surface loads");
    assert_eq!(second.config.steering.expiry().to_string(), "48h");
    assert_eq!(second.config.consent.posture(), consent::Answer::Allow);
    assert_eq!(
        second
            .config
            .taste
            .tastes()
            .iter()
            .map(|taste| taste.name().as_str())
            .collect::<Vec<_>>(),
        ["fallbacks"]
    );
    assert_eq!(second.trust, named_current.trust);
    commit_startup(&bus, &repository, &second.trust, test_uuid(10), None)
        .expect("retry seats replacement capture");
    let after_retry = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("retried authority reads");
    assert_eq!(
        trust_pend_digests(&after_retry),
        [second.trust.digest().clone()]
    );
    assert_eq!(
        after_retry
            .facts()
            .iter()
            .filter(|fact| fact.fact.kind() == wire::FactKind::UpSeated)
            .count(),
        1
    );
    bus.verify().expect("retried startup authority verifies");
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn policy_movement_after_pend_refuses_seating_and_retry_pends_the_new_digest() {
    let (root, paths) = resident_fixture("trust-pend-generation");
    let tracked = root.join(wire::TRACKED_POLICY_FILE);
    fs::write(&tracked, "# tracked policy v1\n").expect("write first policy");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let surface = crate::trust_command::Surface::load(&repository).expect("first surface loads");
    let first_digest = surface.digest().clone();
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");

    let refused = commit_startup_with(&bus, &repository, &surface, test_uuid(10), None, || {
        replace_policy(&tracked, "# tracked policy v2\n")
    })
    .expect_err("moved policy must refuse seating");
    assert!(matches!(
        refused,
        Outcome::Refusal(ref refusal) if refusal.reason() == "trust::surface-moved"
    ));
    let after_refusal = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("refused startup facts read");
    assert_eq!(trust_pend_digests(&after_refusal), [first_digest]);
    assert!(
        after_refusal
            .facts()
            .iter()
            .all(|fact| fact.fact.kind() != wire::FactKind::UpSeated)
    );

    let current =
        crate::trust_command::Surface::load(&repository).expect("replacement surface loads");
    let current_digest = current.digest().clone();
    assert_ne!(current_digest, surface.digest().clone());
    commit_startup(&bus, &repository, &current, test_uuid(11), None)
        .expect("retry seats current surface");
    let after_retry = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("retry facts read");
    assert_eq!(
        trust_pend_digests(&after_retry),
        [surface.digest().clone(), current_digest]
    );
    assert_eq!(
        after_retry
            .facts()
            .iter()
            .filter(|fact| fact.fact.kind() == wire::FactKind::UpSeated)
            .count(),
        1
    );
    bus.verify().expect("retried startup authority verifies");
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn equal_byte_replacement_with_an_existing_head_refuses_zero_write_seating() {
    let (root, paths) = resident_fixture("trust-existing-generation");
    let tracked = root.join(wire::TRACKED_POLICY_FILE);
    let bytes = "# stable tracked policy\n";
    fs::write(&tracked, bytes).expect("write tracked policy");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let surface = crate::trust_command::Surface::load(&repository).expect("first surface loads");
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    commit_startup(&bus, &repository, &surface, test_uuid(12), None)
        .expect("first generation seats");
    let before = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("authority before replacement");

    let refused = commit_startup_with(&bus, &repository, &surface, test_uuid(13), None, || {
        replace_policy(&tracked, bytes)
    })
    .expect_err("same-byte generation replacement must refuse");
    assert!(matches!(
        refused,
        Outcome::Refusal(ref refusal) if refusal.reason() == "trust::surface-moved"
    ));
    assert_eq!(
        bus.transact(|journal| bus::JournalTransaction::settled(
            journal.fact_snapshot().expect("journal snapshot")
        ))
        .expect("authority after refusal"),
        before,
        "refused seating changed journal authority"
    );
    bus.verify().expect("refused startup authority verifies");
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn running_resident_pends_current_surface_then_existing_grant_succeeds() {
    let (root, paths) = resident_fixture("trust-live-pend");
    let tracked = root.join(wire::TRACKED_POLICY_FILE);
    fs::write(&tracked, "# initial policy\n").expect("write initial policy");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let initial = crate::trust_command::Surface::load(&repository).expect("initial surface loads");
    let epoch = test_uuid(30);
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    commit_startup(&bus, &repository, &initial, epoch, None)
        .expect("resident seats with initial surface");

    let current_bytes = "# changed executable policy\n";
    replace_policy(&tracked, current_bytes);
    let current = crate::trust_command::Surface::load(&repository).expect("changed surface loads");
    let digest = current.digest().clone();
    let worktree = repository
        .worktree_root()
        .to_str()
        .expect("fixture worktree is UTF-8");
    let reference = TrustRef {
        digest: digest.clone(),
        worktree: Some(worktree.to_owned()),
    };
    let scenario = TrustScenario {
        bus: &bus,
        repository: &repository,
        journal: &paths.journal,
        epoch,
        digest: &digest,
        worktree,
    };
    assert_pend_admission_refusals(&scenario, initial.digest());
    assert_pend_generation_refusals(&scenario, &root, &tracked, current_bytes);
    pend_current_and_refuse_duplicate(&scenario, &reference);
    grant_current_and_refuse_moved_standing(&scenario);
    bus.verify()
        .expect("live pend and grant authority verifies");
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

struct TrustScenario<'a> {
    bus: &'a Bus,
    repository: &'a Repository,
    journal: &'a Path,
    epoch: CanonicalUuid,
    digest: &'a wire::Digest,
    worktree: &'a str,
}

impl TrustScenario<'_> {
    fn pend_reference(&self) -> TrustRef {
        TrustRef {
            digest: self.digest.clone(),
            worktree: Some(self.worktree.to_owned()),
        }
    }

    fn ruling_reference(&self) -> TrustRef {
        TrustRef {
            digest: self.digest.clone(),
            worktree: None,
        }
    }
}

fn assert_pend_admission_refusals(scenario: &TrustScenario<'_>, stale: &wire::Digest) {
    let stale_reference = TrustRef {
        digest: stale.clone(),
        worktree: Some(scenario.worktree.to_owned()),
    };
    let stale_pend = admitted_trust_pend_command(
        CommandKind::TrustPend,
        stale.clone(),
        TrustState::Pended,
        scenario.epoch,
        test_uuid(35),
        scenario.worktree,
    );
    let before = durable_trust_state(scenario.bus, scenario.journal, stale);
    let refused = trust_pend(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &stale_pend,
        &stale_reference,
    );
    assert_trust_refusal(
        &refused,
        "trust::digest-not-current",
        Some(scenario.worktree),
        &["trust", "pend"],
    );
    assert_durable_trust_state(scenario.bus, scenario.journal, stale, before);

    let stale_epoch_pend = admitted_trust_pend_command(
        CommandKind::TrustPend,
        scenario.digest.clone(),
        TrustState::Pended,
        test_uuid(36),
        test_uuid(37),
        scenario.worktree,
    );
    let before = durable_trust_state(scenario.bus, scenario.journal, scenario.digest);
    let refused = trust_pend(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &stale_epoch_pend,
        &scenario.pend_reference(),
    );
    assert_trust_refusal(
        &refused,
        "resident::epoch-moved",
        Some(scenario.worktree),
        &["trust", "pend"],
    );
    assert_durable_trust_state(scenario.bus, scenario.journal, scenario.digest, before);

    let lane_pend = admitted_trust_command_as(
        CommandKind::TrustPend,
        scenario.digest.clone(),
        TrustState::Pended,
        scenario.epoch,
        test_uuid(31),
        Some(scenario.worktree),
        Actor::Lane(test_uuid(32)),
    );
    let before = durable_trust_state(scenario.bus, scenario.journal, scenario.digest);
    let refused = trust_pend(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &lane_pend,
        &scenario.pend_reference(),
    );
    assert_trust_refusal(
        &refused,
        "trust::operator-required",
        Some(scenario.worktree),
        &["trust", "pend"],
    );
    assert_durable_trust_state(scenario.bus, scenario.journal, scenario.digest, before);
}

fn assert_pend_generation_refusals(
    scenario: &TrustScenario<'_>,
    root: &Path,
    tracked: &Path,
    current_bytes: &str,
) {
    let pend = admitted_trust_pend_command(
        CommandKind::TrustPend,
        scenario.digest.clone(),
        TrustState::Pended,
        scenario.epoch,
        test_uuid(33),
        scenario.worktree,
    );
    let reference = scenario.pend_reference();
    let before = durable_trust_state(scenario.bus, scenario.journal, scenario.digest);
    let replaced = root.with_extension("replaced");
    let refused = trust_pend_with(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &pend,
        &reference,
        || {
            fs::rename(root, &replaced).expect("displace producing worktree during commit");
            fs::create_dir_all(root.join(".git")).expect("create replacement worktree identity");
            fs::write(root.join(wire::TRACKED_POLICY_FILE), current_bytes)
                .expect("write equal-byte replacement policy");
        },
    );
    fs::remove_dir_all(root).expect("remove replacement worktree");
    fs::rename(&replaced, root).expect("restore original producing worktree");
    assert_trust_refusal(
        &refused,
        "trust::surface-moved",
        Some(scenario.worktree),
        &["trust", "pend"],
    );
    assert_durable_trust_state(
        scenario.bus,
        scenario.journal,
        scenario.digest,
        before.clone(),
    );

    let displaced = root.with_extension("displaced");
    let refused = trust_pend_with(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &pend,
        &reference,
        || fs::rename(root, &displaced).expect("remove producing worktree during commit"),
    );
    fs::rename(&displaced, root).expect("restore producing worktree after refusal");
    assert_trust_refusal(
        &refused,
        "trust::worktree-unresolved",
        Some(scenario.worktree),
        &["trust", "pend"],
    );
    assert_durable_trust_state(
        scenario.bus,
        scenario.journal,
        scenario.digest,
        before.clone(),
    );

    let refused = trust_pend_with(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &pend,
        &reference,
        || replace_policy(tracked, current_bytes),
    );
    assert_trust_refusal(
        &refused,
        "trust::surface-moved",
        Some(scenario.worktree),
        &["trust", "pend"],
    );
    assert_durable_trust_state(scenario.bus, scenario.journal, scenario.digest, before);
}

fn pend_current_and_refuse_duplicate(scenario: &TrustScenario<'_>, reference: &TrustRef) {
    let pend = admitted_trust_pend_command(
        CommandKind::TrustPend,
        scenario.digest.clone(),
        TrustState::Pended,
        scenario.epoch,
        test_uuid(33),
        scenario.worktree,
    );
    let pended = trust_pend(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &pend,
        reference,
    );
    assert!(matches!(pended, Outcome::Reply(_)));
    let pended_head = scenario
        .bus
        .trust_head(scenario.digest)
        .expect("pended head reads")
        .expect("pended head exists");
    assert_eq!(pended_head.body().state, TrustState::Pended);
    assert_eq!(
        pended_head.body().worktree.as_deref(),
        Some(scenario.worktree)
    );

    let before = durable_trust_state(scenario.bus, scenario.journal, scenario.digest);
    let duplicate = trust_pend(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &pend,
        reference,
    );
    assert_trust_refusal(
        &duplicate,
        "trust::digest-already-pended",
        Some(scenario.worktree),
        &["trust"],
    );
    assert_durable_trust_state(scenario.bus, scenario.journal, scenario.digest, before);
}

fn grant_current_and_refuse_moved_standing(scenario: &TrustScenario<'_>) {
    let grant = admitted_trust_command(
        CommandKind::TrustGrant,
        scenario.digest.clone(),
        TrustState::Pended,
        scenario.epoch,
        test_uuid(34),
    );
    let ruling = scenario.ruling_reference();
    let granted = trust_rule(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &grant,
        &ruling,
    );
    assert!(matches!(granted, Outcome::Reply(_)), "{granted:?}");
    assert_eq!(
        scenario
            .bus
            .trust_head(scenario.digest)
            .expect("granted head reads")
            .expect("granted head exists")
            .body()
            .state,
        TrustState::Granted,
    );

    let before = durable_trust_state(scenario.bus, scenario.journal, scenario.digest);
    let standing = trust_rule(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &grant,
        &ruling,
    );
    assert_trust_refusal(
        &standing,
        "trust::standing-moved",
        Some(scenario.worktree),
        &["trust"],
    );
    assert_durable_trust_state(scenario.bus, scenario.journal, scenario.digest, before);
}

#[test]
fn pend_refuses_unresolved_nonroot_and_foreign_worktrees_without_writes() {
    let (root, paths) = resident_fixture("trust-worktree-admission");
    let tracked = root.join(wire::TRACKED_POLICY_FILE);
    fs::write(&tracked, "# resident policy\n").expect("write resident policy");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let surface = crate::trust_command::Surface::load(&repository).expect("trust surface loads");
    let digest = surface.digest().clone();
    let epoch = test_uuid(40);
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    commit_startup(&bus, &repository, &surface, epoch, None).expect("resident seats");

    let nested = root.join("nested");
    fs::create_dir(&nested).expect("nested worktree path exists");
    let missing = root.join("removed");
    let (foreign, _) = resident_fixture("trust-foreign-worktree");
    fs::write(
        foreign.join(wire::TRACKED_POLICY_FILE),
        "# foreign policy\n",
    )
    .expect("write foreign policy");
    let foreign = fs::canonicalize(foreign).expect("canonicalize foreign worktree");
    let cases = [
        (
            missing.to_str().expect("missing path is UTF-8"),
            "trust::worktree-unresolved",
            test_uuid(41),
        ),
        (
            nested.to_str().expect("nested path is UTF-8"),
            "trust::worktree-not-root",
            test_uuid(42),
        ),
        (
            foreign.to_str().expect("foreign path is UTF-8"),
            "trust::foreign-worktree",
            test_uuid(43),
        ),
    ];
    for (worktree, reason, id) in cases {
        let reference = TrustRef {
            digest: digest.clone(),
            worktree: Some(worktree.to_owned()),
        };
        let command = admitted_trust_pend_command(
            CommandKind::TrustPend,
            digest.clone(),
            TrustState::Pended,
            epoch,
            id,
            worktree,
        );
        let before = bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts before refused pend");
        let head = bus.trust_head(&digest).expect("head before refused pend");
        let receipts = receipt_count(&paths.journal);
        let refused = trust_pend(&bus, &repository, epoch, &command, &reference);
        assert_trust_refusal(&refused, reason, Some(worktree), &["trust", "pend"]);
        assert_eq!(
            bus.transact(|journal| bus::JournalTransaction::settled(
                journal.fact_snapshot().expect("journal snapshot")
            ))
            .expect("facts after refused pend"),
            before
        );
        assert_eq!(
            bus.trust_head(&digest).expect("head after refused pend"),
            head
        );
        assert_eq!(receipt_count(&paths.journal), receipts);
    }

    bus.verify().expect("refused pend authority verifies");
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(foreign).expect("foreign fixture cleanup");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn grant_and_revoke_generation_movement_preserve_facts_heads_and_receipts() {
    let (root, paths) = resident_fixture("trust-ruling-generation");
    let tracked = root.join(wire::TRACKED_POLICY_FILE);
    let bytes = "# stable ruling policy\n";
    fs::write(&tracked, bytes).expect("write tracked policy");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let surface = crate::trust_command::Surface::load(&repository).expect("trust surface loads");
    let digest = surface.digest().clone();
    let epoch = test_uuid(20);
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    commit_startup(&bus, &repository, &surface, epoch, None).expect("surface pends and seats");
    let ruling_epoch = test_uuid(23);
    commit_startup(&bus, &repository, &surface, ruling_epoch, None)
        .expect("replacement resident seats without repending the surface");

    let grant = admitted_trust_command(
        CommandKind::TrustGrant,
        digest.clone(),
        TrustState::Pended,
        ruling_epoch,
        test_uuid(21),
    );
    let reference = TrustRef {
        digest: digest.clone(),
        worktree: None,
    };
    let before_grant = durable_trust_state(&bus, &paths.journal, &digest);
    let head_before_grant = before_grant.1.clone();
    let refused = trust_rule_with(&bus, &repository, ruling_epoch, &grant, &reference, || {
        replace_policy(&tracked, bytes)
    });
    assert_trust_refusal(
        &refused,
        "trust::surface-moved",
        Some(root.to_str().expect("worktree is UTF-8")),
        &["trust", "pend"],
    );
    assert_durable_trust_state(&bus, &paths.journal, &digest, before_grant);

    let current = crate::trust_command::Surface::load(&repository).expect("current surface loads");
    assert_eq!(current.digest(), &digest);
    let granted = trust_rule(&bus, &repository, ruling_epoch, &grant, &reference);
    assert!(matches!(granted, Outcome::Reply(_)));
    let head_after_grant = bus
        .trust_head(&digest)
        .expect("granted head reads")
        .expect("granted head exists");
    assert_same_origin_cause(
        &durable_trust_state(&bus, &paths.journal, &digest).0,
        head_before_grant
            .as_ref()
            .expect("pended head exists")
            .position(),
        head_after_grant.position(),
    );

    let revoke = admitted_trust_command(
        CommandKind::TrustRevoke,
        digest.clone(),
        TrustState::Granted,
        ruling_epoch,
        test_uuid(22),
    );
    let before_revoke = durable_trust_state(&bus, &paths.journal, &digest);
    let refused = trust_rule_with(&bus, &repository, ruling_epoch, &revoke, &reference, || {
        publish_local_policy(&root, "# newly published local policy\n")
    });
    assert_trust_refusal(
        &refused,
        "trust::surface-moved",
        Some(root.to_str().expect("worktree is UTF-8")),
        &["trust", "pend"],
    );
    assert_durable_trust_state(&bus, &paths.journal, &digest, before_revoke);
    bus.verify().expect("ruling authority verifies");
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn trust_commit_races_retain_competing_authority_and_actionable_diagnostics() {
    let (root, paths) = resident_fixture("trust-structured-races");
    let tracked = root.join(wire::TRACKED_POLICY_FILE);
    fs::write(&tracked, "# initial race policy\n").expect("write initial race policy");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let initial = crate::trust_command::Surface::load(&repository).expect("initial surface loads");
    let epoch = test_uuid(60);
    let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
    let bus = Bus::start(journal).expect("fixture bus starts");
    commit_startup(&bus, &repository, &initial, epoch, None).expect("resident seats");

    replace_policy(&tracked, "# current race policy\n");
    let current = crate::trust_command::Surface::load(&repository).expect("current surface loads");
    let digest = current.digest().clone();
    let worktree = repository
        .worktree_root()
        .to_str()
        .expect("fixture worktree is UTF-8");
    let scenario = TrustScenario {
        bus: &bus,
        repository: &repository,
        journal: &paths.journal,
        epoch,
        digest: &digest,
        worktree,
    };
    assert_duplicate_pend_race(&scenario);
    assert_worktree_head_race(&scenario);
    assert_standing_race(&scenario);
    assert_epoch_race(&scenario, &current);

    bus.verify().expect("structured race authority verifies");
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

fn assert_duplicate_pend_race(scenario: &TrustScenario<'_>) {
    let pend = admitted_trust_pend_command(
        CommandKind::TrustPend,
        scenario.digest.clone(),
        TrustState::Pended,
        scenario.epoch,
        test_uuid(61),
        scenario.worktree,
    );
    let raced = RefCell::new(None);
    let refused = trust_pend_with(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &pend,
        &scenario.pend_reference(),
        || {
            commit_competing_pend(
                scenario.bus,
                scenario.epoch,
                scenario.digest,
                scenario.worktree,
            );
            raced.replace(Some(durable_trust_state(
                scenario.bus,
                scenario.journal,
                scenario.digest,
            )));
        },
    );
    assert_trust_refusal(
        &refused,
        "trust::digest-already-pended",
        Some(scenario.worktree),
        &["trust"],
    );
    assert_durable_trust_state(
        scenario.bus,
        scenario.journal,
        scenario.digest,
        raced.into_inner().expect("duplicate race state captured"),
    );
}

fn assert_worktree_head_race(scenario: &TrustScenario<'_>) {
    let grant = admitted_trust_command(
        CommandKind::TrustGrant,
        scenario.digest.clone(),
        TrustState::Pended,
        scenario.epoch,
        test_uuid(62),
    );
    let raced = RefCell::new(None);
    let refused = trust_rule_with(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &grant,
        &scenario.ruling_reference(),
        || {
            commit_competing_pend(
                scenario.bus,
                scenario.epoch,
                scenario.digest,
                "/replacement-producing-worktree",
            );
            raced.replace(Some(durable_trust_state(
                scenario.bus,
                scenario.journal,
                scenario.digest,
            )));
        },
    );
    assert_trust_refusal(
        &refused,
        "trust::worktree-moved",
        Some(scenario.worktree),
        &["trust"],
    );
    assert_durable_trust_state(
        scenario.bus,
        scenario.journal,
        scenario.digest,
        raced.into_inner().expect("worktree race state captured"),
    );
}

fn assert_standing_race(scenario: &TrustScenario<'_>) {
    commit_competing_pend(
        scenario.bus,
        scenario.epoch,
        scenario.digest,
        scenario.worktree,
    );
    let grant = admitted_trust_command(
        CommandKind::TrustGrant,
        scenario.digest.clone(),
        TrustState::Pended,
        scenario.epoch,
        test_uuid(63),
    );
    let raced = RefCell::new(None);
    let refused = trust_rule_with(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &grant,
        &scenario.ruling_reference(),
        || {
            commit_competing_grant(scenario.bus, scenario.epoch, scenario.digest);
            raced.replace(Some(durable_trust_state(
                scenario.bus,
                scenario.journal,
                scenario.digest,
            )));
        },
    );
    assert_trust_refusal(
        &refused,
        "trust::standing-moved",
        Some(scenario.worktree),
        &["trust"],
    );
    assert_durable_trust_state(
        scenario.bus,
        scenario.journal,
        scenario.digest,
        raced.into_inner().expect("standing race state captured"),
    );
}

fn assert_epoch_race(scenario: &TrustScenario<'_>, current: &crate::trust_command::Surface) {
    commit_competing_pend(
        scenario.bus,
        scenario.epoch,
        scenario.digest,
        scenario.worktree,
    );
    let grant = admitted_trust_command(
        CommandKind::TrustGrant,
        scenario.digest.clone(),
        TrustState::Pended,
        scenario.epoch,
        test_uuid(64),
    );
    let raced = RefCell::new(None);
    let moved_epoch = test_uuid(65);
    let refused = trust_rule_with(
        scenario.bus,
        scenario.repository,
        scenario.epoch,
        &grant,
        &scenario.ruling_reference(),
        || {
            commit_startup(
                scenario.bus,
                scenario.repository,
                current,
                moved_epoch,
                None,
            )
            .expect("competing resident epoch seats");
            raced.replace(Some(durable_trust_state(
                scenario.bus,
                scenario.journal,
                scenario.digest,
            )));
        },
    );
    assert_trust_refusal(
        &refused,
        "resident::epoch-moved",
        Some(scenario.worktree),
        &["trust", "grant", scenario.digest.as_str()],
    );
    assert_durable_trust_state(
        scenario.bus,
        scenario.journal,
        scenario.digest,
        raced.into_inner().expect("epoch race state captured"),
    );
}

fn assert_same_origin_cause(snapshot: &journal::FactSnapshot, cause: wire::Seq, child: wire::Seq) {
    let cause = snapshot
        .facts()
        .iter()
        .find(|fact| fact.seq == cause)
        .expect("cause fact exists");
    let child = snapshot
        .facts()
        .iter()
        .find(|fact| fact.seq == child)
        .expect("child fact exists");
    assert_eq!(child.cause, Some(cause.seq));
    assert_eq!(child.origin, cause.origin);
}

fn assert_trust_refusal(
    outcome: &Outcome<wire::intent::CommandAnswer>,
    reason: &str,
    worktree: Option<&str>,
    arguments: &[&str],
) {
    let Outcome::Refusal(refusal) = outcome else {
        panic!("trust refusal is not repairable: {outcome:?}");
    };
    let command = refusal
        .command()
        .unwrap_or_else(|| panic!("trust refusal is not repairable: {outcome:?}"));
    let diagnostic = refusal
        .diagnostic()
        .expect("repairable trust refusal carries a diagnostic");
    assert_eq!(diagnostic.reason(), reason);
    assert_eq!(diagnostic.witness(), Some("executable-policy"));
    let detail = diagnostic
        .detail()
        .expect("trust refusal carries producer detail");
    match worktree {
        Some(worktree) => assert!(detail.contains(worktree), "{detail}"),
        None => assert!(detail.contains("producer_worktree=unknown"), "{detail}"),
    }
    assert_eq!(
        command
            .arguments()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        arguments,
    );
}

type DurableTrustState = (
    journal::FactSnapshot,
    Option<journal::JournalHead<TrustHead>>,
    u64,
);

fn durable_trust_state(bus: &Bus, journal: &Path, digest: &wire::Digest) -> DurableTrustState {
    (
        bus.transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("durable trust facts read"),
        bus.trust_head(digest).expect("durable trust head reads"),
        receipt_count(journal),
    )
}

fn assert_durable_trust_state(
    bus: &Bus,
    journal: &Path,
    digest: &wire::Digest,
    expected: DurableTrustState,
) {
    assert_eq!(durable_trust_state(bus, journal, digest), expected);
}

fn commit_competing_pend(bus: &Bus, epoch: CanonicalUuid, digest: &wire::Digest, worktree: &str) {
    let mut write = super::write(Actor::Spice, epoch, Producer::CommandDoor)
        .expect("competing pend write builds");
    let fact = write
        .fact(
            wire::body::TrustPended {
                surface: crate::trust_command::SURFACE_NAME.to_owned(),
                digest: digest.clone(),
                worktree: Some(worktree.to_owned()),
                previous: None,
            },
            journal::FactCause::Root,
        )
        .expect("competing pend fact builds");
    let digest = digest.clone();
    let worktree = worktree.to_owned();
    write.trust_head(fact, move |_| TrustHead {
        digest,
        state: TrustState::Pended,
        worktree: Some(worktree),
    });
    super::commit(bus, write).expect("competing pend commits");
}

fn commit_competing_grant(bus: &Bus, epoch: CanonicalUuid, digest: &wire::Digest) {
    let current = bus
        .trust_head(digest)
        .expect("competing grant head reads")
        .expect("competing grant head exists");
    let mut write = super::write(Actor::Spice, epoch, Producer::CommandDoor)
        .expect("competing grant write builds");
    let fact = write
        .fact(
            wire::body::TrustGranted {
                surface: crate::trust_command::SURFACE_NAME.to_owned(),
                digest: digest.clone(),
            },
            journal::FactCause::Existing(current.position()),
        )
        .expect("competing grant fact builds");
    let digest = digest.clone();
    let worktree = current.body().worktree.clone();
    write.trust_head(fact, move |_| TrustHead {
        digest,
        state: TrustState::Granted,
        worktree,
    });
    super::commit(bus, write).expect("competing grant commits");
}

fn resident_fixture(name: &str) -> (PathBuf, RuntimePaths) {
    let root = std::env::temp_dir().join(format!(
        "spice-resident-{name}-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    crate::test_repository::initialize(&root);
    let paths = RuntimePaths::discover(&root).expect("runtime paths resolve");
    fs::create_dir_all(&paths.data).expect("data directory");
    (root, paths)
}

fn replace_policy(path: &Path, bytes: &str) {
    let replacement = path.with_extension("replacement");
    fs::write(&replacement, bytes).expect("write policy replacement");
    fs::rename(&replacement, path).expect("publish policy replacement");
}

fn publish_local_policy(root: &Path, bytes: &str) {
    let parent = root.join(".git/.spice");
    fs::create_dir_all(&parent).expect("publish local policy parent");
    fs::write(parent.join("config.toml"), bytes).expect("publish local policy");
}

fn trust_pend_digests(snapshot: &journal::FactSnapshot) -> Vec<wire::Digest> {
    snapshot
        .facts()
        .iter()
        .filter(|fact| fact.fact.kind() == wire::FactKind::TrustPended)
        .map(|envelope| {
            let bytes = wire::encode(&envelope.fact).expect("trust fact encodes");
            wire::decode::<wire::Fact<wire::body::TrustPended>>(&bytes)
                .expect("trust fact decodes")
                .body
                .digest
        })
        .collect()
}

fn admitted_trust_command(
    kind: CommandKind,
    digest: wire::Digest,
    standing: TrustState,
    epoch: CanonicalUuid,
    id: CanonicalUuid,
) -> wire::AdmittedCommand {
    admitted_trust_command_as(
        kind,
        digest,
        standing,
        epoch,
        id,
        None,
        Actor::TEST_PRINCIPAL,
    )
}

fn admitted_trust_pend_command(
    kind: CommandKind,
    digest: wire::Digest,
    standing: TrustState,
    epoch: CanonicalUuid,
    id: CanonicalUuid,
    worktree: &str,
) -> wire::AdmittedCommand {
    admitted_trust_command_as(
        kind,
        digest,
        standing,
        epoch,
        id,
        Some(worktree),
        Actor::TEST_PRINCIPAL,
    )
}

fn admitted_trust_command_as(
    kind: CommandKind,
    digest: wire::Digest,
    standing: TrustState,
    epoch: CanonicalUuid,
    id: CanonicalUuid,
    worktree: Option<&str>,
    actor: Actor,
) -> wire::AdmittedCommand {
    let reference = TrustRef {
        digest: digest.clone(),
        worktree: worktree.map(str::to_owned),
    };
    wire::Command {
        v: wire::WireVersion::CURRENT,
        id,
        epoch,
        kind,
        expected: vec![Witness::Trust { digest, standing }],
        body: serde_json::to_value(reference).expect("trust reference encodes"),
    }
    .admit(
        &wire::Accepted {
            wire: wire::WireVersion::CURRENT,
            actor,
            epoch,
            release: "1.2.3".parse().expect("fixture release"),
        },
        &wire::principal::PrincipalRoles::default(),
    )
    .expect("fixture trust command admits")
}

fn receipt_count(path: &Path) -> u64 {
    let count: i64 = rusqlite::Connection::open(path)
        .expect("receipt fixture opens")
        .query_row("SELECT COUNT(*) FROM command_receipts", [], |row| {
            row.get(0)
        })
        .expect("receipt count reads");
    u64::try_from(count).expect("receipt count is nonnegative")
}

fn test_uuid(last: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
        .parse()
        .expect("fixture UUID")
}
