use super::*;

fn detail(bus: &Bus, epoch: CanonicalUuid, task: wire::TaskStamp) -> Outcome<CommandAnswer> {
    issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDetail,
        Vec::new(),
        &wire::intent::TaskDetail { task },
    )
}

fn detail_with_repository(
    bus: &Bus,
    repository: &Repository,
    epoch: CanonicalUuid,
    task: wire::TaskStamp,
) -> Outcome<CommandAnswer> {
    let command = fixture_command(
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDetail,
        Vec::new(),
        &wire::intent::TaskDetail { task },
    );
    dispatch_with_repository(
        bus,
        &command,
        Some(host(repository)),
        &board::NextConfig::default(),
        None,
    )
    .expect("board command owns task detail")
}

#[test]
fn task_show_separates_current_context_and_bounded_history() {
    let (root, bus, epoch, _) = fixture("task-show-bounded-sections", 0);
    let task = add_born(
        &bus,
        epoch,
        stamp("task-show-bounded-sections"),
        "Bound one task read",
    );
    let answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&answer, &task);
    let current = task_show_bounded(
        &answer,
        row,
        &[],
        TaskShowHistory::default(),
        None,
        TaskShowLocal::default(),
        &TaskShowOptions {
            full: false,
            context: false,
            history: false,
            limit: None,
            max_bytes: None,
            max_text: None,
            after: None,
        },
    )
    .expect("current task detail has no cursor")
    .text;
    for required in [
        "handle ",
        "title Bound one task read",
        "topic ",
        "priority -",
        "description: -",
        "phase ",
        "status ",
        "claim ",
        "requirements:",
        "terms: 1 item",
        "history: -",
        "depends:",
        "artifacts:",
        "next: ",
    ] {
        assert!(current.contains(required), "{current}");
    }
    assert!(!current.contains("questions:"), "{current}");
    for historical in [
        "origin ",
        "creation_context ",
        "claim_context ",
        "git_context ",
    ] {
        assert!(!current.contains(historical), "{current}");
    }

    let steps = (1..=205)
        .map(|index| wire::intent::TaskDetailStep {
            position: Seq::new(1_000 + index).expect("fixture position"),
            at: "2026-10-02T00:00:00.000Z".parse().expect("fixture instant"),
            author: Actor::TEST_PRINCIPAL,
            step: wire::intent::TaskStep::Done {
                phase: "todo".parse().expect("fixture phase"),
                validation: format!("evidence-{index}"),
                disposition: None,
            },
        })
        .collect::<Vec<_>>();
    let page = task_show_bounded(
        &answer,
        row,
        &[],
        TaskShowHistory {
            steps: &steps,
            ..TaskShowHistory::default()
        },
        None,
        TaskShowLocal::default(),
        &TaskShowOptions {
            full: false,
            context: false,
            history: true,
            limit: None,
            max_bytes: None,
            max_text: None,
            after: None,
        },
    )
    .expect("first history page has no cursor");
    let typed = page.history.expect("a history read carries typed steps");
    let history = page.text;
    assert_eq!(history.matches("    evidence-").count(), 20, "{history}");
    assert_eq!(
        typed,
        steps.iter().rev().take(20).cloned().collect::<Vec<_>>(),
        "typed steps are the admitted text page, newest first"
    );
    assert!(history.contains("continue: spice task show"), "{history}");
    assert!(history.contains("--after"), "{history}");
    assert!(!history.contains("history: "), "{history}");
    clean(&root, bus);
}

#[test]
fn task_show_context_page_clips_oversized_origin_context() {
    let (root, bus, epoch, _) = fixture("task-show-context-clip", 0);
    let task = add_born(
        &bus,
        epoch,
        stamp("task-show-context-clip"),
        "Clip one origin context",
    );
    let answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&answer, &task);
    let oversized = "x".repeat(5_000);
    let context = task_show_bounded(
        &answer,
        row,
        &[],
        TaskShowHistory::default(),
        Some(&oversized),
        TaskShowLocal::default(),
        &TaskShowOptions {
            full: false,
            context: true,
            history: false,
            limit: None,
            max_bytes: None,
            max_text: None,
            after: None,
        },
    )
    .expect("context page has no cursor")
    .text;
    assert!(context.contains("spice doc ledger"), "{context}");
    assert!(!context.contains(&oversized), "{context}");
    clean(&root, bus);
}

