use super::*;

pub(super) fn configured_repository(root: &Path, words: &[&str]) -> Repository {
    fs::create_dir_all(root.join(".git")).expect("taste-review fixture is a repository");
    let words = words
        .iter()
        .map(|word| format!("{word:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        root.join("spice.toml"),
        format!(
            "[taste.taskwording]\nwords = [{words}]\nmessage = \"Rewrite the task wording.\"\n"
        ),
    )
    .expect("taste-review policy is written");
    Repository::discover(root).expect("taste-review repository resolves")
}

pub(super) fn issue_repository<B: Serialize>(
    bus: &Bus,
    repository: &Repository,
    epoch: CanonicalUuid,
    actor: Actor,
    kind: CommandKind,
    expected: Vec<Witness>,
    body: &B,
) -> Outcome<CommandAnswer> {
    let command = fixture_command(epoch, actor, kind, expected, body);
    dispatch_with_repository(
        bus,
        &command,
        Some(host(repository)),
        &board::NextConfig::default(),
        None,
    )
    .expect("board command owns taste-review fixture kind")
}

fn add_with_repository(
    bus: &Bus,
    repository: &Repository,
    epoch: CanonicalUuid,
    task: &str,
    title: &str,
    term: &[&str],
) -> (TaskHandle, Outcome<CommandAnswer>) {
    let born = stamp(task);
    let handle = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    );
    let outcome = issue_repository(
        bus,
        repository,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born }],
        &TaskAdd {
            topic: Some("/board/test".parse().expect("fixture topic")),
            term: term.iter().map(|item| (*item).to_owned()).collect(),
            origin: Some(fixture_origin()),
            ..adding(title.to_owned(), "This work keeps the fixture honest.")
        },
    );
    (handle, outcome)
}

pub(super) fn modify_with_repository(
    bus: &Bus,
    repository: &Repository,
    epoch: CanonicalUuid,
    actor: Actor,
    task: &TaskHandle,
    title: &str,
) -> Outcome<CommandAnswer> {
    let visible = board(bus, epoch, actor.clone());
    let witness = statement_witness(at(&visible, task));
    modify_witnessed(bus, repository, epoch, actor, task, title, witness)
}

/// Retitle `task` against the statement head `witness` names, as `actor`.
fn modify_witnessed(
    bus: &Bus,
    repository: &Repository,
    epoch: CanonicalUuid,
    actor: Actor,
    task: &TaskHandle,
    title: &str,
    witness: Witness,
) -> Outcome<CommandAnswer> {
    issue_repository(
        bus,
        repository,
        epoch,
        actor,
        CommandKind::TaskEdit,
        vec![witness],
        &TaskEdit {
            title: Some(title.to_owned()),
            ..only(task)
        },
    )
}

fn flow_of(board: &BoardAnswer, task: &TaskHandle) -> (String, Vec<String>) {
    let workflow = &at(board, task).workflow.body;
    (
        workflow.phase.as_str().to_owned(),
        workflow
            .flow
            .iter()
            .map(|phase| phase.as_str().to_owned())
            .collect(),
    )
}

