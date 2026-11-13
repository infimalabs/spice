use wire::rooms::{
    DecisionBasis, DecisionOutcome, DecisionRef, ExecutionAuthorization, ExecutionAuthorized,
    OutputControlled, OutputDecision, OutputVerdict, PreparationDecided, PreparationDecision,
    ProposalRef, RoomAuthorityStamp, RoomPublicationContent, RoomPublished, RoomSource, TextRange,
    TranscriptHunk,
};
use wire::{Producer, SequenceCode, TaskPreparation};

use super::*;
use crate::guard::Applied;
use crate::{CommitError, FactCause, JournalHead, JournalWrite};

type Commit = Result<Applied, CommitError>;

mod authorization;
mod hunks;
mod maturity;

fn write() -> JournalWrite {
    write_as(Actor::TEST_PRINCIPAL)
}

fn write_as(actor: Actor) -> JournalWrite {
    JournalWrite::new(
        actor,
        "task:rooms".parse().expect("room origin"),
        instant(),
        Producer::CommandDoor,
    )
}

fn commit<F: wire::Body>(journal: &mut Journal, body: F) -> Commit {
    commit_as(journal, Actor::TEST_PRINCIPAL, body)
}

fn commit_as<F: wire::Body>(journal: &mut Journal, actor: Actor, body: F) -> Commit {
    let mut write = write_as(actor);
    write.fact(body, FactCause::Root).expect("room fact builds");
    journal.apply(write)
}

fn authorization_write(actor: Actor, body: ExecutionAuthorized) -> JournalWrite {
    let mut write = write_as(actor);
    write
        .fact(body, FactCause::Root)
        .expect("authorization builds");
    write
}

/// Authorize without rewriting statement lineage or ordinary prerequisites.
fn authorize(journal: &mut Journal, body: ExecutionAuthorized) -> Commit {
    let write = authorization_write(Actor::TEST_PRINCIPAL, body);
    journal.apply(write)
}

fn cursor(journal: &Journal) -> Option<Seq> {
    journal.read(|read| read.cursor()).expect("cursor reads")
}

fn refused(journal: &Journal, result: Commit) {
    let before = cursor(journal);
    assert!(
        matches!(result, Err(CommitError::Refused(_))),
        "unsound authority must refuse: {result:?}"
    );
    assert_eq!(cursor(journal), before);
}

fn design_task(journal: &mut Journal, offset: i64) -> TaskStamp {
    task_with_intent(journal, offset, None, Some(wire::TaskIntent::Design))
}

fn task(journal: &mut Journal, offset: i64, preparation: Option<TaskPreparation>) -> TaskStamp {
    task_with_intent(journal, offset, preparation, None)
}

fn task_with_intent(
    journal: &mut Journal,
    offset: i64,
    preparation: Option<TaskPreparation>,
    intent: Option<wire::TaskIntent>,
) -> TaskStamp {
    task_with_term(journal, offset, preparation, intent, Vec::new())
}

