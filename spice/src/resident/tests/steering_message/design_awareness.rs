//! Session-profile Design requests are durable, singular, replaced only by a
//! strictly better match, and released by substantive room publication, source
//! replacement, or a Design statement change.

use wire::rooms::{DesignNoticed, DesignReach};

use super::*;

const PITCH: &str = "Lanes page retained publications through bounded cursors.";
const MATCHING: &str = "Retained publications page through bounded cursors.";
const TASK_TITLE: &str = "Page room catch-up";
const TASK_DESCRIPTION: &str = "Bound the cursor of retained publications per lane.";

#[path = "design_awareness/commit_scope.rs"]
mod commit_scope;
#[path = "design_awareness/decision_floor.rs"]
mod decision_floor;
#[path = "design_awareness/items.rs"]
mod items;
#[path = "design_awareness/replacement.rs"]
mod replacement;

/// Resident seats for each `(lane, occupant)`.
fn seats(occupants: &[(u8, CanonicalUuid)]) -> Seats {
    let mut seats = Seats::new();
    for &(lane, occupant) in occupants {
        seats
            .seat_detached(
                test_uuid(lane),
                occupant,
                Box::new(DetachedFixture {
                    dropped: Arc::new(AtomicU64::new(0)),
                }),
            )
            .expect("claimant seats");
    }
    seats
}

fn claimant_seats() -> Seats {
    seats(&[(FIRST, test_uuid(FIRST))])
}

fn stamp(handle: &wire::TaskHandle) -> wire::TaskStamp {
    handle.born().expect("fixture handle has a birth")
}

fn surface(lanes: &Lanes) -> SurfaceConfig {
    let repository = Repository::discover(&lanes.root).expect("fixture repository resolves");
    load_startup_surface(&repository)
        .expect("fixture surface loads")
        .config
}

fn align_completion_lane(lanes: &Lanes) {
    lanes.vacate(FIRST);
    lanes.renew(FIRST, test_uuid(FIRST));
    lanes.declare(FIRST);
}

impl Lanes {
    fn noticed(&self) -> Vec<DesignNoticed> {
        self.bus
            .transact(|journal| {
                bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
            })
            .expect("facts read")
            .facts()
            .iter()
            .filter_map(|envelope| envelope.fact.read::<DesignNoticed>())
            .collect()
    }

    fn design(&self, stamp: &str, title: &str, description: &str) -> wire::TaskHandle {
        self.design_with_questions(stamp, title, description, &[])
    }

    fn design_with_questions(
        &self,
        stamp: &str,
        title: &str,
        description: &str,
        questions: &[&str],
    ) -> wire::TaskHandle {
        self.seed_task_with_term(
            stamp,
            title,
            Some(description),
            Some(wire::TaskIntent::Design),
            None,
            questions,
        )
    }

    fn claimed(&self, stamp: &str, lane: u8) -> wire::TaskHandle {
        self.seed_task(
            stamp,
            TASK_TITLE,
            Some(TASK_DESCRIPTION),
            None,
            Some(test_uuid(lane)),
        )
    }

    fn claim_head(&self, task: &wire::TaskHandle) -> Option<wire::ClaimHead> {
        let task = stamp(task);
        self.bus
            .read(move |read| read.task_heads_for(&task))
            .expect("claim reads")
            .and_then(|heads| heads.latest_claim().map(|held| held.claim().body().clone()))
    }

    /// Commit one statement edit giving `task` a new title.
    fn retitle(&self, handle: &wire::TaskHandle, title: &str) {
        let title = title.to_owned();
        self.edit_statement(handle, move |head| head.title = title);
    }

    /// Commit one statement edit giving `task` these question items.
    fn set_questions(&self, handle: &wire::TaskHandle, questions: &[&str]) {
        let term = questions.iter().map(|item| (*item).to_owned()).collect();
        self.edit_statement(handle, move |head| head.term = term);
    }

    fn edit_statement(&self, handle: &wire::TaskHandle, change: impl FnOnce(&mut wire::TaskHead)) {
        let task = stamp(handle);
        let mut statement = self
            .bus
            .read({
                let task = task.clone();
                move |read| read.task_head(&task)
            })
            .expect("statement reads")
            .expect("statement exists")
            .into_body();
        change(&mut statement);
        let mut edit = write(Actor::TEST_PRINCIPAL, self.epoch, Producer::CommandDoor)
            .expect("edit write starts");
        let edited = edit
            .fact(
                wire::body::TaskEdited {
                    intent: None,
                    task,
                    revision: 2,
                    dependencies: None,
                    flow: None,
                },
                FactCause::Root,
            )
            .expect("edit fact is valid");
        edit.task_head(edited, move |position| wire::TaskHead {
            revision: position,
            ..statement
        });
        commit(&self.bus, edit).expect("edit commits");
    }

