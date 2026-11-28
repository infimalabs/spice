use super::*;
use crate::body::{DesignConclusion, TaskCompletionTerminal, Verdict};
use crate::rooms::{
    DecisionBasis, DecisionRef, ExecutionAuthorization, OutputDecision, PreparationDecision,
    RoomAuthorityStamp, RoomContributionKind, RoomPublicationKind, RoomRoute, RoomSource,
    StatementItem, TaskPreparation,
};
use crate::{CanonicalUuid, Fact, Instant, SequenceCode, TaskStamp};

fn task(micros: i64) -> TaskStamp {
    TaskStamp::for_micros(micros).expect("fixture task")
}

fn lane(last: u8) -> CanonicalUuid {
    format!("00000000-0000-4000-8000-{last:012}")
        .parse()
        .expect("fixture lane")
}

fn authority(micros: i64) -> RoomAuthorityStamp {
    RoomAuthorityStamp::for_micros(1_760_000_000_000_000 + micros).expect("authority stamp")
}

fn instant() -> Instant {
    "2026-10-16T00:00:00.000Z".parse().expect("fixture instant")
}

fn any<F: crate::Body>(body: F) -> AnyFact {
    crate::decode(&crate::encode(&Fact::new(body)).expect("fixture encodes"))
        .expect("fixture decodes as a stored fact")
}

const DESIGN: i64 = 1;
const OTHER: i64 = 2;
const OUTPUT: i64 = 3;
const SECOND: i64 = 4;

fn design() -> TaskStamp {
    task(DESIGN)
}

#[test]
fn deferred_cycle_presentation_distinguishes_possession() {
    let design = TaskHandle::for_topic(
        &"/wire/rooms"
            .parse::<crate::Topic>()
            .expect("fixture topic"),
        &design(),
    );
    for cycle in [DesignCycle::GatheringInput, DesignCycle::ReadyForPass] {
        let unheld = DesignCyclePresentation::new(
            cycle,
            DesignCycleFacts::default(),
            &design,
            DesignAvailability::DeferredUnclaimed,
            &super::super::DesignCoverage::default(),
            &[],
            std::num::NonZeroU32::MIN,
        );
        assert_eq!(unheld.actor, DesignCycleActor::Lane);
        assert!(
            unheld
                .action
                .contains(&format!("`spice task wake {design} --claim`"))
        );
        assert!(!unheld.action.contains("task free"));

        let held = DesignCyclePresentation::new(
            cycle,
            DesignCycleFacts::default(),
            &design,
            DesignAvailability::DeferredClaimed,
            &super::super::DesignCoverage::default(),
            &[],
            std::num::NonZeroU32::MIN,
        );
        assert_eq!(held.actor, DesignCycleActor::Lane);
        assert!(held.action.contains(&format!("`spice task free {design}`")));
        assert!(!held.action.contains("--claim"));
    }
}

#[test]
fn undeferred_cycle_presentation_keeps_cycle_authority() {
    let design = TaskHandle::for_topic(
        &"/wire/rooms"
            .parse::<crate::Topic>()
            .expect("fixture topic"),
        &design(),
    );
    let facts = DesignCycleFacts::default();
    for cycle in DesignCycle::ALL {
        let unclaimed = DesignCyclePresentation::new(
            cycle,
            facts,
            &design,
            DesignAvailability::Unclaimed,
            &super::super::DesignCoverage::default(),
            &[],
            std::num::NonZeroU32::MIN,
        );
        assert_eq!(unclaimed.actor, cycle.next_actor());

        let claimed = DesignCyclePresentation::new(
            cycle,
            facts,
            &design,
            DesignAvailability::Claimed,
            &super::super::DesignCoverage::default(),
            &[],
            std::num::NonZeroU32::MIN,
        );
        if matches!(
            cycle,
            DesignCycle::GatheringInput | DesignCycle::ReadyForPass
        ) {
            assert_eq!(claimed.actor, DesignCycleActor::Lane);
            assert!(claimed.action.contains("spice task done"));
        } else {
            assert_eq!(claimed, unclaimed);
        }
    }
}

