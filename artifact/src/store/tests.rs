use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use wire::{Instant, TaskHandle, TaskStamp};

use super::capture::{admit_backup_bytes, within_member_bound};
use super::{
    AddBytesRequest, AddRequest, Error, ErrorClass, ImageBytes, MediaType, ReplaceImagesRequest,
    Retention, Store, object_dir, payload_path,
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    base: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "spice-artifact-test-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).expect("create artifact fixture");
        let store = Store::new(base.join("tasks"), base.join("locks/artifacts.lock"));
        Self { base, store }
    }

    fn source(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let sources = self.base.join("sources");
        fs::create_dir_all(&sources).expect("create source directory");
        let path = sources.join(name);
        fs::write(&path, bytes).expect("write source");
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn task(text: &str) -> TaskStamp {
    let micros = text.bytes().fold(0_i64, |value, byte| {
        (value * 131 + i64::from(byte)) % 1_000_000_000
    });
    TaskStamp::for_micros(micros).expect("fixture task stamp")
}

fn handle(task: &TaskStamp) -> TaskHandle {
    TaskHandle::for_topic(
        &"/task/artifact"
            .parse::<wire::Topic>()
            .expect("fixture topic"),
        task,
    )
}

fn instant(text: &str) -> Instant {
    text.parse().expect("fixture instant")
}

fn add(
    store: &Store,
    task: &TaskStamp,
    source: &Path,
    retention: Retention,
    at: Instant,
) -> super::Artifact {
    store
        .add(AddRequest {
            task,
            source,
            name: None,
            media_type: None,
            retention,
            created_at: at,
        })
        .expect("add artifact")
        .0
}

#[test]
fn text_and_binary_artifacts_round_trip_with_stable_metadata() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-a1");
    let text = fixture.source("evidence.md", b"proof\n");
    let binary = fixture.source("image.png", b"\x89PNG\r\n\x1a\n");

    let first = add(
        &fixture.store,
        &task,
        &text,
        Retention::Permanent,
        instant("2030-01-01T00:00:00.000Z"),
    );
    let second = add(
        &fixture.store,
        &task,
        &binary,
        Retention::Prunable,
        instant("2030-01-02T00:00:00.000Z"),
    );

    assert_eq!(first.id, "A1");
    assert_eq!(first.media_type.as_str(), MediaType::TEXT_MARKDOWN);
    assert_eq!(second.id, "A2");
    assert_eq!(second.media_type.as_str(), MediaType::IMAGE_PNG);
    assert_eq!(
        fixture.store.list(&task).expect("list"),
        vec![first.clone(), second.clone()]
    );
    assert_eq!(
        fixture.store.show(&task, "A1").expect("show text").1,
        b"proof\n"
    );
    assert_eq!(
        fixture.store.show(&task, "A2").expect("show binary").1,
        b"\x89PNG\r\n\x1a\n"
    );
    let capture = fixture.store.capture().expect("capture task set");
    assert_eq!(capture.tasks(), std::slice::from_ref(&task));
    let repeated = fixture.store.capture().expect("repeat stable capture");
    assert_eq!(capture.identity(), repeated.identity());

    let payload_suffix = format!("objects/{}/payload", first.digest);
    let captured_text = capture
        .members()
        .iter()
        .find(|member| member.path().ends_with(&payload_suffix))
        .expect("captured text payload");
    fs::write(
        payload_path(&fixture.store.root().join(task.as_str()), &first.digest),
        b"changed",
    )
    .expect("mutate source generation after capture");
    let mut bytes = Vec::new();
    captured_text
        .copy_to(&mut bytes)
        .expect("copy immutable captured payload");
    assert_eq!(bytes, b"proof\n");
    assert_eq!(
        fixture
            .store
            .capture()
            .expect_err("new capture validates the changed source")
            .class(),
        ErrorClass::Corrupt
    );
}

