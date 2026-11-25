//! A lane's typed contribution to an open Design room: admitted against one
//! snapshot and its cited sources, read back grouped by kind beside the
//! complete chronological history, and authorizing nothing.

use super::*;

use serde_json::{Value, json};
use wire::intent::{Intent, RoomContribute, RoomContributeAnswer};
use wire::rooms::{
    ROOM_CONTRIBUTION_SOURCE_LIMIT, ROOM_CONTRIBUTION_TEXT_LIMIT, RoomContributed,
    RoomContributionKind, RoomPost, StatementItem, TranscriptHunk,
};

use crate::room_command::SourceOwners;

/// The task artifact store under `root`.
fn artifacts(root: &Path) -> artifact::Store {
    fs::create_dir_all(root.join("artifacts")).expect("artifact directory exists");
    fs::create_dir_all(root.join("locks")).expect("artifact lock directory exists");
    artifact::Store::new(
        root.join("artifacts"),
        root.join("locks/task-artifacts.lock"),
    )
}

fn retain(root: &Path, task: &TaskStamp, bytes: &[u8]) -> wire::Digest {
    artifacts(root)
        .add_bytes(artifact::AddBytesRequest {
            task,
            bytes,
            name: "evidence.txt",
            media_type: Some(artifact::MediaType::TEXT_PLAIN),
            retention: artifact::Retention::Permanent,
            created_at: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
        })
        .expect("fixture artifact publishes")
        .0
        .digest
}

/// A contribution of `kind`: a charge weighs the fixture Design's one
/// question item, and a question names none.
pub(super) fn contribution_body(
    room: &TaskHandle,
    kind: RoomContributionKind,
    text: &str,
    sources: Vec<RoomSource>,
) -> RoomContribute {
    RoomContribute {
        room: room.clone(),
        kind,
        question: matches!(kind, RoomContributionKind::Charge(_))
            .then(|| StatementItem::of(QUESTION)),
        text: text.to_owned(),
        sources,
    }
}

/// Contribute `body` as `actor`, witnessing the Design as the board reads now.
pub(super) fn contribute(
    bus: &Bus,
    epoch: CanonicalUuid,
    root: &Path,
    actor: Actor,
    body: RoomContribute,
) -> Outcome<CommandAnswer> {
    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&visible, &body.room);
    let mut expected = vec![statement_witness(heads), workflow_witness(heads)];
    expected.extend(body.sources.iter().map(|source| Witness::Source {
        source: source.authority_digest(),
    }));
    let command = fixture_command(epoch, actor, CommandKind::RoomContribute, expected, &body);
    let Intent::RoomContribute(request) = command.body() else {
        panic!("fixture command carries a contribution");
    };
    let store = artifacts(root);
    let tape = root.join("tape");
    let git = root.join("repo");
    room_contribute(
        bus,
        &command,
        request,
        Some(SourceOwners {
            tape: &tape,
            artifacts: &store,
            git: &git,
        }),
        None,
    )
}

pub(super) fn contributed(outcome: Outcome<CommandAnswer>) -> RoomContributeAnswer {
    match assert_committed(outcome) {
        CommandAnswer::RoomContribute(answer) => *answer,
        other => panic!("contribution answered {other:?}"),
    }
}

/// One `spice room history` page, its sources read under `artifacts_root`.
pub(super) fn history(root: &Path, artifacts_root: &Path, words: &[&str]) -> Value {
    let matches = crate::cli::command()
        .try_get_matches_from(["spice", "room", "history"].iter().chain(words))
        .expect("room history parses");
    let contract = crate::cli::selected_contract(&matches).expect("room history is registered");
    let journal = journal::JournalReader::open(root.join("authority.sqlite3"))
        .expect("fixture journal opens to read");
    let store = artifacts(artifacts_root);
    let tape = root.join("tape");
    let git = root.join("repo");
    let outcome = crate::room_command::answer(
        &journal,
        SourceOwners {
            tape: &tape,
            artifacts: &store,
            git: &git,
        },
        crate::cli::selected_matches(&matches),
        contract.path,
    );
    let Outcome::Reply(reply) = outcome else {
        panic!("room history returned another outcome: {outcome:?}");
    };
    serde_json::to_value(reply.body).expect("room history encodes")
}

fn publications(page: &Value) -> Vec<String> {
    page["page"]["rows"]
        .as_array()
        .expect("history rows")
        .iter()
        .map(|row| row["publication"].as_str().expect("publication").to_owned())
        .collect()
}

/// Every authority a contribution must leave alone, as the operator reads it.
pub(super) fn authority(
    bus: &Bus,
    epoch: CanonicalUuid,
    root: &FixtureRoot,
    design: &TaskHandle,
) -> Value {
    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let task = at(&visible, design);
    let mut preparation = preparation_page(root, &[design.as_str()]);
    if let Some(object) = preparation.as_object_mut() {
        object.remove("position");
    }
    json!({
        "statement": serde_json::to_value(&task.statement).expect("statement encodes"),
        "workflow": serde_json::to_value(&task.workflow).expect("workflow encodes"),
        "claim": serde_json::to_value(&task.claims).expect("claim encodes"),
        "preparation": preparation,
    })
}

