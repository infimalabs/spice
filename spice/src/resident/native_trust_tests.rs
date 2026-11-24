//! Native app trust decisions over task-bound producing worktrees.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bus::{Bus, CommitOutcome};
use journal::{FactCause, JournalWrite};
use spice::repository::Repository;
use wire::intent::TrustRef;
use wire::{Actor, CanonicalUuid, CommandKind, Outcome, Producer, TrustState, Witness};

use super::RuntimePaths;
use super::policy_surface::{trust_pend, trust_rule, trust_rule_with};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

#[test]
fn uncaptured_native_decisions_record_provenance_and_the_requested_terminal_standing() {
    for (kind, standing) in [
        (CommandKind::TrustGrant, TrustState::Granted),
        (CommandKind::TrustRevoke, TrustState::Revoked),
    ] {
        let fixture = Fixture::new(match kind {
            CommandKind::TrustGrant => "native-capture-grant",
            CommandKind::TrustRevoke => "native-capture-revoke",
            _ => unreachable!(),
        });
        assert!(fixture.bus.trust_head(&fixture.digest).unwrap().is_none());

        let outcome = fixture.rule(kind, Actor::TEST_PRINCIPAL, false);
        assert!(matches!(outcome, Outcome::Reply(_)), "{outcome:?}");
        let head = fixture
            .bus
            .trust_head(&fixture.digest)
            .expect("trust head reads")
            .expect("native decision records a trust head");
        assert_eq!(head.body().state, standing);
        assert_eq!(
            head.body().worktree.as_deref(),
            fixture.root.to_str(),
            "the resident records its resolved producing worktree"
        );
        let snapshot = fixture
            .bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("trust facts read");
        assert!(
            snapshot
                .facts()
                .iter()
                .any(|row| { row.fact.kind() == wire::FactKind::TrustPended })
        );
        let terminal = match standing {
            TrustState::Granted => wire::FactKind::TrustGranted,
            TrustState::Revoked => wire::FactKind::TrustRevoked,
            _ => unreachable!(),
        };
        assert!(
            snapshot
                .facts()
                .iter()
                .any(|row| row.fact.kind() == terminal)
        );
        fixture.finish();
    }
}

#[test]
fn pended_digests_record_their_documents_and_rule_after_their_producer_moves() {
    for (cli, kind, standing) in [
        (false, CommandKind::TrustGrant, TrustState::Granted),
        (false, CommandKind::TrustRevoke, TrustState::Revoked),
        (true, CommandKind::TrustGrant, TrustState::Granted),
        (true, CommandKind::TrustRevoke, TrustState::Revoked),
    ] {
        let fixture = Fixture::new(match (cli, kind) {
            (false, CommandKind::TrustGrant) => "native-pended-grant",
            (false, _) => "native-pended-revoke",
            (true, CommandKind::TrustGrant) => "cli-pended-grant",
            (true, _) => "cli-pended-revoke",
        });
        fixture.pend();
        let documents = fixture.pended_documents();
        assert_eq!(
            crate::trust_command::documents_digest(&documents),
            fixture.digest,
            "the pend records the exact text its digest names"
        );
        assert_eq!(
            documents.tracked.as_deref(),
            Some("# native trust fixture\n")
        );
        replace_policy(
            &fixture.root.join(wire::TRACKED_POLICY_FILE),
            "# the producer moved on\n",
        );
        let outcome = if cli {
            trust_rule(
                &fixture.bus,
                &fixture.repository,
                fixture.epoch,
                &fixture.cli_command(kind, TrustState::Pended),
                &fixture.reference(),
            )
        } else {
            fixture.rule(kind, Actor::TEST_PRINCIPAL, true)
        };
        assert!(matches!(outcome, Outcome::Reply(_)), "{outcome:?}");
        assert_eq!(
            fixture
                .bus
                .trust_head(&fixture.digest)
                .expect("trust head reads")
                .expect("trust head exists")
                .body()
                .state,
            standing
        );
        fixture.finish();
    }
}

