use std::cell::Cell;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};

use super::*;
use crate::resident::write;
use steer::Standing as _;
use wire::{Disposition, ResidentState};

#[path = "tests/recognition.rs"]
mod recognition;
mod steering;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn taste_context() -> gate::PolicyContext {
    gate::PolicyContext::new().with_vendor("omp")
}

struct Fixture {
    root: std::path::PathBuf,
    paths: RuntimePaths,
    bus: Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    anchor: Seq,
}

impl Fixture {
    fn new(name: &str) -> Self {
        Self::with_vendor(name, "omp")
    }

    fn with_vendor(name: &str, vendor: &str) -> Self {
        let vendor = vendor.parse::<wire::Name>().expect("vendor name");
        let root = std::env::temp_dir().join(format!(
            "spice-taste-runtime-{name}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".git")).expect("fixture Git directory");
        let root = fs::canonicalize(root).expect("fixture root canonicalizes");
        let paths = RuntimePaths::discover(&root).expect("fixture runtime paths resolve");
        fs::create_dir_all(&paths.data).expect("fixture data directory");
        let bus =
            Bus::start(journal::Journal::open(&paths.journal).expect("persistent journal opens"))
                .expect("bus starts");
        let epoch = uuid(1);
        let lane = uuid(2);
        let occupant = uuid(3);

        let mut startup =
            write(Actor::Spice, epoch, Producer::CommandDoor).expect("startup proposal begins");
        let seated = startup
            .fact(wire::body::UpSeated { epoch }, FactCause::Root)
            .expect("resident seats");
        startup.residency_head(seated, move |_| ResidencyHead {
            epoch,
            state: ResidentState::Seated,
            topology_revision: 0,
            lanes: Vec::new(),
            subscriptions: Vec::new(),
            renewals: Vec::new(),
        });
        commit(&bus, startup).expect("startup commits");

        let observed = current_residency(&bus, epoch).expect("startup residency");
        let capability = uuid(4);
        let worktree = root.display().to_string();
        let mut lane_start = write(Actor::Spice, epoch, Producer::ResidentRecognition)
            .expect("lane proposal begins");
        let lane_seated = lane_start
            .fact(
                wire::body::LaneSeated {
                    lane,
                    team: lane,
                    capability,
                    name: None,
                    worktree: worktree.clone(),
                    life: "drive".to_owned(),
                    vendor: vendor.clone(),
                    model: None,
                    effort: None,
                },
                FactCause::Existing(observed.position()),
            )
            .expect("lane seats");
        let started = lane_start
            .fact(
                wire::body::LaneStarted {
                    lane,
                    occupant,
                    epoch,
                    group: 123,
                    fresh_session: false,
                },
                FactCause::Pending(lane_seated),
            )
            .expect("occupant starts");
        let head_worktree = worktree;
        lane_start.residency_head(started, move |_| ResidencyHead {
            epoch,
            state: ResidentState::Seated,
            topology_revision: 1,
            lanes: vec![wire::LaneResidency {
                lane,
                team: lane,
                name: None,
                worktree: head_worktree,
                life: "drive".to_owned(),
                vendor,
                model: None,
                effort: None,
                fast_mode: false,
                occupant: Some(occupant),
                lineage: vec![occupant],
                capabilities: vec![capability],
                session: None,
                disposition: "occupied".to_owned(),
            }],
            subscriptions: vec![wire::TeamSubscription {
                team: lane,
                formed_at: None,
                drive: true,
                drain: false,
                name: None,
                fast_mode: false,
            }],
            renewals: Vec::new(),
        });
        let committed = commit(&bus, lane_start).expect("lane commits");
        let anchor = Seq::new(committed.last()).expect("nonzero anchor");

        Self {
            root,
            paths,
            bus,
            epoch,
            lane,
            occupant,
            anchor,
        }
    }

    fn recognize(&self, bag: &taste::Bag, record: &lane::capture::CaptureRecord) {
        super::recognize(
            &self.bus,
            &self.paths,
            self.epoch,
            bag,
            &steer::Config::default(),
            None,
            record,
        )
        .expect("capture recognizes");
    }

    fn adjudicate(
        &self,
        bag: &taste::Bag,
        judge: &mut dyn Adjudicator,
        record: &lane::capture::CaptureRecord,
    ) {
        super::recognize(
            &self.bus,
            &self.paths,
            self.epoch,
            bag,
            &steer::Config::default(),
            Some(judge),
            record,
        )
        .expect("capture adjudicates");
    }

    fn reminders(&self) -> Vec<wire::body::TasteReminded> {
        reminders(
            &self
                .bus
                .transact(|journal| {
                    bus::JournalTransaction::settled(
                        journal.fact_snapshot().expect("journal snapshot"),
                    )
                })
                .expect("snapshot reads"),
        )
    }

    fn judgments(&self) -> Vec<wire::body::TasteJudged> {
        judgments(
            &self
                .bus
                .transact(|journal| {
                    bus::JournalTransaction::settled(
                        journal.fact_snapshot().expect("journal snapshot"),
                    )
                })
                .expect("snapshot reads"),
        )
    }

    fn compact(&mut self) -> Seq {
        let locus = listen::Locus::new(9, 400, 32);
        let mut proposal = write(Actor::Spice, self.epoch, Producer::ResidentRecognition)
            .expect("compaction proposal");
        proposal
            .fact(
                wire::body::LaneCompacted {
                    lane: self.lane,
                    occupant: self.occupant,
                    stream: "stdout".to_owned(),
                    member: locus.member,
                    offset: locus.offset,
                    length: locus.length,
                },
                FactCause::Existing(self.anchor),
            )
            .expect("compaction fact");
        let committed = commit(&self.bus, proposal).expect("compaction commits");
        self.anchor = Seq::new(committed.last()).expect("nonzero anchor");
        self.anchor
    }

    fn renew(&mut self, successor: CanonicalUuid) -> Seq {
        let observed = current_residency(&self.bus, self.epoch).expect("current residency");
        let mut next = observed.body().clone();
        let row = next.lanes.first_mut().expect("fixture lane");
        row.occupant = Some(successor);
        row.lineage.push(successor);
        let ancestor = self.occupant;
        let mut proposal = write(Actor::Spice, self.epoch, Producer::ResidentRecognition)
            .expect("renewal proposal");
        let renewed = proposal
            .fact(
                wire::body::LaneRenewed {
                    lane: self.lane,
                    occupant: successor,
                    ancestor,
                    epoch: self.epoch,
                    group: 124,
                },
                FactCause::Existing(observed.position()),
            )
            .expect("renewal fact");
        proposal.residency_head(renewed, move |_| next);
        let committed = commit(&self.bus, proposal).expect("renewal commits");
        self.occupant = successor;
        self.anchor = Seq::new(committed.last()).expect("nonzero anchor");
        self.anchor
    }

    fn restart(self) -> Self {
        let Self {
            root,
            paths,
            bus,
            epoch,
            lane,
            occupant,
            anchor,
        } = self;
        bus.shutdown().expect("predecessor bus stops");
        let bus =
            Bus::start(journal::Journal::open(&paths.journal).expect("persistent journal reopens"))
                .expect("successor bus starts");
        Self {
            root,
            paths,
            bus,
            epoch,
            lane,
            occupant,
            anchor,
        }
    }

    /// Replace resident authority while preserving this lane's occupant and claim.
    fn carry_into(mut self, epoch: CanonicalUuid) -> Self {
        let observed = current_residency(&self.bus, self.epoch).expect("predecessor residency");
        let mut next = observed.body().clone();
        next.epoch = epoch;
        let mut startup =
            write(Actor::Spice, epoch, Producer::CommandDoor).expect("successor proposal begins");
        let seated = startup
            .fact(wire::body::UpSeated { epoch }, FactCause::Root)
            .expect("successor resident seats");
        startup.residency_head(seated, move |_| next);
        let seated = Seq::new(
            commit(&self.bus, startup)
                .expect("successor startup commits")
                .last(),
        )
        .expect("nonzero successor seat");
        let mut carry = write(Actor::Spice, epoch, Producer::ResidentRecognition)
            .expect("claim carry proposal begins");
        carry
            .fact(
                wire::body::ClaimCarried {
                    task: "1kKRRsDPx".parse().expect("fixture task stamp"),
                    lane: self.lane,
                    occupant: self.occupant,
                    ancestor: self.occupant,
                    epoch,
                    source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
                },
                FactCause::Existing(seated),
            )
            .expect("claim carries into successor epoch");
        commit(&self.bus, carry).expect("claim carry commits");
        self.epoch = epoch;
        self
    }

    fn finish(self) {
        self.bus.shutdown().expect("bus stops");
        fs::remove_dir_all(self.root).expect("fixture cleanup");
    }
}

#[test]
fn two_tastes_in_one_prose_record_commit_two_ordinary_directives() {
    let fixture = Fixture::new("two-tastes");
    let bag = bag(&[
        ("polling", &["polling"], "Wait for the terminal event."),
        ("fallbacks", &["fallback"], "Keep one current path."),
    ]);
    let record = prose_record(
        fixture.lane,
        fixture.occupant,
        "Polling with a fallback is not completion.",
    );

    fixture.recognize(&bag, &record);

    let reminders = fixture.reminders();
    assert_eq!(reminders.len(), 2);
    assert_eq!(
        reminders
            .iter()
            .map(|body| body.taste.as_str())
            .collect::<Vec<_>>(),
        ["polling", "fallbacks"]
    );
    assert!(
        reminders
            .iter()
            .all(|body| body.compaction == fixture.anchor.get())
    );
    assert!(reminders.iter().all(|body| body.locus.event == 0));
    let minted = reminders
        .iter()
        .map(|body| {
            let key = body
                .key
                .parse::<steer::Key>()
                .expect("current reminder key");
            assert_eq!(key.as_str().len(), steer::Key::WIDTH);
            key.minted_micros()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        minted[1],
        minted[0] + 1,
        "one batch advances one microsecond"
    );
    let snapshot = fixture
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("snapshot");
    let ledger = steer::Ledger::from_snapshot(&snapshot).expect("reminders replay");
    assert_eq!(
        ledger
            .pending(fixture.lane)
            .iter()
            .map(|directive| directive.text())
            .collect::<Vec<_>>(),
        ["Wait for the terminal event.", "Keep one current path."]
    );
    fixture.finish();
}

#[test]
fn vendor_scoped_tastes_follow_the_durable_lane_vendor() {
    let bag = taste::configured(
        r#"
[taste.polling]
words = ["polling"]
message = "Wait for the terminal event."
scopes = { vendors = ["omp"] }
"#,
    )
    .expect("vendor-scoped bag");
    let scoped = Fixture::with_vendor("scoped-omp", "omp");
    let other = Fixture::with_vendor("scoped-other", "other");
    let scoped_record = prose_record(scoped.lane, scoped.occupant, "Polling is not completion.");
    let other_record = prose_record(other.lane, other.occupant, "Polling is not completion.");

    assert!(
        prepare_judgment(&scoped.bus, scoped.epoch, &bag, &scoped_record)
            .expect("scoped judgment preparation resolves")
            .is_some()
    );
    assert!(
        prepare_judgment(&other.bus, other.epoch, &bag, &other_record)
            .expect("other judgment preparation resolves")
            .is_none()
    );

    scoped.recognize(&bag, &scoped_record);
    other.recognize(&bag, &other_record);

    assert_eq!(scoped.reminders().len(), 1);
    assert!(other.reminders().is_empty());
    scoped.finish();
    other.finish();
}

#[test]
fn distinct_contents_in_separate_records_both_commit_in_one_suppression_epoch() {
    let fixture = Fixture::new("distinct-records");
    let bag = bag(&[
        ("polling", &["polling"], "Wait."),
        ("fallbacks", &["fallback"], "Keep one path."),
    ]);

    fixture.recognize(
        &bag,
        &prose_record(fixture.lane, fixture.occupant, "polling now"),
    );
    fixture.recognize(
        &bag,
        &prose_record(fixture.lane, fixture.occupant, "fallback now"),
    );

    let reminders = fixture.reminders();
    assert_eq!(reminders.len(), 2);
    assert_ne!(reminders[0].content, reminders[1].content);
    assert!(
        reminders
            .iter()
            .all(|body| body.compaction == fixture.anchor.get())
    );
    fixture.finish();
}

#[test]
fn a_carried_claim_retains_the_occupants_taste_epoch_across_resident_replacement() {
    let predecessor = Fixture::new("carried-anchor");
    let predecessor_anchor = predecessor.anchor;
    let fixture = predecessor.carry_into(uuid(40));
    let snapshot = fixture
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("snapshot reads");

    let carried = snapshot.facts().last().expect("claim carry is present");
    assert_eq!(carried.fact.kind(), wire::FactKind::ClaimCarried);
    assert_ne!(carried.seq, predecessor_anchor);
    let facts = fixture.bus.task_snapshot().expect("task snapshot reads");
    assert_eq!(
        context_anchor(facts.facts(), fixture.lane, fixture.occupant)
            .expect("continued occupant retains its context anchor"),
        predecessor_anchor,
    );

    let bag = bag(&[("polling", &["polling"], "Wait for completion.")]);
    let mut judge = FunctionAdjudicator(|_: &Candidate| {
        Ok(judgment(
            false,
            vec![attempt(0, "NO", wire::body::TasteJudgeReply::No)],
        ))
    });
    fixture.adjudicate(
        &bag,
        &mut judge,
        &prose_record(fixture.lane, fixture.occupant, "polling now"),
    );
    let judgments = fixture.judgments();
    assert_eq!(judgments.len(), 1);
    assert_eq!(judgments[0].compaction, predecessor_anchor.get());
    fixture.finish();
}

#[test]
fn duplicate_hits_records_and_restart_replay_create_one_reminder_in_the_epoch() {
    let fixture = Fixture::new("deduplicate");
    let bag = bag(&[(
        "polling",
        &["polling", "POLLING"],
        "Wait for the terminal event.",
    )]);
    let first = prose_record(fixture.lane, fixture.occupant, "  POLLING now ");
    let duplicate_text = "I will continue polling until the process finishes.";
    let duplicate = prose_record(fixture.lane, fixture.occupant, duplicate_text);

    fixture.recognize(&bag, &first);
    fixture.recognize(&bag, &first);
    let fixture = fixture.restart();
    fixture.recognize(&bag.clone(), &duplicate);

    assert_eq!(fixture.reminders().len(), 1);
    assert_ne!(
        fixture.reminders()[0].content,
        taste::reminder_identity(bag.tastes()[0].name(), duplicate_text),
        "materially different evidence still selects the suppressed warning",
    );
    let calls = Cell::new(0_u8);
    let mut judge = FunctionAdjudicator(|_: &Candidate| {
        calls.set(calls.get() + 1);
        Ok(judgment(
            true,
            vec![attempt(0, "YES", wire::body::TasteJudgeReply::Yes)],
        ))
    });
    fixture.adjudicate(&bag, &mut judge, &duplicate);
    assert_eq!(
        calls.get(),
        0,
        "the direct evidence suppresses later judging"
    );
    assert!(fixture.judgments().is_empty());
    assert_eq!(fixture.reminders().len(), 1);
    fixture.finish();
}

#[test]
fn one_record_with_distinct_prose_emits_one_warning_per_taste() {
    let fixture = Fixture::new("record-warning-deduplicate");
    let bag = bag(&[("polling", &["polling"], "Wait.")]);
    fixture.recognize(
        &bag,
        &prose_events_record(
            fixture.lane,
            fixture.occupant,
            &["polling once", "polling again with different prose"],
        ),
    );
    assert_eq!(fixture.reminders().len(), 1);
    fixture.finish();
}

#[test]
fn enabled_confirmation_records_complete_evidence_and_opens_one_restart_safe_directive() {
    let fixture = Fixture::new("judge-confirmed");
    let bag = bag(&[("polling", &["polling"], "Wait for the terminal event.")]);
    let record = prose_record(fixture.lane, fixture.occupant, "Polling is not completion.");
    let calls = Cell::new(0_u8);
    let mut judge = FunctionAdjudicator(|candidate: &Candidate| {
        calls.set(calls.get() + 1);
        assert_eq!(candidate.taste, "polling");
        assert_eq!(candidate.trigger, "polling");
        assert_eq!(candidate.vendor, "Wait for the terminal event.");
        assert_eq!(candidate.source, "Polling is not completion.");
        Ok(judgment(
            true,
            vec![
                attempt(2, "perhaps", wire::body::TasteJudgeReply::Ambiguous),
                attempt(0, "YES", wire::body::TasteJudgeReply::Yes),
            ],
        ))
    });

    fixture.adjudicate(&bag, &mut judge, &record);

    assert_eq!(calls.get(), 1);
    assert!(fixture.reminders().is_empty(), "enabled mode uses one fact");
    let judgments = fixture.judgments();
    assert_eq!(judgments.len(), 1);
    let evidence = &judgments[0];
    assert_eq!(evidence.bag, taste::bag_identity(&bag));
    assert_eq!(evidence.trigger, "polling");
    assert_eq!(evidence.vendor, "Wait for the terminal event.");
    assert_eq!(evidence.source, "Polling is not completion.");
    assert_eq!(evidence.compaction, fixture.anchor.get());
    assert_eq!(evidence.attempts.len(), 2);
    let wire::body::TasteDecision::Confirmed { key, text, .. } = &evidence.decision else {
        panic!("positive judgment was not confirmed");
    };
    assert_eq!(text, "Wait for the terminal event.");
    let snapshot = fixture
        .bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("snapshot");
    let ledger = steer::Ledger::from_snapshot(&snapshot).expect("judgment replays");
    assert_eq!(ledger.pending(fixture.lane).len(), 1);
    assert_eq!(ledger.pending(fixture.lane)[0].key().to_string(), *key);
    assert_eq!(wire::FactKind::TasteJudged.producer(), Producer::Judge);

    let fixture = fixture.restart();
    fixture.adjudicate(&bag, &mut judge, &record);
    fixture.recognize(&bag, &record);
    assert_eq!(calls.get(), 1, "restart replay did not re-judge evidence");
    assert_eq!(fixture.judgments().len(), 1);
    assert!(
        fixture.reminders().is_empty(),
        "a disabled restart respects the durable judgment"
    );
    assert_eq!(
        steer::Ledger::from_snapshot(
            &fixture
                .bus
                .transact(|journal| bus::JournalTransaction::settled(
                    journal.fact_snapshot().expect("journal snapshot")
                ))
                .expect("snapshot")
        )
        .expect("restart judgment replays")
        .pending(fixture.lane)
        .len(),
        1,
    );
    fixture.finish();
}

#[test]
fn local_judge_and_durable_evidence_use_only_the_matched_sentence() {
    let fixture = Fixture::new("judge-bounded-source");
    let bag = bag(&[("polling", &["polling"], "Wait for completion.")]);
    let authored =
        "Unrelated setup stays out of judgment. Polling is not completion!\nAnother sentence.";
    let record = prose_record(fixture.lane, fixture.occupant, authored);
    let expected_content =
        taste::reminder_identity(&"polling".parse().expect("taste name"), authored);
    let mut judge = FunctionAdjudicator(|candidate: &Candidate| {
        assert_eq!(candidate.source, "Polling is not completion!");
        assert_eq!(candidate.content, expected_content);
        let prompts = super::judge_prompts(candidate);
        assert_eq!(prompts.len(), 3);
        for prompt in prompts {
            assert!(prompt.text().contains(&candidate.source));
            assert!(!prompt.text().contains("Unrelated setup"));
            assert!(!prompt.text().contains("Another sentence"));
        }
        Ok(judgment(
            false,
            vec![attempt(0, "NO", wire::body::TasteJudgeReply::No)],
        ))
    });

    fixture.adjudicate(&bag, &mut judge, &record);

    let judgments = fixture.judgments();
    assert_eq!(judgments.len(), 1);
    assert_eq!(judgments[0].source, "Polling is not completion!");
    assert_eq!(judgments[0].content, expected_content);
    fixture.finish();
}

#[test]
fn cross_sentence_match_keeps_judge_and_durable_sources_local_and_equal() {
    let fixture = Fixture::new("judge-cross-sentence-source");
    let bag = bag(&[("fallbacks", &["fall back"], "Keep one deterministic path.")]);
    let authored = "Unrelated head. Fall.\nBack now! Unrelated tail.";
    let record = prose_record(fixture.lane, fixture.occupant, authored);
    let expected_content =
        taste::reminder_identity(&"fallbacks".parse().expect("taste name"), authored);
    let mut judge = FunctionAdjudicator(|candidate: &Candidate| {
        assert_eq!(candidate.source, "Fall.\nBack now!");
        assert_eq!(candidate.content, expected_content);
        for prompt in super::judge_prompts(candidate) {
            assert!(prompt.text().contains("Fall.\\nBack now!"));
            assert!(!prompt.text().contains("Unrelated head"));
            assert!(!prompt.text().contains("Unrelated tail"));
        }
        Ok(judgment(
            false,
            vec![attempt(0, "NO", wire::body::TasteJudgeReply::No)],
        ))
    });

    fixture.adjudicate(&bag, &mut judge, &record);

    let judgments = fixture.judgments();
    assert_eq!(judgments.len(), 1);
    assert_eq!(judgments[0].source, "Fall.\nBack now!");
    assert_eq!(judgments[0].content, expected_content);
    fixture.finish();
}

#[test]
fn cleared_evidence_does_not_suppress_a_later_confirmed_warning() {
    let fixture = Fixture::new("judge-cleared");
    let bag = bag(&[("fallbacks", &["fallback"], "Keep one path.")]);
    let record = prose_events_record(
        fixture.lane,
        fixture.occupant,
        &[
            "This paragraph merely mentions fallback terminology.",
            "I will keep a fallback implementation beside the current one.",
        ],
    );
    let calls = Cell::new(0_u8);
    let mut judge = FunctionAdjudicator(|candidate: &Candidate| {
        calls.set(calls.get() + 1);
        let confirmed =
            candidate.source == "I will keep a fallback implementation beside the current one.";
        Ok(judgment(
            confirmed,
            vec![attempt(
                1,
                if confirmed { "YES" } else { "NO" },
                if confirmed {
                    wire::body::TasteJudgeReply::Yes
                } else {
                    wire::body::TasteJudgeReply::No
                },
            )],
        ))
    });

    fixture.adjudicate(&bag, &mut judge, &record);
    fixture.adjudicate(&bag, &mut judge, &record);

    assert_eq!(
        calls.get(),
        2,
        "only identical cleared evidence is suppressed"
    );
    let judgments = fixture.judgments();
    assert_eq!(judgments.len(), 2);
    assert!(matches!(
        judgments[0].decision,
        wire::body::TasteDecision::Cleared
    ));
    assert!(matches!(
        judgments[1].decision,
        wire::body::TasteDecision::Confirmed { .. }
    ));
    assert!(fixture.reminders().is_empty());
    let ledger = steer::Ledger::from_snapshot(
        &fixture
            .bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("snapshot"),
    )
    .expect("judgments replay");
    assert_eq!(ledger.pending(fixture.lane).len(), 1);
    let fixture = fixture.restart();
    fixture.recognize(
        &bag,
        &prose_record(
            fixture.lane,
            fixture.occupant,
            "A third fallback would still violate the same warning.",
        ),
    );
    assert!(
        fixture.reminders().is_empty(),
        "the confirmed warning, not the earlier clearance, suppresses new evidence"
    );
    assert_eq!(
        steer::Ledger::from_snapshot(
            &fixture
                .bus
                .transact(|journal| bus::JournalTransaction::settled(
                    journal.fact_snapshot().expect("journal snapshot")
                ))
                .expect("snapshot")
        )
        .expect("judgments still replay")
        .pending(fixture.lane)
        .len(),
        1,
    );
    fixture.finish();
}

#[test]
fn unavailable_judge_abandons_candidate_without_fact_or_reminder() {
    let fixture = Fixture::new("judge-unavailable");
    let bag = bag(&[("polling", &["polling"], "Wait.")]);
    let record = prose_record(fixture.lane, fixture.occupant, "polling now");
    let mut judge = FunctionAdjudicator(|_: &Candidate| {
        Err(crate::judge::failure_error(
            "unavailable",
            "fixture judge is absent",
        ))
    });

    fixture.adjudicate(&bag, &mut judge, &record);

    assert!(fixture.judgments().is_empty());
    assert!(fixture.reminders().is_empty());
    assert!(
        steer::Ledger::from_snapshot(
            &fixture
                .bus
                .transact(|journal| bus::JournalTransaction::settled(
                    journal.fact_snapshot().expect("journal snapshot")
                ))
                .expect("snapshot")
        )
        .expect("empty judgment replay")
        .pending(fixture.lane)
        .is_empty()
    );
    fixture.finish();
}

#[test]
fn current_thread_progresses_while_bounded_adjudication_is_held_open() {
    let fixture = Fixture::new("judge-current-thread-progress");
    let bag = bag(&[("polling", &["polling"], "Wait.")]);
    let record = prose_record(fixture.lane, fixture.occupant, "polling now");
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let worker_release = Arc::clone(&release);
    let mut adjudication = start_adjudication_with(move |_| {
        FunctionAdjudicator(move |_: &Candidate| {
            entered_sender.send(()).expect("announce held judge");
            let (lock, changed) = &*worker_release;
            let released = lock.lock().expect("release lock");
            let _released = changed
                .wait_while(released, |released| !*released)
                .expect("release wait");
            Ok(judgment(
                true,
                vec![attempt(0, "YES", wire::body::TasteJudgeReply::Yes)],
            ))
        })
    })
    .expect("bounded adjudication starts");
    let resident = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");

    resident.block_on(async {
        let (submitter, completions) = adjudication.parts();
        submitter
            .recognize(&fixture.bus, fixture.epoch, &bag, &record)
            .expect("candidate submits without waiting for judge");
        entered_receiver
            .recv_timeout(test_support::DEADLOCK_GUARD)
            .expect("worker holds the judge open");

        let (progressed, observed_progress) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let _ = progressed.send(());
        });
        tokio::time::timeout(test_support::DEADLOCK_GUARD, observed_progress)
            .await
            .expect("resident executor remains live")
            .expect("progress task reports");
        assert!(
            fixture.judgments().is_empty(),
            "held external authority committed prematurely"
        );

        let (lock, changed) = &*release;
        *lock.lock().expect("release lock") = true;
        changed.notify_all();
        let completed = tokio::time::timeout(test_support::DEADLOCK_GUARD, completions.recv())
            .await
            .expect("worker completes after release")
            .expect("completion authority stays live");
        submitter.finished(&completed);
        complete_judgment(
            &fixture.bus,
            &fixture.paths,
            fixture.epoch,
            &bag,
            &steer::Config::default(),
            completed,
        )
        .expect("resident revalidates and commits completion");
    });

    drop(adjudication);
    assert_eq!(fixture.judgments().len(), 1);
    fixture.finish();
}

#[test]
fn delayed_completion_is_abandoned_after_compaction_or_bag_movement() {
    let mut fixture = Fixture::new("judge-delayed-authority");
    let original_bag = bag(&[("polling", &["polling"], "Wait.")]);
    let changed_bag = bag(&[("polling", &["polling"], "Choose another action.")]);
    let record = prose_record(fixture.lane, fixture.occupant, "polling now");
    let prepared = prepare_judgment(&fixture.bus, fixture.epoch, &original_bag, &record)
        .expect("candidate prepares")
        .expect("candidate exists");
    let completed = CompletedJudgment {
        decisions: vec![JudgedCandidate {
            candidate: prepared.candidates[0].clone(),
            judgment: judgment(
                true,
                vec![attempt(0, "YES", wire::body::TasteJudgeReply::Yes)],
            ),
        }],
        prepared,
    };

    complete_judgment(
        &fixture.bus,
        &fixture.paths,
        fixture.epoch,
        &changed_bag,
        &steer::Config::default(),
        completed.clone(),
    )
    .expect("changed bag abandons stale answer");
    fixture.compact();
    complete_judgment(
        &fixture.bus,
        &fixture.paths,
        fixture.epoch,
        &original_bag,
        &steer::Config::default(),
        completed,
    )
    .expect("changed compaction abandons stale answer");

    assert!(fixture.judgments().is_empty());
    fixture.finish();
}

#[test]
fn taste_framings_are_complete_and_deterministically_shuffled() {
    let bag = bag(&[("polling", &["polling"], "Wait.")]);
    let candidate = (0..128)
        .find_map(|index| {
            let record = prose_record(uuid(2), uuid(3), &format!("polling candidate {index}"));
            let candidate = candidates(&bag, &taste_context(), &record)
                .expect("candidate scan")
                .remove(0);
            let order = judge_prompts(&candidate)
                .iter()
                .map(crate::judge::Prompt::framing)
                .collect::<Vec<_>>();
            (order != [0, 1, 2]).then_some(candidate)
        })
        .expect("at least one digest shuffles the framing order");

    let first = judge_prompts(&candidate);
    let second = judge_prompts(&candidate);
    assert_eq!(first, second, "same evidence has one framing order");
    let mut identities = first
        .iter()
        .map(crate::judge::Prompt::framing)
        .collect::<Vec<_>>();
    identities.sort_unstable();
    assert_eq!(identities, [0, 1, 2]);
    assert!(first.iter().all(|prompt| {
        prompt.text().contains(&candidate.source)
            && prompt.text().contains(&candidate.trigger)
            && prompt.text().contains(&candidate.vendor)
    }));
}

#[test]
fn compaction_and_occupant_renewal_each_open_one_later_suppression_epoch() {
    let mut fixture = Fixture::new("epochs");
    let bag = bag(&[("polling", &["polling"], "Wait.")]);
    let started = fixture.anchor;
    let original = prose_record(fixture.lane, fixture.occupant, "polling now");
    fixture.recognize(&bag, &original);
    let content = fixture.reminders()[0].content.clone();

    let compacted = fixture.compact();
    fixture.recognize(
        &bag,
        &prose_record(
            fixture.lane,
            fixture.occupant,
            "polling again after compaction",
        ),
    );
    let successor = uuid(5);
    let renewed = fixture.renew(successor);
    let successor_record = prose_record(fixture.lane, successor, "POLLING   now");
    fixture.recognize(&bag, &successor_record);

    let reminders = fixture.reminders();
    assert_eq!(reminders.len(), 3);
    assert_eq!(reminders[0].content, content);
    assert_ne!(reminders[1].content, content);
    assert_eq!(
        reminders
            .iter()
            .map(|body| body.compaction)
            .collect::<Vec<_>>(),
        [started.get(), compacted.get(), renewed.get()]
    );
    assert_eq!(reminders[2].occupant, successor);
    fixture.finish();
}

#[test]
fn quoted_echo_and_every_non_prose_surface_are_inert() {
    let fixture = Fixture::new("inert-surfaces");
    let bag = bag(&[("polling", &["polling"], "Wait.")]);
    let echo = prose_record(
        fixture.lane,
        fixture.occupant,
        "> polling is not completion",
    );
    fixture.recognize(&bag, &echo);

    let locus = listen::Locus::new(2, 80, 32);
    let typed = record(
        fixture.lane,
        fixture.occupant,
        listen::Decoded::Events {
            context: Box::new(listen::RecordContext {
                timestamp: Some("2030-04-12T18:44:09.271Z".to_owned()),
                ..Default::default()
            }),
            events: vec![
                listen::Event::Reasoning(listen::Reasoning {
                    content: listen::ReasoningContent::Plain(vec!["polling".to_owned()]),
                    locus,
                }),
                listen::Event::Tool(listen::Tool {
                    call_id: "patch".to_owned(),
                    moment: listen::ToolMoment::Call {
                        name: "file_change".to_owned(),
                        arguments: r#"{"patch":"polling"}"#.to_owned(),
                        intent: None,
                    },
                    locus,
                }),
                listen::Event::Usage(listen::Usage {
                    turns: 1,
                    input_tokens: 1,
                    cached_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    output_tokens: 1,
                    reasoning_output_tokens: None,
                    locus,
                }),
                listen::Event::Compaction(listen::Compaction {
                    summary: None,
                    method: None,
                    locus,
                }),
                listen::Event::Failure(listen::Failure {
                    message: "polling failed".to_owned(),
                    provider: None,
                    locus,
                }),
            ],
        },
    );
    assert!(
        candidates(&bag, &taste_context(), &typed)
            .expect("typed scan")
            .is_empty()
    );
    let residue = record(
        fixture.lane,
        fixture.occupant,
        listen::Decoded::Residue(listen::Residue {
            dialect: listen::Dialect::Omp,
            locus,
            kind: listen::ResidueKind::Unknown {
                discriminator: "polling".to_owned(),
            },
        }),
    );
    assert!(
        candidates(&bag, &taste_context(), &residue)
            .expect("residue scan")
            .is_empty()
    );
    assert!(fixture.reminders().is_empty());
    fixture.finish();
}

#[test]
fn taste_acknowledgement_does_not_reenable_its_warning_but_new_taste_does() {
    let fixture = Fixture::new("acknowledgement-provenance");
    let bag = bag(&[
        ("polling", &["polling"], "Wait for the terminal event."),
        ("fallbacks", &["fallback"], "Keep one current path."),
        ("handoffs", &["handoff"], "Keep ownership explicit."),
    ]);
    fixture.recognize(
        &bag,
        &prose_record(fixture.lane, fixture.occupant, "polling with a fallback"),
    );
    let first = fixture.reminders();
    let polling = first
        .iter()
        .find(|reminder| reminder.taste == "polling")
        .expect("polling reminder exists");
    let response = format!(
        concat!(
            "> {}\n",
            "A separate handoff is now explicit. ",
            "ACK {}: I will avoid polling and fallback churn."
        ),
        polling.text, polling.key,
    );
    fixture.recognize(
        &bag,
        &prose_record(fixture.lane, fixture.occupant, &response),
    );
    fixture.recognize(
        &bag,
        &prose_record(
            fixture.lane,
            fixture.occupant,
            "I resumed polling with entirely different prose.",
        ),
    );

    let reminders = fixture.reminders();
    assert_eq!(reminders.len(), 3);
    assert_eq!(
        reminders
            .iter()
            .map(|reminder| reminder.taste.as_str())
            .collect::<Vec<_>>(),
        ["polling", "fallbacks", "handoffs"],
    );
    assert!(
        reminders
            .iter()
            .all(|reminder| reminder.compaction == fixture.anchor.get())
    );
    fixture.finish();
}

#[test]
fn generated_patch_and_diff_bodies_never_become_taste_candidates() {
    let bag = bag(&[("polling", &["polling"], "Wait.")]);
    let lane = uuid(90);
    let occupant = uuid(91);
    let patch = prose_record(
        lane,
        occupant,
        "*** Begin Patch\n+polling forever\n*** End Patch",
    );
    let mixed = prose_record(
        lane,
        occupant,
        "Authored delivery.\ndiff --git a/a b/a\n@@ -1 +1 @@\n-pause\n+polling",
    );
    let authored = prose_record(
        lane,
        occupant,
        "Polling is discussed here.\ndiff --git a/a b/a\n+polling",
    );
    let tail = prose_record(
        lane,
        occupant,
        "*** Begin Patch\n+polling forever\n*** End Patch\nPolling after the patch is authored.",
    );

    assert!(
        candidates(&bag, &taste_context(), &patch)
            .expect("patch scans")
            .is_empty()
    );
    assert!(
        candidates(&bag, &taste_context(), &mixed)
            .expect("mixed diff scans")
            .is_empty()
    );
    assert_eq!(
        candidates(&bag, &taste_context(), &authored)
            .expect("authored prefix scans")
            .len(),
        1,
        "only the authored prefix remains eligible"
    );
    assert_eq!(
        candidates(&bag, &taste_context(), &tail)
            .expect("post-patch authored tail scans")
            .len(),
        1,
        "authored text after a complete generated block remains eligible"
    );
}

fn bag(rows: &[(&str, &[&str], &str)]) -> taste::Bag {
    taste::Bag::new(
        rows.iter()
            .map(|(name, words, message)| {
                taste::Taste::new(
                    name.parse().expect("taste name"),
                    words.iter().copied(),
                    *message,
                )
                .expect("taste row")
            })
            .collect(),
    )
    .expect("taste bag")
}

fn prose_record(
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    text: &str,
) -> lane::capture::CaptureRecord {
    prose_events_record(lane, occupant, &[text])
}

fn prose_events_record(
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    texts: &[&str],
) -> lane::capture::CaptureRecord {
    record(
        lane,
        occupant,
        listen::Decoded::Events {
            context: Box::new(listen::RecordContext {
                timestamp: Some("2030-04-12T18:44:09.271Z".to_owned()),
                ..Default::default()
            }),
            events: texts
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    listen::Event::Prose(listen::Prose {
                        role: listen::ProseRole::Assistant,
                        text: (*text).to_owned(),
                        phase: Some("final".to_owned()),
                        media: Vec::new(),
                        external_media: Vec::new(),
                        locus: listen::Locus::new(
                            7,
                            120 + u64::try_from(index).expect("fixture event index fits u64") * 64,
                            text.len() as u64,
                        ),
                    })
                })
                .collect(),
        },
    )
}