#[test]
fn lanes_contribute_typed_records_that_read_grouped_and_authorize_nothing() {
    let (root, bus, epoch, seats) = fixture("design-contribution", 1);
    let lane = Actor::Lane(seats[0].lane);
    let design = add_controller(
        &bus,
        epoch,
        "Contributed design",
        wire::TaskIntent::Design,
        None,
    );
    let born = design.born().expect("fixture Design has a birth");
    let digest = retain(root.as_ref(), &born, b"p99 latency 41ms");
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::RoomPost,
        vec![statement_witness(at(&visible, &design))],
        &RoomPost {
            room: design.clone(),
            text: "Weigh the journal index.".to_owned(),
        },
    ));
    let cited = journal_source(&visible, &design);
    let before = authority(&bus, epoch, &root, &design);

    let written = write_contributions(&bus, epoch, root.as_ref(), &lane, &design, &cited, &digest);
    assert_eq!(
        authority(&bus, epoch, &root, &design),
        before,
        "contributing changes no statement, workflow, claim, candidate, decision, or authorization"
    );
    let maturity = journal::JournalReader::open(root.as_ref().join("authority.sqlite3"))
        .expect("fixture journal opens to read")
        .read(|read| read.design_maturity(&born))
        .expect("maturity reads");
    let series = maturity
        .items
        .series(&wire::rooms::StatementItem::of(
            "the exact selected   OUTPUT is justified",
        ))
        .expect("admission keys each charge by its item's current text");
    assert_eq!(
        (series.charges, series.state()),
        (3, wire::rooms::DesignItemState::UnopposedAgainst),
        "a zero charge enters no series, and one lane flipping its sign holds only its latest"
    );

    let page = history(root.as_ref(), root.as_ref(), &[design.as_str()]);
    let rows = page["page"]["rows"].as_array().expect("history rows");
    assert_eq!(
        rows.len(),
        6,
        "the complete history keeps every publication"
    );
    assert_eq!(
        rows[0]["content"],
        json!({ "state": "text", "text": "Weigh the journal index." }),
        "an untyped post stays conversation"
    );
    assert_eq!(publications(&page)[1..], written[..]);
    let first = &rows[1];
    assert_eq!(first["actor"], serde_json::to_value(&lane).expect("actor"));
    assert!(
        first["at"].is_string(),
        "the record names its time: {first}"
    );
    let item = StatementItem::of(QUESTION);
    assert_eq!(
        first["content"],
        json!({
            "state": "contribution",
            "kind": 1,
            "question": { "item": item, "handle": item.handle(&[]), "text": QUESTION },
            "text": "Reuse the journal index.",
            "sources": [{ "state": "available", "source": cited }],
        })
    );
    assert_eq!(
        rows[2]["content"]["kind"], "question",
        "a question weighs no item"
    );
    assert!(rows[2]["content"].get("question").is_none());
    assert_eq!(
        rows[4]["content"]["text"], "Pages already exist.",
        "the recorded text is the trimmed contribution"
    );
    let grouped = json!([
        { "kind": 1, "publications": [written[0], written[3]] },
        { "kind": 0, "publications": [written[2]] },
        { "kind": -1, "publications": [written[4]] },
        { "kind": "question", "publications": [written[1]] },
    ]);
    assert_eq!(page["contributions"], grouped);

    assert_contribution_pages(root.as_ref(), &design, &written, &grouped);

    let (elsewhere, pruned) = assert_unretained_artifact_reads(root.as_ref(), &design, &digest);

    assert_sources_read_as_history(
        &bus,
        root.as_ref(),
        &born,
        (&elsewhere, &pruned),
        &written,
        &publications(&page)[0],
    );

    assert_moved_statement_keeps_the_item(&bus, epoch, root.as_ref(), &design);
    clean(&root, bus);
}

/// An artifact a contribution cites stays retained: removal reads the room's
/// cited digests and refuses that citation while an uncited one still goes.
#[test]
fn a_room_cited_artifact_refuses_removal_and_an_uncited_one_is_removed() {
    let (root, bus, epoch, seats) = fixture("design-contribution-artifact-removal", 1);
    let lane = Actor::Lane(seats[0].lane);
    let design = add_controller(
        &bus,
        epoch,
        "Evidence design",
        wire::TaskIntent::Design,
        None,
    );
    let born = design.born().expect("fixture Design has a birth");
    let digest = retain(root.as_ref(), &born, b"cited evidence");
    retain(root.as_ref(), &born, b"uncited evidence");
    let body = contribution_body(
        &design,
        RoomContributionKind::charge(0),
        "Measured.",
        vec![RoomSource::Artifact {
            digest: digest.clone(),
        }],
    );
    contributed(contribute(&bus, epoch, root.as_ref(), lane, body));

    let cited = crate::task_artifact_command::room_cited(&root.join("authority.sqlite3"), &born)
        .unwrap_or_else(|_| panic!("room citations read"));
    assert_eq!(cited, std::collections::BTreeSet::from([digest]));
    let store = artifacts(root.as_ref());
    let refused = store
        .remove(&born, "A1", &cited)
        .expect_err("a room-cited artifact stays");
    assert_eq!(refused.class(), artifact::ErrorClass::Refusal);
    store
        .remove(&born, "A2", &cited)
        .expect("an uncited artifact is removed");
    assert_eq!(store.list(&born).expect("one citation remains").len(), 1);
    clean(&root, bus);
}