#[test]
fn shared_design_guidance_asks_each_participant_for_done() {
    let design = TaskHandle::for_topic(
        &"/wire/rooms"
            .parse::<crate::Topic>()
            .expect("fixture topic"),
        &design(),
    );
    let questions = vec!["Which work is warranted?".to_owned()];
    for coverage in [
        super::super::DesignCoverage::default(),
        super::super::DesignCoverage {
            covered: vec![],
            missing: vec![StatementItem::of("Which work is warranted?")],
        },
    ] {
        for cycle in [DesignCycle::GatheringInput, DesignCycle::ReadyForPass] {
            for availability in [DesignAvailability::Unclaimed, DesignAvailability::Claimed] {
                let presentation = DesignCyclePresentation::new(
                    cycle,
                    DesignCycleFacts::default(),
                    &design,
                    availability,
                    &coverage,
                    &questions,
                    std::num::NonZeroU32::new(2).expect("shared completion target"),
                );
                if availability == DesignAvailability::Claimed || cycle == DesignCycle::ReadyForPass
                {
                    assert_eq!(presentation.actor, DesignCycleActor::Lane);
                    assert!(presentation.action.contains("spice task done --validation"));
                } else {
                    assert_eq!(presentation.actor, DesignCycleActor::Contributors);
                }
            }
        }
    }
}

fn contributed(room: i64, by: u8, kind: RoomContributionKind) -> AnyFact {
    any(RoomContributed {
        room: task(room),
        publication: authority(1_000 + i64::from(by)),
        lane: lane(by),
        contribution: kind,
        item: None,
        statement: Seq::new(1).expect("statement head"),
        text: "Substantive contribution.".to_owned(),
        sources: Vec::new(),
    })
}

/// The Design's one current question item.
const QUESTION: &str = "Does the cycle steep?";

fn term() -> Vec<String> {
    vec![QUESTION.to_owned()]
}

/// Lane `by`'s charge `value` on the Design's question item.
fn charged(by: u8, value: i8) -> AnyFact {
    any(RoomContributed {
        room: design(),
        publication: authority(1_000 + i64::from(by)),
        lane: lane(by),
        contribution: RoomContributionKind::charge(value),
        item: Some(StatementItem::of(QUESTION)),
        statement: Seq::new(1).expect("statement head"),
        text: "Substantive charge.".to_owned(),
        sources: Vec::new(),
    })
}

fn posted(routes: Vec<RoomRoute>) -> AnyFact {
    any(RoomPosted {
        publication: authority(2_000),
        text: "Operator input.".to_owned(),
        operation: RoomPublicationKind::Message {},
        routes,
        directed: Vec::new(),
        expires: instant(),
    })
}

fn route(destination: RoomDestination, context: Option<TaskStamp>) -> RoomRoute {
    RoomRoute {
        destination,
        context,
        direction: None,
    }
}

fn published(content: RoomPublicationContent) -> AnyFact {
    any(RoomPublished {
        room: design(),
        publication: authority(4_000),
        content,
    })
}

/// The Design's `phase` round completed.
fn round_completed(phase: &str) -> AnyFact {
    any(TaskRoundCompleted {
        task: design(),
        phase: phase.parse().expect("fixture phase"),
        round: Seq::new(1).expect("round"),
        participants: Vec::new(),
        status: TaskCompletionStatus::Pending,
        next: None,
        disposition: None,
    })
}

fn decided(sequence: u64, outcome: DecisionOutcome) -> AnyFact {
    any(PreparationDecided {
        decision: PreparationDecision {
            decision: DecisionRef {
                controller: design(),
                decision: SequenceCode::new(sequence),
            },
            source: RoomSource::Journal {
                fact: SequenceCode::new(1),
            },
            basis: DecisionBasis {
                statement: Seq::new(1).expect("statement"),
                room: None,
                outputs: Vec::new(),
            },
            outcome,
        },
    })
}

fn resolved(verdicts: &[(i64, OutputVerdict)]) -> DecisionOutcome {
    DecisionOutcome::Resolved {
        outputs: verdicts
            .iter()
            .map(|&(output, verdict)| OutputDecision {
                output: task(output),
                verdict,
                comment: None,
            })
            .collect(),
        comment: None,
    }
}

