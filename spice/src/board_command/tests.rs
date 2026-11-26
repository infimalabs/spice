//! Durable board command tests, including lane lifecycle task persistence.

use super::*;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;

use crate::board_command::client::transition_witnesses;
use wire::{Accepted, CanonicalUuid, ResidencyHead, ResidentState};

mod subscription;
// Shared board and journal construction stays separate from the scenarios
// so both source files remain inside the structural review bound.
mod fixture;
use fixture::*;
// Claim generation and resident-generation refreshes share one scenario seam.
mod refresh;
use refresh::*;

// Each allocator re-read refusal is pinned by a scenario that moves only
// what that one comparison reads, so severing them apart is observable.
mod allocation_guard;
use allocation_guard::*;

mod board_view;
mod graph_view;
mod origin;
mod planning;
mod preparation;
mod read_scope;
mod room;
mod task_list;
mod task_listing;
mod taste_review;
mod team_history;

// The order in which one claim's preconditions refuse is documented as a
// contract on `ClaimGrant`, and a caller sees only the first refusal, so it
// is pinned apart from the scenarios that drive a claim that succeeds.
mod claim_grant;
mod close_placement;
mod creation_context;
mod drive;
mod placement;
mod routing_capture;
mod topic_lineage;

/// The process environment every board test's Git host borrows.
static AMBIENT: std::sync::LazyLock<Ambient> = std::sync::LazyLock::new(spice::ambient::capture);

static POLICY_REPOSITORIES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeMap<CanonicalUuid, PathBuf>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::BTreeMap::new()));

pub(crate) fn register_policy_repository(root: &Path) -> CanonicalUuid {
    let epoch = uuid(NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) + 1);
    POLICY_REPOSITORIES
        .lock()
        .expect("fixture policy registry")
        .insert(epoch, root.to_path_buf());
    epoch
}

pub(crate) fn unregister_policy_repository(root: &Path) {
    POLICY_REPOSITORIES
        .lock()
        .expect("fixture policy registry")
        .retain(|_, path| path != root);
}

pub(crate) fn policy_repository(epoch: CanonicalUuid) -> Repository {
    let root = POLICY_REPOSITORIES
        .lock()
        .expect("fixture policy registry")
        .get(&epoch)
        .cloned();
    let root = root.expect("fixture explicitly registers its policy repository");
    Repository::discover(&root).expect("registered policy repository")
}

fn policy_owners(answer: &BoardAnswer) -> std::sync::Arc<board::OwnershipSnapshot> {
    let repository = policy_repository(answer.residency.body.epoch);
    ownership::capture_answer(&repository, answer)
        .expect("fixture policy snapshot")
        .1
}

fn allocation_for(answer: &BoardAnswer, actor: &Actor) -> board::Allocation {
    let team = actor_subscription(&answer.residency.body, actor).expect("fixture current team");
    policy_owners(answer).for_team(team.team)
}

fn host(repository: &Repository) -> Host<'_> {
    Host {
        repository,
        ambient: &AMBIENT,
    }
}
/// Real moved-task outcomes consumed by the task URI submission contract.
pub(crate) struct MovedUriJoinOutcomes {
    pub(crate) named: TaskHandle,
    pub(crate) current: TaskHandle,
    pub(crate) reply: JoinSubmission,
    pub(crate) refusal: JoinSubmission,
}

/// Resolve and execute task URI joins against one in-process board fixture.
///
/// The retained current handle and each outcome are derived from the exact
/// board snapshot used to map that command.
pub(crate) fn moved_uri_join_outcomes() -> MovedUriJoinOutcomes {
    let (root, bus, epoch, seats) = fixture("resource-moved-join", 2);
    let named = add_task_in_topic(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "ALPHA-1kKfYW0Bx",
        "Move while joining",
        "/board/alpha",
        Some("todo"),
    );
    let born = named.born().expect("fixture task carries identity");
    let current = TaskHandle::for_topic(
        &"/board/beta".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    );
    assert_committed(correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            topic: Some("/board/beta".parse().expect("fixture topic")),
            ..only(&named)
        },
    ));

    let inviter = Actor::Lane(seats[0].lane);
    let before_claim = board(&bus, epoch, inviter.clone());
    let mut refused_current = None;
    retain_current_handle(&before_claim, Some(&named), Some(&mut refused_current))
        .expect("moved task resolves before the refusal");
    let refusal = invitation_offer(&before_claim, Some(&named), None, None)
        .expect_err("an unheld task refuses invitation");

    claim_task(&bus, epoch, &seats[0], &inviter, &named);
    assert_committed(completion_target::set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &named,
        ("todo", 2),
    ));
    let joining = Actor::Lane(seats[1].lane);
    let join_view = board(&bus, epoch, joining.clone());
    let mut joined_current = None;
    retain_current_handle(&join_view, Some(&named), Some(&mut joined_current))
        .expect("moved task resolves before the join");
    let (kind, expected, body) =
        direct_join_request(&join_view, named.clone()).expect("direct join maps");
    let intent: wire::intent::TaskJoin =
        serde_json::from_value(body).expect("direct join intent decodes");
    let command = fixture_command(epoch, joining, kind, expected, &intent);
    let reply = task_join(&bus, &command, &intent, None, None);

    clean(&root, bus);
    MovedUriJoinOutcomes {
        named,
        current,
        reply: JoinSubmission {
            current: joined_current,
            outcome: reply,
        },
        refusal: JoinSubmission {
            current: refused_current,
            outcome: refusal,
        },
    }
}