#[test]
fn caller_owned_bytes_publish_without_a_source_file() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-bytes");
    let (artifact, budget) = fixture
        .store
        .add_bytes(AddBytesRequest {
            task: &task,
            bytes: b"\x89PNG\r\n\x1a\n",
            name: "operator-capture.png",
            media_type: Some(MediaType::IMAGE_PNG),
            retention: Retention::Permanent,
            created_at: instant("2030-01-01T00:00:00.000Z"),
        })
        .expect("publish caller-owned bytes");

    assert_eq!(artifact.id, "A1");
    assert_eq!(artifact.name, "operator-capture.png");
    assert_eq!(artifact.media_type.as_str(), MediaType::IMAGE_PNG);
    assert_eq!((budget.count, budget.bound, budget.remaining), (1, 32, 31));
    assert_eq!(
        fixture.store.show(&task, "A1").expect("show bytes").1,
        b"\x89PNG\r\n\x1a\n"
    );
}

#[test]
fn verified_text_reader_preserves_a_scalar_split_across_digest_buffers() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-streamed-text");
    let mut text = "a".repeat(64 * 1024 - 1);
    text.push_str("¢🦀tail");
    fixture
        .store
        .add_bytes(AddBytesRequest {
            task: &task,
            bytes: text.as_bytes(),
            name: "large.txt",
            media_type: Some(MediaType::TEXT_PLAIN),
            retention: Retention::Permanent,
            created_at: instant("2030-01-01T00:00:00.000Z"),
        })
        .expect("publish large text");

    let mut payload = fixture
        .store
        .open_payload(&task, "A1")
        .expect("streaming validation accepts split scalar");
    assert_eq!(
        payload
            .window((64 * 1024 - 2) as u64, 11)
            .expect("bounded window"),
        "a¢🦀tail".as_bytes()
    );
    payload.rewind().expect("rewind payload");
    let mut copied = Vec::new();
    payload.read_to_end(&mut copied).expect("stream payload");
    assert_eq!(copied, text.as_bytes());
}

#[test]
fn opaque_binary_without_an_admitted_extension_refuses_without_publication() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-m3");
    let source = fixture.source("opaque", b"\xff\xfe\xfd");
    let failure = fixture
        .store
        .add(AddRequest {
            task: &task,
            source: &source,
            name: None,
            media_type: None,
            retention: Retention::Permanent,
            created_at: instant("2030-01-01T00:00:00.000Z"),
        })
        .expect_err("opaque binary requires an explicit media type");

    assert_eq!(failure.class(), ErrorClass::Refusal);
    assert!(
        !fixture.store.root().exists(),
        "refused input published an artifact task directory"
    );
}

#[test]
fn concurrent_adds_serialize_without_losing_either_citation() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-b2");
    let first = fixture.source("first.txt", b"first");
    let second = fixture.source("second.txt", b"second");
    let store = Arc::new(fixture.store.clone());
    let handles = [first, second]
        .into_iter()
        .map(|source| {
            let store = Arc::clone(&store);
            let task = task.clone();
            thread::spawn(move || {
                add(
                    &store,
                    &task,
                    &source,
                    Retention::Permanent,
                    instant("2030-01-01T00:00:00.000Z"),
                )
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().expect("writer thread");
    }

    let listed = store.list(&task).expect("list concurrent artifacts");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, "A1");
    assert_eq!(listed[1].id, "A2");
    assert_ne!(listed[0].digest, listed[1].digest);
}

#[test]
fn interrupted_pre_manifest_publication_is_invisible_and_recoverable() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-c3");
    let source = fixture.source("proof.txt", b"durable proof");
    let failure = fixture
        .store
        .add_with_hook(
            AddRequest {
                task: &task,
                source: &source,
                name: None,
                media_type: None,
                retention: Retention::Permanent,
                created_at: instant("2030-01-01T00:00:00.000Z"),
            },
            || {
                Err(Error::environment(
                    "fixture interruption",
                    "before manifest",
                ))
            },
        )
        .expect_err("injected interruption");
    assert_eq!(failure.class(), ErrorClass::Environment);
    assert!(
        fixture
            .store
            .list(&task)
            .expect("orphan is invisible")
            .is_empty()
    );

    let artifact = add(
        &fixture.store,
        &task,
        &source,
        Retention::Permanent,
        instant("2030-01-01T00:00:01.000Z"),
    );
    assert_eq!(artifact.id, "A1");
    assert_eq!(
        fixture.store.show(&task, "A1").expect("recovered show").1,
        b"durable proof"
    );
}

