use super::*;

use wire::intent::{SweepPreview, SweepSplit, SweepTestTime, SweepText, TaskSweep};

fn stamp(micros: i64) -> TaskStamp {
    TaskStamp::for_micros(micros).expect("fixture task stamp")
}

fn text(title: String, term: Vec<String>) -> SweepText {
    SweepText {
        title,
        pitch: "Repairing inherited bound debt once stops lanes from colliding over it.".to_owned(),
        description: "file_loc path=\"src/a.rs\": measured 812 over base 600".to_owned(),
        term,
    }
}

fn preview(until: Option<Instant>, files: &[&str]) -> SweepPreview {
    SweepPreview {
        topic: "/gate".parse().expect("fixture topic"),
        until,
        plan: text(
            format!("Sweep latched bound debt across {} files", files.len()),
            Vec::new(),
        ),
        children: files
            .iter()
            .map(|file| {
                text(
                    format!("Heal latched bound debt in {file}"),
                    vec![format!("file_loc path={file:?} is no longer latched")],
                )
            })
            .collect(),
    }
}

/// Issue one sweep naming `digest` against the board as it stands now, with
/// the second child's per-test report attached.
fn file(
    bus: &Bus,
    epoch: CanonicalUuid,
    digest: Digest,
    content: SweepPreview,
    plan: &TaskStamp,
    children: &[TaskStamp],
) -> Outcome<CommandAnswer> {
    let current = board(bus, epoch, Actor::TEST_PRINCIPAL);
    let mut expected = vec![
        Witness::Preview {
            digest: digest.clone(),
        },
        Witness::Barrier {
            name: "board".parse().expect("board barrier name"),
            digest: current.task_barrier.clone(),
        },
        Witness::Absence { task: plan.clone() },
    ];
    expected.extend(
        children
            .iter()
            .map(|task| Witness::Absence { task: task.clone() }),
    );
    let splits = (children.len() > 1)
        .then(|| SweepSplit {
            child: 1,
            tests: vec![SweepTestTime {
                target: "lib:fixture".to_owned(),
                name: "suite::slow".to_owned(),
                millis: 2_500,
            }],
        })
        .into_iter()
        .collect();
    issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskSweep,
        expected,
        &TaskSweep {
            preview: digest,
            plan: plan.clone(),
            children: children.to_vec(),
            content,
            splits,
        },
    )
}

#[test]
fn a_sweep_files_one_deferred_plan_with_controlled_draft_children() {
    let (root, bus, epoch, _seats) = fixture("sweep-filing", 0);
    let until: Instant = "2027-01-01T00:00:00.000Z".parse().expect("fixture instant");
    let plan = stamp(1_760_000_000_200_000);
    let children = [stamp(1_760_000_000_200_001), stamp(1_760_000_000_200_002)];
    let content = preview(Some(until), &["src/a.rs", "src/b.rs"]);
    let digest = content.digest();

    let CommandAnswer::Task(answer) = assert_committed(file(
        &bus,
        epoch,
        digest.clone(),
        content.clone(),
        &plan,
        &children,
    )) else {
        panic!("a sweep answers with its Plan");
    };
    let view = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let filed = at(&view, &answer.task);
    assert_eq!(filed.statement.body.born, plan);
    assert_eq!(filed.statement.body.intent, Some(wire::TaskIntent::Plan));
    assert_eq!(
        filed.workflow.body.deferral,
        Some(until),
        "the Plan waits until INSTANT"
    );
    for (child, text) in children.iter().zip(&content.children) {
        let heads = view
            .tasks
            .iter()
            .find(|heads| &heads.statement.body.born == child)
            .expect("each file's child is filed");
        assert_eq!(heads.presentation_title(), text.title);
        assert_eq!(heads.statement.body.term, text.term);
        let description = heads
            .statement
            .body
            .description
            .as_deref()
            .unwrap_or_default();
        assert!(
            description.starts_with(&text.description),
            "a child keeps its previewed description: {description}"
        );
        assert!(
            heads.statement.body.dependencies.is_empty(),
            "lifecycle control does not create an ordinary dependency"
        );
        let preparation = heads
            .statement
            .body
            .preparation
            .as_ref()
            .expect("a child retains the Plan's construction lineage");
        assert_eq!(preparation.parent, plan);
        assert_eq!(preparation.controller, plan);
        assert_eq!(preparation.construction, *child);
        assert!(heads.workflow.body.is_draft());
        assert!(!heads.workflow.body.submitted);
        assert_eq!(heads.workflow.body.deferral, None);
    }

    let tasks = view.tasks.len();
    let replay = file(&bus, epoch, digest, content, &plan, &children);
    assert!(matches!(replay, Outcome::Refusal(_)), "{replay:?}");
    assert_eq!(
        board(&bus, epoch, Actor::TEST_PRINCIPAL).tasks.len(),
        tasks,
        "a repeated apply files no duplicate"
    );
    clean(&root, bus);
}

#[test]
fn a_sweep_refuses_content_or_a_board_moved_since_its_preview() {
    let (root, bus, epoch, _seats) = fixture("sweep-filing-moved", 0);
    let plan = stamp(1_760_000_000_300_000);
    let children = [stamp(1_760_000_000_300_001)];
    let previewed = preview(None, &["src/a.rs"]);

    let moved = file(
        &bus,
        epoch,
        previewed.digest(),
        preview(None, &["src/b.rs"]),
        &plan,
        &children,
    );
    assert!(
        matches!(&moved, Outcome::Refusal(refusal) if refusal.reason() == "sweep::preview-moved"),
        "{moved:?}"
    );

    let barrier = board(&bus, epoch, Actor::TEST_PRINCIPAL).task_barrier;
    add(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        "1B6QBLpz",
        "Unrelated board mutation",
        Some("todo"),
    );
    let stale = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskSweep,
        vec![
            Witness::Preview {
                digest: previewed.digest(),
            },
            Witness::Barrier {
                name: "board".parse().expect("board barrier name"),
                digest: barrier,
            },
            Witness::Absence { task: plan.clone() },
            Witness::Absence {
                task: children[0].clone(),
            },
        ],
        &TaskSweep {
            preview: previewed.digest(),
            plan,
            children: children.to_vec(),
            content: previewed,
            splits: Vec::new(),
        },
    );
    assert!(
        matches!(&stale, Outcome::Refusal(refusal) if refusal.reason() == "sweep::barrier-moved"),
        "{stale:?}"
    );
    clean(&root, bus);
}