fn at<'a>(answer: &'a BoardAnswer, task: &TaskHandle) -> &'a BoardTask {
    task_heads(answer, task).expect("fixture task is visible")
}

fn refusal_reason(outcome: &Outcome<CommandAnswer>) -> &str {
    match outcome {
        Outcome::Refusal(refusal) => refusal.reason(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The refusal's reason and the observed value its diagnostic names.
fn refused_as(outcome: &Outcome<CommandAnswer>) -> (String, Option<String>) {
    let Outcome::Refusal(refusal) = outcome else {
        panic!("expected a refusal, got {outcome:?}");
    };
    let observed = refusal
        .diagnostic()
        .and_then(wire::RefusalDiagnostic::observed);
    (refusal.reason().to_owned(), observed.map(str::to_owned))
}

fn fact_count(bus: &Bus) -> usize {
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("fixture facts remain readable")
    .facts()
    .len()
}

/// Put one task in a lane's hands through the allocator rule.
fn hold(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, lane: &Actor, task: &TaskHandle) {
    let available = board(bus, epoch, lane.clone());
    assert_committed(issue(
        bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimWin,
        claim_expected(&available, task, seat),
        &next_rule(),
    ));
}

/// The allocator rule a lane claims its next task by.
fn next_rule() -> ClaimWin {
    ClaimWin {
        rule: "next".parse().expect("allocator rule"),
        recovery: None,
    }
}

/// A review of `task` recording only its finding.
fn verdict(task: &TaskHandle, finding: &str) -> TaskReview {
    TaskReview {
        task: task.clone(),
        finding: finding.to_owned(),
        note: None,
        then: Vec::new(),
        followup: Vec::new(),
    }
}

/// A completion of `task` carrying only its validation.
fn finished(task: &TaskHandle, validation: impl Into<String>) -> TaskDone {
    TaskDone {
        task: task.clone(),
        validation: validation.into(),
        operation: None,
        disposition: None,
    }
}

/// A creation request carrying only its title and pitch.
fn adding(title: impl Into<String>, pitch: impl Into<String>) -> TaskAdd {
    TaskAdd {
        title: title.into(),
        pitch: pitch.into(),
        topic: None,
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        origin: None,
        flow: None,
        depends: Vec::new(),
        related: Vec::new(),
        intent: None,
        preparation: None,
        claim: false,
        draft: false,
    }
}

/// An edit of `task` that changes nothing until a caller names a field.
fn only(task: &TaskHandle) -> TaskEdit {
    TaskEdit {
        task: task.clone(),
        title: None,
        pitch: None,
        description: None,
        term: None,
        requirements: None,
        intent: None,
        priority: None,
        topic: None,
        flow: None,
        related: Vec::new(),
    }
}

/// The workflow status the newest `task::reviewed` fact for `task` recorded.
fn latest_review_status(bus: &Bus, task: &TaskHandle) -> body::TaskCompletionStatus {
    let born = task.born().expect("fixture handle carries a birth stamp");
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("review facts read")
    .facts()
    .iter()
    .rev()
    .find_map(|envelope| {
        envelope
            .fact
            .read::<body::TaskReviewed>()
            .filter(|reviewed| reviewed.task == born)
    })
    .expect("the task was reviewed")
    .status
}

fn residency(answer: &BoardAnswer, seat: &Seat) -> Witness {
    residency_witness(answer, seat.lane, seat.occupant)
}

fn declare_session(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    identity: CanonicalUuid,
    group: u32,
) {
    let before = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let mut residency = before.residency.body;
    let row = residency
        .lanes
        .iter_mut()
        .find(|row| row.lane == seat.lane)
        .expect("fixture lane");
    let session = wire::LaneSession {
        lane: row.lane,
        occupant: row.occupant.expect("fixture occupant"),
        worktree: row.worktree.clone(),
        vendor: row.vendor.clone(),
        launch: wire::body::LaneLaunchIdentity::new("sonnet", "high"),
        epoch,
        stream: "stdout".to_owned(),
        identity: identity.to_string(),
        locus: wire::TapeLocus {
            member: 0,
            offset: u64::from(group),
            length: 1,
        },
    };
    row.session = Some(session.clone());
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("resident fixture origin");
    let now = Instant::new(jiff::Timestamp::now()).expect("current fixture instant");
    let mut write = JournalWrite::new(Actor::Spice, origin, now, Producer::ResidentRecognition);
    let declared = write
        .fact(
            wire::body::LaneSessionDeclared {
                lane: session.lane,
                occupant: session.occupant,
                worktree: session.worktree,
                vendor: session.vendor,
                launch: session.launch,
                epoch: session.epoch,
                stream: session.stream,
                source: identity,
                identity: session.identity,
                member: session.locus.member,
                offset: session.locus.offset,
                length: session.locus.length,
            },
            FactCause::Existing(before.residency.position),
        )
        .expect("session declaration fact builds");
    let started = write
        .fact(
            wire::body::LaneStarted {
                lane: seat.lane,
                occupant: seat.occupant,
                epoch,
                group,
                fresh_session: true,
            },
            FactCause::Pending(declared),
        )
        .expect("lane-started fact builds");
    write.residency_head(started, move |_| residency);
    assert!(matches!(
        bus.commit(write).expect("session declaration commits"),
        CommitOutcome::Committed(_)
    ));
}

fn seed_creation_transcript(bus: &Bus, lane: CanonicalUuid) -> body::TaskCreationContext {
    seed_creation_stream(bus, lane, body::TASK_CREATION_CONTEXT_STREAM)
}

fn seed_creation_stream(bus: &Bus, lane: CanonicalUuid, stream: &str) -> body::TaskCreationContext {
    let vendor: wire::Name = "omp".parse().expect("fixture vendor");
    let source = uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed));
    let next = body::TranscriptPosition {
        member: 1,
        offset: 128,
    };
    let head = wire::TranscriptSourceHead {
        lane,
        vendor: vendor.clone(),
        stream: stream.to_owned(),
        source,
        next,
    };
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("lane:{lane}").parse().expect("fixture origin"),
        Instant::new(jiff::Timestamp::now()).expect("current fixture instant"),
        Producer::ResidentRecognition,
    );
    let ingested = write
        .fact(
            body::JournalIngested {
                lane,
                vendor,
                stream: stream.to_owned(),
                source,
                supersedes: None,
                from: body::TranscriptPosition {
                    member: 1,
                    offset: 0,
                },
                to: next,
                usage: Vec::new(),
                non_usage_records: 1,
                residue_records: 0,
            },
            FactCause::Root,
        )
        .expect("fixture ingestion fact builds");
    write.transcript_source_head(ingested, move |_| head.clone());
    assert!(matches!(
        bus.commit(write).expect("fixture transcript commits"),
        CommitOutcome::Committed(_)
    ));
    body::TaskCreationContext::Transcript {
        session: String::new(),
        boundary: wire::TranscriptSourceHead {
            lane,
            vendor: "omp".parse().expect("fixture vendor"),
            stream: stream.to_owned(),
            source,
            next,
        },
        creator_team: None,
    }
}

