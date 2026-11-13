//! Bounded metrics crossing the current-state ledger: latch transitions, latch
//! origins, and the decisions evidence may bind.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use gate::metrics::{
    EvidenceState, LatchBinding, MetricEvidence, MetricId, MetricKey, MetricStateBinding,
    ParserEvidence, ParserId, resolve_metric_rule,
};
use gate::state::{
    ActorInputs, CurrentState, LatchKind, RenameMap, STATE_LEDGER_VERSION, StateLedger,
    read_current_state,
};
use gate::{PolicyContext, RulePolicy};

use super::metrics::{FIXTURE_BASE_LIMIT, scoped_policy};

const STATE_CLEAR_VALUE: u64 = 20;
const STATE_HELD_VALUE: u64 = 30;
const STATE_LATCH_VALUE: u64 = 42;
const STATE_TEST_CLOCK: f64 = 10_000.0;
const HOUR: f64 = 3_600.0;

static NEXT_STATE_FIXTURE: AtomicU64 = AtomicU64::new(0);

#[test]
fn bounded_metrics_cross_current_state_with_every_latch_transition() {
    let policy = scoped_policy();
    let fixture = MetricStateFixture::new();

    let latched = reconcile_file_state(&fixture, &policy, STATE_LATCH_VALUE, STATE_TEST_CLOCK);
    let latch = measured_latch(&latched);
    assert_eq!(latch.transition(), "latched");
    assert_eq!(latch.ratchet_limit(), 40);
    assert_eq!(latch.active_limit(), 40);
    assert!(latch.over_limit());
    assert!(latch.over_base());
    assert!(latch.ratchet_breach());

    let held = reconcile_file_state(&fixture, &policy, STATE_HELD_VALUE, STATE_TEST_CLOCK + 1.0);
    let latch = measured_latch(&held);
    assert_eq!(latch.transition(), "held");
    assert_eq!(latch.active_limit(), STATE_CLEAR_VALUE);
    assert!(latch.over_limit());
    assert!(latch.over_base());
    assert!(!latch.ratchet_breach());

    let healed = reconcile_file_state(&fixture, &policy, STATE_CLEAR_VALUE, STATE_TEST_CLOCK + 2.0);
    let latch = measured_latch(&healed);
    assert_eq!(latch.transition(), "healed");
    assert!(!latch.over_limit());
    assert!(!latch.over_base());

    let clear = reconcile_file_state(&fixture, &policy, STATE_CLEAR_VALUE, STATE_TEST_CLOCK + 3.0);
    let latch = measured_latch(&clear);
    assert_eq!(latch.transition(), "clear");
    assert_eq!(latch.active_limit(), 40);
}

/// A latch's origin is the accepted run that created it, and a later held run
/// reports that origin as inherited debt without rewriting it.
#[test]
fn a_latch_records_its_creating_run_and_held_runs_name_it_as_inherited() {
    let policy = scoped_policy();
    let fixture = MetricStateFixture::new();

    let latched = latch_staged_on_c0ffee(&fixture, &policy);
    assert_eq!(measured_latch(&latched).transition(), "latched");
    assert_eq!(
        measured_latch(&latched).inherited(),
        None,
        "the change that creates a latch does not inherit it"
    );
    let ledger = read_current_state(&fixture.destination).expect("read the latched ledger");
    let [created] = ledger.origins() else {
        panic!("one latch records one origin: {:?}", ledger.origins());
    };
    assert_eq!(created.latch().key().repo_path(), "src/app.rs");
    assert_eq!(
        (
            created.commit(),
            created.at(),
            created.actor(),
            created.measured(),
            created.base_limit(),
        ),
        (
            Some("c0ffee"),
            STATE_TEST_CLOCK,
            Some("actor-a"),
            STATE_LATCH_VALUE,
            STATE_CLEAR_VALUE,
        )
    );
    let debt = ledger.debt();
    assert_eq!(
        (
            debt.latched(),
            debt.oldest_age_seconds(STATE_TEST_CLOCK + 2.0 * HOUR)
        ),
        (1, Some(7_200))
    );

    let held = reconcile_run(
        &fixture,
        &file_binding(&policy, "src/app.rs", STATE_HELD_VALUE),
        "src/app.rs",
        STATE_TEST_CLOCK + HOUR,
        &RenameMap::default(),
        Some("beef"),
    );
    let inherited = measured_latch(&held)
        .inherited()
        .expect("a held latch is inherited debt");
    assert_eq!(
        (inherited.commit(), inherited.age_seconds()),
        (Some("c0ffee"), Some(3_600)),
        "a held run names the creating commit, not its own base"
    );
    let wording = inherited.to_string();
    assert!(
        wording.contains("inherited") && wording.contains("c0ffee") && wording.contains("1h"),
        "the refusal must read as inherited debt with its commit and age: {wording}"
    );
    assert_eq!(
        read_current_state(&fixture.destination)
            .expect("read the held ledger")
            .origins(),
        std::slice::from_ref(created),
        "a held run leaves the origin as its creator recorded it"
    );
}

