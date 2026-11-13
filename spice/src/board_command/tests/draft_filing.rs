use super::*;

fn draft_add(name: &str, intent: Option<wire::TaskIntent>, term: &[&str]) -> TaskAdd {
    TaskAdd {
        topic: Some("/board/test".parse().expect("fixture topic")),
        description: Some(format!("Context for {name}")),
        term: term.iter().map(|item| (*item).to_owned()).collect(),
        origin: Some(fixture_origin()),
        intent,
        draft: true,
        ..adding(
            format!("Prospective {name}"),
            "This work keeps the fixture honest.",
        )
    }
}

fn add_draft(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    name: &str,
    intent: Option<wire::TaskIntent>,
    term: &[&str],
) -> TaskHandle {
    let born = stamp(name);
    let CommandAnswer::Task(answer) = assert_committed(issue(
        bus,
        epoch,
        actor,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born.clone() }],
        &draft_add(name, intent, term),
    )) else {
        panic!("draft creation returned another answer shape");
    };
    let draft = draft_handle(&born);
    assert_eq!(answer.task, draft);
    draft
}

fn draft_handle(born: &wire::TaskStamp) -> TaskHandle {
    TaskHandle::for_phase(
        Some(&"/board/test".parse().expect("fixture topic")),
        born,
        wire::Phase::Draft,
    )
}

fn file_witnesses(heads: &BoardTask) -> Vec<Witness> {
    vec![
        workflow_witness(heads),
        deferral_witness(heads),
        statement_witness(heads),
    ]
}

fn flow_edit_witnesses(heads: &BoardTask) -> Vec<Witness> {
    vec![
        statement_witness(heads),
        workflow_witness(heads),
        completion_witness(heads),
    ]
}

fn file(bus: &Bus, epoch: CanonicalUuid, task: &TaskHandle) -> Outcome<CommandAnswer> {
    let current = board(bus, epoch, Actor::TEST_PRINCIPAL);
    issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskFile,
        file_witnesses(at(&current, task)),
        &TaskFile {
            task: task.clone(),
            topic: None,
            claim: false,
        },
    )
}

fn flow_of(heads: &BoardTask) -> Vec<String> {
    heads
        .workflow
        .body
        .flow
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn listed(answer: &BoardAnswer, all: bool) -> String {
    task_list(
        answer,
        &TaskListOptions {
            all,
            topic: None,
            limit: None,
            status: None,
        },
    )
}

#[test]
fn origin_refusal_offers_the_draft_route_only_to_an_operator_add() {
    let (root, bus, epoch, seats) = fixture("draft-origin-route", 1);
    let lane = Actor::Lane(seats[0].lane);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let facts = fact_count(&bus);
    for (actor, draft, offered) in [
        (Actor::TEST_PRINCIPAL, false, true),
        (lane.clone(), false, false),
        (lane, true, false),
    ] {
        let add = TaskAdd {
            origin: None,
            draft,
            ..draft_add("DRAFT-route001", None, &[])
        };
        let outcome = issue(
            &bus,
            epoch,
            actor.clone(),
            CommandKind::TaskAdd,
            vec![Witness::Absence {
                task: stamp(&add.title),
            }],
            &add,
        );
        assert_eq!(refusal_reason(&outcome), "task::origin-required");
        let Outcome::Refusal(refusal) = outcome else {
            unreachable!("refusal was matched above");
        };
        let explanation = refusal.explanation();
        assert!(explanation.contains("--origin ack:"), "{explanation}");
        assert_eq!(
            explanation.contains("--draft --topic /TOPIC"),
            offered,
            "{actor:?} draft={draft}: {explanation}",
        );
    }
    assert_eq!(fact_count(&bus), facts, "refused creation wrote facts");
    clean(&root, bus);
}

#[test]
fn an_operator_draft_keeps_editable_content_off_the_allocator() {
    let (root, bus, epoch, seats) = fixture("draft-edit", 1);
    let lane = Actor::Lane(seats[0].lane);
    let born = stamp("DRAFT-edit0001");
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born.clone() }],
        &TaskAdd {
            origin: None,
            ..draft_add("DRAFT-edit0001", None, &[])
        },
    ));
    let draft = draft_handle(&born);
    let created = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&created, &draft);
    assert!(heads.workflow.body.is_draft());
    assert_eq!(
        heads.statement.body.provenance.to_string(),
        "operator:draft"
    );
    assert_eq!(allocated(&bus, epoch, &lane), None, "a Draft was offered");

    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        flow_edit_witnesses(heads),
        &TaskEdit {
            title: Some("Revised prospective work".to_owned()),
            term: Some(vec!["The revised criterion holds".to_owned()]),
            intent: Some(wire::TaskIntentEdit::Set {
                intent: wire::TaskIntent::Plan,
            }),
            flow: Some("plan,review".to_owned()),
            topic: Some("/board/identity".parse().expect("fixture topic")),
            ..only(&draft)
        },
    ));
    let revised_handle = TaskHandle::for_phase(
        Some(&"/board/identity".parse().expect("fixture topic")),
        &draft.born().expect("draft handle carries a birth stamp"),
        wire::Phase::Draft,
    );
    let revised = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&revised, &revised_handle);
    assert_eq!(heads.presentation_title, "Revised prospective work");
    assert_eq!(heads.statement.body.term, ["The revised criterion holds"]);
    assert_eq!(heads.statement.body.intent, Some(wire::TaskIntent::Plan));
    assert_eq!(
        heads
            .statement
            .body
            .topic
            .path
            .as_ref()
            .map(wire::Topic::as_str),
        Some("/board/identity")
    );
    assert_eq!(flow_of(heads), ["draft", "plan", "review"]);
    assert!(heads.workflow.body.is_draft());
    assert_eq!(
        allocated(&bus, epoch, &lane),
        None,
        "an edited Draft was offered"
    );
    clean(&root, bus);
}