fn record(
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    decoded: listen::Decoded,
) -> lane::capture::CaptureRecord {
    lane::capture::CaptureRecord {
        lane,
        occupant,
        stream: vendor::RecordStream::Stdout,
        boundary: sill::RecordBoundary::Complete,
        locus: listen::Locus::new(7, 120, 64),
        declaration: None,
        decoded,
    }
}

fn reminders(snapshot: &journal::FactSnapshot) -> Vec<wire::body::TasteReminded> {
    snapshot
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read())
        .collect()
}

fn judgments(snapshot: &journal::FactSnapshot) -> Vec<wire::body::TasteJudged> {
    snapshot
        .facts()
        .iter()
        .filter_map(|envelope| envelope.fact.read())
        .collect()
}

fn judgment(confirmed: bool, attempts: Vec<wire::body::TasteJudgeAttempt>) -> Judgment {
    Judgment {
        confirmed,
        attempts,
    }
}

fn attempt(
    framing: u8,
    reply: &str,
    verdict: wire::body::TasteJudgeReply,
) -> wire::body::TasteJudgeAttempt {
    wire::body::TasteJudgeAttempt {
        framing,
        reply: reply.to_owned(),
        verdict,
    }
}

fn append_send(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    directive: CanonicalUuid,
    key: &str,
    cause: Seq,
) {
    let at = current_instant().expect("command instant");
    let origin: wire::Origin = format!("resident:{epoch}").parse().expect("command origin");
    let mut proposal = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let body = wire::body::SteerSent {
        sender: Actor::TEST_PRINCIPAL,
        room: None,
        directive,
        fanout: None,
        recipients: Vec::new(),
        key: key.to_owned(),
        recipient: Actor::Lane(lane),
        text: "concurrent command".to_owned(),
        control: None,
        attachments: Vec::new(),
        expires: "2030-01-01T00:00:00.000Z".parse().expect("expiry"),
    };
    let pending = proposal
        .fact(body.clone(), FactCause::Existing(cause))
        .expect("command fact");
    proposal.steering_head(pending, move |seq| {
        steer::Ledger::sent_head(&body, origin, at, seq).expect("command head")
    });
    commit(bus, proposal).expect("concurrent command commits");
}