#[test]
fn task_show_counts_the_history_it_collapses() {
    let (root, bus, epoch, _) = fixture("task-show-collapsed-history", 0);
    let task = add_born(
        &bus,
        epoch,
        stamp("task-show-collapsed-history"),
        "Count one task's history",
    );
    let answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let row = at(&answer, &task);
    let steps = (1..=3)
        .map(|index| wire::intent::TaskDetailStep {
            position: Seq::new(1_000 + index).expect("fixture position"),
            at: "2026-10-02T00:00:00.000Z".parse().expect("fixture instant"),
            author: Actor::TEST_PRINCIPAL,
            step: wire::intent::TaskStep::Done {
                phase: "todo".parse().expect("fixture phase"),
                validation: format!("evidence-{index}"),
                disposition: None,
            },
        })
        .collect::<Vec<_>>();
    let show = |full: bool| {
        task_show_bounded(
            &answer,
            row,
            &[],
            TaskShowHistory {
                steps: &steps,
                ..TaskShowHistory::default()
            },
            None,
            TaskShowLocal::default(),
            &TaskShowOptions {
                full,
                context: false,
                history: false,
                limit: None,
                max_bytes: None,
                max_text: None,
                after: None,
            },
        )
        .expect("unpaged detail has no cursor")
        .text
    };
    let collapsed = show(false);
    assert!(
        collapsed.contains(&format!("\nhistory: 3 steps spice://task/{task}/history\n")),
        "{collapsed}"
    );
    assert!(!collapsed.contains("evidence-"), "{collapsed}");
    let full = show(true);
    assert!(!full.contains("history: "), "{full}");
    clean(&root, bus);
}

#[test]
fn task_show_labels_design_items_as_questions() {
    let (root, bus, epoch, _) = fixture("task-show-design-questions", 0);
    let task = add_born(
        &bus,
        epoch,
        stamp("task-show-design-questions"),
        "Question one Design",
    );
    let answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let mut design = at(&answer, &task).clone();
    design.statement.body.intent = Some(wire::TaskIntent::Design);
    design.statement.body.term = vec!["Which boundary remains open?".to_owned()];
    let maturity = wire::rooms::DesignMaturity::default();
    let show = |full: bool| {
        task_show_bounded(
            &answer,
            &design,
            &[],
            TaskShowHistory::default(),
            None,
            TaskShowLocal {
                maturity: Some(&maturity),
                room: None,
            },
            &TaskShowOptions {
                full,
                context: false,
                history: false,
                limit: None,
                max_bytes: None,
                max_text: None,
                after: None,
            },
        )
        .expect("Design detail has no cursor")
        .text
    };
    let handle = &design.statement.body.handle;
    let collapsed = show(false);
    assert!(
        collapsed.contains(&format!(
            "\nquestions: 1 item spice://task/{handle}/terms\n"
        )),
        "{collapsed}"
    );
    assert!(!collapsed.contains("Which boundary"), "{collapsed}");
    let full = show(true);
    let item = wire::rooms::StatementItem::of("Which boundary remains open?");
    assert!(
        full.contains(&format!(
            "questions:\n  - {} Which boundary remains open?",
            item.handle(&[])
        )),
        "{full}"
    );
    assert!(
        full.contains("term evidence regime contested")
            && full.contains("quiet · 0+/0- net +0")
            && full.contains("term total +0; agreement n/a; cancellation 0; zero-net terms 1"),
        "{full}"
    );
    for text in [collapsed, full] {
        assert!(!text.contains("terms:"), "{text}");
    }
    clean(&root, bus);
}