#[test]
fn an_activated_task_edits_only_its_unstarted_suffix() {
    let (root, bus, epoch, _seats) = fixture("draft-flow-tail", 1);
    let active = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-tail0001",
        "Active",
        Some("todo,review"),
    );
    let heads = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &active).clone();
    let reordered_prefix = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        flow_edit_witnesses(&heads),
        &TaskEdit {
            flow: Some("review,todo".to_owned()),
            ..only(&active)
        },
    );
    assert_eq!(refusal_reason(&reordered_prefix), "task::flow-prefix");

    let removed_review = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        flow_edit_witnesses(&heads),
        &TaskEdit {
            flow: Some("todo".to_owned()),
            ..only(&active)
        },
    );
    assert_eq!(
        refusal_reason(&removed_review),
        "task::flow-review-required"
    );

    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        flow_edit_witnesses(&heads),
        &TaskEdit {
            flow: Some("todo,plan,review".to_owned()),
            ..only(&active)
        },
    ));
    let revised = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        flow_of(at(&revised, &active)),
        ["draft", "todo", "plan", "review"]
    );
    let born = active.born().expect("fixture handle carries a stamp");
    let facts = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("fixture facts remain readable");
    let change = facts
        .facts()
        .iter()
        .rev()
        .find_map(|fact| {
            let edited = fact.fact.read::<body::TaskEdited>()?;
            (edited.task == born).then_some(edited.flow).flatten()
        })
        .expect("route edit records its prior and current routes");
    assert_eq!(
        change
            .previous
            .iter()
            .map(|phase| phase.as_str())
            .collect::<Vec<_>>(),
        ["draft", "todo", "review"]
    );
    assert_eq!(
        change
            .current
            .iter()
            .map(|phase| phase.as_str())
            .collect::<Vec<_>>(),
        ["draft", "todo", "plan", "review"]
    );
    clean(&root, bus);
}