#[test]
fn a_matched_filing_records_a_hold_and_keeps_the_authored_flow() {
    let (root, bus, epoch, _seats) = fixture("taste-review-create", 0);
    let repository = configured_repository(&root, &["delay"]);

    let (clean_task, clean_outcome) = add_with_repository(
        &bus,
        &repository,
        epoch,
        "TASTE-clean001",
        "Ship the lifecycle change",
        &["focused tests pass"],
    );
    let CommandAnswer::Task(clean_answer) = assert_committed(clean_outcome) else {
        panic!("task add answers with a task status");
    };
    assert_eq!(clean_answer.taste, None);
    let (review_task, review) = add_with_repository(
        &bus,
        &repository,
        epoch,
        "TASTE-review01",
        "Delay the lifecycle change",
        &["focused tests pass"],
    );
    let CommandAnswer::Task(answer) = assert_committed(review) else {
        panic!("task add answers with a task status");
    };

    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let authored = (
        "todo".to_owned(),
        vec!["draft".into(), "todo".into(), "review".into()],
    );
    assert_eq!(flow_of(&visible, &clean_task), authored);
    assert_eq!(at(&visible, &clean_task).statement.body.taste, None);
    assert_eq!(
        flow_of(&visible, &review_task),
        authored,
        "a hold leaves the authored flow and first phase exactly as filed",
    );
    let held = at(&visible, &review_task);
    let hold = held
        .statement
        .body
        .taste
        .as_ref()
        .expect("a matched statement is held");
    assert_eq!(hold.rechecks, 0);
    let [found] = hold.matches.as_slice() else {
        panic!("one title match holds the statement: {hold:?}");
    };
    assert_eq!(
        (found.field, found.line, found.taste.as_str()),
        (wire::StatementField::Title, 1, "taskwording"),
    );
    assert_eq!(
        found
            .finding
            .as_ref()
            .map(|finding| finding.trigger.as_str()),
        Some("delay"),
        "the operator reads what matched",
    );
    assert_eq!(
        answer.taste.as_ref().map(|taste| &taste.matches),
        Some(&hold.matches),
        "the filing answer carries the hold's matches"
    );
    let filed = held.statement.position;
    assert_eq!(
        hold.opened,
        Some(filed),
        "the hold records the filing write that opened it",
    );
    add_dependency(&bus, epoch, &review_task, &clean_task);
    let rewired = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let carried = at(&rewired, &review_task);
    assert_ne!(carried.statement.position, filed);
    assert_eq!(
        carried
            .statement
            .body
            .taste
            .as_ref()
            .and_then(|hold| hold.opened),
        Some(filed),
        "a dependency edit carries the hold without claiming to have opened it",
    );

    let shown = task_show(&visible, held, &[]);
    assert!(shown.contains("taste_review required"));
    assert!(shown.contains(&format!("next: spice task modify {review_task}")));
    assert!(!shown.contains("next: spice task done"));

    clean(&root, bus);
}

#[test]
fn an_operator_closed_hold_reads_as_history_without_rewording_guidance() {
    let (root, bus, epoch, _seats) = fixture("taste-review-settled", 0);
    let repository = configured_repository(&root, &["delay"]);
    let (task, outcome) = add_with_repository(
        &bus,
        &repository,
        epoch,
        "TASTE-settled1",
        "Delay the lifecycle change",
        &["focused tests pass"],
    );
    assert_committed(outcome);
    let held = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let hold = at(&held, &task).statement.body.taste.clone();
    assert!(hold.is_some(), "a matched statement is held");

    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskClose,
        transition_witnesses(at(&held, &task)),
        &TaskClose {
            task: task.clone(),
            validation: "the operator settled it".to_owned(),
            force_claimed: false,
        },
    ));
    let settled = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let closed = at(&settled, &task);
    assert!(!closed.workflow.body.status.is_pending());
    assert_eq!(
        closed.statement.body.taste, hold,
        "closure retains the hold as statement evidence",
    );

    let shown = task_show(&settled, closed, &[]);
    assert!(shown.contains("taste_review historical"), "{shown}");
    assert!(!shown.contains("taste_review required"), "{shown}");
    assert!(shown.contains("title line 1 taste taskwording"), "{shown}");
    assert!(shown.ends_with("next: -"), "{shown}");
    assert!(!shown.contains("spice task modify"), "{shown}");
    assert!(!shown.contains("spice task done"), "{shown}");

    clean(&root, bus);
}

#[test]
fn a_matching_edit_records_a_hold_on_the_statement_alone() {
    let (root, bus, epoch, seats) = fixture("taste-review-edit-hold", 1);
    let repository = configured_repository(&root, &["delay"]);
    let (task, outcome) = add_with_repository(
        &bus,
        &repository,
        epoch,
        "TASTE-edit0001",
        "Ship the lifecycle change",
        &["focused tests pass"],
    );
    assert_committed(outcome);
    claim_held(&bus, epoch, &seats[0], &task);
    let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);

    let CommandAnswer::Task(edited) = assert_committed(modify_with_repository(
        &bus,
        &repository,
        epoch,
        Actor::TEST_PRINCIPAL,
        &task,
        "Delay the lifecycle change",
    )) else {
        panic!("task edit answers with a task status");
    };
    assert_eq!(edited.status, "edited; taste_review required");
    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let hold = at(&after, &task)
        .statement
        .body
        .taste
        .as_ref()
        .expect("a matching edit records a hold");
    assert_eq!(hold.rechecks, 0);
    assert_eq!(hold.opened, Some(at(&after, &task).statement.position));
    assert_eq!(
        edited.taste.as_ref().map(|taste| &taste.matches),
        Some(&hold.matches)
    );
    assert_eq!(flow_of(&after, &task), flow_of(&before, &task));
    assert_eq!(
        at(&after, &task)
            .latest_claim()
            .map(|held| &held.claim)
            .map(|claim| &claim.body),
        at(&before, &task)
            .latest_claim()
            .map(|held| &held.claim)
            .map(|claim| &claim.body),
        "recording a hold leaves the holder's claim in place",
    );

    clean(&root, bus);
}

