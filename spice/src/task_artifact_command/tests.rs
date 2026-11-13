//! Bounded artifact reads against a real store in a scratch directory.

use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use artifact::AddBytesRequest;

use super::*;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    base: PathBuf,
    store: Store,
    born: TaskStamp,
    task: TaskHandle,
}

impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "spice-artifact-read-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).expect("fixture directory");
        let store = Store::new(base.join("tasks"), base.join("locks/artifacts.lock"));
        let born = TaskStamp::for_micros(1_900_000_000).expect("fixture stamp");
        let task = TaskHandle::for_topic(
            &"/task/artifact"
                .parse::<wire::Topic>()
                .expect("fixture topic"),
            &born,
        );
        Self {
            base,
            store,
            born,
            task,
        }
    }

    fn add(&self, name: &str, bytes: &[u8], media_type: &str) -> Artifact {
        self.store
            .add_bytes(AddBytesRequest {
                task: &self.born,
                bytes,
                name,
                media_type: Some(media_type),
                retention: Retention::Permanent,
                created_at: "2030-01-01T00:00:00.000Z".parse().expect("fixture instant"),
            })
            .expect("artifact adds")
            .0
    }

    fn list(
        &self,
        request: wire::output::Request,
        after: Option<&str>,
    ) -> Result<ArtifactAnswer, Failure> {
        list_answer(&self.store, &self.born, self.task.clone(), request, after)
    }

    fn show(&self, id: &str, window: Window<'_>) -> Result<ArtifactAnswer, Failure> {
        show_answer(&self.store, &self.born, self.task.clone(), id, window)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn inspect(offset: Option<usize>, length: Option<usize>) -> Window<'static> {
    window(offset, length, None).expect("window admitted")
}

fn refused(result: Result<ArtifactAnswer, Failure>) -> String {
    match result {
        Err(Failure::Refusal(refusal)) => refusal.reason().to_owned(),
        Err(Failure::Fault(fault)) => panic!("faulted instead of refusing: {}", fault.reason),
        Ok(answer) => panic!("answered instead of refusing: {answer:?}"),
    }
}

fn faulted(result: Result<ArtifactAnswer, Failure>) -> String {
    match result {
        Err(Failure::Fault(fault)) => fault.reason,
        Err(Failure::Refusal(refusal)) => {
            panic!("refused instead of faulting: {}", refusal.reason())
        }
        Ok(answer) => panic!("answered instead of faulting: {answer:?}"),
    }
}

/// Text-form output is the answer's JSON: no form may carry a raw control byte.
fn assert_terminal_safe(answer: &ArtifactAnswer) {
    let encoded = wire::encode(answer).expect("answer encodes");
    assert!(
        encoded.iter().all(|byte| !byte.is_ascii_control()),
        "answer carries a raw control byte"
    );
}

#[test]
fn many_artifacts_page_newest_first_and_continuations_visit_each_once() {
    let fixture = Fixture::new();
    // The store's per-task cap; well past one default page.
    let added: Vec<Artifact> = (0..32)
        .map(|index| fixture.add(&format!("note-{index}.txt"), b"evidence\n", "text/plain"))
        .collect();

    let ArtifactAnswer::Listed {
        artifacts,
        omission,
        budget,
        ..
    } = fixture
        .list(wire::output::Request::default(), None)
        .expect("default page")
    else {
        panic!("list answered another shape")
    };
    let default = wire::output::INTERACTIVE.default_limits();
    assert_eq!(artifacts.len(), default.rows);
    assert_eq!(artifacts[0].id, "A32", "newest first");
    let omission = omission.expect("a partial page states its omission");
    assert_eq!((omission.population, omission.withheld), (32, 12));
    assert_eq!(
        (budget.count, budget.bound, budget.remaining),
        (32, 32, 0),
        "a partial page reports the whole task's budget"
    );
    assert_eq!(
        omission.continuation.arguments(),
        [
            "task",
            "artifact",
            "list",
            &fixture.task.to_string(),
            "--after",
            "A12",
            "--limit",
            &default.rows.to_string(),
            "--max-bytes",
            &default.bytes.to_string(),
        ]
    );

    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let ArtifactAnswer::Listed {
            artifacts,
            omission,
            ..
        } = fixture
            .list(
                wire::output::Request {
                    rows: Some(7),
                    ..wire::output::Request::default()
                },
                after.as_deref(),
            )
            .expect("page")
        else {
            panic!("list answered another shape")
        };
        seen.extend(artifacts);
        let Some(omission) = omission else { break };
        let arguments = omission.continuation.arguments();
        let cursor = arguments
            .iter()
            .position(|argument| argument == "--after")
            .expect("continuation names a cursor");
        after = Some(arguments[cursor + 1].clone());
    }
    let mut newest_first = added;
    newest_first.reverse();
    assert_eq!(seen, newest_first, "every artifact once, newest first");

    assert_eq!(
        refused(fixture.list(wire::output::Request::default(), Some("A99"))),
        "artifact::cursor-unknown"
    );
    assert_eq!(
        refused(fixture.list(
            wire::output::Request {
                rows: Some(wire::output::INTERACTIVE.maximum().rows + 1),
                ..wire::output::Request::default()
            },
            None
        )),
        "artifact::list-limit",
        "a limit past the maximum is refused, not clamped"
    );
}