#[test]
fn a_pended_digest_shows_its_recorded_text_against_the_latest_other_grant() {
    use wire::intent::{TrustDiffLine, TrustLineEnding, TrustShowAnswer};

    let show = |fixture: &Fixture, digest: &wire::Digest| -> TrustShowAnswer {
        match super::trust_show::read(
            &fixture.bus,
            &TrustRef {
                digest: digest.clone(),
                worktree: None,
            },
        ) {
            Outcome::Reply(reply) => match reply.body {
                wire::intent::CommandAnswer::TrustShow(answer) => *answer,
                body => panic!("unexpected answer {body:?}"),
            },
            outcome => panic!("trust show failed: {outcome:?}"),
        }
    };
    let fixture = Fixture::new("trust-show");
    fixture.pend();
    let first = show(&fixture, &fixture.digest);
    assert_eq!(first.baseline, None);
    let documents = first.documents.expect("the pend recorded its documents");
    assert_eq!(
        documents[0].lines,
        [TrustDiffLine::Added {
            text: "# native trust fixture".to_owned(),
            ending: TrustLineEnding::Lf,
        }],
        "with nothing else granted the full text reads as added"
    );
    let granted = trust_rule(
        &fixture.bus,
        &fixture.repository,
        fixture.epoch,
        &fixture.cli_command(CommandKind::TrustGrant, TrustState::Pended),
        &fixture.reference(),
    );
    assert!(matches!(granted, Outcome::Reply(_)), "{granted:?}");

    replace_policy(
        &fixture.root.join(wire::TRACKED_POLICY_FILE),
        "# native trust fixture\n[gate]\n",
    );
    let changed = crate::trust_command::Surface::load(&fixture.repository)
        .expect("changed surface loads")
        .digest()
        .clone();
    fixture.pend_digest(&changed);
    let second = show(&fixture, &changed);
    assert_eq!(second.standing, TrustState::Pended);
    let baseline = second.baseline.expect("the earlier grant is the baseline");
    assert_eq!(baseline.digest, fixture.digest);
    assert!(baseline.recorded);
    assert_eq!(
        second.documents.expect("the pend recorded its documents")[0].lines,
        [
            TrustDiffLine::Kept {
                text: "# native trust fixture".to_owned(),
                ending: TrustLineEnding::Lf,
            },
            TrustDiffLine::Added {
                text: "[gate]".to_owned(),
                ending: TrustLineEnding::Lf,
            },
        ]
    );
    fixture.finish();
}

#[test]
fn native_decisions_refuse_nonoperators_browser_paths_and_commit_time_surface_movement() {
    let fixture = Fixture::new("native-decision-guards");
    let before = fixture
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("facts before refusals");
    let unauthorized = raw_command(
        CommandKind::TrustGrant,
        fixture.epoch,
        vec![Witness::Task {
            task: fixture.task.clone(),
            head: fixture.statement,
        }],
        fixture.reference(),
        uuid(8_999),
    )
    .admit(
        &accepted(Actor::Lane(fixture.lane), fixture.epoch),
        &wire::principal::PrincipalRoles::default(),
    )
    .expect_err("lane trust ruling is refused at wire admission");
    assert!(matches!(
        unauthorized,
        wire::CommandRejection::Refusal(ref refusal)
            if refusal.reason() == "command::authority"
    ));

    let command = fixture.native_command(CommandKind::TrustGrant, Actor::TEST_PRINCIPAL, false);
    let mut browser_path = fixture.reference();
    browser_path.worktree = fixture.root.to_str().map(str::to_owned);
    let refused = trust_rule(
        &fixture.bus,
        &fixture.repository,
        fixture.epoch,
        &command,
        &browser_path,
    );
    assert!(matches!(
        refused,
        Outcome::Refusal(ref refusal) if refusal.reason() == "trust::worktree-unexpected"
    ));

    let tracked = fixture.root.join(wire::TRACKED_POLICY_FILE);
    let refused = trust_rule_with(
        &fixture.bus,
        &fixture.repository,
        fixture.epoch,
        &command,
        &fixture.reference(),
        || replace_policy(&tracked, "# moved before native trust commit\n"),
    );
    assert!(matches!(
        refused,
        Outcome::Refusal(ref refusal) if refusal.reason() == "trust::surface-moved"
    ));
    assert_eq!(
        fixture
            .bus
            .transact(|journal| bus::JournalTransaction::settled(
                journal.fact_snapshot().expect("journal snapshot")
            ))
            .expect("facts after refusals"),
        before,
        "refused native decisions changed authority"
    );
    fixture.finish();
}