#[test]
fn compound_capture_taste_review_refuses_before_creating_task_or_claim() {
    let (root, bus, epoch, seats) = fixture("taste-review-capture", 1);
    let repository = configured_repository(&root, &["delay"]);
    let seat = &seats[0];
    authorize_lane_creation(&bus, epoch, seat);
    let actor = Actor::Lane(seat.lane);
    let before = board(&bus, epoch, actor.clone());
    let born = stamp("TASTE-capture1");
    let handle = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    );
    let facts_before = bus
        .transact(|journal| {
            bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
        })
        .expect("taste-review facts read")
        .facts()
        .len();
    let outcome = issue_repository(
        &bus,
        &repository,
        epoch,
        actor.clone(),
        CommandKind::TaskAdd,
        vec![
            Witness::Absence { task: born },
            Witness::Eligibility {
                rule: CAPTURE_WORK_RULE
                    .parse()
                    .expect("capturework is a wire name"),
            },
            residency(&before, seat),
        ],
        &TaskAdd {
            topic: Some("/board/test".parse().expect("fixture topic")),
            origin: Some("ack:1kLGp2NJx".parse().expect("fixture steering origin")),
            claim: true,
            ..adding(
                "Delay the captured implementation",
                "This work keeps the fixture honest.",
            )
        },
    );
    assert!(
        matches!(&outcome, Outcome::Refusal(refusal)
            if refusal.reason() == "task::taste-review-required"
                && refusal.explanation().contains("taste taskwording")
                && refusal.explanation().contains("word \"delay\"")),
        "taste-required compound capture must refuse with its matches: {outcome:?}",
    );
    assert_eq!(
        bus.transact(|journal| bus::JournalTransaction::settled(
            journal.fact_snapshot().expect("journal snapshot")
        ))
        .expect("taste-review facts read")
        .facts()
        .len(),
        facts_before,
    );
    assert!(
        board(&bus, epoch, actor)
            .tasks
            .iter()
            .all(|task| task.statement.body.handle != handle),
        "refused compound capture must not leave a task",
    );
    clean(&root, bus);
}

#[test]
fn modification_rechecks_the_complete_current_policy_before_clearing() {
    let (root, bus, epoch, _seats) = fixture("taste-review-recheck", 0);
    let repository = configured_repository(&root, &["delay"]);
    let (task, outcome) = add_with_repository(
        &bus,
        &repository,
        epoch,
        "TASTE-recheck1",
        "Delay the lifecycle change",
        &["focused tests pass"],
    );
    assert_committed(outcome);

    configured_repository(&root, &["current"]);
    assert_committed(modify_with_repository(
        &bus,
        &repository,
        epoch,
        Actor::TEST_PRINCIPAL,
        &task,
        "Current lifecycle change",
    ));
    let still_held = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let hold = at(&still_held, &task)
        .statement
        .body
        .taste
        .clone()
        .expect("the current policy still matches");
    assert_eq!(hold.rechecks, 1);
    assert_eq!(
        hold.matches
            .iter()
            .map(|found| found
                .finding
                .as_ref()
                .map(|finding| finding.trigger.as_str()))
            .collect::<Vec<_>>(),
        [Some("current")],
        "the hold records what the current policy matched",
    );

    configured_repository(&root, &["unrelated"]);
    assert_committed(modify_with_repository(
        &bus,
        &repository,
        epoch,
        Actor::TEST_PRINCIPAL,
        &task,
        "Current lifecycle change",
    ));
    let cleared = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&cleared, &task).statement.body.taste, None);

    clean(&root, bus);
}

/// The claim's possession, without the generation it acknowledged: an edit
/// by its own holder acknowledges in place.
fn claim_of(
    board: &BoardAnswer,
    task: &TaskHandle,
) -> Option<(
    ClaimDisposition,
    wire::Seq,
    Actor,
    CanonicalUuid,
    CanonicalUuid,
)> {
    at(board, task)
        .latest_claim()
        .map(|held| &held.claim)
        .map(|claim| {
            (
                claim.body.disposition,
                claim.body.claimed,
                claim.body.actor.clone(),
                claim.body.lane,
                claim.body.occupant,
            )
        })
}