fn task_with_term(
    journal: &mut Journal,
    offset: i64,
    preparation: Option<TaskPreparation>,
    intent: Option<wire::TaskIntent>,
    term: Vec<String>,
) -> TaskStamp {
    let task = TaskStamp::for_micros(20_000 + offset).expect("task stamp");
    let current = if intent == Some(wire::TaskIntent::Design) {
        phase("design")
    } else {
        phase("todo")
    };
    let flow = if intent == Some(wire::TaskIntent::Design) {
        vec![current]
    } else {
        vec![current, phase("review")]
    };
    let dependencies = preparation
        .as_ref()
        .map(|preparation| vec![preparation.parent.clone()])
        .unwrap_or_default();
    let mut write = write();
    let fact = write
        .fact(
            wire::body::TaskAdded {
                preparation: preparation.clone(),
                intent,
                task: task.clone(),
                title: format!("Room task {offset}"),
                pitch: "Rooms need work to talk about.".to_owned(),
                description: None,
                term: term.clone(),
                requirements: wire::EnvironmentRequirements::default(),
                topic: "/rooms".parse().expect("fixture topic"),
                submitted: false,
                priority: None,
                taste: None,
                depends: dependencies.clone(),
                flow: flow.clone(),
                phase: current,
                creation: wire::body::TaskCreationContext::unavailable(
                    wire::body::TaskCreationUnavailable::PrincipalActor,
                ),
                revision: 1,
            },
            FactCause::Root,
        )
        .expect("task fact builds");
    let born = task.clone();
    write.task_head(fact, move |revision| TaskHead {
        preparation,
        intent,
        born,
        topic_origin: revision,
        title: format!("Room task {offset}"),
        pitch: "Rooms need work to talk about.".to_owned(),
        description: None,
        term,
        requirements: wire::EnvironmentRequirements::default(),
        resources: wire::ResourceRequirements::default(),
        topic: "/rooms".parse().expect("fixture topic"),
        priority: None,
        taste: None,
        dependencies,
        provenance: wire::TaskProvenance::OperatorDraft,
        creation: wire::body::TaskCreationContext::unavailable(
            wire::body::TaskCreationUnavailable::PrincipalActor,
        ),
        revision,
    });
    let workflow = task.clone();
    write.workflow_head(workflow, fact, move |_| WorkflowHead {
        flow,
        phase: current,
        status: wire::WorkflowStatus::pending(),
        deferral: None,
        submitted: false,
        generation: 0,
    });
    journal.apply(write).expect("task commits");
    task
}

fn statement(journal: &Journal, task: &TaskStamp) -> TaskHead {
    journal
        .read(|read| read.task_head(task))
        .expect("task head reads")
        .expect("task exists")
        .into_body()
}

/// Adopt `output` with a task head carrying `head_preparation`, which a sound
/// write makes equal to the controlled preparation.
fn adopt(
    journal: &mut Journal,
    output: &TaskStamp,
    preparation: TaskPreparation,
    head_preparation: TaskPreparation,
) -> Commit {
    let head = statement(journal, output);
    let mut write = write();
    let fact = write
        .fact(
            OutputControlled {
                output: output.clone(),
                preparation,
            },
            FactCause::Root,
        )
        .expect("control fact builds");
    write.task_head(fact, move |revision| TaskHead {
        preparation: Some(head_preparation),
        revision,
        ..head
    });
    journal.apply(write)
}

fn edit_topic(journal: &mut Journal, task: &TaskStamp, topic: &str) {
    let head = statement(journal, task);
    let mut write = write();
    let fact = write
        .fact(
            wire::body::TaskEdited {
                task: task.clone(),
                revision: 2,
                dependencies: None,
                flow: None,
                intent: None,
            },
            FactCause::Root,
        )
        .expect("edit fact builds");
    let topic: wire::TaskTopic = topic.parse().expect("fixture topic");
    write.task_head(fact, move |revision| TaskHead {
        topic,
        revision,
        ..head
    });
    journal.apply(write).expect("re-homing commits");
}

fn authority(micros: i64) -> RoomAuthorityStamp {
    RoomAuthorityStamp::for_micros(1_760_000_000_000_000 + micros).expect("authority stamp")
}

fn journal_source(fact: u64) -> RoomSource {
    RoomSource::Journal {
        fact: SequenceCode::new(fact),
    }
}

fn hunk(head: &TranscriptSourceHead) -> TranscriptHunk {
    TranscriptHunk {
        head: head.clone(),
        locus: wire::MessageLocus {
            member: 1,
            offset: 0,
            length: 64,
            event: 0,
        },
        text: TextRange { start: 0, end: 8 },
    }
}

fn ingest(
    journal: &mut Journal,
    head: TranscriptSourceHead,
    from: TranscriptPosition,
    supersedes: Option<CanonicalUuid>,
) {
    let mut write = JournalWrite::new(
        Actor::Spice,
        format!("lane:{}", head.lane)
            .parse()
            .expect("resident origin"),
        instant(),
        Producer::ResidentRecognition,
    );
    let body = wire::body::JournalIngested {
        lane: head.lane,
        vendor: head.vendor.clone(),
        stream: head.stream.clone(),
        source: head.source,
        supersedes,
        from,
        to: head.next,
        usage: Vec::new(),
        non_usage_records: 1,
        residue_records: 0,
    };
    let pending = write
        .fact(body, FactCause::Root)
        .expect("transcript ingestion builds");
    write.transcript_source_head(pending, move |_| head);
    journal.apply(write).expect("transcript ingestion commits");
}