struct Fixture {
    root: PathBuf,
    bus: Bus,
    repository: Repository,
    epoch: CanonicalUuid,
    task: wire::TaskStamp,
    statement: wire::Seq,
    lane: CanonicalUuid,
    digest: wire::Digest,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let number = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "spice-native-trust-{name}-{}-{number}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        crate::test_repository::initialize(&root);
        fs::write(
            root.join(wire::TRACKED_POLICY_FILE),
            "# native trust fixture\n",
        )
        .expect("tracked policy writes");
        let paths = RuntimePaths::discover(&root).expect("runtime paths resolve");
        fs::create_dir_all(&paths.data).expect("data directory exists");
        let repository = Repository::discover(&root).expect("fixture repository resolves");
        let root = repository.worktree_root().to_path_buf();
        let digest = crate::trust_command::Surface::load(&repository)
            .expect("fixture trust surface loads")
            .digest()
            .clone();
        let journal = journal::Journal::open(&paths.journal).expect("fixture journal opens");
        let bus = Bus::start(journal).expect("fixture bus starts");
        let epoch = uuid(number * 10 + 1);
        let lane = uuid(number * 10 + 2);
        let occupant = uuid(number * 10 + 3);
        let task = wire::TaskStamp::for_micros(
            i64::try_from(number + 1).expect("fixture number fits a task stamp"),
        )
        .expect("fixture task stamp");
        let statement = seed_task_authority(&bus, epoch, lane, occupant, &task, &digest, &root);
        Self {
            root,
            bus,
            repository,
            epoch,
            task,
            statement,
            lane,
            digest,
        }
    }

    fn reference(&self) -> TrustRef {
        TrustRef {
            digest: self.digest.clone(),
            worktree: None,
        }
    }

    fn native_command(
        &self,
        kind: CommandKind,
        actor: Actor,
        pended: bool,
    ) -> wire::AdmittedCommand {
        let mut expected = vec![Witness::Task {
            task: self.task.clone(),
            head: self.statement,
        }];
        if pended {
            expected.push(Witness::Trust {
                digest: self.digest.clone(),
                standing: TrustState::Pended,
            });
        }
        admitted_command(
            kind,
            self.epoch,
            actor,
            expected,
            self.reference(),
            uuid(9_000 + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)),
        )
    }

    fn cli_command(&self, kind: CommandKind, standing: TrustState) -> wire::AdmittedCommand {
        admitted_command(
            kind,
            self.epoch,
            Actor::TEST_PRINCIPAL,
            vec![Witness::Trust {
                digest: self.digest.clone(),
                standing,
            }],
            self.reference(),
            uuid(9_000 + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)),
        )
    }

    fn rule(
        &self,
        kind: CommandKind,
        actor: Actor,
        pended: bool,
    ) -> Outcome<wire::intent::CommandAnswer> {
        trust_rule(
            &self.bus,
            &self.repository,
            self.epoch,
            &self.native_command(kind, actor, pended),
            &self.reference(),
        )
    }

    fn pend(&self) {
        self.pend_digest(&self.digest);
    }

    fn pend_digest(&self, digest: &wire::Digest) {
        let worktree = self.root.to_str().expect("fixture path is UTF-8");
        let reference = TrustRef {
            digest: digest.clone(),
            worktree: Some(worktree.to_owned()),
        };
        let command = admitted_command(
            CommandKind::TrustPend,
            self.epoch,
            Actor::TEST_PRINCIPAL,
            vec![Witness::Trust {
                digest: digest.clone(),
                standing: TrustState::Pended,
            }],
            reference.clone(),
            uuid(9_000 + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)),
        );
        let outcome = trust_pend(
            &self.bus,
            &self.repository,
            self.epoch,
            &command,
            &reference,
        );
        assert!(matches!(outcome, Outcome::Reply(_)), "{outcome:?}");
    }

    fn pended_documents(&self) -> wire::body::TrustDocuments {
        let snapshot = self
            .bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read");
        let pends = snapshot
            .facts()
            .iter()
            .filter(|fact| fact.fact.kind() == wire::FactKind::TrustPended)
            .map(|envelope| {
                let bytes = wire::encode(&envelope.fact).expect("trust fact encodes");
                wire::decode::<wire::Fact<wire::body::TrustPended>>(&bytes)
                    .expect("trust fact decodes")
                    .body
            })
            .collect::<Vec<_>>();
        let [pended] = pends.as_slice() else {
            panic!("one pend, found {}", pends.len());
        };
        pended
            .documents
            .clone()
            .expect("the pend records its documents")
    }

    fn finish(self) {
        self.bus.verify().expect("native trust authority verifies");
        self.bus.shutdown().expect("fixture bus stops");
        fs::remove_dir_all(self.root).expect("fixture cleanup");
    }
}