/// The browser's contribution read says what each cited journal fact is: a
/// task fact and a room record name their task's current handle, and a fact
/// about no task names only its kind.
#[test]
fn a_cited_journal_fact_reads_with_its_kind_and_task() {
    let (root, bus, epoch, seats) = fixture("design-contribution-facts", 1);
    let design = add_controller(&bus, epoch, "Cited design", wire::TaskIntent::Design, None);
    let born = design.born().expect("fixture Design has a birth");
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::RoomPost,
        vec![statement_witness(at(&visible, &design))],
        &RoomPost {
            room: design.clone(),
            text: "Weigh the journal index.".to_owned(),
        },
    ));
    let post = journal::JournalReader::open(root.as_ref().join("authority.sqlite3"))
        .expect("fixture journal opens to read")
        .read(|read| Ok(read.cursor()?.expect("the post is recorded")))
        .expect("cursor reads");
    let journal = |fact: u64| RoomSource::Journal {
        fact: wire::SequenceCode::new(fact),
    };
    let first = wire::Seq::FIRST.get();
    let statement = at(&visible, &design).statement.position.get();
    let written = contributed(contribute(
        &bus,
        epoch,
        root.as_ref(),
        Actor::Lane(seats[0].lane),
        contribution_body(
            &design,
            RoomContributionKind::charge(0),
            "Cited three ways.",
            vec![journal(statement), journal(post.get()), journal(first)],
        ),
    ));
    let store = artifacts(root.as_ref());
    let tape = root.as_ref().join("tape");
    let Outcome::Reply(reply) = crate::room_command::contribution_read(
        &bus,
        SourceOwners {
            tape: &tape,
            artifacts: &store,
            git: &root.as_ref().join("repo"),
        },
        &wire::intent::RoomContributionRead {
            room: born.clone(),
            publication: written.publication,
        },
    ) else {
        panic!("the contribution read answers");
    };
    let CommandAnswer::RoomContributionRead(answer) = reply.body else {
        panic!("another answer shape: {:?}", reply.body);
    };
    let task = Some(wire::intent::RoomFactTask {
        born,
        handle: design.clone(),
    });
    assert_eq!(
        answer
            .facts
            .iter()
            .map(|summary| (summary.fact.get(), summary.task.clone()))
            .collect::<Vec<_>>(),
        [(statement, task.clone()), (post.get(), task), (first, None)],
    );
    assert_ne!(
        answer.facts[0].kind, answer.facts[1].kind,
        "each summary names its own fact's kind"
    );
    clean(&root, bus);
}

/// An artifact the reader's store no longer retains reads as a typed
/// unavailable link, never as fabricated text. Returns the pruned reader root
/// and its history page.
fn assert_unretained_artifact_reads(
    root: &Path,
    design: &TaskHandle,
    digest: &wire::Digest,
) -> (std::path::PathBuf, Value) {
    let elsewhere = root.join("pruned");
    let pruned = history(root, &elsewhere, &[design.as_str()]);
    assert_eq!(
        pruned["page"]["rows"][3]["content"]["sources"],
        json!([{
            "state": "unavailable",
            "source": { "kind": "artifact", "digest": digest },
            "unavailable": { "reason": "missing" },
        }])
    );
    assert_eq!(
        pruned["page"]["rows"][3]["content"]["text"],
        "Measured under load."
    );
    (elsewhere, pruned)
}

/// Once the Design's statement moves, the journal keeps no text of the
/// statement a charge weighed, so the item stands without borrowed text.
fn assert_moved_statement_keeps_the_item(
    bus: &Bus,
    epoch: CanonicalUuid,
    root: &Path,
    design: &TaskHandle,
) {
    assert_committed(super::super::edit::correct(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            term: Some(vec!["A different question".to_owned()]),
            ..only(design)
        },
    ));
    let revised = history(root, root, &[design.as_str()]);
    let item = StatementItem::of(QUESTION);
    assert_eq!(
        revised["page"]["rows"][1]["content"]["question"],
        json!({ "item": item, "handle": item.handle(&[]) })
    );
}