/// A rename carries a latch's origin to the new key unchanged, and healing
/// drops the origin with the latch.
#[test]
fn a_renamed_latch_carries_its_origin_until_healing_drops_it() {
    let policy = scoped_policy();
    let fixture = MetricStateFixture::new();
    latch_staged_on_c0ffee(&fixture, &policy);

    let moved = RenameMap::new([("src/app.rs", "src/moved.rs")]).expect("one staged rename");
    let renamed = reconcile_run(
        &fixture,
        &file_binding(&policy, "src/moved.rs", STATE_HELD_VALUE),
        "src/moved.rs",
        STATE_TEST_CLOCK + 2.0 * HOUR,
        &moved,
        None,
    );
    let inherited = measured_latch(&renamed)
        .inherited()
        .expect("a renamed latch is still inherited debt");
    assert_eq!(
        (inherited.commit(), inherited.age_seconds()),
        (Some("c0ffee"), Some(7_200))
    );
    let ledger = read_current_state(&fixture.destination).expect("read the renamed ledger");
    let [carried] = ledger.origins() else {
        panic!("a rename keeps exactly one origin: {:?}", ledger.origins());
    };
    assert_eq!(carried.latch().key().repo_path(), "src/moved.rs");
    assert_eq!(
        (carried.commit(), carried.at(), carried.measured()),
        (Some("c0ffee"), STATE_TEST_CLOCK, STATE_LATCH_VALUE)
    );

    let healed = reconcile_run(
        &fixture,
        &file_binding(&policy, "src/moved.rs", STATE_CLEAR_VALUE),
        "src/moved.rs",
        STATE_TEST_CLOCK + 3.0 * HOUR,
        &RenameMap::default(),
        None,
    );
    assert_eq!(measured_latch(&healed).transition(), "healed");
    assert_eq!(measured_latch(&healed).inherited(), None);
    let ledger = read_current_state(&fixture.destination).expect("read the healed ledger");
    assert!(ledger.latches().is_empty() && ledger.origins().is_empty());
    let debt = ledger.debt();
    assert_eq!(
        (
            debt.latched(),
            debt.oldest_age_seconds(STATE_TEST_CLOCK + 3.0 * HOUR)
        ),
        (0, None)
    );
}

/// Latch `src/app.rs` in an accepted run staged on commit `c0ffee`.
fn latch_staged_on_c0ffee(fixture: &MetricStateFixture, policy: &RulePolicy) -> MetricEvidence {
    reconcile_run(
        fixture,
        &file_binding(policy, "src/app.rs", STATE_LATCH_VALUE),
        "src/app.rs",
        STATE_TEST_CLOCK,
        &RenameMap::default(),
        Some("c0ffee"),
    )
}