fn authorize_lane_creation(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
) -> body::TaskCreationContext {
    let identity = uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed));
    let group = u32::try_from(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed))
        .expect("fixture group fits u32");
    declare_session(bus, epoch, seat, identity, group);
    let mut context = seed_creation_transcript(bus, seat.lane);
    let body::TaskCreationContext::Transcript {
        session,
        creator_team,
        ..
    } = &mut context
    else {
        unreachable!("fixture transcript helper returns transcript authority")
    };
    *session = identity.to_string();
    *creator_team = Some(seat.team);
    context
}

fn claim_expected(answer: &BoardAnswer, task: &TaskHandle, seat: &Seat) -> Vec<Witness> {
    let heads = at(answer, task);
    vec![
        statement_witness(heads),
        workflow_witness(heads),
        claim_witness(heads),
        Witness::Eligibility {
            rule: "next".parse().expect("fixture allocator rule"),
        },
        residency(answer, seat),
    ]
}

fn clean(root: &Path, bus: Bus) {
    bus.verify().expect("fixture authority verifies");
    bus.shutdown().expect("fixture bus stops");
    fs::remove_dir_all(root).expect("fixture cleanup");
}

fn add_born(bus: &Bus, epoch: CanonicalUuid, born: wire::TaskStamp, title: &str) -> TaskHandle {
    let task = TaskHandle::for_topic(
        &"/board/identity"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        &born,
    );
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born.clone() }],
        &TaskAdd {
            topic: Some("/board/identity".parse().expect("fixture topic")),
            term: vec!["the fixture work is verified".to_owned()],
            origin: Some(fixture_origin()),
            flow: Some("todo".to_owned()),
            ..adding(title.to_owned(), "This work keeps the fixture honest.")
        },
    ));
    task
}