    /// Complete the task under its active claim with one durable `TaskDone`.
    fn complete(&self, handle: &wire::TaskHandle, validation: &str) {
        let claim = self.claim_head(handle).expect("the task is claimed");
        let task = stamp(handle);
        let workflow = self
            .bus
            .read({
                let task = task.clone();
                move |read| read.workflow_head(&task)
            })
            .expect("workflow reads")
            .expect("workflow exists")
            .into_body();
        let at = Instant::new(jiff::Timestamp::now()).expect("fixture instant");
        let operation = wire::intent::TaskDone::operation_for_claim(claim.claimed);
        let validation = validation.to_owned();
        let mut completion = write(claim.actor.clone(), self.epoch, Producer::CommandDoor)
            .expect("completion write starts");
        let done = completion
            .fact(
                wire::body::TaskDone {
                    task: task.clone(),
                    validation,
                    disposition: None,
                    terminal: Some(wire::body::TaskCompletionTerminal {
                        operation,
                        status: wire::body::TaskCompletionStatus::Completed,
                        next: None,
                    }),
                    git: None,
                    source: wire::body::SourceBoundary::Proven { heads: Vec::new() },
                },
                FactCause::Root,
            )
            .expect("completion fact is valid");
        completion.workflow_head(task.clone(), done, move |_| wire::WorkflowHead {
            status: wire::WorkflowStatus::completed(at),
            ..workflow
        });
        completion.claim_head(task, done, move |fact| wire::ClaimHead {
            fact,
            disposition: wire::ClaimDisposition::Freed,
            ..claim
        });
        commit(&self.bus, completion).expect("completion commits");
    }
    fn compact(&self, lane: u8) -> Seq {
        let mut write = write(Actor::Spice, self.epoch, Producer::ResidentRecognition)
            .expect("compaction write starts");
        write
            .fact(
                wire::body::LaneCompacted {
                    lane: test_uuid(lane),
                    occupant: current_residency(&self.bus, self.epoch)
                        .expect("residency reads")
                        .body()
                        .lanes
                        .iter()
                        .find(|row| row.lane == test_uuid(lane))
                        .and_then(|row| row.occupant)
                        .expect("the lane has an occupant"),
                    stream: "stdout".to_owned(),
                    member: 0,
                    offset: 0,
                    length: 1,
                },
                FactCause::Root,
            )
            .expect("compaction fact is valid");
        let landed = commit(&self.bus, write).expect("compaction commits");
        Seq::new(landed.first()).expect("compaction has a position")
    }