fn prepared_by(controller: &TaskStamp) -> TaskPreparation {
    TaskPreparation {
        parent: controller.clone(),
        controller: controller.clone(),
        construction: controller.clone(),
    }
}

/// One controller with a born output, an adoptable task, and a governed input.
struct Preparation {
    controller: TaskStamp,
    output: TaskStamp,
    spare: TaskStamp,
    input: TaskStamp,
    statement: Seq,
}

impl Preparation {
    fn new(journal: &mut Journal) -> Self {
        let controller = design_task(journal, 1);
        let statement = journal
            .read(|read| read.task_head(&controller))
            .expect("controller statement reads")
            .expect("controller exists")
            .position();
        let output = task(journal, 2, Some(prepared_by(&controller)));
        let spare = task(journal, 3, None);
        let input = task(journal, 4, None);
        Self {
            controller,
            output,
            spare,
            input,
            statement,
        }
    }

    fn decision(&self, sequence: u64, outcome: DecisionOutcome) -> PreparationDecided {
        PreparationDecided {
            decision: PreparationDecision {
                decision: self.decision_ref(sequence),
                source: journal_source(1),
                basis: DecisionBasis {
                    statement: self.statement,
                    room: None,
                    outputs: Vec::new(),
                },
                outcome,
            },
        }
    }

    fn decision_ref(&self, sequence: u64) -> DecisionRef {
        DecisionRef {
            controller: self.controller.clone(),
            decision: SequenceCode::new(sequence),
        }
    }

    fn authorization(&self, decision: u64, outputs: Vec<TaskStamp>) -> ExecutionAuthorized {
        ExecutionAuthorized {
            authorization: ExecutionAuthorization {
                decision: self.decision_ref(decision),
                selected_outputs: outputs,
                output_revisions: Vec::new(),
            },
        }
    }
}

fn selected(outputs: Vec<TaskStamp>) -> DecisionOutcome {
    DecisionOutcome::Resolved {
        outputs: outputs
            .into_iter()
            .map(|output| OutputDecision {
                output,
                verdict: OutputVerdict::Accepted,
                comment: None,
            })
            .collect(),
        comment: None,
    }
}

fn publication(room: &TaskStamp, micros: i64, content: RoomPublicationContent) -> RoomPublished {
    RoomPublished {
        room: room.clone(),
        publication: authority(micros),
        content,
    }
}

fn projection(journal: &Journal) -> crate::rooms::Projection {
    crate::rooms::Projection::load(&journal.connection).expect("projection reads")
}

fn fact_bytes(journal: &Journal) -> Vec<Vec<u8>> {
    let mut statement = journal
        .connection
        .prepare("SELECT envelope FROM fact_envelopes ORDER BY seq")
        .expect("prepare fact bytes");
    statement
        .query_map([], |row| row.get(0))
        .expect("query fact bytes")
        .map(|row| row.expect("fact bytes"))
        .collect()
}

/// Accepted preparation through exact authorization, positioned for further
/// adversarial writes.
fn authorized(journal: &mut Journal) -> Preparation {
    let preparation = Preparation::new(journal);
    commit(
        journal,
        preparation.decision(1, selected(vec![preparation.output.clone()])),
    )
    .expect("decision recorded");
    authorize(
        journal,
        preparation.authorization(1, vec![preparation.output.clone()]),
    )
    .expect("exact selection authorized");
    preparation
}