/// A latch written before origins were recorded keeps its ledger bytes, counts
/// as debt of unknown age, and is reported as inherited debt of unknown origin
/// rather than given an invented one.
#[test]
fn a_latch_without_a_recorded_origin_reads_unknown_and_keeps_its_bytes() {
    let policy = scoped_policy();
    let fixture = MetricStateFixture::new();
    let earlier: StateLedger = serde_json::from_value(serde_json::json!({
        "version": STATE_LEDGER_VERSION,
        "latches": [{"kind": "file_loc", "key": {"path": "src/app.rs"}}],
        "claims": [],
    }))
    .expect("an origin-less ledger decodes");
    let bytes = earlier.encode().expect("encode the origin-less ledger");
    fs::create_dir_all(fixture.destination.parent().expect("ledger directory"))
        .expect("create the ledger directory");
    fs::write(&fixture.destination, &bytes).expect("seed the origin-less ledger");

    let held = reconcile_file_state(&fixture, &policy, STATE_HELD_VALUE, STATE_TEST_CLOCK);
    assert_eq!(measured_latch(&held).transition(), "held");
    let inherited = measured_latch(&held)
        .inherited()
        .expect("a held latch is inherited debt even without an origin");
    assert_eq!((inherited.commit(), inherited.age_seconds()), (None, None));
    assert!(
        inherited.to_string().contains("unknown"),
        "an unrecorded origin must read as unknown: {inherited}"
    );
    assert_eq!(
        fs::read(&fixture.destination).expect("read the ledger after the held run"),
        bytes,
        "a held run neither backfills an origin nor rewrites the row"
    );
    let debt = read_current_state(&fixture.destination)
        .expect("read the origin-less ledger")
        .debt();
    assert_eq!(
        (debt.latched(), debt.oldest_age_seconds(STATE_TEST_CLOCK)),
        (1, None)
    );
}

#[test]
fn state_binding_preserves_function_keys_and_unlimited_document_rules() {
    let policy = scoped_policy();
    let function_rule = resolve_metric_rule(
        &policy,
        MetricId::RoutineCcn,
        &PolicyContext::new().with_path("src/app.rs"),
    )
    .expect("function rule resolves");
    MetricStateBinding::new(
        MetricId::RoutineCcn,
        MetricKey::function("src/app.rs", "run").unwrap(),
        5,
        ParserEvidence::Native {
            parser: ParserId::Utf8FileShapeV1,
        },
        function_rule.clone(),
    )
    .expect_err("a foreign parser cannot enter bounded state");
    let function = MetricStateBinding::new(
        MetricId::RoutineCcn,
        MetricKey::function("src/app.rs", "run").unwrap(),
        5,
        ParserEvidence::Native {
            parser: ParserId::TreeSitterComplexityV1,
        },
        function_rule,
    )
    .expect("function measurement binds");
    assert_eq!(function.entry().kind(), LatchKind::ComplexityCcn);
    assert_eq!(function.entry().key().repo_path(), "src/app.rs");
    assert_eq!(function.entry().key().function_name(), Some("run"));
    function
        .observation(ActorInputs::new("src/app.rs").unwrap())
        .expect("function observation crosses current state");

    let document_path = "docs/reference/generated/DOCS_MAIN.mdoc";
    let document_rule = resolve_metric_rule(
        &policy,
        MetricId::RepositoryDocumentCharacters,
        &PolicyContext::new().with_path(document_path),
    )
    .expect("unlimited document rule resolves");
    let binding = MetricStateBinding::new(
        MetricId::RepositoryDocumentCharacters,
        MetricKey::path(document_path).unwrap(),
        FIXTURE_BASE_LIMIT,
        ParserEvidence::Native {
            parser: ParserId::Utf8DocumentCharactersV1,
        },
        document_rule,
    )
    .expect("document measurement binds");
    let fixture = MetricStateFixture::new();
    let row = reconcile_binding(&fixture, &binding, document_path, STATE_TEST_CLOCK);
    let latch = measured_latch(&row);
    assert!(latch.unlimited());
    assert!(!latch.over_limit());
    assert!(!latch.over_base());
    assert!(!latch.ratchet_breach());
}