    fn command(
        &self,
        id: u8,
        actor: Actor,
        kind: CommandKind,
        expected: Vec<Witness>,
        body: &impl serde::Serialize,
    ) -> wire::AdmittedCommand {
        Command {
            v: wire::WireVersion::CURRENT,
            id: test_uuid(id),
            epoch: self.epoch,
            kind,
            expected,
            body: serde_json::to_value(body).expect("command encodes"),
        }
        .admit(
            &wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor,
                epoch: self.epoch,
                release: "1.2.3".parse().expect("fixture release"),
            },
            &wire::principal::PrincipalRoles::default(),
        )
        .expect("command admits")
    }

    fn contribute(&self, id: u8, lane: u8, design: &wire::TaskHandle) {
        self.contribute_kind(
            id,
            lane,
            design,
            wire::rooms::RoomContributionKind::Question,
            None,
        );
    }

    /// `design`'s current question items in statement order.
    fn questions(&self, design: &wire::TaskStamp) -> Vec<wire::rooms::StatementItem> {
        let task = design.clone();
        let head = self
            .bus
            .read(move |read| read.task_head(&task))
            .expect("Design statement read")
            .expect("the Design has a statement");
        wire::rooms::StatementItem::each(&head.body().term).collect()
    }

    /// The one-based statement position of each of `items` in `design`.
    fn positions<'a>(
        &self,
        design: &wire::TaskStamp,
        items: impl IntoIterator<Item = &'a wire::rooms::StatementItem>,
    ) -> Vec<u16> {
        let current = self.questions(design);
        items
            .into_iter()
            .map(|item| {
                let offset = current
                    .iter()
                    .position(|current| current == item)
                    .expect("the ask names a current question");
                u16::try_from(offset + 1).expect("fixture position")
            })
            .collect()
    }

    fn contribute_kind(
        &self,
        id: u8,
        lane: u8,
        design: &wire::TaskHandle,
        kind: wire::rooms::RoomContributionKind,
        question: Option<wire::rooms::StatementItem>,
    ) {
        let request = wire::intent::RoomContribute {
            room: design.clone(),
            kind,
            question,
            text: "Does the catch-up page already bound its cursor?".to_owned(),
            sources: Vec::new(),
        };
        let task = stamp(design);
        let (statement, workflow) = self
            .bus
            .read(move |read| Ok((read.task_head(&task)?, read.workflow_head(&task)?)))
            .expect("Design heads read");
        let workflow = workflow.expect("the Design has a workflow");
        let command = self.command(
            id,
            Actor::Lane(test_uuid(lane)),
            CommandKind::RoomContribute,
            vec![
                Witness::Task {
                    task: stamp(design),
                    head: statement.expect("the Design exists").position(),
                },
                Witness::Workflow {
                    task: stamp(design),
                    head: workflow.position(),
                    phase: workflow.body().phase,
                },
            ],
            &request,
        );
        let repository = Repository::discover(&self.root).expect("fixture policy repository");
        let outcome = crate::board_command::room_contribute(
            &self.bus,
            &command,
            &request,
            None,
            Some(&repository),
        );
        assert!(
            matches!(outcome, Outcome::Reply(_)),
            "contribution commits: {outcome:?}"
        );
    }

    fn leave(&self, id: u8, lane: u8, design: &wire::TaskHandle) {
        let request = wire::intent::RoomLeave {
            room: design.clone(),
            clause: "not my problem space".to_owned(),
        };
        let command = self.command(
            id,
            Actor::Lane(test_uuid(lane)),
            CommandKind::RoomLeave,
            Vec::new(),
            &request,
        );
        let repository = Repository::discover(&self.root).expect("fixture policy repository");
        let outcome =
            crate::board_command::room_leave(&self.bus, &command, &request, Some(&repository));
        assert!(
            matches!(outcome, Outcome::Reply(_)),
            "leave commits: {outcome:?}"
        );
    }

    /// Post one operator message to `design` and return the listeners its
    /// reply reports.
    fn post_listeners(
        &self,
        id: u8,
        config: &SurfaceConfig,
        design: &wire::TaskHandle,
    ) -> Vec<CanonicalUuid> {
        let request = wire::rooms::RoomPost {
            room: design.clone(),
            text: "Operator input for the room.".to_owned(),
        };
        let task = stamp(design);
        let statement = self
            .bus
            .read(move |read| read.task_head(&task))
            .expect("Design head reads")
            .expect("the Design exists");
        let command = self.command(
            id,
            Actor::TEST_PRINCIPAL,
            CommandKind::RoomPost,
            vec![Witness::Task {
                task: stamp(design),
                head: statement.position(),
            }],
            &request,
        );
        let outcome = crate::board_command::dispatch_with_repository(
            &self.bus,
            &command,
            None,
            &config.allocation,
            None,
        )
        .expect("room post dispatches");
        let Outcome::Reply(wire::Reply {
            body: wire::intent::CommandAnswer::RoomPost(answer),
            ..
        }) = outcome
        else {
            panic!("room post commits: {outcome:?}");
        };
        match answer.outcome {
            wire::rooms::RoomPostOutcome::RecordedWithListeners { listeners } => {
                listeners.iter().map(|listener| listener.lane).collect()
            }
            _ => Vec::new(),
        }
    }

    /// The operator's indefinite deferral of `design`.
    fn defer(&self, id: u8, config: &SurfaceConfig, design: &wire::TaskHandle) {
        let task = stamp(design);
        let workflow = self
            .bus
            .read(move |read| read.workflow_head(&task))
            .expect("workflow reads")
            .expect("the Design exists");
        let command = self.command(
            id,
            Actor::TEST_PRINCIPAL,
            CommandKind::TaskDefer,
            vec![
                Witness::Workflow {
                    task: stamp(design),
                    head: workflow.position(),
                    phase: workflow.body().phase,
                },
                Witness::Deferral {
                    task: stamp(design),
                    head: workflow.position(),
                    deferred: false,
                },
            ],
            &wire::intent::TaskDefer {
                task: design.clone(),
                wait: wire::Instant::indefinite(),
            },
        );
        let wire::intent::Intent::TaskDefer(request) = command.body() else {
            panic!("fixture command carries a deferral");
        };
        let (outcome, _) =
            crate::board_command::task_defer(&self.bus, &command, request, None, &config.steering);
        assert!(
            matches!(outcome, Outcome::Reply(_)),
            "deferral commits: {outcome:?}"
        );
    }

    fn reconcile_completions(
        &self,
        seats: &Seats,
        completions: &mut crate::resident::design_awareness::CompletionCursor,
    ) -> Vec<crate::resident::design_awareness::Recorded> {
        let profiles =
            std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
        self.reconcile_profiles(seats, completions, &profiles)
    }

    /// Reconcile Design awareness as a pass that knows no commit, reading
    /// every Design, and return the requests this pass recorded.
    fn reconcile_profiles(
        &self,
        seats: &Seats,
        completions: &mut crate::resident::design_awareness::CompletionCursor,
        profiles: &std::sync::Mutex<crate::resident::message_surface::MessageState>,
    ) -> Vec<crate::resident::design_awareness::Recorded> {
        let mut diagnostics = Vec::new();
        let recorded = crate::resident::design_awareness::reconcile(
            &self.bus,
            seats,
            completions,
            profiles,
            None,
            &mut diagnostics,
        )
        .expect("Design awareness reconciles");
        assert!(
            diagnostics.is_empty(),
            "{}",
            String::from_utf8_lossy(&diagnostics)
        );
        recorded
    }

    fn next(&self, config: &SurfaceConfig, lane: u8) -> wire::intent::NextWorkState {
        let observed = self.bus.task_snapshot().expect("task snapshot reads");
        crate::board_command::read_next_work(
            &self.bus,
            &observed,
            Actor::Lane(test_uuid(lane)),
            &config.allocation,
            None,
        )
        .expect("next-work projection succeeds")
        .next
        .expect("next-work projection returns a state")
    }

    fn request(&self, config: &SurfaceConfig, lane: u8) -> Option<wire::intent::DesignRequest> {
        match self.next(config, lane) {
            wire::intent::NextWorkState::DesignRequest { request } => Some(request),
            _ => None,
        }
    }
}

#[test]
fn an_unsteeped_design_invites_wide() {
    let lanes = Lanes::new("design-request-wide");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let task = lanes.claimed("dVrD245Kc", FIRST);
    let design = lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();

    lanes.complete(&task, MATCHING);
    lanes.reconcile_completions(&seated, &mut completions);
    let request = lanes
        .request(&config, FIRST)
        .expect("a filed open Design can be requested");
    assert_eq!(request.design, design);
    assert_eq!(request.admission.reach(), DesignReach::Wide);
    seated.release_all();
    lanes.finish();
}