#[test]
fn one_write_maintains_the_projection_that_rebuild_reproduces_without_touching_facts() {
    let mut journal = Journal::in_memory().expect("journal");
    let preparation = authorized(&mut journal);
    let room = preparation.controller.clone();
    commit(
        &mut journal,
        publication(
            &room,
            1,
            RoomPublicationContent::Authored {
                text: "first".to_owned(),
            },
        ),
    )
    .expect("authored publication");
    commit(
        &mut journal,
        publication(
            &room,
            2,
            RoomPublicationContent::Linked {
                link: journal_source(1),
            },
        ),
    )
    .expect("linked publication");
    journal.verify().expect("committed projection verifies");

    let (decision, granted, page) = journal
        .read(|read| {
            let decision = read.proposal_head(&ProposalRef::Decision {
                decision: preparation.decision_ref(1),
            })?;
            let granted = read.authorization_head(&preparation.output)?;
            let page = read.room_publications(&room, None, fact_budget(1))?;
            Ok((decision, granted, page))
        })
        .expect("room authority reads");
    assert!(matches!(
        decision.map(JournalHead::into_body),
        Some(PreparationDecision {
            outcome: DecisionOutcome::Resolved { .. },
            ..
        })
    ));
    assert_eq!(
        granted
            .expect("output is authorized")
            .body()
            .selected_outputs,
        vec![preparation.output.clone()]
    );
    assert_eq!(page.rows.len(), 1, "the budget bounds the page");
    let rest = journal
        .read(|read| read.room_publications(&room, page.continuation, fact_budget(8)))
        .expect("continued publication page");
    assert_eq!(rest.rows.len(), 1);
    assert_eq!(rest.continuation, None);
    assert!(rest.rows[0].seq > page.rows[0].seq);

    let derived = projection(&journal);
    let facts = fact_bytes(&journal);
    journal
        .connection
        .execute_batch(
            "DELETE FROM authorization_heads;
             UPDATE proposal_heads SET head_seq = (SELECT min(seq) FROM fact_envelopes)
             WHERE head_seq = (SELECT max(head_seq) FROM proposal_heads);
             INSERT INTO room_publications(room, publication, seq)
             VALUES ('ghost', 'ghost', (SELECT seq FROM fact_envelopes ORDER BY seq LIMIT 1 OFFSET 1));",
        )
        .expect("corrupt the projection");
    assert!(refused_classes(&journal).contains(&ViolationClass::Head));

    journal.reindex().expect("projection rebuilds from facts");
    assert_eq!(projection(&journal), derived);
    assert_eq!(fact_bytes(&journal), facts, "rebuild never rewrites facts");
    journal.verify().expect("rebuilt projection verifies");
}

#[test]
fn publication_reads_start_after_the_cursor_without_decoding_older_room_history() {
    const PRE_HORIZON_PUBLICATIONS: usize = 1_025;
    const POST_HORIZON_PUBLICATIONS: usize = 3;

    let mut journal = Journal::in_memory().expect("journal");
    let preparation = authorized(&mut journal);
    let room = preparation.controller;
    let mut retained = write();
    for index in 0..PRE_HORIZON_PUBLICATIONS {
        retained
            .fact(
                publication(
                    &room,
                    i64::try_from(index).expect("fixture index") + 1,
                    RoomPublicationContent::Authored {
                        text: format!("retained {index}"),
                    },
                ),
                FactCause::Root,
            )
            .expect("retained publication builds");
    }
    journal
        .apply(retained)
        .expect("retained publications commit");
    let retained_start = journal
        .read(|read| read.room_publications(&room, None, fact_budget(1)))
        .expect("retained start reads")
        .rows[0]
        .seq;
    let after = cursor(&journal).expect("retained horizon");

    let mut current = write();
    for index in 0..POST_HORIZON_PUBLICATIONS {
        current
            .fact(
                publication(
                    &room,
                    i64::try_from(PRE_HORIZON_PUBLICATIONS + index + 1).expect("fixture index"),
                    RoomPublicationContent::Authored {
                        text: format!("current {index}"),
                    },
                ),
                FactCause::Root,
            )
            .expect("current publication builds");
    }
    journal.apply(current).expect("current publications commit");

    journal
        .connection
        .execute(
            "UPDATE fact_envelopes SET envelope = x'00' WHERE seq = ?1",
            [i64::try_from(retained_start.get()).expect("fixture sequence")],
        )
        .expect("make an older publication undecodable");
    let (page, latest) = journal
        .read(|read| {
            Ok((
                read.room_publications(&room, Some(after), fact_budget(8))?,
                read.latest_room_publication(&room, &Actor::TEST_PRINCIPAL)?,
            ))
        })
        .expect("post-horizon authority reads without touching older bytes");
    assert_eq!(page.rows.len(), POST_HORIZON_PUBLICATIONS);
    assert!(page.rows.iter().all(|publication| publication.seq > after));
    assert_eq!(
        latest.map(|publication| publication.seq),
        page.rows.last().map(|publication| publication.seq)
    );
}