fn seed_task_added(
    write: &mut JournalWrite,
    task: &wire::TaskStamp,
    digest: &wire::Digest,
) -> journal::PendingFact {
    write
        .fact(
            wire::body::TaskAdded {
                preparation: None,
                intent: None,
                task: task.clone(),
                title: "Native trust fixture".into(),
                pitch: "The exact executable trust decision is native.".to_owned(),
                description: None,
                term: vec!["The trust decision is durable".to_owned()],
                requirements: wire::EnvironmentRequirements::one(
                    wire::EnvironmentRequirement::trust_grant(digest.clone()),
                ),
                topic: "/app/native_trust".parse().expect("fixture topic"),
                submitted: false,
                priority: None,
                taste: None,
                depends: Vec::new(),
                flow: vec![wire::Phase::Draft, "todo".parse().expect("fixture phase")],
                phase: "todo".parse().expect("fixture phase"),
                creation: wire::body::TaskCreationContext::unavailable(
                    wire::body::TaskCreationUnavailable::SpiceActor,
                ),
                revision: 1,
            },
            FactCause::Root,
        )
        .expect("task fact builds")
}

fn seed_claim_won(
    write: &mut JournalWrite,
    task: &wire::TaskStamp,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    until: wire::Instant,
    added: journal::PendingFact,
) -> journal::PendingFact {
    write
        .fact(
            wire::body::ClaimWon {
                task: task.clone(),
                actor: Actor::TEST_PRINCIPAL,
                lane,
                occupant,
                epoch: uuid(42_000),
                cell: wire::ClaimCell {
                    topic: Some("/app/native_trust".parse().expect("fixture topic")),
                    phase: "todo".parse().expect("fixture phase"),
                },
                until,
                git: None,
                recovery: None,
                source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Pending(added),
        )
        .expect("claim fact builds")
}

struct AuthoritySeed<'a> {
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    task: &'a wire::TaskStamp,
    digest: &'a wire::Digest,
    root: &'a Path,
    seated: journal::PendingFact,
    added: journal::PendingFact,
    won: journal::PendingFact,
}