#[test]
fn a_completion_profile_opportunity_selects_the_best_single_design() {
    let lanes = Lanes::new("design-request-completion");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let task = lanes.claimed("dVrD245Kc", FIRST);
    let best = lanes.design_with_questions(
        "dVrD245Kd",
        "Page room catch-up",
        "Problem. Lanes page retained publications through bounded cursors.",
        &[
            "Should each lane retain its own cursor?",
            "Which publications belong on the bounded page?",
        ],
    );
    let _lesser = lanes.design(
        "dVrD245Kg",
        "Page cursor proposal",
        "Retained cursor notes for one lane.",
    );
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();

    assert!(
        lanes.noticed().is_empty(),
        "a live claim records no request"
    );
    assert!(
        matches!(
            lanes.next(&config, FIRST),
            wire::intent::NextWorkState::Active { .. }
        ),
        "a live claim remains the next-work answer"
    );

    lanes.complete(&task, MATCHING);
    lanes.reconcile_completions(&seated, &mut completions);
    let request = lanes
        .request(&config, FIRST)
        .expect("completion returns its owed Design");
    assert_eq!(request.design, best);
    assert!(matches!(
        request.evidence,
        wire::rooms::DesignNoticeEvidence::Task {
            task: ref evidence_task,
            ..
        } if evidence_task == &task.born().expect("fixture task has a birth")
    ));
    assert!(!request.terms.is_empty());
    assert_eq!(request.pitch, "This is enough.");
    assert_eq!(
        request.questions,
        [
            "Should each lane retain its own cursor?",
            "Which publications belong on the bounded page?"
        ]
    );
    assert!(lanes.request(&config, FIRST).is_some());
    assert_eq!(
        lanes.noticed().len(),
        1,
        "one completion records at most one request"
    );
    seated.release_all();
    lanes.finish();
}

#[test]
fn profile_selection_uses_the_shared_tfidf_statement_window() {
    let lanes = Lanes::new("design-request-common-profile");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    for index in 0..8 {
        let stamp = wire::TaskStamp::for_micros(20_000 + index)
            .expect("fixture stamp")
            .to_string();
        let task = lanes.seed_task(
            &stamp,
            "Lane room cursor retained publication bounded",
            None,
            None,
            Some(test_uuid(FIRST)),
        );
        lanes.complete(&task, "fixture completion");
    }
    let task = lanes.seed_task(
        "dVrD245Kc",
        "Lane room cursor retained publication bounded",
        Some("Catchup pagination snapshot."),
        None,
        Some(test_uuid(FIRST)),
    );
    let _generic = lanes.design(
        "dVrD245Kd",
        "Lane notes",
        "A lane retains ordinary room notes.",
    );
    let distinctive = lanes.design(
        "dVrD245Km",
        "Catchup pagination snapshot",
        "Catchup pagination snapshot.",
    );
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());

    lanes.complete(&task, "Catchup pagination snapshot.");
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let planning =
        crate::resident::room_catch_up::Planning::load(&lanes.bus).expect("planning reads");
    let mut state = crate::resident::message_surface::lock_state(&profiles).expect("profiles lock");
    let matcher = board::similarity::MatchCache::shared();
    let expected_fit = matcher.fit(planning.entries.iter().map(board::Entry::statement), || {
        state.profiles.conversation_background(&planning.residency)
    });
    let selected = planning
        .entries
        .iter()
        .find(|entry| entry.born() == &stamp(&distinctive))
        .expect("selected Design is in the planning board");
    let selected = matcher
        .prepare(&expected_fit, selected.statement())
        .expect("selected Design has weighted terms");
    let expected_score = state
        .profiles
        .profile_mut(test_uuid(FIRST))
        .expect("claimant profile exists")
        .evidence_mut()
        .best_score(&expected_fit, &selected)
        .expect("profile and Design share a fit")
        .expect("retained evidence has a finite score");
    drop(state);
    let request = lanes
        .request(&config, FIRST)
        .expect("resident choice is returned by task next");

    assert_eq!(request.design, distinctive);
    assert_eq!(request.terms, ["catchup", "pagination", "snapshot"]);
    let notices = lanes.noticed();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].design, stamp(&distinctive));
    assert_eq!(notices[0].terms, request.terms);
    let notice = &notices[0];
    let matching = &notice.admission;
    assert_eq!(
        matching.score(),
        expected_score,
        "the fact records the selected best-view cosine"
    );
    assert_eq!(
        matching.floor(),
        crate::resident::design_awareness::match_score_floor(matching.reach()),
    );
    let expected_generation =
        wire::rooms::DesignFitGeneration::from_bytes(expected_fit.generation().into_bytes());
    assert_eq!(matching.generation(), &expected_generation);
    seated.release_all();
    lanes.finish();
}