#[test]
fn preparation_authority_pages_one_controller_in_journal_order() {
    let mut journal = Journal::in_memory().expect("journal");
    let preparation = authorized(&mut journal);

    let controller = preparation.controller.clone();
    let (first, population) = journal
        .read(|read| {
            Ok((
                read.preparation_authority_page(&controller, None, fact_budget(1))?,
                read.preparation_authority_count(&controller, None)?,
            ))
        })
        .expect("first preparation page reads");
    assert_eq!(population, 2);
    assert_eq!(first.rows.len(), 1);
    let continuation = first.continuation.expect("first page continues");
    let rest = journal
        .read(|read| {
            read.preparation_authority_page(&controller, Some(continuation), fact_budget(8))
        })
        .expect("preparation continuation reads");
    assert_eq!(rest.rows.len(), 1);
    assert_eq!(rest.continuation, None);
    let kinds = first
        .rows
        .iter()
        .chain(&rest.rows)
        .map(|envelope| envelope.fact.kind())
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![FactKind::PreparationDecided, FactKind::ExecutionAuthorized]
    );
    assert!(
        journal
            .read(|read| read.preparation_authority_contains(&controller, continuation))
            .expect("controller cursor reads")
    );
    assert!(
        !journal
            .read(|read| read.preparation_authority_contains(&preparation.input, continuation))
            .expect("foreign controller cursor reads")
    );
}

#[test]
fn duplicate_recordings_refuse_and_a_duplicated_history_fails_verification() {
    let mut journal = Journal::in_memory().expect("journal");
    let preparation = authorized(&mut journal);
    let room = preparation.controller.clone();
    let post = publication(
        &room,
        1,
        RoomPublicationContent::Authored {
            text: "once".to_owned(),
        },
    );
    commit(&mut journal, post.clone()).expect("first publication");
    let result = commit(&mut journal, post.clone());
    refused(&journal, result);
    let result = commit(
        &mut journal,
        preparation.decision(1, selected(vec![preparation.output.clone()])),
    );
    refused(&journal, result);

    // Replaying one fact twice inside a single write refuses the whole write.
    let mut twice = write();
    let second = publication(
        &room,
        2,
        RoomPublicationContent::Authored {
            text: "twice".to_owned(),
        },
    );
    twice
        .fact(second.clone(), FactCause::Root)
        .expect("fact builds");
    twice.fact(second, FactCause::Root).expect("fact builds");
    let result = journal.apply(twice);
    refused(&journal, result);

    // History that bypassed the commit fold cannot verify.
    let next = Seq::new(cursor(&journal).expect("nonempty").get() + 1).expect("next position");
    insert_fact(
        &journal,
        fact_at(
            next,
            None,
            FactKind::RoomPublished,
            serde_json::to_value(&post).expect("publication body"),
        ),
    );
    let sql_seq = sql_position(next);
    journal
        .connection
        .execute_batch(&format!(
            "INSERT INTO thread_index(seq, origin, cause, root, depth) VALUES ({sql_seq}, 'task:1kKRRsDPx', NULL, {sql_seq}, 0);
             UPDATE cursor_head SET seq = {sql_seq} WHERE singleton = 1;
             UPDATE steering_index SET indexed_seq = {sql_seq} WHERE singleton = 1;",
        ))
        .expect("index the duplicate");
    let violations = refused_violations(&journal);
    assert!(
        violations
            .iter()
            .any(|violation| violation.subject == format!("room_authority:{next}")),
        "{violations:?}"
    );
    assert!(matches!(
        journal.reindex(),
        Err(crate::ReindexError::Violations(_))
    ));
}