#[test]
fn interrupted_image_replacement_keeps_the_complete_preceding_set() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-images");
    fixture
        .store
        .add_bytes(AddBytesRequest {
            task: &task,
            bytes: b"old image",
            name: "old.png",
            media_type: Some(MediaType::IMAGE_PNG),
            retention: Retention::Permanent,
            created_at: instant("2030-01-01T00:00:00.000Z"),
        })
        .expect("publish preceding image");
    let note = fixture.source("note.txt", b"retained");
    add(
        &fixture.store,
        &task,
        &note,
        Retention::Permanent,
        instant("2030-01-01T00:00:01.000Z"),
    );
    let before = fixture.store.list(&task).expect("preceding set");
    let images = [
        ImageBytes {
            bytes: b"first new image",
            name: "first.png",
            media_type: MediaType::IMAGE_PNG,
            retention: Retention::Permanent,
        },
        ImageBytes {
            bytes: b"second new image",
            name: "second.png",
            media_type: MediaType::IMAGE_PNG,
            retention: Retention::Permanent,
        },
    ];

    let failure = fixture
        .store
        .replace_images_with_hook(
            ReplaceImagesRequest {
                task: &task,
                images: &images,
                cited: &BTreeSet::new(),
                created_at: instant("2030-01-01T00:00:02.000Z"),
            },
            || {
                Err(Error::environment(
                    "fixture interruption",
                    "before replacement manifest",
                ))
            },
        )
        .expect_err("injected interruption");

    assert_eq!(failure.class(), ErrorClass::Environment);
    assert_eq!(
        fixture.store.list(&task).expect("set after refusal"),
        before
    );
    assert_eq!(
        fixture
            .store
            .show(&task, "A1")
            .expect("preceding image remains")
            .1,
        b"old image"
    );
}

#[test]
fn duplicate_payload_survives_pruning_of_one_citation() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-d4");
    let source = fixture.source("same.txt", b"same bytes");
    let permanent = add(
        &fixture.store,
        &task,
        &source,
        Retention::Permanent,
        instant("2030-01-01T00:00:00.000Z"),
    );
    let prunable = add(
        &fixture.store,
        &task,
        &source,
        Retention::Prunable,
        instant("2030-01-01T00:00:01.000Z"),
    );
    assert_eq!(permanent.digest, prunable.digest);

    let statuses = BTreeMap::from([(task.clone(), (handle(&task), "completed".to_owned()))]);
    let plan = fixture
        .store
        .plan_prune(&statuses, None, None)
        .expect("plan prune");
    assert_eq!(plan.operations.len(), 1);
    assert_eq!(plan.operations[0].artifact_id, "A2");
    assert_eq!(
        fixture.store.list(&task).expect("preview is inert").len(),
        2
    );
    fixture.store.apply_prune(&plan).expect("apply prune");

    assert_eq!(
        fixture.store.list(&task).expect("remaining citation"),
        vec![permanent]
    );
    assert_eq!(
        fixture.store.show(&task, "A1").expect("payload remains").1,
        b"same bytes"
    );
}