#[test]
fn flow_edits_refuse_advancement_and_staffing_races() {
    let (root, bus, epoch, _seats) = fixture("draft-flow-races", 1);
    let draft = add_draft(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-race0001",
        None,
        &["The routed work is complete"],
    );
    let before_file = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &draft).clone();
    let stale_advancement = flow_edit_witnesses(&before_file);
    assert_committed(file(&bus, epoch, &draft));
    let moved = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        stale_advancement,
        &TaskEdit {
            flow: Some("todo,review".to_owned()),
            ..only(&draft)
        },
    );
    assert_eq!(refusal_reason(&moved), "board::witness-moved");

    let active = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-race0002",
        "Staffed",
        Some("todo,review"),
    );
    let stale_staffing =
        flow_edit_witnesses(at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &active));
    assert_committed(super::completion_target::set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &active,
        ("review", 2),
    ));
    let moved = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        stale_staffing,
        &TaskEdit {
            flow: Some("todo,plan,review".to_owned()),
            ..only(&active)
        },
    );
    assert_eq!(refusal_reason(&moved), "board::witness-moved");

    let before = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &active).clone();
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        flow_edit_witnesses(&before),
        &TaskEdit {
            flow: Some("todo,plan,review".to_owned()),
            ..only(&active)
        },
    ));
    let revised = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &active).clone();
    let plan: wire::Phase = "plan".parse().expect("fixture phase");
    let review: wire::Phase = "review".parse().expect("fixture phase");
    assert_eq!(revised.completion.body.target(&review).get(), 2);
    assert_eq!(revised.completion.body.target(&plan).get(), 1);
    assert_eq!(revised.completion.body.round, before.completion.body.round);
    assert_eq!(
        revised.completion.body.arrivals,
        before.completion.body.arrivals
    );

    assert_committed(super::completion_target::set_target(
        &bus,
        epoch,
        &Actor::TEST_PRINCIPAL,
        &active,
        ("plan", 3),
    ));
    let staffed = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &active).clone();
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        flow_edit_witnesses(&staffed),
        &TaskEdit {
            flow: Some("todo,review".to_owned()),
            ..only(&active)
        },
    ));
    let pruned = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &active).clone();
    assert!(!pruned.completion.body.targets.contains_key(&plan));
    assert_eq!(pruned.completion.body.target(&review).get(), 2);
    clean(&root, bus);
}

#[test]
fn an_active_flow_edit_requires_the_exact_current_holder() {
    let (root, bus, epoch, seats) = fixture("draft-flow-authority", 2);
    let holder = Actor::Lane(seats[0].lane);
    let bystander = Actor::Lane(seats[1].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-auth0001",
        "Held",
        Some("todo,review"),
    );
    claim_task(&bus, epoch, &seats[0], &holder, &task);

    let heads = at(&board(&bus, epoch, bystander.clone()), &task).clone();
    let refused = issue(
        &bus,
        epoch,
        bystander,
        CommandKind::TaskEdit,
        flow_edit_witnesses(&heads),
        &TaskEdit {
            flow: Some("todo,plan,review".to_owned()),
            ..only(&task)
        },
    );
    assert_eq!(refusal_reason(&refused), "task::flow-authority");

    let heads = at(&board(&bus, epoch, holder.clone()), &task).clone();
    let claim = heads
        .held_by(&holder)
        .expect("fixture holder has the current claim");
    let claimed = claim.claim.position;
    let mut expected = flow_edit_witnesses(&heads);
    expected.push(claim.witness(&heads.statement.body.born));
    assert_committed(issue(
        &bus,
        epoch,
        holder,
        CommandKind::TaskEdit,
        expected,
        &TaskEdit {
            flow: Some("todo,plan,review".to_owned()),
            ..only(&task)
        },
    ));
    let revised = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task).clone();
    assert_eq!(
        revised
            .claims
            .iter()
            .find(|claim| claim.claim.position == claimed)
            .map(|claim| claim.claim.body.disposition),
        Some(wire::ClaimDisposition::Active)
    );
    clean(&root, bus);
}