fn authorized(sequence: u64, outputs: &[i64]) -> AnyFact {
    any(ExecutionAuthorized {
        authorization: ExecutionAuthorization {
            decision: DecisionRef {
                controller: design(),
                decision: SequenceCode::new(sequence),
            },
            selected_outputs: outputs.iter().copied().map(task).collect(),
            output_revisions: Vec::new(),
        },
    })
}

/// Lane 1 wins `subject` in `phase`.
fn won(subject: i64, phase: &str) -> AnyFact {
    any(ClaimWon {
        task: task(subject),
        actor: Actor::Lane(lane(1)),
        lane: lane(1),
        occupant: lane(3),
        epoch: lane(4),
        cell: crate::ClaimCell {
            topic: Some("/spice/rooms".parse().expect("fixture topic")),
            phase: phase.parse().expect("fixture phase"),
        },
        until: instant(),
        git: None,
        recovery: None,
        source: crate::body::SourceBoundary::Proven { heads: Vec::new() },
    })
}

fn freed(subject: i64) -> AnyFact {
    any(ClaimFreed {
        task: task(subject),
        actor: Actor::Lane(lane(1)),
        parked: None,
    })
}

fn completed(subject: i64) -> AnyFact {
    done(subject, TaskCompletionStatus::Completed)
}

fn done(subject: i64, status: TaskCompletionStatus) -> AnyFact {
    any(TaskDone {
        task: task(subject),
        validation: "checked".to_owned(),
        disposition: None,
        terminal: Some(TaskCompletionTerminal {
            operation: lane(9),
            status,
            next: None,
        }),
        git: None,
        source: crate::body::SourceBoundary::Proven { heads: Vec::new() },
    })
}

fn reviewed(subject: i64, verdict: Verdict, status: TaskCompletionStatus) -> AnyFact {
    any(TaskReviewed {
        task: task(subject),
        reviewer: Actor::Lane(lane(2)),
        verdict,
        status,
        finding: (verdict == Verdict::Finding).then(|| "finding".to_owned()),
        note: None,
        followups: Vec::new(),
        round: None,
        git: None,
        source: crate::body::SourceBoundary::Proven { heads: Vec::new() },
    })
}

/// A fold driven in journal order, numbering facts from 1.
struct Journal {
    cycles: DesignCycles,
    next: u64,
}

impl Journal {
    fn new() -> Self {
        Self {
            cycles: DesignCycles::default(),
            next: 1,
        }
    }

    fn by(&mut self, actor: &Actor, fact: &AnyFact) -> &mut Self {
        let seq = Seq::new(self.next).expect("seq");
        self.next += 1;
        assert!(
            DesignCycles::concerns(fact.kind()),
            "the cheap filter admits every kind the fold reads"
        );
        self.cycles.observe(seq, actor, fact);
        self
    }

    fn lane(&mut self, fact: &AnyFact) -> &mut Self {
        self.by(&Actor::Lane(lane(1)), fact)
    }

    fn operator(&mut self, fact: &AnyFact) -> &mut Self {
        self.by(&Actor::Principal(lane(200)), fact)
    }

    fn facts(&self) -> DesignCycleFacts {
        self.cycles.facts(&design(), &term())
    }

    fn cycle(&self, working: bool) -> DesignCycle {
        self.facts().cycle(working)
    }

    /// One completed pass: a design-phase claim, its completed round,
    /// release, and a decision.
    fn passed(&mut self, outcome: DecisionOutcome) -> &mut Self {
        self.lane(&won(DESIGN, "design"))
            .lane(&round_completed("design"))
            .lane(&freed(DESIGN))
            .operator(&decided(1, outcome))
    }
}

/// The first `posts` charges of the cheapest steep: A +1, B -1, A -2.
fn steeping(journal: &mut Journal, posts: usize) {
    for (by, value) in [(1, 1), (2, -1), (1, -2)].into_iter().take(posts) {
        journal.lane(&charged(by, value));
    }
}