/// Without `--full` the statement's prose collapses to its count lines, each
/// naming the resource that expands exactly it; `--full` prints it whole: a
/// description past the text limit keeps every line, and context does not
/// repeat statement fields.
#[test]
fn task_show_collapses_prose_and_full_prints_it_whole() {
    let (root, bus, epoch, _) = fixture("task-show-authored-statement", 0);
    let task = add_born(
        &bus,
        epoch,
        stamp("task-show-authored-statement"),
        "Read the statement whole",
    );
    let answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let mut row = at(&answer, &task).clone();
    let description = format!(
        "First paragraph {}\n\n  indented second paragraph",
        "x".repeat(300)
    );
    row.statement.body.description = Some(description);
    row.statement.body.priority = Some(wire::TaskPriority::High);
    row.statement.body.term = vec!["One".to_owned(), "Two".to_owned()];
    let show = |full: bool, context: bool| {
        task_show_bounded(
            &answer,
            &row,
            &[],
            TaskShowHistory::default(),
            None,
            TaskShowLocal::default(),
            &TaskShowOptions {
                full,
                context,
                history: false,
                limit: None,
                max_bytes: None,
                max_text: None,
                after: None,
            },
        )
        .expect("task detail has no cursor")
        .text
    };
    let handle = &row.statement.body.handle;
    let collapsed = show(false, false);
    assert!(
        collapsed.contains(&format!(
            "\ndescription: 3 lines spice://task/{handle}/description\n\
             terms: 2 items spice://task/{handle}/terms\n\
             history: -\n"
        )),
        "{collapsed}"
    );
    assert!(!collapsed.contains("First paragraph"), "{collapsed}");
    let indented = format!(
        "  First paragraph {}\n\n    indented second paragraph",
        "x".repeat(300)
    );
    let current = show(true, false);
    assert!(current.contains("priority high"), "{current}");
    let items = wire::StatementItem::each(&row.statement.body.term).collect::<Vec<_>>();
    assert!(
        current.contains(&format!(
            "description:\n{indented}\nterms:\n  - {} One\n  - {} Two\ndepends:",
            items[0].handle(&items),
            items[1].handle(&items),
        )),
        "{current}"
    );
    assert!(!current.contains("history: "), "{current}");
    let context = show(true, true);
    assert_eq!(
        context.matches("indented second paragraph").count(),
        1,
        "{context}"
    );
    clean(&root, bus);
}

#[test]
fn task_show_full_surfaces_author_validation_only_in_review() {
    let (root, bus, epoch, _) = fixture("task-show-review-validation", 0);
    let task = add_born(
        &bus,
        epoch,
        stamp("task-show-review-validation"),
        "Show review evidence",
    );
    let answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let mut row = at(&answer, &task).clone();
    row.workflow.body.phase = wire::Phase::Review;
    let steps = [wire::intent::TaskDetailStep {
        position: Seq::new(1_001).expect("fixture position"),
        at: "2026-10-02T00:00:00.000Z".parse().expect("fixture instant"),
        author: Actor::TEST_PRINCIPAL,
        step: wire::intent::TaskStep::Done {
            phase: wire::Phase::Todo,
            validation: "gate 20/20\nartifact://proof-one".to_owned(),
            disposition: None,
        },
    }];
    let show = |full| {
        task_show_bounded(
            &answer,
            &row,
            &[],
            TaskShowHistory {
                steps: &steps,
                ..TaskShowHistory::default()
            },
            None,
            TaskShowLocal::default(),
            &TaskShowOptions {
                full,
                context: false,
                history: false,
                limit: None,
                max_bytes: None,
                max_text: None,
                after: None,
            },
        )
        .expect("task detail has no cursor")
        .text
    };

    let bounded = show(false);
    assert!(!bounded.contains("gate 20/20"), "{bounded}");
    assert!(!bounded.contains("artifact://proof-one"), "{bounded}");

    let full = show(true);
    assert!(
        full.contains("validation todo:\n  gate 20/20\n  artifact://proof-one"),
        "{full}"
    );
    clean(&root, bus);
}