fn creation_fact_origin(bus: &Bus, task: &TaskHandle) -> wire::Origin {
    let born = task.born().expect("fixture handle carries a birth stamp");
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("creation facts read")
    .facts()
    .iter()
    .find(|fact| {
        fact.fact.kind() == wire::FactKind::TaskAdded
            && fact
                .fact
                .body()
                .get("task")
                .and_then(serde_json::Value::as_str)
                == Some(born.as_str())
    })
    .unwrap_or_else(|| panic!("task {task} has one creation fact"))
    .origin
    .clone()
}

fn fixture_instant(context: &str) -> Instant {
    "2030-04-12T18:44:09.271Z"
        .parse()
        .unwrap_or_else(|error| panic!("{context}: {error}"))
}

fn fixture_instant_after(at: Instant, seconds: i64) -> Instant {
    Instant::new(
        at.get()
            .checked_add(jiff::SignedDuration::from_secs(seconds))
            .expect("fixture instant remains in range"),
    )
    .expect("fixture instant is canonical")
}

/// A held task is named on the claim line and counted as active.
///
/// The snapshot above reports every claim field at rest, where a wrong
/// predicate and a right one agree: with nothing claimed, `claim -` and
/// `active 0` are what any filter produces. Bootstrap step three exists to
/// tell an agent which handle it already holds, so the reading that matters
/// is the one taken while a claim is live, and it has to distinguish the
/// principal asking from the other lanes holding rows of their own.
#[test]
fn task_status_names_the_claim_the_asking_principal_holds() {
    let (root, bus, epoch, seats) = fixture("task-bootstrap-claim", 2);
    let lane = Actor::Lane(seats[0].lane);
    let held = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CLI-held00001",
        "Hold one native bootstrap row",
        Some("todo"),
    );
    let available = board(&bus, epoch, lane.clone());
    assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimWin,
        claim_expected(&available, &held, &seats[0]),
        &next_rule(),
    ));

    let claimed = board(&bus, epoch, lane);
    let lease = at(&claimed, &held)
        .latest_claim()
        .and_then(|held| held.lease.as_ref())
        .expect("manual claim writes a lease");
    assert_eq!(
        lease.body.expiry,
        fixture_instant_after(lease.body.start, 60 * 60),
        "manual task claims keep the one-hour lease",
    );
    assert_eq!(
        lease.body.maximum, lease.body.expiry,
        "ordinary claims carry no separate manual extension cap",
    );

    let holder = task_status(&claimed).expect("holder reads status");
    assert!(
        holder.lines().any(|line| line
            == format!(
                "claim {held} phase:{} rolling until {}",
                at(&claimed, &held).workflow.body.phase,
                lease.body.expiry
            )),
        "{holder}"
    );
    assert!(holder.lines().any(|line| line == "active 1"), "{holder}");

    // A peer's claim is still active work on the board, but it is not this
    // principal's, so the same row has to count without being named.
    let other =
        task_status(&board(&bus, epoch, Actor::Lane(seats[1].lane))).expect("peer reads status");
    assert!(other.lines().any(|line| line == "claim -"), "{other}");
    assert!(other.lines().any(|line| line == "active 1"), "{other}");
    clean(&root, bus);
}

#[test]
fn current_board_edits_are_guarded_deterministic_and_render_form_invariant() {
    let (root, bus, epoch, seats) = fixture("edits", 1);
    let initial = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        actor_seat(&initial).expect("sole operator seat"),
        (seats[0].lane, seats[0].occupant,)
    );
    let (alpha, beta, _) = seed_edit_tasks(&bus, epoch);
    exercise_deferral_staleness(&bus, epoch, &alpha);
    add_dependency(&bus, epoch, &alpha, &beta);
    let graph = assert_graph_handout_and_ledger(&bus, epoch, &alpha);
    assert_render_form_parity(&graph);
    clean(&root, bus);
}

#[test]
fn capture_claims_one_exact_eligible_non_allocator_task() {
    let (root, bus, epoch, seats) = fixture("capture", 1);
    let lane = Actor::Lane(seats[0].lane);
    let alpha = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CAP-alpha001",
        "Allocator winner",
        Some("todo"),
    );
    let beta = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "CAP-beta0001",
        "Exact capture target",
        Some("todo"),
    );
    let available = board(&bus, epoch, lane.clone());
    assert_eq!(available.active_tasks().count(), 2);
    let entries = actor_entries(&available).expect("capture board entries are valid");
    assert_eq!(
        board::select(
            &entries,
            &lane,
            &allocation_for(&available, &lane),
            &available.environment(),
        )
        .expect("allocator winner")
        .entry()
        .handle(),
        &alpha,
    );
    let heads = at(&available, &beta);
    let rule: Name = "capture".parse().expect("capture rule");
    assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimWin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility { rule: rule.clone() },
            residency(&available, &seats[0]),
        ],
        &ClaimWin {
            rule,
            recovery: None,
        },
    ));

    let claimed = board(&bus, epoch, lane);
    assert!(at(&claimed, &alpha).latest_claim().is_none());
    assert_eq!(
        at(&claimed, &beta)
            .latest_claim()
            .map(|held| &held.claim)
            .expect("capture claim")
            .body
            .disposition,
        ClaimDisposition::Active,
    );
    clean(&root, bus);
}