/// The browser's per-contribution read resolves each source exactly as the
/// history read `shown` did against the same owners, and refuses a
/// publication that is not a contribution of the room.
fn assert_sources_read_as_history(
    bus: &Bus,
    root: &Path,
    born: &TaskStamp,
    (elsewhere, shown): (&Path, &Value),
    written: &[String],
    untyped: &str,
) {
    let resolved = |artifacts_root: &Path, publication: &str| {
        let store = artifacts(artifacts_root);
        let tape = root.join("tape");
        crate::room_command::contribution_read(
            bus,
            SourceOwners {
                tape: &tape,
                artifacts: &store,
                git: &root.join("repo"),
            },
            &wire::intent::RoomContributionRead {
                room: born.clone(),
                publication: publication.parse().expect("fixture publication"),
            },
        )
    };
    for (index, publication) in written.iter().enumerate() {
        let Outcome::Reply(reply) = resolved(elsewhere, publication) else {
            panic!("the sources read answers {publication}");
        };
        let CommandAnswer::RoomContributionRead(answer) = reply.body else {
            panic!("another answer shape: {:?}", reply.body);
        };
        let row = &shown["page"]["rows"][index + 1]["content"];
        assert_eq!(
            serde_json::to_value(&answer.sources).expect("sources encode"),
            row.get("sources").cloned().unwrap_or_else(|| json!([])),
            "contribution {publication} resolves as the history read shows it"
        );
    }
    assert_eq!(
        refusal_reason(&resolved(root, untyped)),
        "room::contribution-missing",
        "an untyped post has no contribution sources"
    );
}

/// Write five contributions, one with each source kind it can cite, in order.
fn write_contributions(
    bus: &Bus,
    epoch: CanonicalUuid,
    root: &Path,
    lane: &Actor,
    design: &TaskHandle,
    cited: &RoomSource,
    digest: &wire::Digest,
) -> Vec<String> {
    let artifact = RoomSource::Artifact {
        digest: digest.clone(),
    };
    [
        (
            RoomContributionKind::charge(1),
            "Reuse the journal index.",
            vec![cited.clone()],
        ),
        (
            RoomContributionKind::Question,
            "Who owns retention?",
            Vec::new(),
        ),
        (
            RoomContributionKind::charge(0),
            "Measured under load.",
            vec![artifact],
        ),
        (
            RoomContributionKind::charge(1),
            "  Pages already exist.\n",
            Vec::new(),
        ),
        (
            RoomContributionKind::charge(-1),
            "Index rebuilds are slow.",
            Vec::new(),
        ),
    ]
    .into_iter()
    .map(|(kind, text, sources)| {
        let body = contribution_body(design, kind, text, sources);
        let answer = contributed(contribute(bus, epoch, root, lane.clone(), body));
        assert_eq!(answer.kind, kind);
        answer.publication.to_string()
    })
    .collect()
}

/// `--contributions` pages only contributions, grouping just what it shows,
/// and a continuation names the first withheld contribution.
fn assert_contribution_pages(
    root: &Path,
    design: &TaskHandle,
    written: &[String],
    grouped: &Value,
) {
    let room = design.as_str();
    let only = history(root, root, &["--contributions", room]);
    assert_eq!(publications(&only), written, "only contributions, in order");
    assert_eq!(&only["contributions"], grouped);
    let first_page = history(root, root, &["--contributions", "--limit", "2", room]);
    assert_eq!(publications(&first_page), written[..2]);
    assert_eq!(
        first_page["contributions"],
        json!([
            { "kind": 1, "publications": [written[0]] },
            { "kind": "question", "publications": [written[1]] },
        ]),
        "a page groups only the contributions it shows"
    );
    let rest = history(
        root,
        root,
        &["--contributions", "--after", &written[2], room],
    );
    assert_eq!(publications(&rest), written[2..]);
}