#[test]
fn removal_keeps_a_payload_another_citation_shares_and_drops_an_unshared_one() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-r1");
    let task_dir = fixture.store.root().join(task.as_str());
    let same = fixture.source("same.txt", b"same bytes");
    let other = fixture.source("other.txt", b"other bytes");
    let at = instant("2030-01-01T00:00:00.000Z");
    let kept = add(&fixture.store, &task, &same, Retention::Permanent, at);
    let shared = add(&fixture.store, &task, &same, Retention::Permanent, at);
    let unshared = add(&fixture.store, &task, &other, Retention::Permanent, at);
    let uncited = BTreeSet::new();

    let (removed, budget) = fixture
        .store
        .remove(&task, "A2", &uncited)
        .expect("remove shared citation");
    assert_eq!(removed, shared);
    assert_eq!((budget.count, budget.remaining), (2, 30));
    assert!(object_dir(&task_dir, &kept.digest).exists());
    assert_eq!(
        fixture.store.show(&task, "A1").expect("shared payload").1,
        b"same bytes"
    );

    fixture
        .store
        .remove(&task, "A3", &uncited)
        .expect("remove unshared citation");
    assert!(!object_dir(&task_dir, &unshared.digest).exists());
    assert_eq!(fixture.store.list(&task).expect("remaining"), vec![kept]);
}

#[test]
fn removal_at_the_bound_frees_one_slot_without_reissuing_the_removed_id() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-r2");
    let at = instant("2030-01-01T00:00:00.000Z");
    let budget = (0..32).fold(crate::ArtifactBudget::of(0), |_, index| {
        let source = fixture.source(&format!("{index}.txt"), index.to_string().as_bytes());
        fixture
            .store
            .add(AddRequest {
                task: &task,
                source: &source,
                name: None,
                media_type: None,
                retention: Retention::Permanent,
                created_at: at,
            })
            .expect("citation below the bound adds")
            .1
    });
    let extra = fixture.source("extra.txt", b"extra");
    let full = fixture
        .store
        .add(AddRequest {
            task: &task,
            source: &extra,
            name: None,
            media_type: None,
            retention: Retention::Permanent,
            created_at: at,
        })
        .expect_err("the bound refuses a 33rd citation");
    assert_eq!(full.class(), ErrorClass::Refusal);
    assert_eq!((budget.count, budget.bound, budget.remaining), (32, 32, 0));

    let (_, budget) = fixture
        .store
        .remove(&task, "A32", &BTreeSet::new())
        .expect("remove the newest citation");
    assert_eq!(budget.remaining, 1);
    let readded = add(&fixture.store, &task, &extra, Retention::Permanent, at);
    assert_eq!(readded.id, "A33");
    fixture
        .store
        .remove(&task, "A33", &BTreeSet::new())
        .expect("remove the readded citation");
    let again = add(&fixture.store, &task, &extra, Retention::Permanent, at);
    assert_eq!(again.id, "A34");
}

#[test]
fn removal_refuses_an_unknown_or_room_cited_artifact_without_change() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-r3");
    let source = fixture.source("proof.txt", b"proof");
    let artifact = add(
        &fixture.store,
        &task,
        &source,
        Retention::Permanent,
        instant("2030-01-01T00:00:00.000Z"),
    );
    let cited = BTreeSet::from([artifact.digest.clone()]);

    for (id, cited) in [("A9", BTreeSet::new()), ("A1", cited)] {
        let refused = fixture
            .store
            .remove(&task, id, &cited)
            .expect_err("removal refuses");
        assert_eq!(refused.class(), ErrorClass::Refusal, "{id}");
    }
    assert_eq!(
        fixture.store.list(&task).expect("unchanged"),
        vec![artifact]
    );
}

