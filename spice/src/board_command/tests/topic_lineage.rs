//! The board's tree continues across rewrites of a held task that leave the
//! text the engine reads unchanged, and regrows when that text changes.

use super::*;
use crate::board_command::topic_tree::TopicTree;
use wire::topic::{GroupingFreshness, GroupingRebuild, GroupingRecovery};

fn preview(bus: &Bus, repository: &Repository, tree: &TopicTree) -> Outcome<CommandAnswer> {
    super::super::topic_tree::preview(
        bus,
        repository,
        &Actor::TEST_PRINCIPAL,
        tree,
        "Inspect the archive",
        "Find the release manifest.",
    )
}

fn view(bus: &Bus, repository: &Repository, tree: &TopicTree) -> GroupingFreshness {
    let Outcome::Reply(Reply {
        body: CommandAnswer::Topic(answer),
        ..
    }) = preview(bus, repository, tree)
    else {
        panic!("expected a published tree")
    };
    answer.grouping.unwrap().freshness
}

fn opened(bus: &Bus, repository: &Repository) -> TopicTree {
    let tree = TopicTree::default();
    assert_eq!(
        refusal_reason(&preview(bus, repository, &tree)),
        "topic::preview-catching-up"
    );
    tree.update(&bus.task_snapshot().unwrap());
    tree
}

#[test]
fn a_held_task_starts_a_new_lineage_only_when_its_evidence_changes() {
    let (root, bus, epoch, _) = fixture("topic-lineage-held-edit", 0);
    let [archive, manifest] = [
        ("lineage-archive", "Seal the release archive"),
        ("lineage-manifest", "Seal the release manifest"),
    ]
    .map(|(task, title)| {
        add_task_in_topic(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            task,
            title,
            "/board/test",
            Some("todo"),
        )
    });
    let repository = Repository::discover(&*root).expect("fixture repository resolves");
    let tree = opened(&bus, &repository);
    let first = view(&bus, &repository, &tree);

    // A dependency edge rewrites the statement head without touching its text.
    add_dependency(&bus, epoch, &archive, &manifest);
    tree.update(&bus.task_snapshot().unwrap());
    let kept = view(&bus, &repository, &tree);
    assert_eq!(kept.lineage, first.lineage);
    assert!(kept.projection > first.projection);

    assert_committed(correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            title: Some("Publish the signed installer".to_owned()),
            ..only(&archive)
        },
    ));
    tree.update(&bus.task_snapshot().unwrap());
    let regrown = view(&bus, &repository, &tree);
    assert_ne!(regrown.lineage, first.lineage);
    assert_eq!(
        regrown.recovery,
        GroupingRecovery::Rebuilt {
            reason: GroupingRebuild::InputChanged,
        }
    );
    clean(&root, bus);
}

#[test]
fn an_older_view_can_survive_one_edit_but_not_a_member_withdrawal() {
    let (root, bus, epoch, _) = fixture("topic-checked-lag", 0);
    let [one, two, _] = [
        ("lag-optics", "Align an optical bench"),
        ("lag-mirror", "Polish the telescope mirror"),
        ("lag-lens", "Clean the telescope lens"),
    ]
    .map(|(id, title)| {
        add_task_in_topic(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            id,
            title,
            "/board/test",
            Some("todo"),
        )
    });
    let repository = Repository::discover(&*root).unwrap();
    let tree = opened(&bus, &repository);
    let original = view(&bus, &repository, &tree);
    assert_committed(correct(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        &TaskEdit {
            title: Some("Catalogue the shipping crates".into()),
            ..only(&one)
        },
    ));
    let lagged = view(&bus, &repository, &tree);
    assert_eq!(lagged.lineage, original.lineage);
    assert_eq!(lagged.projection, original.projection);
    assert!(lagged.checked_against > original.projection);

    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDelete,
        transition_witnesses(at(&visible, &two)),
        &TaskDelete {
            task: two,
            reason: "withdraw the mirror inspection".into(),
            force_claimed: false,
        },
    ));
    let observed = bus.task_snapshot().unwrap();
    let result = tree.with_current(&observed, |_| ());
    assert!(
        matches!(result, Err(Outcome::Refusal(r)) if r.reason() == "topic::preview-catching-up")
    );
    clean(&root, bus);
}