#[test]
fn exact_recovery_names_one_ready_task_and_journals_its_transaction() {
    let (root, bus, epoch, seats) = fixture("recover-exact", 1);
    let lane = Actor::Lane(seats[0].lane);
    let alpha = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "RECOVER-alpha1",
        "Allocator winner",
        Some("todo"),
    );
    let beta = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "RECOVER-beta01",
        "Exact recovery target",
        Some("todo"),
    );
    let available = board(&bus, epoch, lane.clone());
    let entries = actor_entries(&available).expect("recovery board entries are valid");
    assert_eq!(
        board::select(
            &entries,
            &lane,
            &allocation_for(&available, &lane),
            &available.environment(),
        )
        .expect("allocator winner")
        .entry()
        .handle(),
        &alpha,
    );
    let heads = at(&available, &beta);
    let rule: Name = "recover".parse().expect("recovery rule");
    let transaction = "a".repeat(40);
    assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimWin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility { rule: rule.clone() },
            residency(&available, &seats[0]),
        ],
        &ClaimWin {
            rule,
            recovery: Some(transaction.clone()),
        },
    ));

    let claimed = board(&bus, epoch, lane);
    assert!(at(&claimed, &alpha).latest_claim().is_none());
    let claim = at(&claimed, &beta)
        .latest_claim()
        .map(|held| &held.claim)
        .expect("recovery claim is active");
    assert_eq!(claim.body.recovery.as_deref(), Some(transaction.as_str()));
    clean(&root, bus);
}

#[test]
fn recovery_identity_is_admitted_only_by_the_recover_rule() {
    let (root, bus, epoch, seats) = fixture("recover-rule", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "RECOVER-rule01",
        "Rule-bound recovery",
        Some("todo"),
    );
    let available = board(&bus, epoch, lane.clone());
    let heads = at(&available, &task);
    let rule: Name = "next".parse().expect("next rule");
    let refused = issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimWin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility { rule: rule.clone() },
            residency(&available, &seats[0]),
        ],
        &ClaimWin {
            rule,
            recovery: Some("a".repeat(40)),
        },
    );
    assert!(
        matches!(
            refused,
            Outcome::Refusal(ref refusal) if refusal.reason() == "git::recovery-rule-mismatch"
        ),
        "ordinary next admitted a recovery identity: {refused:?}",
    );
    for recover in ["recover", RECOVER_STEAL_RULE] {
        let recover: Name = recover.parse().expect("recovery rule");
        for (identity, reason) in [
            (None, "git::recovery-transaction-missing"),
            (Some("a".repeat(39)), "git::recovery-transaction-invalid"),
        ] {
            let refused = issue(
                &bus,
                epoch,
                lane.clone(),
                CommandKind::ClaimWin,
                vec![
                    statement_witness(heads),
                    workflow_witness(heads),
                    claim_witness(heads),
                    Witness::Eligibility {
                        rule: recover.clone(),
                    },
                    residency(&available, &seats[0]),
                ],
                &ClaimWin {
                    rule: recover.clone(),
                    recovery: identity,
                },
            );
            assert!(
                matches!(&refused, Outcome::Refusal(refusal) if refusal.reason() == reason),
                "invalid {recover:?} identity did not refuse as {reason}: {refused:?}",
            );
        }
    }
    assert!(
        at(&board(&bus, epoch, Actor::Lane(seats[0].lane)), &task)
            .latest_claim()
            .is_none()
    );
    clean(&root, bus);
}

#[test]
fn recovery_restart_classifies_only_its_exact_claim_as_postclaim() {
    let (root, bus, epoch, seats) = fixture("recover-restart", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "RECOVER-restart",
        "Restart-visible recovery",
        Some("todo"),
    );
    let available = board(&bus, epoch, lane.clone());
    let transaction = "b".repeat(40);
    let boundary = GitBoundary {
        head: "synchronized-head".to_owned(),
        integrated: "synchronized-head".to_owned(),
        upstream: Some("origin/main".to_owned()),
        upstream_head: Some("synchronized-head".to_owned()),
        local_commits: 0,
    };
    let sealed = recovery_preflight(&available, &task)
        .expect("ready task admits recovery preflight")
        .seal(boundary);
    assert!(matches!(
        recovery_board_state(&available, &task, &transaction, &sealed),
        RecoveryBoardState::PreClaim
    ));

    let heads = at(&available, &task);
    let rule: Name = "recover".parse().expect("recovery rule");
    assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimWin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility { rule: rule.clone() },
            residency(&available, &seats[0]),
        ],
        &ClaimWin {
            rule,
            recovery: Some(transaction.clone()),
        },
    ));
    let claimed = board(&bus, epoch, lane);
    assert!(matches!(
        recovery_board_state(&claimed, &task, &transaction, &sealed),
        RecoveryBoardState::PostClaim { .. }
    ));

    let mut impostor = claimed.clone();
    impostor
        .tasks
        .iter_mut()
        .find(|heads| heads.statement.body.handle == task)
        .expect("claimed task remains visible")
        .claims
        .last_mut()
        .expect("claimed task")
        .claim
        .body
        .recovery = None;
    assert!(matches!(
        recovery_board_state(&impostor, &task, &transaction, &sealed),
        RecoveryBoardState::Ambiguous(_)
    ));
    clean(&root, bus);
}