#[test]
fn contribution_admission_refuses_typed_and_writes_nothing() {
    let (root, bus, epoch, seats) = fixture("design-contribution-refusals", 1);
    let lane = Actor::Lane(seats[0].lane);
    let design = add_controller(
        &bus,
        epoch,
        "Refusing design",
        wire::TaskIntent::Design,
        None,
    );
    let plan = add_plan(&bus, epoch, "Not a design");
    let other = retain(
        root.as_ref(),
        &plan.born().expect("fixture Plan has a birth"),
        b"retained elsewhere",
    );
    let journal = |fact: u64| RoomSource::Journal {
        fact: wire::SequenceCode::new(fact),
    };
    let attempt = |actor: Actor, room: &TaskHandle, text: &str, sources: Vec<RoomSource>| {
        let body = contribution_body(room, RoomContributionKind::charge(1), text, sources);
        contribute(&bus, epoch, root.as_ref(), actor, body)
    };
    let operator = attempt(Actor::TEST_PRINCIPAL, &design, "Operator view.", Vec::new());
    assert_eq!(refusal_reason(&operator), "room::contribution-lane-only");
    let wrong_room = attempt(lane.clone(), &plan, "Wrong room.", Vec::new());
    assert_eq!(refusal_reason(&wrong_room), "room::contribution-not-design");
    let too_long = "x".repeat(ROOM_CONTRIBUTION_TEXT_LIMIT + 1);
    let too_many = (1..=ROOM_CONTRIBUTION_SOURCE_LIMIT as u64 + 1)
        .map(journal)
        .collect();
    let unavailable = "room::contribution-source-unavailable";
    for (text, sources, reason) in [
        (" \n ", Vec::new(), "room::contribution-empty"),
        (too_long.as_str(), Vec::new(), "room::contribution-too-long"),
        ("Too many.", too_many, "room::contribution-sources-exceeded"),
        (
            "Twice.",
            vec![journal(1), journal(1)],
            "room::contribution-source-duplicate",
        ),
        ("Future fact.", vec![journal(1_000_000)], unavailable),
        (
            "Another task's artifact.",
            vec![RoomSource::Artifact { digest: other }],
            unavailable,
        ),
        (
            "Unwritten tape.",
            vec![RoomSource::Transcript(unwritten_hunk(seats[0].lane))],
            unavailable,
        ),
    ] {
        let outcome = attempt(lane.clone(), &design, text, sources);
        assert_eq!(refusal_reason(&outcome), reason, "{outcome:?}");
    }
    // The Design has one question item; a charge must name it, a stale item
    // refuses, and only an opinion may stand without a source.
    let charged = |kind: i8, question: Option<&str>| {
        let body = RoomContribute {
            question: question.map(StatementItem::of),
            ..contribution_body(
                &design,
                RoomContributionKind::charge(kind),
                "Charged.",
                Vec::new(),
            )
        };
        contribute(&bus, epoch, root.as_ref(), lane.clone(), body)
    };
    for (kind, question, reason) in [
        (1, None, "room::contribution-question-missing"),
        (
            -1,
            Some("A question this statement never named"),
            "room::contribution-question-stale",
        ),
        (0, Some(QUESTION), "room::contribution-uncited"),
        (2, Some(QUESTION), "room::contribution-uncited"),
        (-2, Some(QUESTION), "room::contribution-uncited"),
    ] {
        let outcome = charged(kind, question);
        assert_eq!(refusal_reason(&outcome), reason, "{outcome:?}");
    }
    assert_exact_source_refusals(&bus, epoch, root.as_ref(), &lane, &design, seats[0].lane);
    let empty = history(
        root.as_ref(),
        root.as_ref(),
        &["--contributions", design.as_str()],
    );
    assert_eq!(publications(&empty), Vec::<String>::new());

    assert_committed(conclude(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &design,
        done_conclusion(),
    ));
    let late = attempt(lane, &design, "After the fact.", Vec::new());
    assert_eq!(refusal_reason(&late), "room::contribution-design-concluded");
    clean(&root, bus);
}

fn assert_exact_source_refusals(
    bus: &Bus,
    epoch: CanonicalUuid,
    root: &Path,
    lane: &Actor,
    design: &TaskHandle,
    author: CanonicalUuid,
) {
    let first = unwritten_hunk(author);
    let mut advanced = first.clone();
    advanced.head.next.offset += 64;
    let equivalent = contribute(
        bus,
        epoch,
        root,
        lane.clone(),
        contribution_body(
            design,
            RoomContributionKind::charge(1),
            "Same transcript bytes twice.",
            vec![
                RoomSource::Transcript(first),
                RoomSource::Transcript(advanced),
            ],
        ),
    );
    assert_eq!(
        refusal_reason(&equivalent),
        "room::contribution-source-duplicate"
    );

    let visible = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let cited = journal_source(&visible, design);
    let body = contribution_body(
        design,
        RoomContributionKind::charge(0),
        "Unwitnessed citation.",
        vec![cited],
    );
    let heads = at(&visible, design);
    let command = fixture_command(
        epoch,
        lane.clone(),
        CommandKind::RoomContribute,
        vec![statement_witness(heads), workflow_witness(heads)],
        &body,
    );
    let Intent::RoomContribute(request) = command.body() else {
        panic!("fixture command carries a contribution");
    };
    let store = artifacts(root);
    let tape = root.join("tape");
    let unwitnessed = room_contribute(
        bus,
        &command,
        request,
        Some(SourceOwners {
            tape: &tape,
            artifacts: &store,
            git: &root.join("repo"),
        }),
        None,
    );
    assert_eq!(
        refusal_reason(&unwitnessed),
        "room::contribution-source-witness"
    );
}

/// A hunk of a transcript no tape holds.
fn unwritten_hunk(lane: CanonicalUuid) -> TranscriptHunk {
    TranscriptHunk {
        head: wire::TranscriptSourceHead {
            lane,
            vendor: "omp".parse().expect("fixture vendor"),
            stream: "session".to_owned(),
            source: uuid(91_001),
            next: wire::body::TranscriptPosition {
                member: 1,
                offset: 64,
            },
        },
        locus: wire::MessageLocus {
            member: 1,
            offset: 0,
            length: 64,
            event: 0,
        },
        text: wire::rooms::TextRange { start: 0, end: 6 },
    }
}