#[test]
fn large_text_previews_a_bounded_prefix_and_windows_page_on_character_boundaries() {
    let fixture = Fixture::new();
    let text = "naïve 🦀 crab\u{1b}[31m\n".repeat(400);
    let artifact = fixture.add("log.md", text.as_bytes(), "text/markdown");

    let preview = fixture
        .show(&artifact.id, inspect(None, None))
        .expect("preview");
    assert_terminal_safe(&preview);
    let ArtifactAnswer::Shown {
        content:
            ArtifactContent::Text {
                offset,
                text: shown,
                remaining,
            },
        next,
        ..
    } = preview
    else {
        panic!("text preview answered another shape")
    };
    assert_eq!(offset, 0);
    assert!(shown.len() <= PREVIEW_BYTES && text.starts_with(&shown));
    assert_eq!(remaining, wide(text.len() - shown.len()));
    assert_eq!(
        next.expect("a partial preview names the next window")
            .arguments(),
        [
            "task",
            "artifact",
            "show",
            &fixture.task.to_string(),
            &artifact.id,
            "--offset",
            &shown.len().to_string(),
        ]
    );

    // One-byte windows over multi-byte characters still advance and
    // reassemble the exact text, starting even inside a character. The first
    // few lines cover every character width the text carries.
    let walked = text.find('\n').expect("text has lines") * 3;
    let mut rebuilt = String::new();
    let mut at = 1;
    loop {
        let ArtifactAnswer::Shown {
            content: ArtifactContent::Text { offset, text, .. },
            next,
            ..
        } = fixture
            .show(&artifact.id, inspect(Some(at), Some(1)))
            .expect("window")
        else {
            panic!("text window answered another shape")
        };
        assert!(text.len() <= 4, "a window widens by at most one character");
        if rebuilt.is_empty() {
            assert_eq!(offset, 1, "a start inside no character stays put");
        }
        rebuilt.push_str(&text);
        let Some(next) = next else { break };
        let arguments = next.arguments();
        at = arguments[arguments.len() - 3].parse().expect("next offset");
        if rebuilt.len() > walked {
            break;
        }
    }
    assert!(text[1..].starts_with(&rebuilt));
    let ArtifactAnswer::Shown {
        content: ArtifactContent::Text { offset, .. },
        ..
    } = fixture
        .show(&artifact.id, inspect(Some(3), Some(8)))
        .expect("window inside a character")
    else {
        panic!("text window answered another shape")
    };
    assert_eq!(offset, 4, "a start inside 'ï' moves to the character's end");

    for length in [0, WINDOW_MAX + 1] {
        let Err(Failure::Refusal(refusal)) = window(None, Some(length), None) else {
            panic!("--bytes {length} admitted")
        };
        assert_eq!(refusal.reason(), "artifact::window-bytes");
    }
}

