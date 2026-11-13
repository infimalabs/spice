use std::sync::Mutex;

use spice::rooms::evidence::Evidence;
use spice::rooms::profile::{EvidenceKey, LaneProfiles};
use wire::Seq;
use wire::body::LaneSessionDeclared;

use super::super::super::message_surface::MessageState;
use super::*;

/// Declare one retained session for `lane` holding a completed task card
/// titled `title`, keyed apart from either board task.
fn retain(profiles: &mut LaneProfiles, lane: CanonicalUuid, key: u64, title: &str) {
    let occupant = uuid(90 + u8::try_from(key).expect("small key"));
    profiles.declare(&LaneSessionDeclared {
        lane,
        occupant,
        worktree: "/worktree".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        launch: wire::body::LaneLaunchIdentity::new("model", "effort"),
        epoch: epoch(),
        stream: "session".to_owned(),
        source: uuid(80),
        identity: format!("session-{key}"),
        member: 1,
        offset: 2,
        length: 3,
    });
    profiles.insert(
        lane,
        occupant,
        Evidence::task(
            EvidenceKey::Task {
                task: TaskStamp::for_micros(i64::try_from(key).expect("key")).expect("stamp"),
                statement: Seq::new(key * 2 + 1).expect("seq"),
                completion: Seq::new(key * 2 + 2).expect("seq"),
                support: wire::rooms::DesignSupport::Implemented,
            },
            title,
            "",
            None,
            &[],
            "checked",
        ),
    );
}

fn selected<'a>(targets: &[Target<'a>], lane: CanonicalUuid) -> &'a Entry {
    targets
        .iter()
        .find(|target| target.lane == lane)
        .and_then(|target| target.task)
        .expect("a vacant drain member selects ready work")
}

/// Two vacant teammates share one priority and weight group of two ready
/// rows. Without profiles both take the unscored ladder's top row; with each
/// lane's retained session matching a different row, each selects its own
/// match and its wake rank is that row's score.
#[test]
fn each_vacant_lane_selects_the_task_its_retained_sessions_match() {
    let (bus, mut projection) = policy_fixture(true, false);
    add_vacant_org_members(&bus, &[lane_at(2)]);
    committed(
        bus.commit(titled_ready_task_write(
            NEWER_TASK_MOMENT,
            instant(7),
            "Newer ready lifecycle candidate",
        ))
        .expect("newer ready candidate commits"),
    );
    let snapshot = bus.task_snapshot().expect("two-row snapshot reads");
    projection
        .advance(&snapshot)
        .expect("two-row projection advances");
    let targets_with = |profiles: Option<&Mutex<MessageState>>| {
        project_targets(
            &snapshot,
            &projection,
            instant(50),
            &Holds::new(3),
            &board::NextConfig::default(),
            profiles,
            &owners(&snapshot),
        )
        .expect("two-row targets project")
    };

    let unscored = targets_with(None);
    assert_eq!(selected(&unscored, lane()).born(), &task(TASK_MOMENT));
    assert_eq!(selected(&unscored, lane_at(2)).born(), &task(TASK_MOMENT));
    assert!(unscored.iter().all(|target| target.matched.is_none()));

    let mut state = MessageState::default();
    retain(
        &mut state.profiles,
        lane(),
        1,
        "Newer ready lifecycle candidate",
    );
    retain(
        &mut state.profiles,
        lane_at(2),
        2,
        "Project incremental lifecycle",
    );
    let profiles = Mutex::new(state);
    let scored = targets_with(Some(&profiles));
    assert_eq!(selected(&scored, lane()).born(), &task(NEWER_TASK_MOMENT));
    assert_eq!(selected(&scored, lane_at(2)).born(), &task(TASK_MOMENT));
    assert!(
        scored
            .iter()
            .all(|target| target.matched.is_some_and(|score| score > 0.0)),
        "each lane's wake rank is its own selected row's score: {scored:?}"
    );

    // A lane whose retained work shares one term of the newer row under forty
    // unrelated ones scores it positively but below the wide-reach floor, so it
    // selects as today and carries no wake rank.
    let faint = (b'a'..=b'z')
        .flat_map(|first| [(first, b'x'), (first, b'y')])
        .fold("candidate".to_owned(), |text, (first, second)| {
            format!("{text} {}{}filler", char::from(first), char::from(second))
        });
    let mut state = MessageState::default();
    retain(&mut state.profiles, lane(), 3, &faint);
    let profiles = Mutex::new(state);
    let floored = targets_with(Some(&profiles));
    assert_eq!(selected(&floored, lane()).born(), &task(TASK_MOMENT));
    assert!(floored.iter().all(|target| target.matched.is_none()));
    bus.shutdown().expect("two-row fixture stops");
}
