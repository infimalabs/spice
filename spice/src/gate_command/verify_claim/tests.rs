use std::cell::Cell;

use wire::{
    Actor, ClaimCell, ClaimDisposition, ClaimHead, LeaseHead, Phase, ResidentState, Seq, TaskHead,
    TaskStamp, WorkflowHead,
};

use super::*;

const WORKTREE: &str = "/worktrees/current";

#[test]
fn no_claim_refuses_before_the_verifier_is_launched() {
    let fixture = Fixture::new();
    let launched = Cell::new(false);
    let refusal = launch_after_admission(admit_snapshot(fixture.snapshot(None)), || {
        launched.set(true)
    })
    .expect_err("missing claim refuses");

    assert!(matches!(refusal, VerifyClaimRefusal::MissingClaim));
    assert!(!launched.get(), "refusal launched the full verifier");
}

#[test]
fn stale_claim_refuses() {
    let fixture = Fixture::new();
    let mut entry = fixture.entry("review");
    entry.lease = Some(lease(fixture.epoch, "2026-08-21T00:00:04.000Z"));
    let entry = entry.admitted();

    let refusal =
        admit_snapshot(fixture.snapshot(Some(&entry))).expect_err("expired claim refuses");
    assert!(matches!(
        refusal,
        VerifyClaimRefusal::InactiveClaim(board::ClaimRefusal::Expired { .. })
    ));
}

#[test]
fn foreign_worktree_claim_refuses() {
    let mut fixture = Fixture::new();
    fixture.residency.lanes[0].worktree = "/worktrees/peer".to_owned();
    let entry = fixture.entry("review").admitted();

    let refusal = admit_snapshot(fixture.snapshot(Some(&entry)))
        .expect_err("another worktree's claim refuses");
    assert!(matches!(
        refusal,
        VerifyClaimRefusal::ForeignWorktree { .. }
    ));
}

#[test]
fn non_review_phase_refuses() {
    let fixture = Fixture::new();
    let entry = fixture.entry("todo").admitted();

    let refusal = admit_snapshot(fixture.snapshot(Some(&entry)))
        .expect_err("ordinary task phase refuses full gate");
    assert!(matches!(refusal, VerifyClaimRefusal::Phase(phase) if phase == "todo"));
}

#[test]
fn matching_live_review_claim_admits() {
    let fixture = Fixture::new();
    let entry = fixture.entry("review").admitted();

    admit_snapshot(fixture.snapshot(Some(&entry))).expect("exact live review claim admits");
}

struct Fixture {
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    epoch: CanonicalUuid,
    residency: ResidencyHead,
    at: Instant,
}

impl Fixture {
    fn new() -> Self {
        let lane = uuid(1);
        let occupant = uuid(2);
        let epoch = uuid(3);
        let residency = ResidencyHead {
            epoch,
            state: ResidentState::Seated,
            topology_revision: 0,
            lanes: vec![wire::LaneResidency {
                lane,
                team: lane,
                name: None,
                worktree: WORKTREE.to_owned(),
                life: "drive".to_owned(),
                vendor: "omp".parse().expect("fixture vendor"),
                model: None,
                effort: None,
                fast_mode: false,
                occupant: Some(occupant),
                lineage: vec![occupant],
                capabilities: Vec::new(),
                session: None,
                disposition: "occupied".to_owned(),
            }],
            subscriptions: Vec::new(),
            renewals: Vec::new(),
        };
        Self {
            lane,
            occupant,
            epoch,
            residency,
            at: instant("2026-08-21T00:00:05.000Z"),
        }
    }

    fn snapshot<'a>(&'a self, entry: Option<&'a Entry>) -> VerifyClaimSnapshot<'a> {
        VerifyClaimSnapshot {
            worktree: Path::new(WORKTREE),
            lane: self.lane,
            residency: &self.residency,
            entry,
            at: self.at,
        }
    }

    fn entry(&self, current: &str) -> EntryFixture {
        EntryFixture {
            statement: statement(),
            workflow: workflow(current),
            claim: Some(ClaimHead {
                actor: Actor::Lane(self.lane),
                fact: seq(4),
                claimed: seq(4),
                cell: ClaimCell {
                    topic: Some("/gate/selection".parse().expect("fixture topic")),
                    phase: phase(current),
                },
                lane: self.lane,
                occupant: self.occupant,
                epoch: self.epoch,
                recovery: None,
                disposition: ClaimDisposition::Active,
                acknowledged: 0,
            }),
            lease: Some(lease(self.epoch, "2026-08-21T00:00:10.000Z")),
        }
    }
}

struct EntryFixture {
    statement: TaskHead,
    workflow: WorkflowHead,
    claim: Option<ClaimHead>,
    lease: Option<LeaseHead>,
}

impl EntryFixture {
    fn admitted(&self) -> Entry {
        Entry::new(
            self.statement.clone(),
            self.workflow.clone(),
            wire::CompletionHead::opened(seq(1)),
            self.claim
                .clone()
                .map(|claim| (claim, self.lease.clone()))
                .into_iter()
                .collect(),
        )
        .expect("fixture entry admits")
    }
}

fn statement() -> TaskHead {
    let born = TaskStamp::for_micros(1).expect("fixture task stamp");
    TaskHead {
        preparation: None,
        intent: None,
        born,
        topic_origin: seq(1),
        title: "Run full verification".to_owned(),
        pitch: "This work keeps the fixture honest.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: "/gate/selection".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: wire::TaskProvenance::OperatorDraft,
        creation: wire::body::TaskCreationContext::unavailable(
            wire::body::TaskCreationUnavailable::SpiceActor,
        ),
        revision: seq(1),
    }
}

fn workflow(current: &str) -> WorkflowHead {
    WorkflowHead {
        flow: ["draft", "todo", "review"].map(phase).to_vec(),
        phase: phase(current),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    }
}

fn lease(epoch: CanonicalUuid, expiry: &str) -> LeaseHead {
    LeaseHead {
        claimed: seq(4),
        start: instant("2026-08-21T00:00:00.000Z"),
        renewal: instant("2026-08-21T00:00:02.000Z"),
        maximum: instant("2026-08-21T00:00:20.000Z"),
        expiry: instant(expiry),
        epoch,
    }
}

fn phase(value: &str) -> Phase {
    value.parse().expect("fixture phase")
}

fn instant(value: &str) -> Instant {
    value.parse().expect("fixture instant")
}

fn seq(value: u64) -> Seq {
    Seq::new(value).expect("fixture sequence")
}

fn uuid(value: u64) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{value:012x}")
        .parse()
        .expect("fixture UUID")
}