#[test]
fn recovery_foreign_claim_then_free_is_explicitly_restorable() {
    let (root, bus, epoch, seats) = fixture("recover-foreign-free", 2);
    let recovering = Actor::Lane(seats[0].lane);
    let foreign = Actor::Lane(seats[1].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "RECOVER-foreign",
        "Foreign claim moved the recovery head",
        Some("todo"),
    );
    let available = board(&bus, epoch, recovering.clone());
    let transaction = "c".repeat(40);
    let sealed = recovery_preflight(&available, &task)
        .expect("ready task admits recovery preflight")
        .seal(GitBoundary {
            head: "synchronized-head".to_owned(),
            integrated: "synchronized-head".to_owned(),
            upstream: Some("origin/main".to_owned()),
            upstream_head: Some("synchronized-head".to_owned()),
            local_commits: 0,
        });

    claim_task(&bus, epoch, &seats[1], &foreign, &task);
    let held = board(&bus, epoch, foreign);
    let heads = at(&held, &task);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::Lane(seats[1].lane),
        CommandKind::ClaimFree,
        vec![
            held_claim_witness(heads),
            held_lease_witness(heads),
            residency(&held, &seats[1]),
        ],
        &TaskRef { task: task.clone() },
    ));

    let moved = board(&bus, epoch, recovering);
    assert!(matches!(
        recovery_board_state(&moved, &task, &transaction, &sealed),
        RecoveryBoardState::Restorable(_)
    ));
    assert_eq!(
        at(&moved, &task)
            .latest_claim()
            .map(|held| &held.claim)
            .expect("foreign claim head remains")
            .body
            .disposition,
        ClaimDisposition::Freed,
    );
    clean(&root, bus);
}

#[test]
fn claims_warn_on_shared_statement_terms_not_a_shared_topic_prefix() {
    let (root, bus, epoch, seats) = fixture("claim-overlap", 3);
    let first = add_task_with_term(
        &bus,
        epoch,
        "MOSAIC-1kKQTw1Vx",
        "App lacks the mosaic lane interface",
        "/board/alpha",
        Some("todo"),
        &["The watchband mosaic uses per-lane cards and stays stable"],
    );
    let overlapping = add_task_with_term(
        &bus,
        epoch,
        "FIDELIT-1kKRSfyVx",
        "Rebuild the lane watchband and message mosaic",
        "/rim/beta",
        Some("todo"),
        &["The mosaic watchband keeps focused-agent state visible"],
    );
    let shared_prefix = add_task_with_term(
        &bus,
        epoch,
        "MOSAIC-1kKQTw1Wx",
        "Audit unrelated board output",
        "/board/gamma",
        Some("todo"),
        &["Export signed ledger receipts for audit and keep output stable"],
    );

    let first_answer = capture_for(&bus, epoch, &seats[0], &first);
    assert!(first_answer.warnings.is_empty());

    let overlap_answer = capture_for(&bus, epoch, &seats[1], &overlapping);
    assert_eq!(
        overlap_answer.warnings.as_ref(),
        &[wire::intent::ClaimWarning {
            task: first,
            lane: seats[0].lane,
            surface: vec![
                "lane".to_owned(),
                "mosaic".to_owned(),
                "watchband".to_owned(),
                "work".to_owned(),
            ],
        }]
    );

    let shared_prefix_answer = capture_for(&bus, epoch, &seats[2], &shared_prefix);
    assert!(shared_prefix_answer.warnings.is_empty());
    clean(&root, bus);
}

fn capture_for(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
) -> wire::intent::ClaimAnswer {
    let lane = Actor::Lane(seat.lane);
    let available = board(bus, epoch, lane.clone());
    let heads = at(&available, task);
    let rule: Name = "capture".parse().expect("capture rule");
    let answer = assert_committed(issue(
        bus,
        epoch,
        lane,
        CommandKind::ClaimWin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility { rule: rule.clone() },
            residency(&available, seat),
        ],
        &ClaimWin {
            rule,
            recovery: None,
        },
    ));
    let CommandAnswer::Claim(answer) = answer else {
        panic!("claim win returned another answer shape: {answer:?}");
    };
    *answer
}