#[test]
fn first_pass_needs_steeped_maturity() {
    let mut journal = Journal::new();
    steeping(&mut journal, 2);
    assert_eq!(journal.cycle(false), DesignCycle::GatheringInput);
    journal.lane(&charged(1, -2));
    assert_eq!(journal.cycle(false), DesignCycle::ReadyForPass);
    assert_eq!(
        journal.cycles.facts(&design(), &[]).cycle(false),
        DesignCycle::GatheringInput,
        "steeping is judged against the current statement's items"
    );
}

/// `subject` is born with `intent`, prepared under the Design when `report`.
fn added(subject: i64, intent: Option<crate::TaskIntent>, report: bool) -> AnyFact {
    any(crate::body::TaskAdded {
        task: task(subject),
        title: "Fixture".to_owned(),
        pitch: "Fixture pitch.".to_owned(),
        description: None,
        term: Vec::new(),
        requirements: crate::EnvironmentRequirements::default(),
        topic: "/oops/design".parse().expect("fixture topic"),
        submitted: false,
        priority: None,
        taste: None,
        depends: Vec::new(),
        intent,
        preparation: report.then(|| TaskPreparation {
            parent: design(),
            controller: design(),
            construction: task(subject),
        }),
        flow: vec![crate::Phase::Draft, crate::Phase::Plan],
        phase: crate::Phase::Plan,
        revision: 1,
        creation: crate::body::TaskCreationContext::Unavailable {
            reason: crate::body::TaskCreationUnavailable::SpiceActor,
        },
    })
}

#[test]
fn a_prepared_report_is_never_input_for_an_oops_or_ordinary_design() {
    for intent in [
        crate::TaskIntent::OpportunisticDesign,
        crate::TaskIntent::Design,
    ] {
        let mut journal = Journal::new();
        journal
            .lane(&added(DESIGN, Some(intent), false))
            .lane(&added(OUTPUT, Some(crate::TaskIntent::Plan), true));
        assert_eq!(
            journal.cycle(false),
            DesignCycle::GatheringInput,
            "{intent:?}: a report alone never readies the first pass"
        );
        journal.passed(DecisionOutcome::Concluded {});
        journal.lane(&added(SECOND, Some(crate::TaskIntent::Plan), true));
        assert_eq!(
            journal.cycle(false),
            DesignCycle::GatheringInput,
            "{intent:?}: a report after a pass never readies the next"
        );
    }
}

#[test]
fn only_counted_input_after_the_horizon_refills_a_design() {
    let concluded = || DecisionOutcome::Concluded {};
    let task_route = || vec![route(RoomDestination::Task { task: design() }, None)];
    let context_route = || {
        vec![route(
            RoomDestination::Lane { lane: lane(2) },
            Some(design()),
        )]
    };
    let counted: Vec<(&str, bool, AnyFact)> = vec![
        (
            "contribution",
            false,
            contributed(DESIGN, 2, RoomContributionKind::charge(0)),
        ),
        ("task-routed message", true, posted(task_route())),
        ("context message", true, posted(context_route())),
        (
            "authored publication",
            true,
            published(RoomPublicationContent::Authored {
                text: "Operator note.".to_owned(),
            }),
        ),
    ];
    for (name, operator, fact) in counted {
        let mut journal = Journal::new();
        steeping(&mut journal, 3);
        journal.passed(concluded());
        assert_eq!(journal.cycle(false), DesignCycle::GatheringInput, "{name}");
        if operator {
            journal.operator(&fact);
        } else {
            journal.lane(&fact);
        }
        assert_eq!(journal.cycle(false), DesignCycle::ReadyForPass, "{name}");
    }

    let inert: Vec<(&str, bool, AnyFact)> = vec![
        ("lane-authored message", false, posted(task_route())),
        (
            "linked publication",
            true,
            published(RoomPublicationContent::Linked {
                link: RoomSource::Journal {
                    fact: SequenceCode::new(1),
                },
            }),
        ),
        (
            "another room",
            false,
            contributed(OTHER, 2, RoomContributionKind::charge(0)),
        ),
        ("unrelated release", false, freed(OTHER)),
        ("unrelated completion", false, completed(OTHER)),
    ];
    for (name, operator, fact) in inert {
        let mut journal = Journal::new();
        steeping(&mut journal, 3);
        journal.passed(concluded());
        if operator {
            journal.operator(&fact);
        } else {
            journal.lane(&fact);
        }
        assert_eq!(journal.cycle(false), DesignCycle::GatheringInput, "{name}");
    }
}