#[test]
fn prune_requires_completed_status_and_honors_the_age_bound() {
    let fixture = Fixture::new();
    let completed = task("ARTIFACT-e5");
    let pending = task("ARTIFACT-f6");
    let old = fixture.source("old.txt", b"old");
    let recent = fixture.source("recent.txt", b"recent");
    add(
        &fixture.store,
        &completed,
        &old,
        Retention::Prunable,
        instant("2030-01-01T00:00:00.000Z"),
    );
    add(
        &fixture.store,
        &completed,
        &recent,
        Retention::Prunable,
        instant("2030-01-03T00:00:00.000Z"),
    );
    add(
        &fixture.store,
        &pending,
        &old,
        Retention::Prunable,
        instant("2030-01-01T00:00:00.000Z"),
    );
    let statuses = BTreeMap::from([
        (
            completed.clone(),
            (handle(&completed), "completed".to_owned()),
        ),
        (pending.clone(), (handle(&pending), "pending".to_owned())),
    ]);

    let plan = fixture
        .store
        .plan_prune(
            &statuses,
            Some(instant("2030-01-02T00:00:00.000Z")),
            Some(86_400),
        )
        .expect("bounded plan");
    assert_eq!(plan.operations.len(), 1);
    assert_eq!(plan.operations[0].born, completed);
    assert_eq!(plan.operations[0].handle, handle(&completed));
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].born, pending);
    assert_eq!(plan.skipped[0].handle, Some(handle(&pending)));
    assert_eq!(plan.skipped[0].status, "pending");
}

#[test]
fn missing_and_corrupt_payloads_fail_as_store_corruption() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-g7");
    let source = fixture.source("proof.txt", b"proof");
    let artifact = add(
        &fixture.store,
        &task,
        &source,
        Retention::Permanent,
        instant("2030-01-01T00:00:00.000Z"),
    );
    let task_dir = fixture.store.root().join(task.as_str());
    let payload = payload_path(&task_dir, &artifact.digest);
    fs::remove_file(&payload).expect("remove cited payload");
    let missing = fixture
        .store
        .list(&task)
        .expect_err("missing payload refused");
    assert_eq!(missing.class(), ErrorClass::Corrupt);

    fs::write(&payload, b"different").expect("write corrupt payload");
    let corrupt = fixture
        .store
        .list(&task)
        .expect_err("digest mismatch refused");
    assert_eq!(corrupt.class(), ErrorClass::Corrupt);
}

#[test]
fn a_linked_manifest_is_corruption_not_an_omitted_backup_task() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-l2");
    let task_dir = fixture.store.root().join(task.as_str());
    fs::create_dir_all(&task_dir).expect("create task directory");
    sill::authority::link_directory(&task_dir, &task_dir.join("manifest.json"))
        .expect("plant manifest link");

    let failure = fixture
        .store
        .capture()
        .expect_err("linked manifest must not disappear from backup discovery");
    assert_eq!(failure.class(), ErrorClass::Corrupt);
}

#[test]
fn capture_refuses_a_linked_object_directory() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-l3");
    let source = fixture.source("proof.txt", b"outside proof");
    let artifact = add(
        &fixture.store,
        &task,
        &source,
        Retention::Permanent,
        instant("2030-01-01T00:00:00.000Z"),
    );
    let task_dir = fixture.store.root().join(task.as_str());
    let object = object_dir(&task_dir, &artifact.digest);
    let outside = fixture.base.join("outside-object");
    fs::rename(&object, &outside).expect("move object outside artifact store");
    sill::authority::link_directory(&outside, &object).expect("plant linked object directory");

    let failure = fixture
        .store
        .capture()
        .expect_err("capture must not follow an intermediate object link");
    assert_eq!(failure.class(), ErrorClass::Corrupt);
}

#[test]
fn records_of_another_store_version_are_refused_by_version_before_their_shape() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-v2");
    let source = fixture.source("note.txt", b"note");
    let artifact = add(
        &fixture.store,
        &task,
        &source,
        Retention::Permanent,
        instant("2030-01-01T00:00:00.000Z"),
    );
    let task_dir = fixture.store.root().join(task.as_str());
    let from_the_future = |path: &Path| {
        let text = fs::read_to_string(path).expect("read store record");
        let future = text.replacen("\"version\":1", "\"version\":2,\"future\":true", 1);
        assert_ne!(future, text, "fixture record names version 1");
        fs::write(path, future).expect("write future store record");
    };
    let refused_as_version_two = |failure: Error| {
        assert_eq!(failure.class(), ErrorClass::Refusal, "{failure}");
        assert_eq!(failure.unsupported_release(), Some(2), "{failure}");
    };

    from_the_future(&object_dir(&task_dir, &artifact.digest).join(super::METADATA));
    refused_as_version_two(
        fixture
            .store
            .show(&task, &artifact.id)
            .expect_err("future object metadata is refused"),
    );

    from_the_future(&task_dir.join(super::MANIFEST));
    refused_as_version_two(
        fixture
            .store
            .list(&task)
            .expect_err("future manifest is refused"),
    );
    refused_as_version_two(
        fixture
            .store
            .capture()
            .expect_err("capture refuses a future manifest"),
    );
}