#[test]
fn stealing_takes_a_live_claim_at_once_where_capture_is_refused() {
    let (root, bus, epoch, seats) = fixture("steal", 2);
    let holder = Actor::Lane(seats[0].lane);
    let rival = Actor::Lane(seats[1].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "STEAL-live01",
        "Held under a live lease",
        Some("todo"),
    );

    let available = board(&bus, epoch, holder.clone());
    let heads = at(&available, &task);
    let capture: Name = "capture".parse().expect("capture rule");
    assert_committed(issue(
        &bus,
        epoch,
        holder.clone(),
        CommandKind::ClaimWin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility {
                rule: capture.clone(),
            },
            residency(&available, &seats[0]),
        ],
        &ClaimWin {
            rule: capture,
            recovery: None,
        },
    ));

    let held = board(&bus, epoch, rival.clone());
    let heads = at(&held, &task);
    let witnesses = |rule: &Name| {
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility { rule: rule.clone() },
            residency(&held, &seats[1]),
        ]
    };

    // Same board, same rival, same witnesses: only the rule differs, so a
    // steal that behaved like a capture would be refused right here.
    let capture: Name = "capture".parse().expect("capture rule");
    let declined = issue(
        &bus,
        epoch,
        rival.clone(),
        CommandKind::ClaimWin,
        witnesses(&capture),
        &ClaimWin {
            rule: capture,
            recovery: None,
        },
    );
    assert!(
        matches!(
            declined,
            Outcome::Refusal(ref refusal)
                if refusal.reason() == "claim::capture-ineligible"
        ),
        "capture took a held claim: {declined:?}",
    );

    let steal: Name = "steal".parse().expect("steal rule");
    let stolen = assert_committed(issue(
        &bus,
        epoch,
        rival.clone(),
        CommandKind::ClaimWin,
        witnesses(&steal),
        &ClaimWin {
            rule: steal,
            recovery: None,
        },
    ));
    let CommandAnswer::Claim(stolen) = stolen else {
        panic!("steal returned another answer shape: {stolen:?}");
    };
    assert!(!stolen.nothing_stolen, "a held claim was displaced");

    // The lease the holder still had did not delay the transfer, and the
    // ClaimWon naming the new holder is what tells the old one.
    let after = board(&bus, epoch, rival.clone());
    let taken = at(&after, &task)
        .latest_claim()
        .map(|held| &held.claim)
        .expect("stolen claim");
    assert_eq!(taken.body.actor, rival);
    assert_eq!(taken.body.disposition, ClaimDisposition::Active);
    assert_ne!(taken.body.actor, holder);
    clean(&root, bus);
}

#[test]
fn stealing_takes_an_assigned_review_after_the_previous_holder_freed_it() {
    let (root, bus, epoch, seats) = fixture("steal-review", 2);
    let author = Actor::Lane(seats[0].lane);
    let reviewer = Actor::TEST_PRINCIPAL;
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "STEAL-r01",
        "Freed holder review",
        Some("todo,review"),
    );
    advance_task_to_review(&bus, epoch, &seats[0], &author, &task);

    let operator_board = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let operator_heads = at(&operator_board, &task);
    let capture: Name = "capture".parse().expect("capture rule");
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::ClaimWin,
        vec![
            statement_witness(operator_heads),
            workflow_witness(operator_heads),
            claim_witness(operator_heads),
            Witness::Eligibility {
                rule: capture.clone(),
            },
            residency(&operator_board, &seats[0]),
        ],
        &ClaimWin {
            rule: capture,
            recovery: None,
        },
    ));
    let assigned = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let assigned_heads = at(&assigned, &task);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::ClaimFree,
        vec![
            held_claim_witness(assigned_heads),
            held_lease_witness(assigned_heads),
            residency(&assigned, &seats[0]),
        ],
        &TaskRef { task: task.clone() },
    ));

    let steal_board = board(&bus, epoch, reviewer.clone());
    let steal_heads = at(&steal_board, &task);
    assert_eq!(
        steal_heads
            .latest_claim()
            .map(|held| &held.claim)
            .expect("claim")
            .body
            .disposition,
        ClaimDisposition::Freed
    );
    let steal: Name = "steal".parse().expect("steal rule");
    let stolen = assert_committed(issue(
        &bus,
        epoch,
        reviewer.clone(),
        CommandKind::ClaimWin,
        vec![
            statement_witness(steal_heads),
            workflow_witness(steal_heads),
            claim_witness(steal_heads),
            Witness::Eligibility {
                rule: steal.clone(),
            },
            residency(&steal_board, &seats[1]),
        ],
        &ClaimWin {
            rule: steal,
            recovery: None,
        },
    ));
    let CommandAnswer::Claim(stolen_answer) = stolen else {
        panic!("steal returned another answer shape: {stolen:?}");
    };
    assert!(
        stolen_answer.nothing_stolen,
        "a freed claim has no holder to displace"
    );

    let after = board(&bus, epoch, reviewer.clone());
    let stolen = at(&after, &task);
    let claim = stolen
        .latest_claim()
        .map(|held| &held.claim)
        .expect("claim");
    assert_eq!(claim.body.actor, reviewer);
    assert_eq!(claim.body.disposition, ClaimDisposition::Active);
    assert_review_assignment_fact(&bus, &after, &task, &reviewer);
    clean(&root, bus);
}

