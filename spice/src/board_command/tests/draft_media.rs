use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::*;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
const GIF: &[u8] = b"GIF89a";

fn image(name: &str, content_type: &str, bytes: &[u8]) -> wire::Media {
    wire::Media::from_base64(name, content_type, &STANDARD.encode(bytes))
        .expect("fixture image admits")
}

fn draft(bus: &Bus, epoch: CanonicalUuid, name: &str) -> TaskHandle {
    let born = stamp(name);
    assert_committed(issue(
        bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskAdd,
        vec![Witness::Absence { task: born.clone() }],
        &TaskAdd {
            topic: Some("/board/test".parse().expect("fixture topic")),
            term: vec!["the fixture work is verified".to_owned()],
            origin: Some(fixture_origin()),
            draft: true,
            ..adding(
                format!("Prospective {name}"),
                "This work keeps the fixture honest.",
            )
        },
    ));
    TaskHandle::for_phase(
        Some(&"/board/test".parse().expect("fixture topic")),
        &born,
        wire::Phase::Draft,
    )
}

fn resident_store(root: &Path) -> (Repository, artifact::Store) {
    let repository = Repository::discover(root).expect("fixture repository resolves");
    let state = repository
        .admit_state()
        .expect("fixture repository state admits");
    fs::create_dir_all(state.artifacts()).expect("artifact directory exists");
    fs::create_dir_all(state.locks()).expect("artifact lock directory exists");
    let store = artifact::Store::new(state.artifacts(), state.artifacts_lock());
    (repository, store)
}

fn dispatch<T: serde::Serialize>(
    bus: &Bus,
    repository: &Repository,
    epoch: CanonicalUuid,
    kind: CommandKind,
    expected: Vec<Witness>,
    body: &T,
) -> Outcome<CommandAnswer> {
    let command = fixture_command(epoch, Actor::TEST_PRINCIPAL, kind, expected, body);
    dispatch_with_repository(
        bus,
        &command,
        Some(host(repository)),
        &board::NextConfig::default(),
        None,
    )
    .expect("board command owns the kind")
}

fn replace(
    bus: &Bus,
    repository: &Repository,
    epoch: CanonicalUuid,
    task: &TaskHandle,
    media: Vec<wire::Media>,
) -> Outcome<CommandAnswer> {
    let born = task.born().expect("fixture handle carries a birth stamp");
    dispatch(
        bus,
        repository,
        epoch,
        CommandKind::TaskMedia,
        vec![Witness::Existence { task: born }],
        &wire::intent::TaskMedia {
            task: task.clone(),
            media,
        },
    )
}

/// Every stored image of `task` as name and exact bytes, in manifest order.
fn images(store: &artifact::Store, task: &TaskHandle) -> Vec<(String, Vec<u8>)> {
    let born = task.born().expect("fixture handle carries a birth stamp");
    store
        .list(&born)
        .expect("artifacts list")
        .into_iter()
        .filter(|artifact| artifact.media_type.is_image())
        .map(|artifact| {
            let (artifact, bytes) = store.show(&born, &artifact.id).expect("image reads");
            (artifact.name, bytes)
        })
        .collect()
}

fn detail_media(
    bus: &Bus,
    repository: &Repository,
    epoch: CanonicalUuid,
    task: &TaskHandle,
) -> Vec<wire::Media> {
    let born = task.born().expect("fixture handle carries a birth stamp");
    let Outcome::Reply(Reply {
        body: CommandAnswer::TaskDetail(answer),
        ..
    }) = dispatch(
        bus,
        repository,
        epoch,
        CommandKind::TaskDetail,
        Vec::new(),
        &wire::intent::TaskDetail { task: born },
    )
    else {
        panic!("task detail did not answer")
    };
    answer.media
}

fn replaced(
    bus: &Bus,
    repository: &Repository,
    epoch: CanonicalUuid,
    task: &TaskHandle,
    media: Vec<wire::Media>,
) {
    let outcome = replace(bus, repository, epoch, task, media);
    assert!(matches!(outcome, Outcome::Reply(_)), "{outcome:?}");
}

#[test]
fn task_media_replaces_exactly_the_draft_image_set_and_keeps_other_artifacts() {
    let (root, bus, epoch, _) = fixture("draft-media-replace", 0);
    let task = draft(&bus, epoch, "DRAFT-media001");
    let (repository, store) = resident_store(&root);
    let born = task.born().expect("fixture handle carries a birth stamp");
    store
        .add_bytes(artifact::AddBytesRequest {
            task: &born,
            bytes: b"diagnostic text",
            name: "diagnostic.txt",
            media_type: Some(artifact::MediaType::TEXT_PLAIN),
            retention: artifact::Retention::Permanent,
            created_at: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
        })
        .expect("text artifact publishes");

    let first = vec![
        image("capture.png", "image/png", PNG),
        image("sketch.gif", "image/gif", GIF),
    ];
    replaced(&bus, &repository, epoch, &task, first.clone());
    assert_eq!(
        images(&store, &task),
        vec![
            ("capture.png".to_owned(), PNG.to_vec()),
            ("sketch.gif".to_owned(), GIF.to_vec())
        ]
    );
    // Reopening rebuilds the editor from task detail, so detail must return
    // the replaced set itself in presentation order.
    assert_eq!(detail_media(&bus, &repository, epoch, &task), first);

    // Reordering is a change: the stored order follows the sent order.
    let reordered = vec![first[1].clone(), first[0].clone()];
    replaced(&bus, &repository, epoch, &task, reordered.clone());
    assert_eq!(detail_media(&bus, &repository, epoch, &task), reordered);

    replaced(&bus, &repository, epoch, &task, vec![first[1].clone()]);
    assert_eq!(
        images(&store, &task),
        vec![("sketch.gif".to_owned(), GIF.to_vec())]
    );

    replaced(&bus, &repository, epoch, &task, Vec::new());
    assert!(images(&store, &task).is_empty());
    let kept = store.list(&born).expect("artifacts list");
    assert_eq!(kept.len(), 1, "the non-image artifact stays");
    assert_eq!(kept[0].name, "diagnostic.txt");

    clean(&root, bus);
}