fn phase_facts(bus: &Bus, task: &TaskHandle) -> usize {
    let born = task.born().expect("fixture handles carry their stamp");
    bus.transact(|journal| {
        bus::JournalTransaction::settled(journal.fact_snapshot().expect("journal snapshot"))
    })
    .expect("taste-review facts read")
    .facts()
    .iter()
    .filter(|fact| {
        fact.fact
            .read::<body::TaskPhased>()
            .is_some_and(|phased| phased.task == born)
    })
    .count()
}

#[test]
fn clearing_a_hold_moves_only_the_statement_whoever_edits() {
    let (root, bus, epoch, seats) = fixture("taste-review-clear", 2);
    let repository = configured_repository(&root, &["delay"]);
    let mut held = Vec::new();
    for (name, seat) in [("TASTE-holder01", &seats[0]), ("TASTE-creator1", &seats[1])] {
        let (task, outcome) = add_with_repository(
            &bus,
            &repository,
            epoch,
            name,
            "Delay the lifecycle change",
            &["focused tests pass"],
        );
        assert_committed(outcome);
        claim_held(&bus, epoch, seat, &task);
        held.push(task);
    }

    // The holding lane clears its own hold; the operator who filed the other
    // task clears that one while another lane holds it.
    for (task, editor) in [
        (&held[0], Actor::Lane(seats[0].lane)),
        (&held[1], Actor::TEST_PRINCIPAL),
    ] {
        let before = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        let phased = phase_facts(&bus, task);
        let CommandAnswer::Task(edited) = assert_committed(modify_with_repository(
            &bus,
            &repository,
            epoch,
            editor,
            task,
            "Ship the lifecycle change",
        )) else {
            panic!("task edit answers with a task status");
        };
        assert_eq!(edited.status, "edited; taste_review cleared");
        assert_eq!(edited.taste, None);
        let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
        assert_eq!(at(&after, task).statement.body.taste, None);
        assert_eq!(flow_of(&after, task), flow_of(&before, task));
        assert!(claim_of(&before, task).is_some());
        assert_eq!(
            claim_of(&after, task),
            claim_of(&before, task),
            "clearing the hold leaves the active claim exactly where it was",
        );
        assert_eq!(phase_facts(&bus, task), phased, "clearing writes no phase");
    }
    clean(&root, bus);
}

#[test]
fn an_authored_plan_leaves_through_ordinary_done_once_its_hold_clears() {
    let (root, bus, epoch, seats) = fixture("taste-review-authored-plan", 1);
    let repository = configured_repository(&root, &["delay"]);
    let seat = &seats[0];
    let born = stamp("TASTE-authplan");
    let task = TaskHandle::for_topic(
        &"/board/test".parse::<wire::Topic>().expect("fixture topic"),
        &born,
    );
    assert_committed(issue_repository(
        &bus,
        &repository,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born }],
        &TaskAdd {
            topic: Some("/board/test".parse().expect("fixture topic")),
            term: vec!["focused tests pass".to_owned()],
            origin: Some(fixture_origin()),
            flow: Some("plan,todo".to_owned()),
            ..adding(
                "Delay the planned change",
                "This work keeps the fixture honest.",
            )
        },
    ));
    claim_held(&bus, epoch, seat, &task);
    let refused = held_done(&bus, epoch, seat, &task);
    assert!(
        matches!(&refused, Outcome::Refusal(refusal)
            if refusal.reason() == "task::taste-review-required"),
        "a held plan completed: {refused:?}",
    );

    assert_committed(modify_with_repository(
        &bus,
        &repository,
        epoch,
        Actor::Lane(seat.lane),
        &task,
        "Ship the planned change",
    ));
    assert_eq!(
        flow_of(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task).0,
        "plan",
        "clearing the hold leaves the plan phase in place",
    );
    assert_committed(held_done(&bus, epoch, seat, &task));
    assert_eq!(
        flow_of(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task).0,
        "todo"
    );
    clean(&root, bus);
}