#[test]
fn task_show_context_uses_aggregate_detail_budget_and_exact_continuation() {
    let (root, bus, epoch, _) = fixture("task-show-context-budget", 0);
    let task = add_born(
        &bus,
        epoch,
        stamp("task-show-context-budget"),
        "Budget context rows",
    );
    let answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let bounded = task_show_bounded(
        &answer,
        at(&answer, &task),
        &[],
        TaskShowHistory::default(),
        None,
        TaskShowLocal::default(),
        &TaskShowOptions {
            full: false,
            context: true,
            history: false,
            limit: None,
            max_bytes: Some(1),
            max_text: None,
            after: None,
        },
    )
    .expect("first context page has no cursor")
    .text;
    assert!(
        bounded.contains("continue: spice task show")
            && bounded.contains("--context")
            && bounded.contains("--after context:0"),
        "{bounded}"
    );
    clean(&root, bus);
}

#[test]
fn task_detail_carries_browser_renderable_artifacts_without_text_sidecars() {
    let (root, bus, epoch, _) = fixture("task-detail-media", 0);
    let born = stamp("task-detail-media");
    add_born(&bus, epoch, born.clone(), "Inspect task media");
    let repository = Repository::discover(&*root).expect("fixture repository resolves");
    let state = repository
        .admit_state()
        .expect("fixture repository state admits");
    fs::create_dir_all(state.artifacts()).expect("artifact directory exists");
    fs::create_dir_all(state.locks()).expect("artifact lock directory exists");
    let store = artifact::Store::new(state.artifacts(), state.artifacts_lock());
    let created_at = "2030-04-12T18:44:09.271Z".parse().expect("fixture instant");
    store
        .add_bytes(artifact::AddBytesRequest {
            task: &born,
            bytes: b"\x89PNG\r\n\x1a\n",
            name: "operator-capture.png",
            media_type: Some(artifact::MediaType::IMAGE_PNG),
            retention: artifact::Retention::Permanent,
            created_at,
        })
        .expect("image artifact publishes");
    store
        .add_bytes(artifact::AddBytesRequest {
            task: &born,
            bytes: b"diagnostic text",
            name: "diagnostic.txt",
            media_type: Some(artifact::MediaType::TEXT_PLAIN),
            retention: artifact::Retention::Permanent,
            created_at,
        })
        .expect("text artifact publishes");

    let Outcome::Reply(Reply {
        body: CommandAnswer::TaskDetail(answer),
        ..
    }) = detail_with_repository(&bus, &repository, epoch, born)
    else {
        panic!("task detail did not return media")
    };
    assert_eq!(answer.media.len(), 1);
    assert_eq!(answer.media[0].name, "operator-capture.png");
    assert_eq!(answer.media[0].content_type, "image/png");
    assert_eq!(
        answer.media[0]
            .decoded_bytes()
            .expect("projected media remains admitted"),
        b"\x89PNG\r\n\x1a\n"
    );

    clean(&root, bus);
}