#[test]
fn the_journal_admits_only_a_lane_own_bounded_contribution_to_an_open_design() {
    let (root, bus, epoch, seats) = fixture("design-contribution-journal", 2);
    let design = add_controller(
        &bus,
        epoch,
        "Journal design",
        wire::TaskIntent::Design,
        None,
    );
    let plan = add_plan(&bus, epoch, "Journal plan");
    let author = seats[0].lane;
    let minted = std::cell::Cell::new(1_790_000_000_000_001);
    let head = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &design)
        .statement
        .position;
    let commit = |actor: Actor, room: &TaskHandle, text: &str, sources: Vec<RoomSource>| {
        let room = room.born().expect("fixture task has a birth");
        let publication = wire::rooms::RoomAuthorityStamp::for_micros(minted.get())
            .expect("fixture publication identity");
        minted.set(minted.get() + 1);
        let mut write = JournalWrite::new(
            actor,
            "fixture:contribution".parse().expect("fixture origin"),
            Instant::new(jiff::Timestamp::now()).expect("fixture instant"),
            Producer::CommandDoor,
        );
        write
            .fact(
                RoomContributed {
                    room,
                    publication,
                    lane: author,
                    contribution: RoomContributionKind::charge(0),
                    item: None,
                    statement: head,
                    text: text.to_owned(),
                    sources,
                },
                FactCause::Root,
            )
            .expect("fixture contribution builds");
        bus.commit(write)
            .expect("fixture contribution reaches writer")
    };
    let refused = |outcome: CommitOutcome| match outcome {
        CommitOutcome::Refused(refusal) => refusal.reason().to_owned(),
        other => panic!("the journal admitted an unsound contribution: {other:?}"),
    };
    let ahead = RoomSource::Journal {
        fact: wire::SequenceCode::new(1_000_000),
    };
    for outcome in [
        commit(
            Actor::TEST_PRINCIPAL,
            &design,
            "Operator-authored.",
            Vec::new(),
        ),
        commit(
            Actor::Lane(seats[1].lane),
            &design,
            "Another lane.",
            Vec::new(),
        ),
        commit(Actor::Lane(author), &design, "   ", Vec::new()),
        commit(
            Actor::Lane(author),
            &design,
            "Cites the future.",
            vec![ahead],
        ),
        commit(Actor::Lane(author), &plan, "Not a design.", Vec::new()),
    ] {
        assert_eq!(refused(outcome), journal::ROOM_AUTHORITY_REFUSED);
    }
    assert_eq!(
        refused(commit(Actor::Lane(author), &design, "Sound.", Vec::new())),
        journal::ROOM_AUTHORITY_REFUSED,
        "a raw contribution without its successful command receipt is refused"
    );
    assert_committed(conclude(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &design,
        done_conclusion(),
    ));
    assert_eq!(
        refused(commit(
            Actor::Lane(author),
            &design,
            "Too late.",
            Vec::new()
        )),
        journal::ROOM_AUTHORITY_REFUSED,
        "a concluded Design accepts no contribution at commit"
    );
    clean(&root, bus);
}

#[test]
fn verification_binds_a_contribution_fact_to_its_successful_answer() {
    let (root, bus, epoch, seats) = fixture("design-contribution-verification", 1);
    let design = add_controller(
        &bus,
        epoch,
        "Verified contribution",
        wire::TaskIntent::Design,
        None,
    );
    let answer = contributed(contribute(
        &bus,
        epoch,
        root.as_ref(),
        Actor::Lane(seats[0].lane),
        contribution_body(
            &design,
            RoomContributionKind::charge(1),
            "The receipt proves this record.",
            Vec::new(),
        ),
    ));
    let replacement = wire::rooms::RoomAuthorityStamp::for_micros(1_790_000_000_999_999)
        .expect("replacement publication identity");
    assert_ne!(answer.publication, replacement);
    bus.shutdown().expect("fixture writer stops");

    let authority = root.as_ref().join("authority.sqlite3");
    let connection = rusqlite::Connection::open(&authority).expect("fixture authority opens");
    connection
        .execute(
            "UPDATE command_receipts SET response = CAST(json_set(\
             CAST(response AS TEXT), '$.outcome.body.publication', ?1, \
             '$.outcome.body.source.publication', ?1) AS BLOB) \
             WHERE command_kind = 'room::contribute'",
            [replacement.to_string()],
        )
        .expect("plant mismatched contribution answer");
    drop(connection);

    let journal = journal::Journal::open(&authority).expect("fixture authority reopens");
    let failure = journal
        .verify()
        .expect_err("a mismatched contribution answer must fail verification");
    assert!(
        failure.violations().iter().any(|violation| {
            violation.class == journal::ViolationClass::Receipt
                && violation
                    .detail
                    .contains("contribution fact does not match")
        }),
        "semantic contribution corruption was not reported: {failure}"
    );
    drop(journal);
    fs::remove_dir_all(root.as_ref()).expect("fixture cleanup");
}