#[test]
fn a_completed_design_round_awaits_the_operator_decision() {
    let mut journal = Journal::new();
    steeping(&mut journal, 3);
    journal.lane(&won(DESIGN, "design"));
    journal.lane(&round_completed("design"));
    journal.lane(&freed(DESIGN));
    journal.lane(&contributed(DESIGN, 2, RoomContributionKind::charge(0)));
    assert_eq!(journal.cycle(false), DesignCycle::AwaitingDecision);
    let facts = journal.facts();
    assert_eq!(facts.end, Some(DesignPassEnd::Completed));
    assert_eq!(
        facts.pass,
        Seq::new(5),
        "the owed action cites the completed round"
    );

    journal.operator(&decided(
        1,
        DecisionOutcome::Returned {
            reason: "cover the failure path".to_owned(),
        },
    ));
    assert_eq!(
        journal.cycle(false),
        DesignCycle::ReadyForPass,
        "a return settles the pass and its feedback is input"
    );

    let mut other_phase = Journal::new();
    steeping(&mut other_phase, 3);
    other_phase.lane(&round_completed("plan"));
    assert_eq!(
        other_phase.cycle(false),
        DesignCycle::ReadyForPass,
        "only a design-phase round ends a pass"
    );
}

#[test]
fn decisions_wait_on_released_work_until_it_settles() {
    let mut accepted = Journal::new();
    accepted.passed(resolved(&[
        (OUTPUT, OutputVerdict::Accepted),
        (SECOND, OutputVerdict::Accepted),
    ]));
    accepted.operator(&authorized(1, &[OUTPUT, SECOND]));
    assert_eq!(accepted.cycle(true), DesignCycle::WaitingOnSelectedWork);
    accepted.lane(&completed(OUTPUT));
    assert_eq!(
        accepted.cycle(true),
        DesignCycle::WaitingOnSelectedWork,
        "a partial settlement leaves the remaining selected work running"
    );
    accepted.lane(&completed(SECOND));
    assert_eq!(accepted.cycle(false), DesignCycle::ReadyForPass);

    let mut contributed_while_running = Journal::new();
    contributed_while_running.passed(resolved(&[(OUTPUT, OutputVerdict::Accepted)]));
    contributed_while_running.operator(&authorized(1, &[OUTPUT]));
    contributed_while_running.lane(&contributed(DESIGN, 2, RoomContributionKind::charge(-1)));
    assert_eq!(
        contributed_while_running.cycle(true),
        DesignCycle::WaitingOnSelectedWork,
        "input recorded while released work runs does not reopen a pass"
    );
    assert_eq!(
        contributed_while_running.cycle(false),
        DesignCycle::ReadyForPass,
        "that input counts once the released work has settled"
    );

    let mut mixed = Journal::new();
    mixed.passed(resolved(&[
        (OUTPUT, OutputVerdict::Accepted),
        (SECOND, OutputVerdict::Rejected),
    ]));
    assert_eq!(
        mixed.cycle(true),
        DesignCycle::WaitingOnSelectedWork,
        "a rejection is feedback, but the accepted output still holds the Design"
    );
    for (name, outcome) in [
        (
            "all rejected",
            resolved(&[
                (OUTPUT, OutputVerdict::Rejected),
                (SECOND, OutputVerdict::Rejected),
            ]),
        ),
        (
            "returned",
            DecisionOutcome::Returned {
                reason: "cover the failure path".to_owned(),
            },
        ),
    ] {
        let mut journal = Journal::new();
        journal.passed(outcome);
        assert_eq!(journal.cycle(false), DesignCycle::ReadyForPass, "{name}");
    }
}