/// A recorded request is told once to the occupant seated when it was
/// recorded, in a notice one datagram carries. Room catch-up carries nothing
/// for it, and `task next` keeps showing it, naming the supporting work by
/// its claim phase, exact source, and authored excerpt, until the lane
/// answers.
#[test]
fn a_recorded_request_is_told_once_to_its_seated_occupant_while_task_next_keeps_it() {
    let lanes = Lanes::new("design-request-seated");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let task = lanes.claimed("dVrD245Kc", FIRST);
    let design = lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    lanes.complete(&task, MATCHING);
    let recorded = lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let [told] = recorded.as_slice() else {
        panic!("one request is recorded: {}", recorded.len());
    };
    assert_eq!(
        (told.lane, told.occupant),
        (test_uuid(FIRST), test_uuid(FIRST))
    );
    assert_eq!(told.request.design, design);
    let notice = crate::resident::design_input::request_notice(told);
    assert_eq!(notice.occupant, test_uuid(FIRST));
    assert!(notice.confirmed_fits(), "{}", notice.text);
    assert!(
        notice
            .text
            .contains(&format!("spice room leave {design} --clause TEXT")),
        "{}",
        notice.text
    );
    let landed = listen::Event::Prose(listen::Prose {
        role: listen::ProseRole::Operator,
        text: notice.text.clone(),
        phase: None,
        media: Vec::new(),
        external_media: Vec::new(),
        locus: listen::Locus::new(1, 0, 1),
    });
    assert_eq!(
        spice::rooms::profile::design_request_causes([&landed]),
        [stamp(&design)],
        "the turn the notice lands in is caused by its request"
    );

    let shown = lanes
        .request(&config, FIRST)
        .map(|request| spice::rooms::awareness::request_text(&request))
        .expect("task next shows the owed request");
    assert_eq!(
        shown,
        spice::rooms::awareness::request_text(&told.request),
        "the occupant is told about the request task next shows"
    );
    let evidence_task = task.born().expect("fixture task has a birth");
    let noticed = lanes.noticed();
    let wire::rooms::DesignNoticeEvidence::Task {
        task: ref recorded_task,
        statement,
        completed,
        ..
    } = noticed[0].evidence
    else {
        panic!("a completion records task evidence");
    };
    assert_eq!(recorded_task, &evidence_task);
    for expected in [
        format!(
            "Supporting work: you implemented task {evidence_task} (statement revision {}, \
             completion {}): \"{MATCHING}\".",
            wire::SequenceCode::new(statement.get()),
            wire::SequenceCode::new(completed.get()),
        ),
        format!("`spice task show {evidence_task} --history`"),
    ] {
        assert!(shown.contains(&expected), "{expected} is missing: {shown}");
    }
    assert_eq!(
        lanes.unread(FIRST),
        super::rooms::NOTHING_UNREAD,
        "unread room activity carries no Design request"
    );

    let later = lanes.claimed("dVrD245Kp", FIRST);
    lanes.complete(&later, &format!("{MATCHING} Paged again."));
    assert!(
        lanes
            .reconcile_profiles(&seated, &mut completions, &profiles)
            .is_empty(),
        "an owed request is recorded and told once"
    );
    assert_eq!(lanes.noticed().len(), 1);
    assert_eq!(
        lanes
            .request(&config, FIRST)
            .map(|owed| spice::rooms::awareness::request_text(&owed)),
        Some(shown),
        "task next keeps the request owed"
    );
    seated.release_all();
    lanes.finish();
}

#[test]
fn contribution_discharges_the_request_for_the_lane() {
    let lanes = Lanes::new("design-request-contribution");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let task = lanes.claimed("dVrD245Kc", FIRST);
    let design = lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    lanes.complete(&task, MATCHING);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert!(lanes.request(&config, FIRST).is_some());

    lanes.contribute(50, FIRST, &design);
    assert!(lanes.request(&config, FIRST).is_none());
    let later = lanes.claimed("dVrD245Kp", FIRST);
    lanes.complete(&later, &format!("{MATCHING} Paged again."));
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert!(
        lanes.request(&config, FIRST).is_none(),
        "a lane is not asked again about a Design it published in"
    );
    seated.release_all();
    lanes.finish();
}

#[test]
fn compaction_never_reopens_an_answered_request() {
    let lanes = Lanes::new("design-request-compaction-participation");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let first = lanes.claimed("dVrD245Kc", FIRST);
    let design = lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());

    lanes.complete(&first, MATCHING);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert_eq!(lanes.noticed().len(), 1);
    lanes.contribute(50, FIRST, &design);
    assert!(lanes.request(&config, FIRST).is_none());

    lanes.compact(FIRST);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let after = lanes.claimed("dVrD245Kp", FIRST);
    lanes.complete(&after, &format!("{MATCHING} Paged again."));
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert_eq!(
        lanes.noticed().len(),
        1,
        "a compacted session is not asked again what its lane answered"
    );
    assert!(lanes.request(&config, FIRST).is_none());
    seated.release_all();
    lanes.finish();
}

#[test]
fn compaction_preserves_one_unanswered_request() {
    let lanes = Lanes::new("design-request-compaction-outstanding");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let first = lanes.claimed("dVrD245Kc", FIRST);
    lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());

    lanes.complete(&first, MATCHING);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let expected = lanes
        .request(&config, FIRST)
        .map(|request| spice::rooms::awareness::request_text(&request))
        .expect("the first opportunity records one request");

    lanes.compact(FIRST);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let after = lanes.claimed("dVrD245Kp", FIRST);
    lanes.complete(&after, &format!("{MATCHING} Paged after compaction."));
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);

    assert_eq!(lanes.noticed().len(), 1);
    assert_eq!(
        lanes
            .request(&config, FIRST)
            .map(|request| spice::rooms::awareness::request_text(&request)),
        Some(expected),
        "compaction and later opportunities preserve the exact unanswered request"
    );
    seated.release_all();
    lanes.finish();
}

/// An answer settles the request for the lane, not for one occupant: a
/// successor under a fresh vendor session, matching the same Design with its
/// own work, is neither asked nor told again.
#[test]
fn a_successor_occupant_is_never_asked_what_its_lane_answered() {
    let lanes = Lanes::new("design-request-successor-answered");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let first = lanes.claimed("dVrD245Kc", FIRST);
    let design = lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    let mut seated = claimant_seats();

    lanes.complete(&first, MATCHING);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    lanes.leave(60, FIRST, &design);
    assert_eq!(lanes.noticed().len(), 1);

    seated.release_all();
    let successor = test_uuid(FIRST.saturating_add(40));
    lanes.vacate(FIRST);
    lanes.renew(FIRST, successor);
    lanes.declare_identity(FIRST, test_uuid(FIRST.saturating_add(41)).to_string());
    seated = seats(&[(FIRST, successor)]);
    let after = lanes.seed_task(
        "dVrD245Kp",
        "Bound a renewed room cursor",
        Some("Retain a renewed cursor over room publications."),
        None,
        Some(test_uuid(FIRST)),
    );
    lanes.complete(&after, MATCHING);
    assert!(
        lanes
            .reconcile_profiles(&seated, &mut completions, &profiles)
            .is_empty(),
        "the successor is told nothing"
    );
    assert_eq!(
        lanes.noticed().len(),
        1,
        "a fresh vendor session does not reopen what the lane left"
    );
    assert!(lanes.request(&config, FIRST).is_none());
    seated.release_all();
    lanes.finish();
}