#[test]
fn a_controlled_draft_flow_edit_requires_its_controller_holder() {
    let (root, bus, epoch, seats) = fixture("draft-controlled-flow", 2);
    let holder = Actor::Lane(seats[0].lane);
    let bystander = Actor::Lane(seats[1].lane);
    let controller = add_draft(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-control01",
        Some(wire::TaskIntent::Plan),
        &["The plan has a deliverable"],
    );
    assert_committed(file(&bus, epoch, &controller));
    claim_task(&bus, epoch, &seats[0], &holder, &controller);

    let child_born = stamp("DRAFT-output001");
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let parent = at(&current, &controller);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![
            Witness::Absence {
                task: child_born.clone(),
            },
            statement_witness(parent),
            workflow_witness(parent),
        ],
        &TaskAdd {
            preparation: Some(controller.clone()),
            flow: Some("todo,review".to_owned()),
            ..draft_add("DRAFT-output001", None, &["The output is delivered"])
        },
    ));
    let child = draft_handle(&child_born);

    let heads = at(&board(&bus, epoch, bystander.clone()), &child).clone();
    // Connected statement EDIT does not grant the controller's route authority.
    assert_committed(issue(
        &bus,
        epoch,
        bystander.clone(),
        CommandKind::TaskEdit,
        vec![statement_witness(&heads)],
        &TaskEdit {
            title: Some("Connected statement revision".to_owned()),
            ..only(&child)
        },
    ));

    let heads = at(&board(&bus, epoch, bystander.clone()), &child).clone();
    let refused = issue(
        &bus,
        epoch,
        bystander,
        CommandKind::TaskEdit,
        flow_edit_witnesses(&heads),
        &TaskEdit {
            flow: Some("plan,todo,review".to_owned()),
            ..only(&child)
        },
    );
    assert_eq!(refusal_reason(&refused), "task::flow-authority");

    let written = child.to_string();
    let matches = crate::cli::command()
        .try_get_matches_from([
            "spice",
            "task",
            "modify",
            written.as_str(),
            "--flow",
            "plan,todo,review",
        ])
        .expect("controlled draft edit parses");
    let contract = crate::cli::selected_contract(&matches).expect("task modify contract");
    let (_, read) = mutation_read(contract, crate::cli::selected_matches(&matches))
        .expect("task modify selects its read");
    let view = super::read_scope::scoped(&bus, epoch, holder.clone(), read);
    let child_heads = at(&view, &child);
    let controller_heads = at(&view, &controller);
    let claim = controller_heads
        .held_by(&holder)
        .expect("fixture holder has the controller claim");
    let mut expected = flow_edit_witnesses(child_heads);
    expected.push(claim.witness(&controller_heads.statement.body.born));

    let controller_stamp = controller_heads.statement.body.born.clone();
    let mut incoherent = view.clone();
    incoherent
        .tasks
        .retain(|task| task.statement.body.born != controller_stamp);
    let missing = crate::board_command::client::task_heads_by_stamp(&incoherent, &controller_stamp);
    assert!(matches!(
        missing,
        Err(Outcome::Fault(fault)) if fault.reason == "task::board-incoherent"
    ));
    assert_committed(issue(
        &bus,
        epoch,
        holder,
        CommandKind::TaskEdit,
        expected,
        &TaskEdit {
            flow: Some("plan,todo,review".to_owned()),
            ..only(&child)
        },
    ));
    assert_eq!(
        flow_of(at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &child)),
        ["draft", "plan", "todo", "review"]
    );
    clean(&root, bus);
}
#[test]
fn a_draft_edit_that_leaves_a_design_any_other_route_refuses() {
    let (root, bus, epoch, _seats) = fixture("draft-design-route", 1);
    let design = add_draft(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-desnflow",
        Some(wire::TaskIntent::Design),
        &["The design is supported"],
    );
    let ordinary = add_draft(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-ordflow1",
        None,
        &["The work is verified"],
    );
    let set_design = Some(wire::TaskIntentEdit::Set {
        intent: wire::TaskIntent::Design,
    });
    let facts = fact_count(&bus);
    for (task, refused) in [
        (
            &design,
            TaskEdit {
                flow: Some("design,review".to_owned()),
                ..only(&design)
            },
        ),
        (
            &ordinary,
            TaskEdit {
                intent: set_design,
                ..only(&ordinary)
            },
        ),
        (
            &ordinary,
            TaskEdit {
                intent: set_design,
                flow: Some("todo".to_owned()),
                ..only(&ordinary)
            },
        ),
    ] {
        let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        let heads = at(&current, task);
        let expected = if refused.flow.is_some() {
            flow_edit_witnesses(heads)
        } else {
            vec![statement_witness(heads)]
        };
        let outcome = issue(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            CommandKind::TaskEdit,
            expected,
            &refused,
        );
        assert_eq!(refusal_reason(&outcome), "task::design-flow");
    }
    assert_eq!(fact_count(&bus), facts, "a refused route edit wrote facts");

    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        flow_edit_witnesses(at(&current, &ordinary)),
        &TaskEdit {
            intent: set_design,
            flow: Some("design".to_owned()),
            ..only(&ordinary)
        },
    ));
    let heads = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &ordinary).clone();
    assert_eq!(heads.statement.body.intent, Some(wire::TaskIntent::Design));
    assert_eq!(flow_of(&heads), ["draft", "design"]);
    clean(&root, bus);
}