#[test]
fn each_output_has_one_controller_and_rehoming_keeps_room_identity() {
    let mut journal = Journal::in_memory().expect("journal");
    let preparation = Preparation::new(&mut journal);
    let other = task(&mut journal, 9, None);
    let control = prepared_by(&preparation.controller);

    // A head that disagrees with the controlled preparation refuses.
    let result = adopt(
        &mut journal,
        &preparation.spare,
        control.clone(),
        prepared_by(&other),
    );
    refused(&journal, result);
    // A task cannot control itself.
    let result = adopt(
        &mut journal,
        &preparation.controller,
        control.clone(),
        control.clone(),
    );
    refused(&journal, result);

    adopt(
        &mut journal,
        &preparation.spare,
        control.clone(),
        control.clone(),
    )
    .expect("adoption commits");
    // Neither an adopted nor a born output can move to another controller.
    for output in [&preparation.spare, &preparation.output] {
        let result = adopt(
            &mut journal,
            output,
            prepared_by(&other),
            prepared_by(&other),
        );
        refused(&journal, result);
    }
    commit(
        &mut journal,
        preparation.decision(
            1,
            selected(vec![preparation.output.clone(), preparation.spare.clone()]),
        ),
    )
    .expect("both controlled outputs join one decision");
    let foreign = preparation.decision(2, selected(vec![preparation.output.clone()]));
    let result = commit(
        &mut journal,
        PreparationDecided {
            decision: PreparationDecision {
                decision: DecisionRef {
                    controller: other.clone(),
                    decision: SequenceCode::new(1),
                },
                ..foreign.decision
            },
        },
    );
    refused(&journal, result);

    let room = preparation.output.clone();
    commit(
        &mut journal,
        publication(
            &room,
            1,
            RoomPublicationContent::Authored {
                text: "before".to_owned(),
            },
        ),
    )
    .expect("publication before re-homing");
    edit_topic(&mut journal, &room, "/elsewhere");
    commit(
        &mut journal,
        publication(
            &room,
            2,
            RoomPublicationContent::Authored {
                text: "after".to_owned(),
            },
        ),
    )
    .expect("publication after re-homing");
    let page = journal
        .read(|read| read.room_publications(&room, None, fact_budget(8)))
        .expect("room reads after re-homing");
    assert_eq!(page.rows.len(), 2, "the room is its anchor's birth stamp");
    journal
        .verify()
        .expect("controlled and re-homed authority verifies");
}

#[test]
fn links_must_name_existing_authority() {
    let mut journal = Journal::in_memory().expect("journal");
    let preparation = Preparation::new(&mut journal);
    let room = preparation.controller.clone();
    let future = cursor(&journal).expect("nonempty").get() + 1;
    let transcript = RoomSource::Transcript(TranscriptHunk {
        head: transcript_source_head(),
        locus: wire::MessageLocus {
            member: 1,
            offset: 0,
            length: 1,
            event: 0,
        },
        text: TextRange { start: 0, end: 1 },
    });
    for link in [journal_source(0), journal_source(future), transcript] {
        let result = commit(
            &mut journal,
            publication(&room, 1, RoomPublicationContent::Linked { link }),
        );
        refused(&journal, result);
    }
    let absent = TaskStamp::for_micros(99_999).expect("absent stamp");
    let result = commit(
        &mut journal,
        publication(
            &absent,
            1,
            RoomPublicationContent::Authored {
                text: "nowhere".to_owned(),
            },
        ),
    );
    refused(&journal, result);
    let result = commit(
        &mut journal,
        preparation.decision(1, selected(vec![preparation.spare.clone()])),
    );
    refused(&journal, result);
    commit(
        &mut journal,
        publication(
            &room,
            1,
            RoomPublicationContent::Linked {
                link: RoomSource::Artifact {
                    digest: digest("a"),
                },
            },
        ),
    )
    .expect("artifact bytes stay with their digest");
    journal.verify().expect("linked authority verifies");
}