/// A request its occupant never answered stays with the vacated lane: the
/// next occupant reads it from `task next` under a fresh vendor session, and
/// neither its arrival nor its own matching work records or tells it again.
#[test]
fn a_vacated_lane_keeps_its_request_for_the_next_occupant_through_task_next() {
    let lanes = Lanes::new("design-request-vacated-lane");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let mut seated = claimant_seats();
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let task = lanes.claimed("dVrD245Kc", FIRST);
    let design = lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    lanes.complete(&task, MATCHING);
    assert_eq!(
        lanes.reconcile_completions(&seated, &mut completions).len(),
        1
    );
    let owed = lanes
        .request(&config, FIRST)
        .map(|request| spice::rooms::awareness::request_text(&request))
        .expect("the first occupant owes the request");

    seated.release_all();
    let successor = test_uuid(FIRST.saturating_add(42));
    lanes.vacate(FIRST);
    lanes.renew(FIRST, successor);
    lanes.declare_identity(FIRST, test_uuid(FIRST.saturating_add(43)).to_string());
    seated = seats(&[(FIRST, successor)]);
    assert_eq!(
        lanes
            .request(&config, FIRST)
            .map(|request| spice::rooms::awareness::request_text(&request)),
        Some(owed),
        "the next occupant reads its lane's unanswered request"
    );

    let mut restarted = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("restarted completion cursor starts");
    let after = lanes.seed_task(
        "dVrD245Kp",
        "Bound a renewed room cursor",
        Some("Retain a renewed cursor over room publications."),
        None,
        Some(test_uuid(FIRST)),
    );
    lanes.complete(&after, MATCHING);
    assert!(
        lanes
            .reconcile_completions(&seated, &mut restarted)
            .is_empty(),
        "the request the lane already owes is not told again"
    );
    assert_eq!(lanes.noticed().len(), 1);

    lanes.contribute(50, FIRST, &design);
    assert!(
        lanes.request(&config, FIRST).is_none(),
        "the next occupant's answer settles its lane's request"
    );
    seated.release_all();
    lanes.finish();
}

/// Work an earlier occupant's session completed still matches its lane: a
/// Design filed after the lane was renewed under a fresh session asks the
/// lane, citing that earlier work, and tells the seated successor.
#[test]
fn a_design_asks_a_lane_whose_matching_work_was_an_earlier_sessions() {
    let lanes = Lanes::new("design-request-earlier-session");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let mut seated = claimant_seats();
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    let task = lanes.claimed("dVrD245Kc", FIRST);
    lanes.complete(&task, MATCHING);
    assert!(
        lanes
            .reconcile_profiles(&seated, &mut completions, &profiles)
            .is_empty(),
        "no Design is open yet"
    );

    seated.release_all();
    let successor = test_uuid(FIRST.saturating_add(42));
    lanes.vacate(FIRST);
    lanes.renew(FIRST, successor);
    lanes.declare_identity(FIRST, test_uuid(FIRST.saturating_add(43)).to_string());
    seated = seats(&[(FIRST, successor)]);
    assert!(
        lanes
            .reconcile_profiles(&seated, &mut completions, &profiles)
            .is_empty()
    );

    let design = lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    let recorded = lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    let [told] = recorded.as_slice() else {
        panic!("the renewed lane is asked once: {}", recorded.len());
    };
    assert_eq!((told.lane, told.occupant), (test_uuid(FIRST), successor));
    assert_eq!(told.request.design, design);
    assert!(matches!(
        told.request.evidence,
        wire::rooms::DesignNoticeEvidence::Task { task: ref cited, .. } if cited == &stamp(&task)
    ));
    assert!(lanes.request(&config, FIRST).is_some());
    seated.release_all();
    lanes.finish();
}

/// A vacant lane whose retained work matches a newly filed Design owes its
/// request durably: nothing tells or starts an occupant, and the next
/// occupant's `task next` shows it.
#[test]
fn a_vacant_lane_owes_a_matching_request_without_being_told() {
    let lanes = Lanes::new("design-request-vacant-lane");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let mut seated = claimant_seats();
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    let task = lanes.claimed("dVrD245Kc", FIRST);
    lanes.complete(&task, MATCHING);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);

    seated.release_all();
    lanes.vacate(FIRST);
    let vacant = Seats::new();
    let design = lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    assert!(
        lanes
            .reconcile_profiles(&vacant, &mut completions, &profiles)
            .is_empty(),
        "a vacant lane is told nothing"
    );
    let noticed = lanes.noticed();
    assert_eq!(noticed.len(), 1, "the vacant lane owes one request");
    assert_eq!(noticed[0].lane, test_uuid(FIRST));

    let successor = test_uuid(FIRST.saturating_add(42));
    lanes.renew(FIRST, successor);
    lanes.declare_identity(FIRST, test_uuid(FIRST.saturating_add(43)).to_string());
    assert_eq!(
        lanes.request(&config, FIRST).map(|request| request.design),
        Some(design),
        "the next occupant's task next shows the owed request"
    );
    lanes.finish();
}