#[test]
fn task_detail_presents_description_quotes_in_order_and_refuses_an_unknown_source() {
    let (root, bus, epoch, _) = fixture("task-detail-quotes", 0);
    super::origin::seed_steering_directive(&bus, "1kLGp2NQx", uuid(16_201));
    let born = stamp("task-detail-quotes");
    let task = add_born(&bus, epoch, born.clone(), "Carry quoted sources");
    let steering = |key: &str| wire::QuoteTarget::Steering {
        key: key.to_owned(),
        digest: wire::QuoteTarget::text_digest(
            "Preserve this directive\nverbatim across hydration.",
        ),
    };
    let (unknown, known) = (steering("1kLGp2NRx"), steering("1kLGp2NQx"));
    assert_committed(super::edit::correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            description: Some(format!(
                "{}\n\n{}\n\nWhy it matters.",
                unknown.markdown(),
                known.markdown()
            )),
            ..only(&task)
        },
    ));
    let repository = Repository::discover(&*root).expect("fixture repository resolves");
    let state = repository
        .admit_state()
        .expect("fixture repository state admits");
    fs::create_dir_all(state.artifacts()).expect("artifact directory exists");
    fs::create_dir_all(state.locks()).expect("artifact lock directory exists");

    let Outcome::Reply(Reply {
        body: CommandAnswer::TaskDetail(answer),
        ..
    }) = detail_with_repository(&bus, &repository, epoch, born)
    else {
        panic!("task detail did not answer")
    };
    let [first, second] = answer.description_quotes.as_slice() else {
        panic!("{:?}", answer.description_quotes)
    };
    assert_eq!(
        *first,
        wire::QuotePresentation::Unavailable {
            target: unknown.to_string(),
            reason: wire::QuoteUnavailable::Unknown,
        }
    );
    assert!(
        matches!(
            second,
            wire::QuotePresentation::Resolved { target, text, .. }
                if *target == known.to_string() && text.starts_with("Preserve this directive")
        ),
        "{second:?}"
    );

    clean(&root, bus);
}

#[test]
fn task_detail_presents_the_steering_origin_directive_with_its_quotes() {
    let (root, bus, epoch, _) = fixture("task-detail-origin", 0);
    super::origin::seed_steering_directive(&bus, "1kLGp2NKx", uuid(16_203));
    let steering = |key: &str| wire::QuoteTarget::Steering {
        key: key.to_owned(),
        digest: wire::QuoteTarget::text_digest(super::origin::DIRECTIVE_TEXT),
    };
    let (known, unknown) = (steering("1kLGp2NKx"), steering("1kLGp2NRx"));
    let text = format!(
        "Carry these forward.\n\n{}\n\n{}",
        known.markdown(),
        unknown.markdown()
    );
    super::origin::send_directive(
        &bus,
        Actor::TEST_PRINCIPAL,
        "1kLGp2NJx",
        uuid(16_201),
        uuid(16_202),
        &text,
    );
    let born = stamp("task-detail-origin");
    super::origin::add_task_noted_under_a_second_ask(&bus, epoch, &born);
    let repository = Repository::discover(&*root).expect("fixture repository resolves");
    let state = repository
        .admit_state()
        .expect("fixture repository state admits");
    fs::create_dir_all(state.artifacts()).expect("artifact directory exists");
    fs::create_dir_all(state.locks()).expect("artifact lock directory exists");

    let Outcome::Reply(Reply {
        body: CommandAnswer::TaskDetail(answer),
        ..
    }) = detail_with_repository(&bus, &repository, epoch, born)
    else {
        panic!("task detail did not answer")
    };
    let origin = answer
        .origin
        .expect("a steering origin presents its directive");
    assert_eq!(
        origin.published,
        "2030-04-12T18:44:09.271Z"
            .parse::<Instant>()
            .expect("fixture publication instant")
    );
    let wire::QuotePresentation::Resolved {
        target,
        text: shown,
        attribution,
        quotes,
        ..
    } = origin.directive
    else {
        panic!("the origin directive resolves: {:?}", origin.directive)
    };
    let own = wire::QuoteTarget::Steering {
        key: "1kLGp2NJx".to_owned(),
        digest: wire::QuoteTarget::text_digest(&text),
    };
    assert_eq!(target, own.to_string());
    assert_eq!(shown, text, "the whole directive text, never a preview");
    assert_eq!(
        attribution.author,
        wire::QuoteAuthor::Directive {
            actor: Actor::TEST_PRINCIPAL
        }
    );
    let [nested, refused] = quotes.as_slice() else {
        panic!("{quotes:?}")
    };
    assert!(
        matches!(
            nested,
            wire::QuotePresentation::Resolved { target, text, .. }
                if *target == known.to_string() && text.starts_with("Preserve this directive")
        ),
        "{nested:?}"
    );
    assert_eq!(
        *refused,
        wire::QuotePresentation::Unavailable {
            target: unknown.to_string(),
            reason: wire::QuoteUnavailable::Unknown,
        }
    );

    clean(&root, bus);
}