fn append_compaction(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    cause: Seq,
) -> Seq {
    let mut proposal =
        write(Actor::Spice, epoch, Producer::ResidentRecognition).expect("compaction proposal");
    proposal
        .fact(
            wire::body::LaneCompacted {
                lane,
                occupant,
                stream: "stdout".to_owned(),
                member: 12,
                offset: 500,
                length: 32,
            },
            FactCause::Existing(cause),
        )
        .expect("compaction fact");
    Seq::new(commit(bus, proposal).expect("compaction commits").last()).expect("nonzero compaction")
}

fn append_reminder(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    anchor: Seq,
    candidate: &Candidate,
) {
    let at = current_instant().expect("reminder instant");
    let origin: wire::Origin = format!("resident:{epoch}")
        .parse()
        .expect("reminder origin");
    let mut proposal = JournalWrite::new(
        Actor::Spice,
        origin.clone(),
        at,
        Producer::ResidentRecognition,
    );
    let body = wire::body::TasteReminded {
        directive: uuid(70),
        key: "00000000x".to_owned(),
        lane,
        taste: candidate.taste.clone(),
        content: candidate.content.clone(),
        occupant,
        compaction: anchor.get(),
        locus: candidate.locus,
        text: candidate.text.clone(),
        expires: "2030-01-01T00:00:00.000Z".parse().expect("expiry"),
    };
    let pending = proposal
        .fact(body.clone(), FactCause::Existing(anchor))
        .expect("reminder fact");
    proposal.steering_head(pending, move |seq| {
        steer::Ledger::reminded_head(&body, origin, at, seq).expect("reminder head")
    });
    commit(bus, proposal).expect("concurrent reminder commits");
}