#[test]
fn binary_and_malformed_utf8_payloads_never_reach_the_terminal_raw() {
    let fixture = Fixture::new();
    let payload = b"\x89PNG\r\n\x1a\n\xff\xfe\x00\x1b[2J";
    let artifact = fixture.add("capture.png", payload, "image/png");

    let bare = fixture
        .show(&artifact.id, inspect(None, None))
        .expect("bare show");
    assert_terminal_safe(&bare);
    let ArtifactAnswer::Shown {
        content: ArtifactContent::Binary { path },
        next: None,
        ..
    } = bare
    else {
        panic!("binary bare show answered another shape")
    };
    assert_eq!(fs::read(path).expect("store path reads"), payload);

    let window = fixture
        .show(&artifact.id, inspect(Some(8), Some(4)))
        .expect("byte window");
    assert_terminal_safe(&window);
    let ArtifactAnswer::Shown {
        content:
            ArtifactContent::Bytes {
                offset,
                hex,
                remaining,
            },
        next,
        ..
    } = window
    else {
        panic!("binary window answered another shape")
    };
    assert_eq!((offset, hex.as_str(), remaining), (8, "fffe001b", 3));
    assert!(next.is_some(), "a window short of the end names the next");
}

#[test]
fn extraction_writes_the_exact_payload_and_verifies_its_retained_digest() {
    let fixture = Fixture::new();
    let payload: Vec<u8> = (0..=255).cycle().take(300_000).collect();
    let artifact = fixture.add("blob.pdf", &payload, "application/pdf");
    let output = fixture.base.join("blob-copy.pdf");
    let output_text = output.display().to_string();

    let extracted = fixture
        .show(
            &artifact.id,
            window(None, None, Some(&output_text)).expect("extract"),
        )
        .expect("extraction");
    assert_terminal_safe(&extracted);
    let ArtifactAnswer::Extracted { export, .. } = extracted else {
        panic!("extraction answered another shape")
    };
    assert_eq!(
        fs::read(&output).expect("extracted file"),
        payload,
        "never truncated"
    );
    assert_eq!(export.bytes, artifact.bytes);

    assert_eq!(
        refused(fixture.show(&artifact.id, window(None, None, Some("-")).expect("window"))),
        "output::export-stdout"
    );
    let Err(Failure::Refusal(refusal)) = window(Some(0), None, Some(&output_text)) else {
        panic!("extraction with window bounds admitted")
    };
    assert_eq!(refusal.reason(), "artifact::window-extract");
}

#[test]
fn a_payload_that_no_longer_matches_its_digest_is_neither_shown_nor_extracted() {
    let fixture = Fixture::new();
    let artifact = fixture.add("evidence.txt", b"the retained bytes\n", "text/plain");
    let payload = payload_path(fixture.store.root(), &fixture.born, &artifact.digest);
    sill::authority::set_owner_mode(&payload, 0o600).expect("payload writable");
    fs::write(&payload, b"tampered bytes here\n").expect("payload tampered");
    let output = fixture.base.join("evidence-copy.txt");
    let output_text = output.display().to_string();

    assert_eq!(
        faulted(fixture.show(&artifact.id, inspect(None, None))),
        "artifact::store-corrupt"
    );
    assert_eq!(
        faulted(fixture.show(
            &artifact.id,
            window(None, None, Some(&output_text)).expect("extract")
        )),
        "artifact::store-corrupt"
    );
    assert!(!output.exists(), "a mismatched payload is never extracted");
}

/// An operator reading text output learns which operand the store refused and
/// what it would admit, not only the refusal's reason code.
#[test]
fn a_store_refusal_prints_the_refused_operand_and_its_admitted_values() {
    let fixture = Fixture::new();
    let error = fixture
        .store
        .add_bytes(AddBytesRequest {
            task: &fixture.born,
            bytes: b"notes\n",
            name: "notes.md",
            media_type: Some("design"),
            retention: Retention::Permanent,
            created_at: "2030-01-01T00:00:00.000Z".parse().expect("fixture instant"),
        })
        .expect_err("an unsupported media type refuses");
    let outcome = artifact_failure(error).outcome();
    let rendering =
        crate::render::render(&outcome, crate::render::Form::Text).expect("refusal renders");
    let text = String::from_utf8(rendering.document().to_vec()).expect("text output");

    assert!(
        text.contains("media type"),
        "the operand is unnamed: {text}"
    );
    assert!(
        text.contains("\"design\""),
        "the refused value is unnamed: {text}"
    );
    for admitted in artifact::MediaType::ALL {
        assert!(text.contains(admitted), "{admitted} is missing: {text}");
    }
}