#[test]
fn backup_admission_and_capture_bounds_reject_incomplete_generations() {
    struct BoundSource {
        bytes: u64,
    }

    impl super::BackupSource for BoundSource {
        fn path(&self) -> &str {
            "member"
        }

        fn mode(&self) -> u32 {
            0o600
        }

        fn bytes(&self) -> u64 {
            self.bytes
        }

        fn copy_to(&self, target: &mut dyn std::io::Write) -> std::io::Result<u64> {
            assert_eq!(self.bytes, 0, "oversized source copied before refusal");
            target.write_all(b"x")?;
            Ok(1)
        }
    }

    let invalid = Fixture::new();
    fs::create_dir_all(invalid.store.root().join("not-a-task"))
        .expect("create invalid task directory");
    fs::write(invalid.store.root().join("not-a-task/manifest.json"), b"{}")
        .expect("write invalid manifest");
    let invalid = invalid
        .store
        .capture()
        .expect_err("invalid task directory refused");
    assert_eq!(invalid.class(), ErrorClass::Corrupt);

    let fixture = Fixture::new();
    let task = task("ARTIFACT-h8");
    let source = fixture.source("proof.txt", b"proof");
    let artifact = add(
        &fixture.store,
        &task,
        &source,
        Retention::Permanent,
        instant("2030-01-01T00:00:00.000Z"),
    );
    fs::remove_file(payload_path(
        &fixture.store.task_dir(&task),
        &artifact.digest,
    ))
    .expect("remove cited payload");
    let missing = fixture
        .store
        .capture()
        .expect_err("missing payload refused");
    assert_eq!(missing.class(), ErrorClass::Corrupt);

    fs::remove_dir_all(object_dir(&fixture.store.task_dir(&task), &artifact.digest))
        .expect("remove cited object directory");
    let missing = fixture
        .store
        .capture()
        .expect_err("missing object directory refused");
    assert_eq!(missing.class(), ErrorClass::Corrupt);

    assert!(within_member_bound(4096));
    assert!(!within_member_bound(4097));

    const EIGHT_GIB: u64 = 8 << 30;
    assert_eq!(admit_backup_bytes(EIGHT_GIB - 1, 1), Some(EIGHT_GIB));
    assert_eq!(admit_backup_bytes(EIGHT_GIB, 1), None);
    assert_eq!(admit_backup_bytes(0, EIGHT_GIB + 1), None);
    assert_eq!(admit_backup_bytes(u64::MAX, 1), None);

    let oversized_source = BoundSource {
        bytes: EIGHT_GIB + 1,
    };
    let oversized_failure = super::BackupGeneration::from_sources(&[&oversized_source])
        .expect_err("declared aggregate beyond the bound refused before copy");
    assert_eq!(oversized_failure.class(), ErrorClass::Corrupt);

    let overrun = BoundSource { bytes: 0 };
    let overrun_failure = super::BackupGeneration::from_sources(&[&overrun])
        .expect_err("member crossing its declared bound refused during copy");
    assert_eq!(overrun_failure.class(), ErrorClass::Corrupt);
}