#[test]
fn an_asked_lane_hears_later_posts_until_it_leaves_and_is_never_asked_again() {
    let lanes = Lanes::new("design-request-listening");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let task = lanes.claimed("dVrD245Kc", FIRST);
    let design = lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    assert!(lanes.post_listeners(70, &config, &design).is_empty());

    lanes.complete(&task, MATCHING);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert!(lanes.request(&config, FIRST).is_some());
    assert_eq!(
        lanes.post_listeners(71, &config, &design),
        vec![test_uuid(FIRST)]
    );

    lanes.leave(72, FIRST, &design);
    assert!(lanes.post_listeners(73, &config, &design).is_empty());
    let later = lanes.claimed("dVrD245Kp", FIRST);
    lanes.complete(&later, &format!("{MATCHING} Paged after leaving."));
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert!(lanes.request(&config, FIRST).is_none());
    assert!(lanes.post_listeners(74, &config, &design).is_empty());
    seated.release_all();
    lanes.finish();
}

#[test]
fn deferring_an_open_design_ends_its_request_and_its_listening() {
    let lanes = Lanes::new("design-request-deferred");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let task = lanes.claimed("dVrD245Kc", FIRST);
    let design = lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    lanes.complete(&task, MATCHING);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert!(lanes.request(&config, FIRST).is_some());
    assert_eq!(
        lanes.post_listeners(70, &config, &design),
        vec![test_uuid(FIRST)]
    );

    lanes.defer(71, &config, &design);
    assert!(
        lanes.request(&config, FIRST).is_none(),
        "a deferred Design is owed to no lane"
    );
    assert!(
        lanes.post_listeners(72, &config, &design).is_empty(),
        "no lane keeps listening for a deferred Design"
    );

    let later = lanes.claimed("dVrD245Kp", FIRST);
    lanes.complete(&later, &format!("{MATCHING} Paged while deferred."));
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert!(
        lanes.request(&config, FIRST).is_none(),
        "a deferred Design is never invited"
    );
    seated.release_all();
    lanes.finish();
}

/// A statement revision is a new ask: it releases the request recorded at the
/// revision before it and may ask the lane again, even one that answered
/// there, once.
#[test]
fn a_statement_revision_asks_a_lane_again_once() {
    let lanes = Lanes::new("design-request-edit");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let task = lanes.claimed("dVrD245Kc", FIRST);
    let design = lanes.design("dVrD245Kd", "Page room catch-up", PITCH);
    let mut completions = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("completion cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());
    lanes.complete(&task, MATCHING);
    lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    lanes.contribute(50, FIRST, &design);
    assert!(
        lanes.request(&config, FIRST).is_none(),
        "the lane answered at the first revision"
    );

    lanes.retitle(&design, "Page room catch-up by cursor");
    let recorded = lanes.reconcile_profiles(&seated, &mut completions, &profiles);
    assert_eq!(recorded.len(), 1, "the new revision is told once");
    let notices = lanes.noticed();
    assert_eq!(notices.len(), 2);
    assert_ne!(notices[1].statement, notices[0].statement);
    assert_eq!(
        lanes.request(&config, FIRST).map(|request| request.title),
        Some("Page room catch-up by cursor".to_owned()),
        "the lane owes the revised Design its view"
    );

    let later = lanes.claimed("dVrD245Kp", FIRST);
    lanes.complete(&later, &format!("{MATCHING} Paged after the edit."));
    assert!(
        lanes
            .reconcile_profiles(&seated, &mut completions, &profiles)
            .is_empty(),
        "one revision asks once"
    );
    assert_eq!(lanes.noticed().len(), 2);
    seated.release_all();
    lanes.finish();
}

#[test]
fn conversation_evidence_predating_a_design_is_selected_when_the_design_is_filed() {
    let lanes = Lanes::new("design-request-conversation-before-design");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let mut cursor = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("awareness cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());

    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    lanes.account(FIRST);
    let source = lanes
        .bus
        .read(|read| read.transcript_source_heads(&test_uuid(FIRST)))
        .expect("transcript sources read")
        .into_iter()
        .next()
        .expect("declared lane has a transcript source")
        .into_body()
        .source;
    let prose = "Windows native runner validates archive compression headers libraries toolchain \
                 manifests receipts deployment replay smoke";
    let conversation = spice::rooms::evidence::Conversation::read(prose)
        .expect("fixture prose is substantive evidence");
    {
        let mut state =
            crate::resident::message_surface::lock_state(&profiles).expect("profile state locks");
        assert_eq!(
            state.profiles.insert(
                test_uuid(FIRST),
                test_uuid(FIRST),
                spice::rooms::evidence::Evidence::conversation(
                    spice::rooms::profile::EvidenceKey::Conversation {
                        source,
                        member: 0,
                        offset: 0,
                        length: 1,
                        caused_by: Vec::new(),
                    },
                    &conversation,
                ),
            ),
            spice::rooms::profile::Inserted::Stored
        );
    }
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);

    let design = lanes.design(
        "dVrD245Kd",
        "Validate Windows native runner archives",
        "Compression headers libraries toolchain manifests receipts deployment replay smoke.",
    );
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);

    let request = lanes
        .request(&config, FIRST)
        .expect("Design filing evaluates retained conversation evidence");
    assert_eq!(request.design, design);
    assert!(matches!(
        request.opportunity,
        wire::rooms::DesignNoticeOpportunity::DesignStatement
    ));
    assert!(matches!(
        request.evidence,
        wire::rooms::DesignNoticeEvidence::Conversation { .. }
    ));
    lanes.contribute(80, FIRST, &design);
    let message_design = lanes.design(
        "dVrD245Km",
        "Repair database migration replay",
        "Indexes transactions snapshots rollback consistency schema storage locks recovery checks.",
    );
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    let message = spice::rooms::evidence::Conversation::read(
        "Database migration replay indexes transactions snapshots rollback consistency schema \
         storage locks recovery checks",
    )
    .expect("second fixture prose is substantive evidence");
    {
        let mut state =
            crate::resident::message_surface::lock_state(&profiles).expect("profile state locks");
        assert_eq!(
            state.profiles.insert(
                test_uuid(FIRST),
                test_uuid(FIRST),
                spice::rooms::evidence::Evidence::conversation(
                    spice::rooms::profile::EvidenceKey::Conversation {
                        source,
                        member: 0,
                        offset: 1,
                        length: 1,
                        caused_by: Vec::new(),
                    },
                    &message,
                ),
            ),
            spice::rooms::profile::Inserted::Stored
        );
    }
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    let request = lanes
        .request(&config, FIRST)
        .expect("settled conversation evaluates the session profile");
    assert_eq!(request.design, message_design);
    assert!(matches!(
        request.opportunity,
        wire::rooms::DesignNoticeOpportunity::Conversation { .. }
    ));
    assert!(matches!(
        request.evidence,
        wire::rooms::DesignNoticeEvidence::Conversation { .. }
    ));
    assert_eq!(lanes.noticed().len(), 2);
    seated.release_all();
    lanes.finish();
}