#[test]
fn concurrent_creator_and_holder_edits_serialize_on_the_statement_witness() {
    let (root, bus, epoch, seats) = fixture("taste-review-concurrent", 1);
    let repository = configured_repository(&root, &["delay"]);
    let lane = Actor::Lane(seats[0].lane);
    let (task, outcome) = add_with_repository(
        &bus,
        &repository,
        epoch,
        "TASTE-concur01",
        "Delay the lifecycle change",
        &["focused tests pass"],
    );
    assert_committed(outcome);
    claim_held(&bus, epoch, &seats[0], &task);
    let read = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let witness = statement_witness(at(&read, &task));
    let standing = (flow_of(&read, &task), claim_of(&read, &task));

    assert_committed(modify_witnessed(
        &bus,
        &repository,
        epoch,
        Actor::TEST_PRINCIPAL,
        &task,
        "Ship the lifecycle change",
        witness.clone(),
    ));
    let stale = modify_witnessed(
        &bus,
        &repository,
        epoch,
        lane.clone(),
        &task,
        "Ship the lifecycle change today",
        witness,
    );
    assert!(
        matches!(&stale, Outcome::Refusal(refusal) if refusal.reason() == "board::witness-moved"),
        "the holder's edit read before the creator's landed: {stale:?}",
    );
    assert_committed(modify_with_repository(
        &bus,
        &repository,
        epoch,
        lane,
        &task,
        "Ship the lifecycle change today",
    ));

    let after = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(at(&after, &task).statement.body.taste, None);
    assert_eq!(
        (flow_of(&after, &task), claim_of(&after, &task)),
        standing,
        "neither edit moved the phase or the claim",
    );
    clean(&root, bus);
}

#[test]
fn blank_term_is_refused_and_whitespace_is_absent_for_default_flow() {
    let (root, bus, epoch, _seats) = fixture("taste-review-blank-term", 0);
    let repository = configured_repository(&root, &["unrelated"]);
    let (_task, outcome) = add_with_repository(
        &bus,
        &repository,
        epoch,
        "TASTE-blank001",
        "Ship the lifecycle change",
        &["   "],
    );
    assert!(
        matches!(outcome, Outcome::Refusal(ref refusal) if refusal.reason() == "task::term-empty"),
        "blank term did not refuse: {outcome:?}",
    );
    clean(&root, bus);
}

fn claim_held(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
) -> wire::intent::ClaimAnswer {
    let lane = Actor::Lane(seat.lane);
    let available = board(bus, epoch, lane.clone());
    let won = assert_committed(issue(
        bus,
        epoch,
        lane,
        CommandKind::ClaimWin,
        claim_expected(&available, task, seat),
        &next_rule(),
    ));
    let CommandAnswer::Claim(won) = won else {
        panic!("claim win returned another answer shape");
    };
    *won
}

/// Complete `task`'s current phase under the claim `seat` holds. The hold
/// gate reads the statement's recorded hold, never the policy.
fn held_done(
    bus: &Bus,
    epoch: CanonicalUuid,
    seat: &Seat,
    task: &TaskHandle,
) -> Outcome<CommandAnswer> {
    let lane = Actor::Lane(seat.lane);
    let held = board(bus, epoch, lane.clone());
    issue(
        bus,
        epoch,
        lane,
        CommandKind::TaskDone,
        progress_witnesses(&held, at(&held, task), seat.lane, seat.occupant, false)
            .expect("held taste review has progress witnesses"),
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(task, "acceptance is already substantive")
        },
    )
}

#[test]
fn completion_refuses_in_any_phase_while_the_hold_stands() {
    let (root, bus, epoch, seats) = fixture("taste-review-done-gate", 1);
    let repository = configured_repository(&root, &["delay"]);
    let (task, outcome) = add_with_repository(
        &bus,
        &repository,
        epoch,
        "TASTE-done0001",
        "Delay the lifecycle change",
        &["focused tests pass"],
    );
    assert_committed(outcome);
    let won = claim_held(&bus, epoch, &seats[0], &task);
    assert_eq!(
        won.next.as_deref(),
        Some(format!("spice task modify {task} --title \"...\"").as_str()),
        "assignment named a read instead of the rewording the hold needs",
    );
    let standing = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(flow_of(&standing, &task).0, "todo");
    let phased = phase_facts(&bus, &task);

    let outcome = held_done(&bus, epoch, &seats[0], &task);
    let Outcome::Refusal(refusal) = outcome else {
        panic!("a held todo phase completed: {outcome:?}");
    };
    assert_eq!(refusal.reason(), "task::taste-review-required");
    assert_refusal_carries_the_hold(&refusal, &standing, &task, "not been rechecked");
    assert_eq!(
        flow_of(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task),
        flow_of(&standing, &task),
    );
    assert_eq!(
        phase_facts(&bus, &task),
        phased,
        "a refusal advances nothing"
    );

    assert_committed(modify_with_repository(
        &bus,
        &repository,
        epoch,
        Actor::Lane(seats[0].lane),
        &task,
        "Delay the lifecycle change once more",
    ));
    assert_eq!(
        at(&board(&bus, epoch, Actor::TEST_PRINCIPAL), &task)
            .statement
            .body
            .taste
            .as_ref()
            .map(|hold| hold.rechecks),
        Some(1),
        "a still-matching revision counts as one recheck",
    );
    let reworded = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let outcome = held_done(&bus, epoch, &seats[0], &task);
    let Outcome::Refusal(refusal) = outcome else {
        panic!("a reworded but still held phase completed: {outcome:?}");
    };
    assert_eq!(refusal.reason(), "task::taste-review-required");
    assert_refusal_carries_the_hold(&refusal, &reworded, &task, "1 recheck");

    clean(&root, bus);
}