#[test]
fn default_reads_show_a_draft_only_to_the_operator_and_its_origin_thread() {
    let (root, bus, epoch, seats) = fixture("draft-visibility", 2);
    authorize_lane_creation(&bus, epoch, &seats[0]);
    let operator_draft = add_draft(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-oper0001",
        None,
        &[],
    );
    let lane_draft = add_draft(
        &bus,
        epoch,
        Actor::Lane(seats[0].lane),
        "DRAFT-lane0001",
        None,
        &[],
    );

    let operator = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let author = board(&bus, epoch, Actor::Lane(seats[0].lane));
    let peer = board(&bus, epoch, Actor::Lane(seats[1].lane));
    let operator_list = listed(&operator, false);
    assert!(
        operator_list.contains(operator_draft.as_str()),
        "{operator_list}"
    );
    assert!(
        operator_list.contains(lane_draft.as_str()),
        "{operator_list}"
    );
    let author_list = listed(&author, false);
    assert!(author_list.contains(lane_draft.as_str()), "{author_list}");
    assert!(
        !author_list.contains(operator_draft.as_str()),
        "{author_list}"
    );
    let peer_list = listed(&peer, false);
    assert!(!peer_list.contains(lane_draft.as_str()), "{peer_list}");
    assert!(!peer_list.contains(operator_draft.as_str()), "{peer_list}");
    let complete = listed(&operator, true);
    assert!(complete.contains(operator_draft.as_str()), "{complete}");
    assert!(complete.contains(lane_draft.as_str()), "{complete}");
    clean(&root, bus);
}

#[test]
fn a_draft_is_never_offered_or_captured() {
    let (root, bus, epoch, seats) = fixture("draft-authority", 1);
    let seat = &seats[0];
    let lane = Actor::Lane(seat.lane);
    let draft = add_draft(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-auth0001",
        None,
        &["The work is verified"],
    );
    let facts = fact_count(&bus);
    let offered = allocated(&bus, epoch, &lane).map(|(handle, _)| handle);
    assert_ne!(
        offered.as_deref(),
        Some(draft.as_str()),
        "a Draft was offered"
    );

    let current = board(&bus, epoch, lane.clone());
    let heads = at(&current, &draft);
    let rule: Name = "capture".parse().expect("capture rule");
    let capture = issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::ClaimWin,
        vec![
            statement_witness(heads),
            workflow_witness(heads),
            claim_witness(heads),
            Witness::Eligibility { rule: rule.clone() },
            residency(&current, seat),
        ],
        &ClaimWin {
            rule,
            recovery: None,
        },
    );
    assert_eq!(refusal_reason(&capture), "task::draft-capture-refused");
    assert_eq!(fact_count(&bus), facts, "a refused capture wrote facts");
    clean(&root, bus);
}

/// Work may wait on a draft, which is an unfinished prerequisite until it
/// leaves draft; the draft's own prerequisites stay editable content.
#[test]
fn a_draft_is_an_unfinished_prerequisite_and_keeps_its_own() {
    let (root, bus, epoch, seats) = fixture("draft-dependencies", 1);
    let draft = add_draft(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-auth0002",
        None,
        &["The work is verified"],
    );
    let public = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-public01",
        "Public",
        Some("todo"),
    );
    let operator = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDepends,
        dependency_witnesses(&operator, &public, &[&draft], None, false),
        &TaskDepends {
            task: public.clone(),
            after: vec![draft.clone()],
            remove: false,
        },
    ));
    let waiting = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let entries = actor_entries(&waiting).expect("the board projects to entries");
    let public_entry = entries
        .iter()
        .find(|entry| Ok(entry.born()) == public.born().as_ref())
        .expect("the public task projects");
    assert!(
        !board::eligible_entry(
            &board::resolved_tasks(&entries),
            public_entry,
            &Actor::Lane(seats[0].lane),
            &waiting.environment(),
        ),
        "a draft prerequisite is unfinished, so the dependent waits"
    );
    let other = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-public02",
        "Other public",
        Some("todo"),
    );
    let operator = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDepends,
        dependency_witnesses(&operator, &draft, &[&other], None, false),
        &TaskDepends {
            task: draft.clone(),
            after: vec![other.clone()],
            remove: false,
        },
    ));
    let revised = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&revised, &draft).statement.body.dependencies,
        [other.born().expect("public handle carries a birth stamp")],
        "a Draft's own prerequisites are editable content",
    );
    clean(&root, bus);
}