#[test]
fn stealing_claims_a_task_nobody_holds_and_warns_nothing_was_stolen() {
    let (root, bus, epoch, seats) = fixture("steal-unheld", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "STEAL-free01",
        "Never claimed",
        Some("todo"),
    );

    let available = board(&bus, epoch, lane.clone());
    let heads = at(&available, &task);
    assert!(heads.latest_claim().is_none(), "the task starts unclaimed");
    let steal: Name = "steal".parse().expect("steal rule");
    let won = assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimWin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility {
                rule: steal.clone(),
            },
            residency(&available, &seats[0]),
        ],
        &ClaimWin {
            rule: steal,
            recovery: None,
        },
    ));
    let CommandAnswer::Claim(won) = won else {
        panic!("steal returned another answer shape: {won:?}");
    };
    assert!(won.nothing_stolen, "nobody held the task");

    let after = board(&bus, epoch, lane.clone());
    let claim = at(&after, &task)
        .latest_claim()
        .map(|held| &held.claim)
        .expect("claim");
    assert_eq!(claim.body.actor, lane);
    assert_eq!(claim.body.disposition, ClaimDisposition::Active);
    clean(&root, bus);
}

// Lane lifecycle, review, and document tests share this fixture harness.
// Their execution seam lives separately so each source stays reviewable.
mod resolution;
use resolution::*;

// Review diagrams join durable lifecycle facts, rather than live review
// claims, through the same command surface operators use.
mod review_graph;
// A review whose reviewer lapsed must reach the next lane; the lapse ends that
// review claim and moves no workflow.
mod lapsed_review;

// The closing boundaries the completion and review transactions record.
// Kept beside the resolution seam because they drive its helpers, and
// separate from it because each fact is pinned on its own.
mod closing_boundary;

// Board row fixtures the sibling modules share.
mod fixture_rows;
use fixture_rows::*;

// Drafts are prospective work outside every execution rail until the same
// explicit file transition admits them as ordinary work.
mod draft_filing;
// Crude unplaced intake rests submitted in draft and is triaged by an ordinary
// claim; held and controlled drafts stay off the allocator.
mod intake;
// A Draft's images are an artifact set replaced whole, admitted before any
// artifact moves, and refused once the row is filed or settled.
mod draft_media;

// Unexpected-behavior intake is ordinary Plan preparation under one typed,
// repository-owned Design; exact candidate decisions are its only release.
mod oops_design;

// A previewed debt sweep files one Plan and one child per file atomically.
mod sweep_filing;

// Statement correction and its refusals are one bounded scenario seam, so
// this file stays inside the structural review bound as it grows.
mod edit;
use edit::*;
// The pitch every statement leads with: its mechanical refusals, the hold its
// prose findings raise, and the revision an authored change advances.
mod pitch;
// Whole-text replacement of a description or the terms through the modify
// intent, admitted only over the digest of the text its writer read.
mod task_text;
// Declared Plan and Design intent: durable across phases and corrections,
// moved only by its own witnessed edit.
mod intent;

// Claimed dependency edits carry possession through one atomic statement-head
// replacement and keep the complete graph acyclic.
mod dependencies;
use dependencies::{dependency_witnesses, todo_task};

// The empty-drain readout a steered lane receives measures the parked board
// with the same split task status reports, so both are pinned on one board.
mod drain_readout;
mod duplicate;
mod task_creation_flow;
mod wake_claim;
// A completion that enters a continuing phase without publishing mints the
// next phase's claim for the completing seat inside the same write.
mod carried_claim;
// Completion targets: who may set them, and how open capacity seats distinct
// lanes whose claims end one exact claim at a time.
mod completion_target;
// Direct and invited joins share one guarded claim path; the scenarios pin its
// capacity and invitation-retirement boundaries together.
mod join;
// Occupant codes select lanes through every occupant a lane has started.
mod lane_codes;
// Several lanes review one task: each clean verdict arrives in the shared
// round, and a finding opens a fresh one.
mod shared_review;

// Withdrawal, its refusals, and what a withdrawn row releases are one
// bounded scenario seam, kept beside the correction seam for the same
// structural reason.
mod delete;

// Operator closure shares withdrawal's settlement seam but completes the row.
mod close;

// A hand-taken claim's completion contract follows its linked work's standing.
mod claim_contracts;

// One selected task's append-only notes are a bounded read contract, separate
// from the board summary and its allocation scenarios.
mod detail;
mod relations;
mod timeline;