#[test]
fn task_media_count_refusal_preserves_a_full_preceding_artifact_set() {
    let (root, bus, epoch, _) = fixture("draft-media-artifact-bound", 0);
    let task = draft(&bus, epoch, "DRAFT-media-bound");
    let (repository, store) = resident_store(&root);
    let born = task.born().expect("fixture handle carries a birth stamp");
    let kept = vec![image("capture.png", "image/png", PNG)];
    replaced(&bus, &repository, epoch, &task, kept);
    let bound =
        usize::try_from(artifact::ArtifactBudget::of(0).bound).expect("artifact bound fits usize");
    for index in 0..bound - 1 {
        store
            .add_bytes(artifact::AddBytesRequest {
                task: &born,
                bytes: b"retained",
                name: &format!("retained-{index}.txt"),
                media_type: Some(artifact::MediaType::TEXT_PLAIN),
                retention: artifact::Retention::Permanent,
                created_at: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
            })
            .expect("retained artifact publishes");
    }
    let before = store.list(&born).expect("full preceding set");
    assert_eq!(before.len(), bound);

    let outcome = replace(
        &bus,
        &repository,
        epoch,
        &task,
        vec![
            image("first.png", "image/png", PNG),
            image("second.gif", "image/gif", GIF),
        ],
    );

    assert_eq!(refusal_reason(&outcome), "artifact::operation-refused");
    assert_eq!(store.list(&born).expect("set after refusal"), before);
    assert_eq!(
        images(&store, &task),
        vec![("capture.png".to_owned(), PNG.to_vec())]
    );
    clean(&root, bus);
}

#[test]
fn task_media_refuses_inadmissible_sets_and_non_draft_tasks_without_changing_artifacts() {
    let (root, bus, epoch, _) = fixture("draft-media-refusals", 0);
    let task = draft(&bus, epoch, "DRAFT-media002");
    let (repository, store) = resident_store(&root);
    let kept = vec![image("capture.png", "image/png", PNG)];
    replaced(&bus, &repository, epoch, &task, kept.clone());
    let before = images(&store, &task);
    assert_eq!(before, vec![("capture.png".to_owned(), PNG.to_vec())]);

    let over_count = (0..=wire::media::MAX_MEDIA_ITEMS)
        .map(|index| image(&format!("capture-{index}.png"), "image/png", PNG))
        .collect();
    let unrecognized = vec![wire::Media {
        name: "capture.png".to_owned(),
        content_type: "image/png".to_owned(),
        data_url: format!("data:image/png;base64,{}", STANDARD.encode(b"not an image")),
    }];
    let mut large = b"RIFF\0\0\0\0WEBP".to_vec();
    large.resize(wire::media::MAX_MEDIA_BYTES + 1, 0);
    let oversized = vec![wire::Media {
        name: "capture.webp".to_owned(),
        content_type: "image/webp".to_owned(),
        data_url: format!("data:image/webp;base64,{}", STANDARD.encode(large)),
    }];
    for set in [over_count, unrecognized, oversized] {
        let outcome = replace(&bus, &repository, epoch, &task, set);
        assert_eq!(refusal_reason(&outcome), "task::media-refused");
        assert_eq!(images(&store, &task), before);
    }

    let filed = add_born(&bus, epoch, stamp("media-filed"), "Filed work");
    let outcome = replace(
        &bus,
        &repository,
        epoch,
        &filed,
        vec![image("capture.png", "image/png", PNG)],
    );
    assert_eq!(refusal_reason(&outcome), "task::media-draft-only");
    assert!(images(&store, &filed).is_empty());

    let visible = board(&bus, epoch, Actor::TEST_PRINCIPAL);
    let heads = at(&visible, &task);
    assert_committed(issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::TaskDelete,
        transition_witnesses(heads),
        &TaskDelete {
            task: task.clone(),
            reason: "superseded".to_owned(),
            force_claimed: false,
        },
    ));
    let outcome = replace(&bus, &repository, epoch, &task, Vec::new());
    assert!(matches!(outcome, Outcome::Refusal(_)), "{outcome:?}");
    assert_eq!(images(&store, &task), before);

    clean(&root, bus);
}
