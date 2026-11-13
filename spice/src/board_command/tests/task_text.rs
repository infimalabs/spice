use super::*;

/// The refusal reason and the current digest it reports.
fn refused(
    outcome: Result<(CommandKind, Vec<Witness>, serde_json::Value), Outcome<CommandAnswer>>,
) -> (String, Option<String>) {
    match outcome {
        Err(Outcome::Refusal(refusal)) => (
            refusal.reason().to_owned(),
            refusal
                .diagnostic()
                .and_then(|diagnostic| diagnostic.observed())
                .map(str::to_owned),
        ),
        other => panic!("expected a refusal, found {other:?}"),
    }
}

fn submit(
    bus: &Bus,
    epoch: CanonicalUuid,
    actor: Actor,
    (kind, witnesses, body): (CommandKind, Vec<Witness>, serde_json::Value),
) -> Outcome<CommandAnswer> {
    issue(bus, epoch, actor, kind, witnesses, &body)
}

#[test]
fn a_replacement_lands_only_over_the_text_its_digest_names() {
    let (_root, bus, epoch, seats) = fixture("task-text-digest", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add_task_with_term(
        &bus,
        epoch,
        "TEXT-1kKfYW01x",
        "Replace by digest",
        "/board/test",
        Some("todo"),
        &["the clause as first filed"],
    );
    let read = board(&bus, epoch, lane.clone());
    let heads = at(&read, &task).clone();
    let read_digest = TaskText::Description.digest(&heads.statement.body);

    let (reason, observed) = refused(text_edit(&heads, TaskText::Description, None, "new"));
    assert_eq!(reason, "task::text-digest-missing");
    assert_eq!(observed, Some(read_digest.to_string()));
    let elsewhere = Digest::compute(Domain::TaskText, b"text nobody read");
    let (reason, observed) = refused(text_edit(
        &heads,
        TaskText::Description,
        Some(&elsewhere),
        "new",
    ));
    assert_eq!(reason, "task::text-digest-moved");
    assert_eq!(observed, Some(read_digest.to_string()));

    let replacement = text_edit(
        &heads,
        TaskText::Description,
        Some(&read_digest),
        "what the task means\n",
    )
    .expect("the read's digest admits the replacement");
    assert_committed(submit(&bus, epoch, lane.clone(), replacement.clone()));
    let replaced = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let statement = &at(&replaced, &task).statement.body;
    assert_eq!(
        statement.description.as_deref(),
        Some("what the task means"),
        "the written text replaces the description, less its final line break"
    );
    assert_eq!(statement.term, ["the clause as first filed"]);

    let (reason, _) = refused(text_edit(
        at(&replaced, &task),
        TaskText::Description,
        Some(&read_digest),
        "a second writer's text",
    ));
    assert_eq!(
        reason, "task::text-digest-moved",
        "a digest read before a replacement never admits another"
    );
    assert!(
        matches!(
            &submit(&bus, epoch, lane, replacement),
            Outcome::Refusal(refusal) if refusal.reason() == "board::witness-moved"
        ),
        "a replacement mapped before the statement moved must not land"
    );
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&after, &task).statement.position,
        at(&replaced, &task).statement.position
    );
}

#[test]
fn a_settled_task_refuses_a_replacement_and_keeps_its_text() {
    let (_root, bus, epoch, seats) = fixture("task-text-settled", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "TEXT-1kKfYW02x",
        "Settled work",
        Some("todo"),
    );
    claim_and_complete_task(&bus, epoch, &seats[0], &lane, &task);
    let settled = board(&bus, epoch, lane.clone());
    let heads = at(&settled, &task).clone();
    let replacement = text_edit(
        &heads,
        TaskText::Term,
        Some(&TaskText::Term.digest(&heads.statement.body)),
        "- rewrite what already shipped",
    )
    .expect("a current digest maps onto the modify intent");

    let outcome = submit(&bus, epoch, lane, replacement);
    assert!(
        matches!(&outcome, Outcome::Refusal(refusal) if refusal.reason() == "task::settled"),
        "{outcome:?}"
    );
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&after, &task).statement, heads.statement);
}

#[test]
fn term_text_reads_back_as_the_items_it_names() {
    let (_root, bus, epoch, _seats) = fixture("task-text-term", 0);
    let items = [
        "a clause a reviewer can check",
        "a clause spanning lines\n- whose second line looks like an item\n\nand survives a blank",
    ];
    let task = add_task_with_term(
        &bus,
        epoch,
        "TEXT-1kKfYW03x",
        "Terms as text",
        "/board/test",
        Some("todo"),
        &items,
    );
    let read = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&read, &task);
    let text = TaskText::Term.current(&heads.statement.body);
    assert_eq!(term_items(&text).expect("a read's text parses"), items);
    assert_eq!(
        term_items("- one\n\n- two\n  continued\n\n").expect("items parse"),
        ["one", "two\ncontinued"]
    );
    assert!(term_items("no bullet\n- item").is_err());

    let digest = TaskText::Term.digest(&heads.statement.body);
    for empty in ["", "\n\n", "-\n- "] {
        let (reason, _) = refused(text_edit(heads, TaskText::Term, Some(&digest), empty));
        assert_eq!(reason, "task::term-empty", "{empty:?}");
    }
}

#[test]
fn one_term_is_replaced_at_its_handle_and_a_reworded_term_refuses_the_old_one() {
    let (_root, bus, epoch, seats) = fixture("task-text-one-term", 1);
    let lane = Actor::Lane(seats[0].lane);
    let task = add_task_with_term(
        &bus,
        epoch,
        "TEXT-1kKfYW04x",
        "Replace one term",
        "/board/test",
        Some("todo"),
        &["the clause that stays", "the clause as first filed"],
    );
    let read = board(&bus, epoch, lane.clone());
    let heads = at(&read, &task).clone();
    let items = wire::StatementItem::each(&heads.statement.body.term).collect::<Vec<_>>();
    let filed = items[1].handle(&items).to_owned();
    assert_eq!(
        term_text(&heads.statement.body, &filed).expect("a current handle names its term"),
        (filed.clone(), "the clause as first filed".to_owned())
    );

    for blank in ["", " \n"] {
        let (reason, _) = refused(term_edit(&heads, &filed, blank));
        assert_eq!(reason, "task::term-empty", "{blank:?}");
    }
    let replacement = term_edit(&heads, &filed, "the clause as reworded\n")
        .expect("a current handle admits the replacement");
    assert_committed(submit(&bus, epoch, lane, replacement));
    let replaced = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(
        at(&replaced, &task).statement.body.term,
        ["the clause that stays", "the clause as reworded"],
        "only the named term changes, in place"
    );

    let (reason, _) = refused(term_edit(
        at(&replaced, &task),
        &filed,
        "a second writer's text",
    ));
    assert_eq!(
        reason, "task::statement-item-handle-stale",
        "a handle read before the rewording no longer names a term"
    );
}
