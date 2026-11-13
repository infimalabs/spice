//! Terminal history scoped to one app team.

use super::*;

fn team_page(
    bus: &Bus,
    epoch: CanonicalUuid,
    team: CanonicalUuid,
    before: Option<TaskHistoryCursor>,
    limit: u16,
) -> BoardAnswer {
    let read = BoardRead::History {
        before,
        query: None,
        completed_before: None,
        team: Some(team),
        limit,
    };
    match issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardRead,
        Vec::new(),
        &read,
    ) {
        Outcome::Reply(Reply {
            body: CommandAnswer::Board(answer),
            ..
        }) => *answer,
        outcome => panic!("team history read failed: {outcome:?}"),
    }
}

fn page_handles(answer: &BoardAnswer) -> Vec<TaskHandle> {
    answer
        .tasks
        .iter()
        .map(|task| task.statement.body.handle.clone())
        .collect()
}

/// A team's history is what it created plus what a lane now in the team
/// finished, paged newest first across the filtered rows only.
#[test]
fn team_history_keeps_created_and_member_completed_work_across_pages() {
    const PAGE: u16 = 2;
    let (root, bus, epoch, seats) = fixture("team-history", 2);
    let (member, outsider) = (&seats[0], &seats[1]);
    let member_lane = Actor::Lane(member.lane);
    let outsider_lane = Actor::Lane(outsider.lane);
    authorize_lane_creation(&bus, epoch, member);

    let created = add(
        &bus,
        epoch,
        member_lane.clone(),
        "HIST-created",
        "Created by the team",
        Some("todo"),
    );
    claim_and_complete_task(&bus, epoch, outsider, &outsider_lane, &created);
    let first = todo_task(&bus, epoch, "HIST-first", "Finished by a member");
    claim_and_complete_task(&bus, epoch, member, &member_lane, &first);
    let foreign = todo_task(&bus, epoch, "HIST-foreign", "Finished elsewhere");
    claim_and_complete_task(&bus, epoch, outsider, &outsider_lane, &foreign);
    let last = todo_task(&bus, epoch, "HIST-last", "Finished by a member again");
    claim_and_complete_task(&bus, epoch, member, &member_lane, &last);

    let head = team_page(&bus, epoch, member.team, None, PAGE);
    assert_eq!(page_handles(&head), [last.clone(), first.clone()]);
    let metadata = head.history.expect("team history carries page metadata");
    assert_eq!(metadata.total, 3);
    let tail = team_page(&bus, epoch, member.team, metadata.next, PAGE);
    assert_eq!(page_handles(&tail), std::slice::from_ref(&created));
    assert_eq!(tail.history.expect("tail page metadata").next, None);

    let unknown = team_page(
        &bus,
        epoch,
        uuid(NEXT_COMMAND.fetch_add(1, Ordering::Relaxed)),
        None,
        PAGE,
    );
    assert!(unknown.tasks.is_empty());
    assert_eq!(unknown.history.expect("unknown team page").total, 0);

    super::subscription::move_fixture_lane_to_team(&bus, epoch, outsider, member.team);
    let joined = team_page(&bus, epoch, member.team, None, TASK_HISTORY_PAGE_LIMIT_MAX);
    assert_eq!(
        page_handles(&joined),
        [last, foreign, first, created],
        "a lane's past completions follow it into its current team",
    );
    clean(&root, bus);
}

/// Membership follows the lane a claim is bound to, not the principal that
/// won it: an operator claim a member lane carried to completion is the team's.
#[test]
fn team_history_follows_the_claim_lane_not_the_winning_principal() {
    const OPERATOR_CLAIMED_MICROS: i64 = 400_000_000_000;
    let (root, bus, epoch, seats) = fixture("team-history-operator-claim", 1);
    let member = seats[0].clone();
    let origin: wire::Origin = "fixture:team-history".parse().expect("fixture origin");
    let at = Instant::new(jiff::Timestamp::now()).expect("current fixture instant");
    let mut write = JournalWrite::new(
        Actor::TEST_PRINCIPAL,
        origin.clone(),
        at,
        Producer::CommandDoor,
    );
    let task = super::read_scope::push_terminal_task(
        &mut write,
        &wire::TaskProvenance::OperatorDraft,
        &body::TaskCreationContext::unavailable(body::TaskCreationUnavailable::PrincipalActor),
        super::read_scope::TerminalTaskFixture {
            micros: OPERATOR_CLAIMED_MICROS,
            title: "Operator claim on a member lane".to_owned(),
            description: None,
            topic: "/board/test".parse().expect("fixture topic"),
            completion: at,
        },
    );
    let cell = wire::ClaimCell {
        topic: Some("/board/test".parse().expect("fixture topic")),
        phase: "todo".parse().expect("fixture phase"),
    };
    let won = write
        .fact(
            body::ClaimWon {
                task: task.clone(),
                actor: Actor::TEST_PRINCIPAL,
                lane: member.lane,
                occupant: member.occupant,
                epoch,
                cell: cell.clone(),
                until: at,
                git: None,
                recovery: None,
                source: body::SourceBoundary::Proven { heads: Vec::new() },
            },
            FactCause::Root,
        )
        .expect("operator claim fact builds");
    write.claim_head(task.clone(), won, move |fact| wire::ClaimHead {
        actor: Actor::TEST_PRINCIPAL,
        fact,
        claimed: fact,
        cell,
        lane: member.lane,
        occupant: member.occupant,
        epoch,
        recovery: None,
        disposition: wire::ClaimDisposition::Freed,
        acknowledged: 0,
    });
    assert!(matches!(
        bus.commit(write).expect("operator claim reaches writer"),
        CommitOutcome::Committed(_)
    ));

    let page = team_page(&bus, epoch, member.team, None, TASK_HISTORY_PAGE_LIMIT_MAX);
    let borns: Vec<_> = page
        .tasks
        .iter()
        .map(|row| row.statement.body.born.clone())
        .collect();
    assert_eq!(borns, [task]);
    clean(&root, bus);
}