#[test]
fn replacement_commits_or_rolls_back_complete_provenance_generations() {
    let first = Fixture::new();
    let second = Fixture::new();
    let task = task("ARTIFACT-i9");
    let old_source = first.source("old.txt", b"old");
    let new_source = second.source("new.txt", b"new");
    let empty_source = second.source("empty.txt", b"");
    add(
        &first.store,
        &task,
        &old_source,
        Retention::Permanent,
        instant("2030-01-01T00:00:00.000Z"),
    );
    add(
        &second.store,
        &task,
        &new_source,
        Retention::Permanent,
        instant("2030-01-02T00:00:00.000Z"),
    );
    add(
        &second.store,
        &task,
        &empty_source,
        Retention::Permanent,
        instant("2030-01-03T00:00:00.000Z"),
    );
    let old = first.store.capture().expect("old generation");
    let old_image = first.store.capture().expect("old image generation");
    let captured_new = second.store.capture().expect("new generation");
    let sources = captured_new
        .members()
        .iter()
        .map(|member| member as &dyn super::BackupSource)
        .collect::<Vec<_>>();
    let new = super::BackupGeneration::from_sources(&sources).expect("streamed new generation");

    first
        .store
        .begin_replace(&new, &old)
        .expect("begin replacement")
        .rollback()
        .expect("rollback replacement");
    assert_eq!(
        first.store.show(&task, "A1").expect("old restored").1,
        b"old"
    );

    first
        .store
        .begin_replace(&new, &old)
        .expect("begin replacement")
        .commit()
        .expect("commit replacement");
    assert_eq!(
        first.store.show(&task, "A1").expect("new restored").1,
        b"new"
    );
    assert_eq!(
        first.store.show(&task, "A2").expect("empty restored").1,
        b""
    );

    let stale = first
        .store
        .begin_replace(&old_image, &old)
        .expect_err("stale expected generation refused");
    assert_eq!(stale.class(), ErrorClass::Refusal);
}

#[test]
fn failed_commit_cleanup_never_removes_the_published_generation() {
    let first = Fixture::new();
    let second = Fixture::new();
    let task = task("ARTIFACT-k1");
    let old_source = first.source("old.txt", b"old");
    let new_source = second.source("new.txt", b"new");
    add(
        &first.store,
        &task,
        &old_source,
        Retention::Permanent,
        instant("2030-01-01T00:00:00.000Z"),
    );
    add(
        &second.store,
        &task,
        &new_source,
        Retention::Permanent,
        instant("2030-01-02T00:00:00.000Z"),
    );
    let old = first.store.capture().expect("old generation");
    let new = second.store.capture().expect("new generation");
    let replacement = first
        .store
        .begin_replace(&new, &old)
        .expect("begin replacement");
    let previous = replacement
        .previous
        .clone()
        .expect("replacement retains the old generation");
    fs::remove_dir_all(&previous).expect("remove old generation fixture");
    fs::write(&previous, b"cleanup obstruction").expect("plant cleanup obstruction");

    let failure = replacement.commit().expect_err("commit cleanup fails");
    assert_eq!(failure.class(), ErrorClass::Environment);
    assert_eq!(
        first
            .store
            .show(&task, "A1")
            .expect("published generation remains")
            .1,
        b"new"
    );
}

#[test]
fn object_path_is_content_addressed_once_for_duplicate_bytes() {
    let fixture = Fixture::new();
    let task = task("ARTIFACT-j0");
    let source = fixture.source("same.txt", b"same");
    let first = add(
        &fixture.store,
        &task,
        &source,
        Retention::Permanent,
        instant("2030-01-01T00:00:00.000Z"),
    );
    let second = add(
        &fixture.store,
        &task,
        &source,
        Retention::Permanent,
        instant("2030-01-01T00:00:01.000Z"),
    );
    assert_eq!(first.digest, second.digest);
    let objects = fs::read_dir(
        object_dir(&fixture.store.root().join(task.as_str()), &first.digest)
            .parent()
            .expect("object parent"),
    )
    .expect("read objects")
    .count();
    assert_eq!(objects, 1);
}