#[test]
fn a_draft_refuses_settlement_but_snoozes_like_any_row() {
    let (root, bus, epoch, _seats) = fixture("draft-authority-rails", 1);
    let draft = add_draft(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-auth0003",
        Some(wire::TaskIntent::Design),
        &["The work is verified"],
    );
    let facts = fact_count(&bus);
    let operator = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&operator, &draft);
    let close = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        transition_witnesses(heads),
        &TaskClose {
            task: draft.clone(),
            validation: "Closed early".to_owned(),
            force_claimed: false,
        },
    );
    assert_eq!(refusal_reason(&close), "task::draft-settlement-refused");
    let conclude = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskConclude,
        transition_witnesses(heads),
        &TaskConclude {
            task: draft.clone(),
            conclusion: wire::body::DesignConclusion::Done {},
        },
    );
    assert!(matches!(conclude, Outcome::Refusal(_)));
    assert_eq!(
        fact_count(&bus),
        facts,
        "a refused Draft authority wrote facts"
    );
    let defer = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDefer,
        vec![workflow_witness(heads), deferral_witness(heads)],
        &TaskDefer {
            task: draft.clone(),
            wait: fixture_instant("draft defer"),
        },
    );
    assert_committed(defer);
    let snoozed = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &draft).clone();
    assert!(snoozed.workflow.body.is_draft());
    assert_eq!(
        snoozed.workflow.body.deferral,
        Some(fixture_instant("draft defer"))
    );
    clean(&root, bus);
}

/// Filing an incomplete draft is construction failure: the row stays in
/// draft, now submitted, with the repair named in the receipt, and a triage
/// lane may acquire it. Repeating the file without a change refuses and
/// writes nothing. Filing an activated row is an authority refusal that
/// writes nothing.
#[test]
fn an_incomplete_file_marks_the_draft_submitted_and_names_the_repair() {
    let (root, bus, epoch, seats) = fixture("draft-file-incomplete", 1);
    let lane = Actor::Lane(seats[0].lane);
    let draft = add_draft(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-incomp01",
        None,
        &[],
    );
    assert_eq!(
        allocated(&bus, epoch, &lane),
        None,
        "a held Draft was offered"
    );
    let CommandAnswer::Task(answer) = assert_committed(file(&bus, epoch, &draft)) else {
        panic!("filing returned another answer shape");
    };
    assert_eq!(answer.task, draft);
    assert_eq!(answer.status, "submitted");
    assert!(
        answer
            .diagnostic
            .as_deref()
            .is_some_and(|diagnostic| diagnostic.contains("task::term-missing")),
        "{:?}",
        answer.diagnostic
    );
    let submitted = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&submitted, &draft).clone();
    assert!(heads.workflow.body.is_submitted_draft());
    assert_eq!(
        allocated(&bus, epoch, &lane).map(|(handle, _)| handle),
        Some(draft.to_string()),
        "a submitted Draft is triage work",
    );

    let facts = fact_count(&bus);
    let repeated = file(&bus, epoch, &draft);
    assert_eq!(refusal_reason(&repeated), "task::term-missing");
    assert_eq!(
        fact_count(&bus),
        facts,
        "a repeated incomplete file wrote facts"
    );

    let ordinary = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-ordin001",
        "Ordinary",
        Some("todo"),
    );
    let facts = fact_count(&bus);
    let not_draft = file(&bus, epoch, &ordinary);
    assert_eq!(refusal_reason(&not_draft), "task::not-draft");
    assert_eq!(fact_count(&bus), facts, "filing activated work wrote facts");

    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskEdit,
        vec![statement_witness(&heads)],
        &TaskEdit {
            term: Some(vec!["The repaired criterion holds".to_owned()]),
            ..only(&draft)
        },
    ));
    let CommandAnswer::Task(answer) = assert_committed(file(&bus, epoch, &draft)) else {
        panic!("filing returned another answer shape");
    };
    assert_eq!(answer.status, "todo");
    assert_eq!(answer.diagnostic, None);
    let activated = at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &answer.task).clone();
    assert!(activated.workflow.body.is_activated());
    assert!(
        activated.workflow.body.submitted,
        "the mark is never cleared"
    );
    clean(&root, bus);
}