#[test]
fn compiled_ceiling_is_actor_invariant() {
    let policy = scoped_policy();
    let context = PolicyContext::new().with_path("src/lib.rs");
    let rule =
        resolve_metric_rule(&policy, MetricId::FileLines, &context).expect("file rule resolves");
    let binding = MetricStateBinding::new(
        MetricId::FileLines,
        MetricKey::path("src/lib.rs").expect("metric path"),
        STATE_LATCH_VALUE,
        ParserEvidence::Native {
            parser: ParserId::Utf8FileShapeV1,
        },
        rule,
    )
    .expect("file measurement binds");

    let decide_for = |actor: &str| {
        let observation = binding
            .observation(
                ActorInputs::new("src/lib.rs")
                    .expect("actor path")
                    .with_active_actor(actor),
            )
            .expect("actor observation");
        let fixture = MetricStateFixture::new();
        let mut state = CurrentState::load(
            &fixture.destination,
            fixture.destination.with_extension("lock"),
        )
        .expect("load metric state");
        state
            .begin_run(
                &RenameMap::default(),
                ["src/lib.rs"],
                STATE_TEST_CLOCK,
                None,
            )
            .expect("begin metric state");
        state
            .reconcile([observation])
            .expect("reconcile actor observation")
            .remove(0)
            .bound()
            .clone()
    };

    let actor_a = decide_for("actor-a");
    let actor_b = decide_for("actor-b");
    assert_eq!(actor_a, actor_b);
    assert_eq!(actor_a.elastic_limit(), 40);
}

struct MetricStateFixture {
    root: PathBuf,
    destination: PathBuf,
}

impl MetricStateFixture {
    fn new() -> Self {
        let serial = NEXT_STATE_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("gate-metric-state-{}-{serial}", std::process::id()));
        fs::create_dir_all(&root).expect("create metric-state fixture root");
        let destination = root.join("current/gate-state.json");
        Self { root, destination }
    }
}

impl Drop for MetricStateFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn reconcile_file_state(
    fixture: &MetricStateFixture,
    policy: &RulePolicy,
    value: u64,
    now: f64,
) -> MetricEvidence {
    reconcile_binding(
        fixture,
        &file_binding(policy, "src/app.rs", value),
        "src/app.rs",
        now,
    )
}

fn file_binding(policy: &RulePolicy, path: &str, value: u64) -> MetricStateBinding {
    let rule = resolve_metric_rule(
        policy,
        MetricId::FileLines,
        &PolicyContext::new().with_path(path),
    )
    .expect("file rule resolves");
    MetricStateBinding::new(
        MetricId::FileLines,
        MetricKey::path(path).unwrap(),
        value,
        ParserEvidence::Native {
            parser: ParserId::Utf8FileShapeV1,
        },
        rule,
    )
    .expect("file measurement binds")
}

fn reconcile_binding(
    fixture: &MetricStateFixture,
    binding: &MetricStateBinding,
    path: &str,
    now: f64,
) -> MetricEvidence {
    reconcile_run(fixture, binding, path, now, &RenameMap::default(), None)
}

/// One accepted run over `path`, staged on `base_commit` with `renames`.
fn reconcile_run(
    fixture: &MetricStateFixture,
    binding: &MetricStateBinding,
    path: &str,
    now: f64,
    renames: &RenameMap,
    base_commit: Option<&str>,
) -> MetricEvidence {
    let actors = ActorInputs::new(path)
        .expect("actor inputs")
        .with_active_actor("actor-a");
    let observation = binding.observation(actors).expect("state observation");
    let mut state = CurrentState::load(
        &fixture.destination,
        fixture.destination.with_extension("lock"),
    )
    .expect("load current metric state");
    state
        .begin_run(renames, [path], now, base_commit.map(str::to_owned))
        .expect("begin metric-state run");
    let decisions = state
        .reconcile([observation])
        .expect("reconcile metric state");
    let evidence = binding
        .evidence(&decisions[0])
        .expect("bind reconciled evidence");
    state
        .finish()
        .expect("finish metric-state run")
        .accept()
        .expect("persist accepted metric state");
    evidence
}

fn measured_latch(evidence: &MetricEvidence) -> &LatchBinding {
    let EvidenceState::Measured { measurement } = evidence.state() else {
        panic!("fixture produced nonmeasurement evidence")
    };
    measurement.latch().expect("bounded evidence carries latch")
}