#[test]
fn task_detail_refuses_an_unknown_durable_identity() {
    let (root, bus, epoch, _) = fixture("task-detail-unknown", 0);
    let outcome = detail(&bus, epoch, stamp("missing-detail"));
    assert!(matches!(
        &outcome,
        Outcome::Refusal(refusal) if refusal.reason() == "task::detail-unknown"
    ));
    clean(&root, bus);
}

#[test]
fn task_detail_next_advances_a_nonfinal_review_phase() {
    let (root, bus, epoch, seats) = fixture("detail-nonfinal-review", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DETAIL-nonfinal",
        "Advance a nonfinal phase",
        Some("review,todo"),
    );
    capture_for(&bus, epoch, &seats[0], &task);
    let held = board(&bus, epoch, lane.clone());
    let row = at(&held, &task);
    let rendered = task_show(&held, row, &[]);
    assert_eq!(
        rendered.lines().last(),
        Some(
            format!(
                "next: spice task review {task} --finding clean --note \"description current; ...\""
            )
            .as_str()
        )
    );
    assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskReview,
        progress_witnesses(&held, row, seats[0].lane, seats[0].occupant, true)
            .expect("held review admits a verdict"),
        &TaskReview {
            note: Some("phase contract proved".to_owned()),
            ..verdict(&task, "clean")
        },
    ));
    let advanced = board(&bus, epoch, lane);
    assert_eq!(at(&advanced, &task).workflow.body.phase.as_str(), "todo");
    clean(&root, bus);
}

#[test]
fn task_detail_keeps_latest_review_authority_without_repository_context() {
    let (root, bus, epoch, seats) = fixture("detail-review-authority", 2);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let reviewer = Actor::Lane(seats[0].lane);
    let implementer = Actor::Lane(seats[1].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DETAIL-review",
        "Review authority across phase boundaries",
        Some("review,todo"),
    );
    let read_detail = |task: &TaskHandle| {
        let Outcome::Reply(Reply {
            body: CommandAnswer::TaskDetail(answer),
            ..
        }) = detail(&bus, epoch, task.born().expect("fixture identity"))
        else {
            panic!("task detail must admit the known identity");
        };
        answer
    };
    assert!(read_detail(&task).review.is_none());
    capture_for(&bus, epoch, &seats[0], &task);
    assert_committed(preparation::review_clean(&bus, epoch, &seats[0], &task));
    let intermediate = read_detail(&task);
    let review = intermediate.review.as_ref().expect("nonterminal review");
    assert_eq!(review.verdict, body::Verdict::Clean);
    assert_eq!(review.status, body::TaskCompletionStatus::Pending);
    assert_eq!(review.reviewer, reviewer);
    assert!(intermediate.task.git_context.is_none());
    let intermediate_position = review.position;

    capture_for(&bus, epoch, &seats[1], &task);
    let held = board(&bus, epoch, implementer.clone());
    assert_committed(issue(
        &bus,
        epoch,
        implementer,
        CommandKind::TaskDone,
        progress_witnesses(
            &held,
            at(&held, &task),
            seats[1].lane,
            seats[1].occupant,
            false,
        )
        .expect("verification completion witnesses"),
        &TaskDone {
            operation: Some(TaskDone::operation_for_claim(
                at(&held, &task)
                    .latest_claim()
                    .map(|held| &held.claim)
                    .expect("held verification")
                    .body
                    .claimed,
            )),
            ..finished(&task, "Verification crossed its accepted boundary.")
        },
    ));
    assert_eq!(
        read_detail(&task)
            .review
            .as_ref()
            .map(|review| review.position),
        Some(intermediate_position),
        "a later phase completion is not another review"
    );
    assert_eq!(
        read_detail(&task)
            .review
            .as_ref()
            .map(|review| review.status),
        Some(body::TaskCompletionStatus::Pending),
        "finishing verification does not make its earlier review final"
    );
    let final_task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DETAIL-final",
        "Preserve a completed clean review",
        Some("review"),
    );
    capture_for(&bus, epoch, &seats[0], &final_task);
    assert_committed(preparation::review_clean(
        &bus,
        epoch,
        &seats[0],
        &final_task,
    ));
    let finished = read_detail(&final_task);
    let final_review = finished.review.as_ref().expect("final clean review");
    assert_eq!(final_review.verdict, body::Verdict::Clean);
    assert_eq!(final_review.status, body::TaskCompletionStatus::Completed);
    assert_eq!(final_review.reviewer, reviewer);
    assert!(finished.task.git_context.is_none());

    let finding = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DETAIL-finding",
        "Preserve a pending finding",
        Some("review"),
    );
    resolution::complete_review_with_finding(&bus, epoch, &seats[0], &reviewer, &finding);
    let returned = read_detail(&finding);
    let finding = returned.review.as_ref().expect("finding review");
    assert_eq!(finding.verdict, body::Verdict::Finding);
    assert_eq!(finding.status, body::TaskCompletionStatus::Pending);
    assert_eq!(finding.reviewer, reviewer);
    assert!(returned.task.git_context.is_none());
    clean(&root, bus);
}