fn retain_conversation(
    profiles: &std::sync::Mutex<crate::resident::message_surface::MessageState>,
    source: CanonicalUuid,
    offset: u64,
    prose: &str,
) {
    let conversation = spice::rooms::evidence::Conversation::read(prose)
        .expect("fixture prose is substantive evidence");
    let mut state =
        crate::resident::message_surface::lock_state(profiles).expect("profile state locks");
    let evidence = spice::rooms::evidence::Evidence::conversation(
        spice::rooms::profile::EvidenceKey::Conversation {
            source,
            member: 0,
            offset,
            length: 1,
            caused_by: Vec::new(),
        },
        &conversation,
    );
    assert_eq!(
        state
            .profiles
            .insert(test_uuid(FIRST), test_uuid(FIRST), evidence),
        spice::rooms::profile::Inserted::Stored
    );
}

#[test]
fn replaced_conversation_source_releases_the_request_and_selection() {
    let lanes = Lanes::new("design-request-conversation-source-replaced");
    align_completion_lane(&lanes);
    let config = surface(&lanes);
    let mut cursor = crate::resident::design_awareness::CompletionCursor::start(&lanes.bus)
        .expect("awareness cursor starts");
    let mut seated = claimant_seats();
    let profiles = std::sync::Mutex::new(crate::resident::message_surface::MessageState::default());

    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    lanes.account(FIRST);
    let source = lanes
        .bus
        .read(|read| read.transcript_source_heads(&test_uuid(FIRST)))
        .expect("transcript sources read")
        .into_iter()
        .next()
        .expect("declared lane has a transcript source")
        .into_body()
        .source;
    let prose = "Windows native runner validates archive compression headers libraries toolchain \
                 manifests receipts deployment replay smoke";
    retain_conversation(&profiles, source, 0, prose);
    let first = lanes.design(
        "dVrD245Kd",
        "Validate Windows native runner archives",
        "Compression headers libraries toolchain manifests receipts deployment replay smoke.",
    );
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    assert_eq!(
        lanes
            .request(&config, FIRST)
            .expect("the current conversation source owes its Design")
            .design,
        first
    );

    let second = lanes.design(
        "dVrD245Km",
        "Validate another Windows native runner",
        "Compression headers libraries toolchain manifests receipts deployment replay smoke.",
    );
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    assert_eq!(
        lanes
            .request(&config, FIRST)
            .expect("an unrelated write leaves the current request owed")
            .design,
        first
    );
    assert_eq!(
        lanes.noticed().len(),
        1,
        "a match no better than the owed Design leaves it owed"
    );
    let shown = spice::rooms::awareness::request_text(
        &lanes
            .request(&config, FIRST)
            .expect("task next shows the conversation request"),
    );

    for expected in [
        format!(
            "Supporting work: you explained this in conversation: \"{prose}\" (transcript \
             {source} member 0 byte 0 length 1). Read it with `spice session context "
        ),
        format!(
            "--lane {} --vendor omp --stream session --source {source} --member 0 --offset 0`",
            test_uuid(FIRST)
        ),
    ] {
        assert!(shown.contains(&expected), "{expected} is missing: {shown}");
    }

    let replacement = lanes.replace_transcript_source(FIRST);
    assert!(
        lanes.request(&config, FIRST).is_none(),
        "task next drops evidence from the replaced source"
    );

    retain_conversation(
        &profiles,
        replacement,
        0,
        "A second Windows native runner validates archive compression headers libraries \
         toolchain manifests receipts deployment replay smoke",
    );
    lanes.reconcile_profiles(&seated, &mut cursor, &profiles);
    assert_eq!(
        lanes
            .request(&config, FIRST)
            .expect("later evidence may select another matching Design")
            .design,
        second
    );
    assert_eq!(lanes.noticed().len(), 2);
    seated.release_all();
    lanes.finish();
}