fn append_judgment(
    bus: &Bus,
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    anchor: Seq,
    opening: &Opening,
) {
    let at = current_instant().expect("judgment instant");
    let origin: wire::Origin = format!("resident:{epoch}")
        .parse()
        .expect("judgment origin");
    let mut proposal = JournalWrite::new(Actor::Spice, origin.clone(), at, Producer::Judge);
    let body = wire::body::TasteJudged {
        lane,
        occupant,
        compaction: anchor.get(),
        taste: opening.candidate.taste.clone(),
        content: opening.candidate.content.clone(),
        bag: opening.candidate.bag.clone(),
        trigger: opening.candidate.trigger.clone(),
        vendor: opening.candidate.vendor.clone(),
        source: opening.candidate.source.clone(),
        locus: opening.candidate.locus,
        attempts: vec![attempt(0, "YES", wire::body::TasteJudgeReply::Yes)],
        decision: wire::body::TasteDecision::Confirmed {
            directive: opening.directive,
            key: opening.key.to_string(),
            text: opening.candidate.text.clone(),
            expires: "2030-01-01T00:00:00.000Z".parse().expect("expiry"),
        },
    };
    let pending = proposal
        .fact(body.clone(), FactCause::Existing(anchor))
        .expect("judgment fact");
    proposal.steering_head(pending, move |seq| {
        steer::Ledger::judged_head(&body, origin, at, seq).expect("judgment head")
    });
    commit(bus, proposal).expect("concurrent judgment commits");
}

fn uuid(last: u8) -> CanonicalUuid {
    format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02}")
        .parse()
        .expect("fixture UUID")
}