#[test]
fn filing_turns_a_complete_task_plan_or_design_draft_into_offered_work() {
    for (name, intent, flow) in [
        ("DRAFT-task0001", None, &["draft", "todo", "review"][..]),
        (
            "DRAFT-plan0001",
            Some(wire::TaskIntent::Plan),
            &["draft", "plan", "review"][..],
        ),
        (
            "DRAFT-desn0001",
            Some(wire::TaskIntent::Design),
            &["draft", "design"][..],
        ),
    ] {
        let (root, bus, epoch, seats) = fixture(name, 1);
        let lane = Actor::Lane(seats[0].lane);
        let draft = add_draft(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            name,
            intent,
            &["The filed work is verified"],
        );
        let CommandAnswer::Task(answer) = assert_committed(file(&bus, epoch, &draft)) else {
            panic!("filing returned another answer shape");
        };
        let filed = TaskHandle::for_topic(
            &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
            &draft.born().expect("draft handle carries a birth stamp"),
        );
        assert_eq!(answer.task, filed);
        let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        let heads = at(&current, &filed);
        assert!(heads.workflow.body.is_activated());
        assert_eq!(heads.statement.body.intent, intent);
        assert_eq!(flow_of(heads), flow);
        assert!(
            current
                .tasks
                .iter()
                .all(|task| task.statement.body.handle != draft)
        );
        let listed = listed(&current, false);
        assert!(listed.contains(filed.as_str()), "{listed}");
        if intent == Some(wire::TaskIntent::Design) {
            assert_eq!(
                allocated(&bus, epoch, &lane).map(|(handle, _)| handle),
                None,
                "a filed Design gathering input is not offered before it is ready",
            );
            capture_design(&bus, epoch, &seats[0], &filed);
        } else {
            assert_eq!(
                allocated(&bus, epoch, &lane).map(|(handle, _)| handle),
                Some(filed.to_string()),
                "filed {name} is not offered once eligible",
            );
        }
        clean(&root, bus);
    }
}

/// A creation without `--draft` runs the same validator at birth: a
/// contractless Task, Plan, or Design lands in draft, submitted, with its
/// diagnostic, on its typed route with no plan bookend inserted.
#[test]
fn a_contractless_creation_lands_submitted_in_draft_with_its_diagnostic() {
    let (root, bus, epoch, seats) = fixture("draft-plan-first", 1);
    let lane = Actor::Lane(seats[0].lane);
    for (name, intent, route) in [
        ("DRAFT-tplan001", None, &["draft", "todo", "review"][..]),
        (
            "DRAFT-pplan001",
            Some(wire::TaskIntent::Plan),
            &["draft", "plan", "review"][..],
        ),
        (
            "DRAFT-dplan001",
            Some(wire::TaskIntent::Design),
            &["draft", "design"][..],
        ),
    ] {
        let born = stamp(name);
        let CommandAnswer::Task(answer) = assert_committed(issue(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            CommandKind::TaskAdd,
            vec![Witness::Absence { task: born.clone() }],
            &TaskAdd {
                draft: false,
                ..draft_add(name, intent, &[])
            },
        )) else {
            panic!("creation returned another answer shape");
        };
        assert_eq!(answer.task, draft_handle(&born));
        assert_eq!(answer.status, "draft");
        assert!(
            answer
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| diagnostic.contains("task::term-missing")),
            "{:?}",
            answer.diagnostic
        );
        let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        let heads = at(&current, &answer.task);
        assert!(heads.workflow.body.is_submitted_draft());
        assert_eq!(flow_of(heads), route);
        assert_eq!(heads.workflow.body.phase, wire::Phase::Draft);
    }
    let offered = allocated(&bus, epoch, &lane).map(|(handle, _)| handle);
    assert!(
        offered
            .as_deref()
            .is_some_and(|handle| handle.starts_with("DRAFT-")),
        "submitted triage work was not offered: {offered:?}"
    );
    clean(&root, bus);
}