fn seed_authority_heads(write: &mut JournalWrite, seed: AuthoritySeed<'_>) {
    let AuthoritySeed {
        epoch,
        lane,
        occupant,
        task,
        digest,
        root,
        seated,
        added,
        won,
    } = seed;
    let worktree = root.to_str().expect("fixture path is UTF-8").to_owned();
    write.residency_head(seated, move |_| wire::ResidencyHead {
        epoch,
        state: wire::ResidentState::Seated,
        topology_revision: 0,
        lanes: vec![wire::LaneResidency {
            lane,
            team: lane,
            name: None,
            worktree,
            life: "drive".to_owned(),
            vendor: "omp".parse().expect("fixture vendor"),
            model: None,
            effort: None,
            fast_mode: false,
            occupant: None,
            lineage: vec![occupant],
            capabilities: Vec::new(),
            session: None,
            disposition: "vacant".to_owned(),
        }],
        subscriptions: Vec::new(),
        renewals: Vec::new(),
    });
    let statement_task = task.clone();
    let requirement_digest = digest.clone();
    write.task_head(added, move |position| wire::TaskHead {
        preparation: None,
        intent: None,
        born: statement_task,
        topic_origin: position,
        title: "Native trust fixture".into(),
        pitch: "The exact executable trust decision is native.".to_owned(),
        description: None,
        term: vec!["The trust decision is durable".to_owned()],
        requirements: wire::EnvironmentRequirements::one(
            wire::EnvironmentRequirement::trust_grant(requirement_digest),
        ),
        resources: wire::ResourceRequirements::default(),
        topic: "/app/native_trust".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies: Vec::new(),
        provenance: wire::TaskProvenance::OperatorDraft,
        creation: wire::body::TaskCreationContext::unavailable(
            wire::body::TaskCreationUnavailable::SpiceActor,
        ),
        revision: position,
    });
    let workflow_task = task.clone();
    write.workflow_head(workflow_task, added, move |_| wire::WorkflowHead {
        flow: vec![wire::Phase::Draft, "todo".parse().expect("fixture phase")],
        phase: "todo".parse().expect("fixture phase"),
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    let claim_task = task.clone();
    write.claim_head(claim_task, won, move |fact| wire::ClaimHead {
        actor: Actor::TEST_PRINCIPAL,
        fact,
        claimed: fact,
        cell: wire::ClaimCell {
            topic: Some("/app/native_trust".parse().expect("fixture topic")),
            phase: "todo".parse().expect("fixture phase"),
        },
        lane,
        occupant,
        epoch: uuid(42_000),
        recovery: None,
        disposition: wire::ClaimDisposition::Active,
        acknowledged: 0,
    });
}

fn seed_task_authority(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    task: &wire::TaskStamp,
    digest: &wire::Digest,
    root: &Path,
) -> wire::Seq {
    let origin = format!("resident:{epoch}")
        .parse()
        .expect("fixture origin is valid");
    let at = wire::Instant::new(jiff::Timestamp::now()).expect("fixture instant");
    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::CommandDoor);
    let seated = write
        .fact(wire::body::UpSeated { epoch }, FactCause::Root)
        .expect("resident seating fact builds");
    let added = seed_task_added(&mut write, task, digest);
    let won = seed_claim_won(&mut write, task, lane, occupant, at, added);
    seed_authority_heads(
        &mut write,
        AuthoritySeed {
            epoch,
            lane,
            occupant,
            task,
            digest,
            root,
            seated,
            added,
            won,
        },
    );
    match bus.commit(write).expect("fixture authority reaches writer") {
        CommitOutcome::Committed(_) => {}
        outcome => panic!("fixture authority did not commit: {outcome:?}"),
    }
    bus.task_snapshot()
        .expect("fixture task snapshot reads")
        .tasks()
        .iter()
        .find(|heads| heads.statement().body().born == *task)
        .expect("fixture task exists")
        .statement()
        .position()
}

fn admitted_command(
    kind: CommandKind,
    epoch: CanonicalUuid,
    actor: Actor,
    expected: Vec<Witness>,
    reference: TrustRef,
    id: CanonicalUuid,
) -> wire::AdmittedCommand {
    raw_command(kind, epoch, expected, reference, id)
        .admit(
            &accepted(actor, epoch),
            &wire::principal::PrincipalRoles::default(),
        )
        .expect("fixture command admits")
}

fn raw_command(
    kind: CommandKind,
    epoch: CanonicalUuid,
    expected: Vec<Witness>,
    reference: TrustRef,
    id: CanonicalUuid,
) -> wire::Command<serde_json::Value> {
    wire::Command {
        v: wire::WireVersion::CURRENT,
        id,
        epoch,
        kind,
        expected,
        body: serde_json::to_value(reference).expect("trust reference encodes"),
    }
}

fn accepted(actor: Actor, epoch: CanonicalUuid) -> wire::Accepted {
    wire::Accepted {
        wire: wire::WireVersion::CURRENT,
        actor,
        epoch,
        release: "1.2.3".parse().expect("fixture release"),
    }
}

fn replace_policy(path: &Path, bytes: &str) {
    let replacement = path.with_extension("replacement");
    fs::write(&replacement, bytes).expect("write policy replacement");
    fs::rename(&replacement, path).expect("publish policy replacement");
}

fn uuid(value: u64) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-{value:012x}")
        .parse()
        .expect("fixture UUID")
}