#[test]
fn transcript_links_stay_within_the_accounted_generation_and_extent() {
    let mut journal = Journal::in_memory().expect("journal");
    let room = task(&mut journal, 1, None);
    let head = transcript_source_head();
    ingest(
        &mut journal,
        head.clone(),
        TranscriptPosition {
            member: 1,
            offset: 0,
        },
        None,
    );
    let linked =
        |head: TranscriptSourceHead, locus: wire::MessageLocus| RoomPublicationContent::Linked {
            link: RoomSource::Transcript(TranscriptHunk {
                head,
                locus,
                text: TextRange { start: 0, end: 1 },
            }),
        };
    let locus = |offset, length| wire::MessageLocus {
        member: 1,
        offset,
        length,
        event: 0,
    };

    commit(
        &mut journal,
        publication(&room, 20, linked(head.clone(), locus(4_095, 1))),
    )
    .expect("the accounted final byte is linkable");
    let mut earlier = head.clone();
    earlier.next.offset = 2_048;
    commit(
        &mut journal,
        publication(&room, 21, linked(earlier, locus(2_047, 1))),
    )
    .expect("an earlier boundary within the current generation is linkable");

    let mut future = head.clone();
    future.next.offset += 1;
    let mut wrong_generation = head.clone();
    wrong_generation.source = uuid("018f82d4-6f59-7d06-92e3-bce913116399");
    let mut wrong_stream = head.clone();
    wrong_stream.stream = "sidecar".to_owned();
    for (micros, source) in [
        (22, linked(future, locus(0, 1))),
        (23, linked(head.clone(), locus(4_095, 2))),
        (24, linked(head.clone(), locus(u64::MAX, 2))),
        (25, linked(wrong_generation, locus(0, 1))),
        (26, linked(wrong_stream, locus(0, 1))),
        (27, linked(head.clone(), locus(0, 0))),
    ] {
        let result = commit(&mut journal, publication(&room, micros, source));
        refused(&journal, result);
    }

    let mut advanced = head.clone();
    advanced.next.offset = 8_192;
    ingest(&mut journal, advanced, head.next, None);
    commit(
        &mut journal,
        publication(&room, 28, linked(head.clone(), locus(4_000, 96))),
    )
    .expect("a later head preserves links within the accounted generation");

    let mut next_member = head.clone();
    next_member.next = TranscriptPosition {
        member: 2,
        offset: 0,
    };
    ingest(
        &mut journal,
        next_member.clone(),
        TranscriptPosition {
            member: 1,
            offset: 8_192,
        },
        None,
    );
    let result = commit(
        &mut journal,
        publication(&room, 280, linked(next_member, locus(u64::MAX, 2))),
    );
    refused(&journal, result);

    let mut replacement = head.clone();
    replacement.source = uuid("018f82d4-6f59-7d06-92e3-bce913116398");
    replacement.next.offset = 128;
    ingest(
        &mut journal,
        replacement,
        TranscriptPosition {
            member: 1,
            offset: 0,
        },
        Some(head.source),
    );
    let result = commit(
        &mut journal,
        publication(&room, 29, linked(head, locus(0, 1))),
    );
    refused(&journal, result);

    let expected = projection(&journal);
    journal.verify().expect("accounted links verify");
    journal.reindex().expect("accounted links rebuild");
    assert_eq!(projection(&journal), expected);
}

#[test]
fn newest_decisions_page_backward_strictly_before_the_cursor() {
    let mut journal = Journal::in_memory().expect("journal");
    let preparation = Preparation::new(&mut journal);
    for sequence in 1..=5 {
        let returned = DecisionOutcome::Returned {
            reason: format!("reason {sequence}"),
        };
        commit(
            &mut journal,
            preparation.decision(sequence as u64, returned),
        )
        .expect("return recorded");
    }
    let budget = crate::FactReadBudget::new(2, crate::FactReadBudget::RETAINED.decoded_bytes())
        .expect("budget");
    let page = |before| {
        let page = journal
            .read(|read| read.newest_preparation_decisions(&preparation.controller, before, budget))
            .expect("decisions read");
        let codes: Vec<u64> = page
            .rows
            .iter()
            .map(|row| {
                row.fact
                    .read::<PreparationDecided>()
                    .expect("only decision facts")
                    .decision
                    .decision
                    .decision
                    .get()
            })
            .collect();
        (codes, page.continuation)
    };

    let (newest, cursor) = page(None);
    assert_eq!(newest, [5, 4], "newest first, the page bounded");
    let (older, cursor) = page(cursor);
    assert_eq!(older, [3, 2], "the cursor is exclusive");
    let (oldest, cursor) = page(cursor);
    assert_eq!(
        (oldest, cursor),
        (vec![1], None),
        "the first decision ends the record"
    );
}
