use super::*;
use wire::intent::{TaskTimeline, TaskTimelineAnswer, TaskTimelineEvent};

fn read_timeline(
    bus: &Bus,
    epoch: CanonicalUuid,
    request: &TaskTimeline,
) -> Outcome<CommandAnswer> {
    issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskTimeline,
        Vec::new(),
        request,
    )
}

fn page(
    bus: &Bus,
    epoch: CanonicalUuid,
    lanes: &[CanonicalUuid],
    before: Option<wire::Seq>,
    limit: u16,
) -> Box<TaskTimelineAnswer> {
    let outcome = read_timeline(
        bus,
        epoch,
        &TaskTimeline {
            lanes: lanes.to_vec(),
            before,
            limit,
        },
    );
    let Outcome::Reply(Reply {
        position: ReplyPosition::Snapshot(position),
        body: CommandAnswer::TaskTimeline(answer),
    }) = outcome
    else {
        panic!("task timeline did not return a snapshot: {outcome:?}")
    };
    assert_eq!(position, answer.snapshot);
    answer
}

type Row = (wire::Seq, CanonicalUuid, TaskHandle, TaskTimelineEvent);

fn rows(answer: &TaskTimelineAnswer) -> Vec<Row> {
    answer
        .entries
        .iter()
        .map(|entry| {
            (
                entry.position,
                entry.lane,
                entry.handle.clone(),
                entry.event.clone(),
            )
        })
        .collect()
}

#[test]
fn single_row_pages_walk_one_lane_back_to_its_first_transition_under_later_commits() {
    let (root, bus, epoch, seats) = fixture("task-timeline-paging", 2);
    let (first, second) = (&seats[0], &seats[1]);
    let first_actor = Actor::Lane(first.lane);
    let second_actor = Actor::Lane(second.lane);
    let held = todo_task(&bus, epoch, "TL-held00001", "Hold then hand over");
    claim_task(&bus, epoch, first, &first_actor, &held);
    free_active(&bus, epoch, &first_actor, first, &held);
    claim_task(&bus, epoch, second, &second_actor, &held);
    let kept = todo_task(&bus, epoch, "TL-kept00001", "Keep possession");
    claim_task(&bus, epoch, first, &first_actor, &kept);

    let whole = page(&bus, epoch, &[first.lane], None, 100);
    assert!(
        whole.next.is_none(),
        "one lane's whole history fits one page"
    );
    let whole = rows(&whole);
    let events = whole
        .iter()
        .map(|(_, lane, handle, event)| (*lane, handle.clone(), event.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        events,
        [
            (first.lane, kept.clone(), TaskTimelineEvent::Claimed {}),
            (first.lane, held.clone(), TaskTimelineEvent::Released {}),
            (first.lane, held.clone(), TaskTimelineEvent::Claimed {}),
        ],
        "operator-authored additions and another lane's claim stay out"
    );

    let mut walked = Vec::new();
    let mut before = None;
    let mut interleaved = false;
    // A cursor that repeats its boundary row would page forever; one page
    // past the whole history is already a failure.
    for _ in 0..=whole.len() {
        let answer = page(&bus, epoch, &[first.lane], before, 1);
        walked.extend(rows(&answer));
        let Some(next) = answer.next else { break };
        if !interleaved {
            // A transition committed between pages is newer than every
            // cursor already issued, so it cannot shift an older page.
            free_active(&bus, epoch, &first_actor, first, &kept);
            interleaved = true;
        }
        before = Some(next);
    }
    assert_eq!(walked, whole, "paging skipped or repeated a transition");

    let newest = page(&bus, epoch, &[first.lane], None, 1);
    assert_eq!(
        rows(&newest)
            .into_iter()
            .map(|(_, _, handle, event)| (handle, event))
            .collect::<Vec<_>>(),
        [(kept, TaskTimelineEvent::Released {})],
        "the newest page reports the transition committed while paging"
    );

    let team = page(&bus, epoch, &[second.lane, first.lane], None, 100);
    assert_eq!(team.lanes.len(), 2);
    assert!(
        team.lanes[0] < team.lanes[1],
        "lanes answer in canonical order"
    );
    assert_eq!(team.entries.len(), 5);
    assert!(
        team.entries
            .windows(2)
            .all(|pair| pair[0].position > pair[1].position),
        "a team page interleaves members in descending journal order"
    );
    assert_eq!(
        (
            team.entries[2].lane,
            &team.entries[2].handle,
            &team.entries[2].event
        ),
        (second.lane, &held, &TaskTimelineEvent::Claimed {}),
        "the member that picked up released work is attributed its own claim"
    );
    clean(&root, bus);
}

#[test]
fn malformed_lane_sets_and_page_limits_are_refused() {
    let (root, bus, epoch, seats) = fixture("task-timeline-refusals", 1);
    let lane = seats[0].lane;
    for (request, reason) in [
        (
            TaskTimeline {
                lanes: Vec::new(),
                before: None,
                limit: 1,
            },
            "task::timeline-lanes-invalid",
        ),
        (
            TaskTimeline {
                lanes: vec![lane; wire::intent::TASK_TIMELINE_LANE_LIMIT + 1],
                before: None,
                limit: 1,
            },
            "task::timeline-lanes-invalid",
        ),
        (
            TaskTimeline {
                lanes: vec![lane, lane],
                before: None,
                limit: 1,
            },
            "task::timeline-lanes-duplicate",
        ),
        (
            TaskTimeline {
                lanes: vec![lane],
                before: None,
                limit: 0,
            },
            "task::timeline-limit-invalid",
        ),
        (
            TaskTimeline {
                lanes: vec![lane],
                before: None,
                limit: wire::intent::TASK_TIMELINE_PAGE_LIMIT_MAX + 1,
            },
            "task::timeline-limit-invalid",
        ),
    ] {
        let outcome = read_timeline(&bus, epoch, &request);
        assert!(
            matches!(&outcome, Outcome::Refusal(refusal) if refusal.reason() == reason),
            "{request:?} was not refused as {reason}: {outcome:?}"
        );
    }
    clean(&root, bus);
}