#[test]
fn task_show_names_lanes_by_roster_name_while_exports_keep_bare_actors() {
    let (root, bus, epoch, seats) = fixture("task-show-lane-names", 2);
    let task = add_born(
        &bus,
        epoch,
        stamp("task-show-lane-names"),
        "Name every lane",
    );
    let named = Actor::Lane(seats[0].lane);
    let unnamed = Actor::Lane(seats[1].lane);
    resolution::claim_task(&bus, epoch, &seats[0], &named, &task);
    assert_committed(super::completion_target::set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &task,
        ("todo", 2),
    ));
    resolution::claim_task(&bus, epoch, &seats[1], &unnamed, &task);
    let mut answer = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    answer
        .residency
        .body
        .lanes
        .iter_mut()
        .find(|row| row.lane == seats[0].lane)
        .expect("fixture lane is on the roster")
        .name = Some("Clomp".to_owned());
    let step = |position: u64, author: &Actor| wire::intent::TaskDetailStep {
        position: Seq::new(position).expect("fixture position"),
        at: "2026-10-02T00:00:00.000Z".parse().expect("fixture instant"),
        author: author.clone(),
        step: wire::intent::TaskStep::Done {
            phase: "todo".parse().expect("fixture phase"),
            validation: "evidence".to_owned(),
            disposition: None,
        },
    };
    let steps = [step(1_001, &named), step(1_002, &unnamed)];
    let text = task_show_bounded(
        &answer,
        at(&answer, &task),
        &[],
        TaskShowHistory {
            steps: &steps,
            ..TaskShowHistory::default()
        },
        None,
        TaskShowLocal::default(),
        &TaskShowOptions {
            full: false,
            context: false,
            history: true,
            limit: None,
            max_bytes: None,
            max_text: None,
            after: None,
        },
    )
    .expect("history renders")
    .text;

    assert!(
        text.lines()
            .any(|line| line == format!("claim Clomp {named}, {unnamed}")),
        "every holder in winning order, with no lease expiry: {text}"
    );
    assert!(
        text.contains(&format!("Z Clomp {named} done todo")),
        "{text}"
    );
    assert!(text.contains(&format!("Z {unnamed} done todo")), "{text}");

    let exported =
        crate::board_command::task_step::step_annotation(&steps[0], |stamp| stamp.to_string());
    assert!(
        exported.starts_with("workflow ") && exported.contains(&format!("Z {named} done todo")),
        "{exported}"
    );
    let json = serde_json::to_string(&answer).expect("board answer encodes");
    assert!(!json.contains(&format!("Clomp {named}")), "{json}");
    clean(&root, bus);
}