/// A repository under `root/repo` holding `spice/src/x.rs` at two commits:
/// five lines at the older, three at HEAD. Answers the repository and both
/// commit ids, older first.
fn source_repository(root: &Path) -> (spice::repository::Repository, String, String) {
    let repository = root.join("repo");
    fs::create_dir_all(repository.join("spice/src")).expect("fixture source directory exists");
    crate::test_repository::initialize(&repository);
    let [older, head] = ["one\ntwo\nthree\nfour\nfive\n", "one\ntwo\nthree"].map(|text| {
        fs::write(repository.join("spice/src/x.rs"), text).expect("fixture source writes");
        crate::test_repository::stage(&repository, &["spice/src/x.rs"]);
        crate::test_repository::commit(&repository, "fixture source")
    });
    let discovered =
        spice::repository::Repository::discover(&repository).expect("fixture repository resolves");
    (discovered, older, head)
}

/// The Git link a cite answered, or why not.
fn git_link(cited: Result<RoomSource, Outcome<CommandAnswer>>) -> wire::rooms::GitSource {
    match cited {
        Ok(RoomSource::Git(source)) => source,
        other => panic!("expected repository source, got {other:?}"),
    }
}

/// Each `--cite` key resolves by its form against the board the command
/// reads: a task by handle or stamp to its statement head fact, a stamp no
/// task bears to a contribution in this room, a journal code to that fact, a
/// digest to an artifact, and a key holding `/`, `.` or `:` to repository
/// source at HEAD or a named commit; any other key refuses rather than
/// guessing.
#[test]
fn cite_keys_resolve_by_form_against_the_board() {
    let (root, bus, epoch, seats) = fixture("design-contribution-cite", 1);
    let (repository, older, head) = source_repository(root.as_ref());
    let design = add_controller(&bus, epoch, "Cited design", wire::TaskIntent::Design, None);
    let born = design.born().expect("fixture Design has a birth");
    let publication = contributed(contribute(
        &bus,
        epoch,
        root.as_ref(),
        Actor::Lane(seats[0].lane),
        contribution_body(
            &design,
            RoomContributionKind::charge(1),
            "An opinion to cite.",
            Vec::new(),
        ),
    ))
    .publication;
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let cite = |key: &str| crate::board_command::cite(&visible, &born, key, &repository);
    let statement = RoomSource::Journal {
        fact: wire::SequenceCode::new(at(&visible, &design).statement.position.get()),
    };
    let digest = "ab".repeat(32);

    assert_eq!(cite(design.as_str()).ok(), Some(statement.clone()));
    assert_eq!(cite(&born.to_string()).ok(), Some(statement));
    assert_eq!(
        cite(&publication.to_string()).ok(),
        Some(RoomSource::Contribution {
            room: born.clone(),
            publication,
        }),
        "a stamp no task bears cites a contribution in this room"
    );
    assert_eq!(
        cite("b").ok(),
        Some(RoomSource::Journal {
            fact: "b".parse().expect("journal code"),
        })
    );
    assert_eq!(
        cite(&digest).ok(),
        Some(RoomSource::Artifact {
            digest: digest.parse().expect("artifact digest"),
        })
    );

    let at_head = git_link(cite("spice/src/x.rs:2-3"));
    assert_eq!(
        (at_head.commit.as_str(), at_head.path.as_str()),
        (head.as_str(), "spice/src/x.rs"),
        "a bare path reads at HEAD"
    );
    assert_eq!(
        at_head.lines.map(|lines| (lines.first(), lines.last())),
        Some((2, 3))
    );
    let pinned = git_link(cite(&format!("{older}:spice/src/x.rs:5")));
    assert_eq!(
        pinned.commit.as_str(),
        older,
        "a named commit pins the read"
    );
    assert_ne!(
        pinned.blob, at_head.blob,
        "the blob is the one the commit holds"
    );
    assert_eq!(
        git_link(cite(&format!("{head}:spice/src/x.rs"))).lines,
        None
    );
    assert_eq!(
        git_link(cite(&format!("{}:spice/src/x.rs:5", &older[..9]))),
        pinned,
        "a unique prefix links what the full id links"
    );
    match cite("deadbeef0:spice/src/x.rs") {
        Err(Outcome::Refusal(refusal)) => {
            assert_eq!(refusal.reason(), "room::contribution-cite-unknown");
            assert!(
                refusal
                    .diagnostic()
                    .and_then(|diagnostic| diagnostic.detail())
                    .is_some_and(|detail| detail.contains("beginning deadbeef0")),
                "{refusal:?}"
            );
        }
        other => panic!("an absent prefix refuses, got {other:?}"),
    }
    for (key, reason) in [
        ("spice/src/x.rs:4", "room::contribution-cite-unknown"),
        ("spice/src/absent.rs", "room::contribution-cite-unknown"),
        (
            format!("{}:spice/src/x.rs", "0".repeat(40)).as_str(),
            "room::contribution-cite-unknown",
        ),
        ("spice/src/x.rs:3-2", "room::contribution-cite-invalid"),
        ("spice/src/x.rs:0", "room::contribution-cite-invalid"),
        ("not a key", "room::contribution-cite-invalid"),
    ] {
        let refusal = cite(key).expect_err("the key refuses");
        assert_eq!(refusal_reason(&refusal), reason, "{key}");
    }
    clean(&root, bus);
}