/// A completion refusal carries the standing hold: each match the hold
/// records, by taste, trigger word, and field line, the recheck standing, and
/// a remedy that names a statement field, so the command a lane copies is an
/// edit rather than the empty modify the edit guard refuses.
fn assert_refusal_carries_the_hold(
    refusal: &Refusal,
    board: &wire::intent::BoardAnswer,
    task: &TaskHandle,
    standing: &str,
) {
    let hold = at(board, task)
        .statement
        .body
        .taste
        .clone()
        .expect("the refused statement stands held");
    let explanation = refusal.explanation();
    assert!(!hold.matches.is_empty(), "{hold:?}");
    for found in &hold.matches {
        let finding = found
            .finding
            .as_ref()
            .expect("a word match records its finding");
        let field = match found.field {
            wire::StatementField::Title => "title",
            wire::StatementField::Pitch => "pitch",
            wire::StatementField::Description => "description",
            wire::StatementField::Term => "term",
        };
        let named = format!(
            "taste {} matched word {:?} in {field} line {}",
            found.taste, finding.trigger, found.line
        );
        assert!(
            explanation.contains(&named),
            "{named} missing: {explanation}"
        );
    }
    assert!(
        explanation.contains(standing),
        "{standing} missing: {explanation}"
    );
    assert!(
        explanation.contains(&format!("spice task modify {task} --title \"...\"")),
        "the remedy must carry a statement field: {explanation}"
    );
}

#[test]
fn a_hold_speaks_on_status_while_its_match_stays_withheld_from_the_lane() {
    let (root, bus, epoch, seats) = fixture("taste-review-hold-visibility", 1);
    let repository = configured_repository(&root, &["delay"]);
    let (task, outcome) = add_with_repository(
        &bus,
        &repository,
        epoch,
        "TASTE-hold0001",
        "Delay the lifecycle change",
        &["focused tests pass"],
    );
    assert_committed(outcome);
    let lane = Actor::Lane(seats[0].lane);
    claim_held(&bus, epoch, &seats[0], &task);

    let held = board(&bus, epoch, lane.clone());
    let status = task_status(&held).expect("validated board yields status");
    assert!(
        status
            .lines()
            .any(|line| line == format!("taste-review {task}")),
        "status did not name the held task: {status}",
    );

    let withheld = |answer: &BoardAnswer| {
        let hold = match &answer.next {
            Some(NextWorkState::Active { task }) => task.statement.body.taste.clone(),
            _ => at(answer, &task).statement.body.taste.clone(),
        }
        .expect("the read carries the hold");
        hold.matches
            .iter()
            .map(|found| (found.field, found.line, found.finding.is_none()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        withheld(&held),
        [(wire::StatementField::Title, 1, true)],
        "the lane's board read names the place but withholds the finding",
    );
    let shown = task_show(&held, at(&held, &task), &[]);
    assert!(!shown.contains("delay"), "{shown}");
    assert!(!shown.contains("Rewrite the task wording"), "{shown}");

    let next = next_board(&bus, epoch, lane);
    assert_eq!(
        withheld(&next),
        [(wire::StatementField::Title, 1, true)],
        "the lane's task next read withholds the finding",
    );
    let owned = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_eq!(withheld(&owned), [(wire::StatementField::Title, 1, false)]);

    clean(&root, bus);
}