/// Work whose flow ends in review completes through its clean review: the
/// phase `task::done` before it is pending. Only a review that positively
/// settled the workflow counts; a finding or a clean phase advance leaves
/// the Design waiting on its released work.
#[test]
fn a_clean_final_review_settles_released_work() {
    let settle = |verdict, status| {
        let mut journal = Journal::new();
        journal.passed(resolved(&[(OUTPUT, OutputVerdict::Accepted)]));
        journal.operator(&authorized(1, &[OUTPUT]));
        journal.lane(&done(OUTPUT, TaskCompletionStatus::Pending));
        assert_eq!(journal.cycle(true), DesignCycle::WaitingOnSelectedWork);
        journal.by(&Actor::Lane(lane(2)), &reviewed(OUTPUT, verdict, status));
        journal
    };
    let settled = settle(Verdict::Clean, TaskCompletionStatus::Completed);
    assert_eq!(settled.cycle(false), DesignCycle::ReadyForPass);
    for (name, verdict, status) in [
        ("finding", Verdict::Finding, TaskCompletionStatus::Pending),
        (
            "clean advance",
            Verdict::Clean,
            TaskCompletionStatus::Pending,
        ),
    ] {
        let journal = settle(verdict, status);
        assert_eq!(
            journal.cycle(true),
            DesignCycle::WaitingOnSelectedWork,
            "{name}"
        );
    }
}

#[test]
fn abandonment_moves_the_horizon_and_settlement_drops_the_fold() {
    let mut journal = Journal::new();
    steeping(&mut journal, 3);
    journal.lane(&won(DESIGN, "design")).lane(&freed(DESIGN));
    let facts = journal.facts();
    assert_eq!(facts.end, Some(DesignPassEnd::Abandoned));
    assert_eq!(
        journal.cycle(false),
        DesignCycle::GatheringInput,
        "maturity reached before an abandoned pass is spent by it"
    );
    journal.lane(&won(DESIGN, "design"));
    journal.by(
        &Actor::Spice,
        &any(ClaimLapsed {
            task: design(),
            occupant: lane(1),
            expiry: instant(),
            parked: None,
        }),
    );
    assert_eq!(journal.facts().end, Some(DesignPassEnd::Abandoned));

    journal.operator(&any(TaskConcluded {
        task: design(),
        conclusion: DesignConclusion::Done {},
    }));
    assert_eq!(journal.facts(), DesignCycleFacts::default());
    assert!(journal.cycles.designs.is_empty());
}

/// An intake claim, released directly or after the phase advance into
/// design, ends no pass: the first pass still waits on maturity or a nudge.
/// A claim carried into design does open one.
#[test]
fn only_a_claim_won_in_the_design_phase_ends_a_pass() {
    for intake in ["plan", "review"] {
        for advanced_first in [false, true] {
            let mut journal = Journal::new();
            journal.lane(&won(DESIGN, intake));
            if advanced_first {
                journal.lane(&done(DESIGN, TaskCompletionStatus::Pending));
            }
            journal.lane(&freed(DESIGN));
            steeping(&mut journal, 1);
            let facts = journal.facts();
            assert_eq!(
                (facts.horizon, facts.end, journal.cycle(false)),
                (None, None, DesignCycle::GatheringInput),
                "{intake} claim released, advanced first: {advanced_first}"
            );
        }
    }

    let mut carried = Journal::new();
    carried
        .lane(&won(DESIGN, "plan"))
        .lane(&done(DESIGN, TaskCompletionStatus::Pending))
        .lane(&won(DESIGN, "design"))
        .lane(&freed(DESIGN));
    assert_eq!(carried.facts().end, Some(DesignPassEnd::Abandoned));
    assert_eq!(carried.facts().horizon, Seq::new(4));
}

#[test]
fn plan_constructed_work_settles_into_its_controlling_design() {
    let mut journal = Journal::new();
    journal.passed(DecisionOutcome::Concluded {});
    journal.operator(&any(OutputControlled {
        output: task(OUTPUT),
        preparation: TaskPreparation {
            parent: task(OTHER),
            controller: design(),
            construction: task(OUTPUT),
        },
    }));
    assert_eq!(journal.cycle(true), DesignCycle::WaitingOnSelectedWork);
    journal.operator(&any(TaskClosed {
        task: task(OUTPUT),
        validation: "done elsewhere".to_owned(),
        phase: "todo".parse().expect("phase"),
        claimed: None,
    }));
    assert_eq!(journal.cycle(false), DesignCycle::ReadyForPass);
}