/// A settled task's handle and bare stamp both cite its statement head
/// through the cited-task read `room contribute` uses: the board joined from
/// the cited keys, which holds the task whether or not it is open.
#[test]
fn a_settled_task_cites_its_statement_through_the_cited_task_read() {
    let (root, bus, epoch, seats) = fixture("design-contribution-cite-settled", 1);
    let (repository, _older, _head) = source_repository(root.as_ref());
    let lane = Actor::Lane(seats[0].lane);
    let settled = todo_task(&bus, epoch, "CITE-settled1", "Settled work");
    claim_and_complete_task(&bus, epoch, &seats[0], &lane, &settled);
    let design = add_controller(&bus, epoch, "Citing design", wire::TaskIntent::Design, None);
    let room = design.born().expect("fixture Design has a birth");
    let stamp = settled
        .born()
        .expect("fixture task has a birth")
        .to_string();
    let keys = [settled.as_str(), stamp.as_str()];
    let Outcome::Reply(Reply {
        body: CommandAnswer::Board(cited),
        ..
    }) = issue(
        &bus,
        epoch,
        lane,
        CommandKind::BoardRead,
        Vec::new(),
        &BoardRead::Tasks {
            tasks: crate::board_command::cited_tasks(keys).collect(),
            dependencies: false,
            claims: false,
            history: false,
        },
    )
    else {
        panic!("the cited-task read did not reply");
    };
    assert!(at(&cited, &settled).workflow.body.status.is_completed());
    let statement = RoomSource::Journal {
        fact: wire::SequenceCode::new(at(&cited, &settled).statement.position.get()),
    };
    for key in keys {
        assert_eq!(
            crate::board_command::cite(&cited, &room, key, &repository).ok(),
            Some(statement.clone()),
            "{key}"
        );
    }
    clean(&root, bus);
}

/// The resident re-proves cited repository source against the object store
/// before admitting it, refuses a link whose blob is not the commit's, and
/// reads the link back as available only while the store still holds it.
#[test]
fn repository_source_is_proven_at_admission_and_on_read() {
    let (root, bus, epoch, seats) = fixture("design-contribution-git", 1);
    let (repository, _older, _head) = source_repository(root.as_ref());
    let design = add_controller(
        &bus,
        epoch,
        "Sourced design",
        wire::TaskIntent::Design,
        None,
    );
    let born = design.born().expect("fixture Design has a birth");
    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let cited = git_link(crate::board_command::cite(
        &visible,
        &born,
        "spice/src/x.rs:1-2",
        &repository,
    ));
    let lane = Actor::Lane(seats[0].lane);
    let charge = |source: &wire::rooms::GitSource| {
        contribution_body(
            &design,
            RoomContributionKind::charge(2),
            "The source says so.",
            vec![RoomSource::Git(source.clone())],
        )
    };
    let forged = wire::rooms::GitSource {
        blob: cited.commit.clone(),
        ..cited.clone()
    };
    assert_eq!(
        refusal_reason(&contribute(
            &bus,
            epoch,
            root.as_ref(),
            lane.clone(),
            charge(&forged)
        )),
        "room::contribution-source-unavailable"
    );
    contributed(contribute(&bus, epoch, root.as_ref(), lane, charge(&cited)));
    let sources = |page: Value| {
        page["page"]["rows"]
            .as_array()
            .expect("history rows")
            .iter()
            .map(|row| row["content"]["sources"].clone())
            .find(|sources| {
                sources
                    .as_array()
                    .is_some_and(|sources| !sources.is_empty())
            })
            .expect("the contribution row lists its source")
    };
    let source = serde_json::to_value(RoomSource::Git(cited)).expect("source encodes");
    assert_eq!(
        sources(history(root.as_ref(), root.as_ref(), &[design.as_str()])),
        json!([{ "state": "available", "source": source }])
    );
    fs::remove_dir_all(root.as_ref().join("repo")).expect("fixture repository is removed");
    assert_eq!(
        sources(history(root.as_ref(), root.as_ref(), &[design.as_str()])),
        json!([{
            "state": "unavailable",
            "source": source,
            "unavailable": { "reason": "unreadable" },
        }])
    );
    clean(&root, bus);
}