#[test]
fn filing_a_draft_can_claim_it_in_the_same_transaction() {
    let (root, bus, epoch, seats) = fixture("draft-file-claim", 1);
    let seat = &seats[0];
    let lane = Actor::Lane(seat.lane);

    authorize_lane_creation(&bus, epoch, seat);
    let held = add_draft(
        &bus,
        epoch,
        lane.clone(),
        "DRAFT-claim002",
        None,
        &["Held work is verified"],
    );
    let visible = board(&bus, epoch, lane.clone());
    let heads = at(&visible, &held);
    let mut expected = file_witnesses(heads);
    expected.push(Witness::Eligibility {
        rule: "capture".parse().expect("fixture capture rule"),
    });
    expected.push(residency(&visible, seat));
    let CommandAnswer::Task(answer) = assert_committed(issue(
        &bus,
        epoch,
        lane.clone(),
        CommandKind::TaskFile,
        expected,
        &TaskFile {
            task: held.clone(),
            topic: None,
            claim: true,
        },
    )) else {
        panic!("file-and-claim returned another answer shape");
    };
    let claim = answer
        .claim
        .expect("filing with claim returns the held claim");
    assert_eq!(claim.holder, Some(lane));
    clean(&root, bus);
}

/// A draft lends no acceptance: an activated task may wait on a draft, but
/// the edge carries the borrower's contract only once the draft advances.
#[test]
fn a_draft_lends_acceptance_only_after_it_advances() {
    let (root, bus, epoch, seats) = fixture("draft-acceptance-lender", 1);
    let seat = &seats[0];
    let lane = Actor::Lane(seat.lane);
    let borrower = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-borrower",
        "Borrower",
        Some("todo"),
    );
    let lender = add_draft(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "DRAFT-lender",
        None,
        &["The borrower's output is verified"],
    );
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        completion_contract(
            &[],
            &at(&current, &borrower).statement.body.born,
            &[lender.born().expect("lender handle carries a birth stamp")],
            None,
            current.tasks.iter(),
        ),
        "Completion contract: unspecified. Work may continue; successful completion needs own AC, directly linked pending AC-bearing work in either direction, or an applicable no-work/duplicate/covered/deferred disposition. No allocation hold or phase change.",
        "a draft lent its acceptance"
    );
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDepends,
        dependency_witnesses(&current, &borrower, &[&lender], None, false),
        &TaskDepends {
            task: borrower.clone(),
            after: vec![lender.clone()],
            remove: false,
        },
    ));
    let linked = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let borrower_heads = at(&linked, &borrower);
    assert_eq!(
        completion_contract(
            &[],
            &borrower_heads.statement.body.born,
            &borrower_heads.statement.body.dependencies,
            None,
            linked.tasks.iter(),
        ),
        "Completion contract: unspecified. Work may continue; successful completion needs own AC, directly linked pending AC-bearing work in either direction, or an applicable no-work/duplicate/covered/deferred disposition. No allocation hold or phase change.",
        "a linked draft lends nothing until it advances"
    );

    assert_committed(file(&bus, epoch, &lender));
    let advanced = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &lender.born().expect("lender handle carries a birth stamp"),
    );
    let current = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let borrower_heads = at(&current, &borrower);
    assert_eq!(
        completion_contract(
            &[],
            &borrower_heads.statement.body.born,
            &borrower_heads.statement.body.dependencies,
            None,
            current.tasks.iter(),
        ),
        format!("Completion contract: linked AC ({advanced})."),
    );
    assert_eq!(
        allocated(&bus, epoch, &lane).map(|(handle, _)| handle),
        Some(advanced.to_string()),
        "the advanced lender is ordinary offered work",
    );
    clean(&root, bus);
}
