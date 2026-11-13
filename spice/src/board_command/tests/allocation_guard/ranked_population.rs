use std::sync::Mutex;

use spice::rooms::evidence::Evidence;
use spice::rooms::profile::EvidenceKey;
use wire::body::LaneSessionDeclared;

use super::*;
use crate::resident::message_surface::MessageState;

/// A lane profile whose one retained card names both candidates' subjects
/// equally, so only how common each subject is on the ranked board splits
/// their scores.
fn even_profile(epoch: CanonicalUuid, seat: &Seat) -> Mutex<MessageState> {
    let mut state = MessageState::default();
    state.profiles.declare(&LaneSessionDeclared {
        lane: seat.lane,
        occupant: seat.occupant,
        worktree: "/fixture".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        launch: wire::body::LaneLaunchIdentity::new("model", "effort"),
        epoch,
        stream: "session".to_owned(),
        source: uuid(9_001),
        identity: "ranked-population".to_owned(),
        member: 1,
        offset: 2,
        length: 3,
    });
    state.profiles.insert(
        seat.lane,
        seat.occupant,
        Evidence::task(
            EvidenceKey::Task {
                task: wire::TaskStamp::for_micros(17).expect("stamp"),
                statement: wire::Seq::new(1).expect("seq"),
                completion: wire::Seq::new(2).expect("seq"),
                support: wire::rooms::DesignSupport::Implemented,
            },
            "kestrel heron",
            "",
            None,
            &[],
            "kestrel heron",
        ),
    );
    Mutex::new(state)
}

/// Complete `task`'s current phase as `seat` through a named capture, so the
/// allocator's own choice never decides fixture setup.
fn finish(bus: &Bus, epoch: CanonicalUuid, seat: &Seat, task: &TaskHandle) {
    capture_for(bus, epoch, seat, task);
    let held = board(bus, epoch, Actor::Lane(seat.lane));
    let expected = progress_witnesses(&held, at(&held, task), seat.lane, seat.occupant, false)
        .expect("held task has progress witnesses");
    assert_committed(issue(
        bus,
        epoch,
        Actor::Lane(seat.lane),
        CommandKind::TaskDone,
        expected,
        &TaskDone {
            operation: Some(completion_operation()),
            ..finished(task, "fixture completion")
        },
    ));
}

/// The task the read hands out is the task its claim admits on an unchanged
/// board, even when a candidate's settled dependency rides into the claim's
/// expanded snapshot: that context row repeats one candidate's subject, and
/// ranking it would make that subject commoner at claim time than at read.
#[test]
fn a_settled_dependency_cannot_rerank_the_claim_against_its_read() {
    let (root, bus, epoch, seats) = fixture("claim-ranked-population", 2);
    let reviewer = Actor::Lane(seats[1].lane);
    let kestrel = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "RANK-kestrel",
        "kestrel",
        None,
    );
    let heron = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "RANK-heron",
        "heron",
        None,
    );
    finish(&bus, epoch, &seats[0], &kestrel);
    finish(&bus, epoch, &seats[0], &heron);
    // Unscored, the reviewer's ladder breaks the tie toward the older row.
    let older = guard_winner(&board(&bus, epoch, reviewer.clone()), &reviewer);
    let (newer, subject) = if older == kestrel {
        (heron, "kestrel")
    } else {
        (kestrel, "heron")
    };
    // The settled row repeats the older candidate's subject: ranked, it would
    // make that subject commoner and hand the newer candidate the claim.
    let settled = add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "RANK-settled",
        &format!("{subject} {subject}"),
        Some("todo"),
    );
    finish(&bus, epoch, &seats[0], &settled);
    add_dependency(&bus, epoch, &newer, &settled);
    assert_eq!(
        guard_winner(&board(&bus, epoch, reviewer.clone()), &reviewer),
        older
    );

    let profiles = even_profile(epoch, &seats[1]);
    let observed = bus.task_snapshot().expect("task snapshot reads");
    let read = read_next_work(
        &bus,
        &observed,
        reviewer.clone(),
        &board::NextConfig::default(),
        Some(&profiles),
    )
    .expect("next-work read projects");
    let Some(wire::intent::NextWorkState::Candidate { task }) = read.next else {
        panic!("the reviewer reads one candidate: {:?}", read.next);
    };
    let candidate = task.statement.body.handle(task.workflow.body.phase);

    let view = board(&bus, epoch, reviewer.clone());
    let command = claim_next_command(epoch, &reviewer, &view, &candidate, &seats[1]);
    let plan = match claim_plan(
        &bus,
        &command,
        &next_rule(),
        None,
        &board::NextConfig::default(),
        Some(&profiles),
    ) {
        Ok(plan) => plan,
        Err(outcome) => panic!("the claim refuses the task its read handed out: {outcome:?}"),
    };
    assert_eq!(plan.selected.handle(), &candidate);
    drop(plan);
    clean(&root, bus);
}
