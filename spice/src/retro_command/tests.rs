use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use listen::{Dialect, EventKind, Observation, StreamOutcome, SurfaceEvent, TailBudget};
use tape::Tape;
use wire::{Digest, Domain, Outcome};

use super::Retro;
use super::view::{Continuation, RetroPage, RetroView};

const LANE: &str = "lane-a";
const DIALECTS: [(&str, Dialect); 1] = [("omp", Dialect::Omp)];
/// About two fixture records, so a page's scan window withholds older bytes.
const SCAN_WINDOW_BYTES: u64 = 256;
const SCAN_SOURCE_BYTES: u64 = 64 * 1024;
const SCAN_RECORDS: usize = 128;
const SCAN_PASSES: usize = 128;
const SCAN_DURATION: Duration = Duration::from_secs(5);
const TEXTS: [&str; 5] = ["first", "second", "third", "fourth", "fifth"];
/// Pages the follow loops may take before they are judged not to converge.
const PAGE_CAP: usize = 32;
/// Room for the status row but not one record with its tape path and spans.
const NARROW_BYTES: usize = 200;
/// Longer than the interactive text limit, so a page clips it.
const LONG_TEXT_BYTES: usize = 1024;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "spice-retro-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("scratch root is exclusively created");
        Self(root)
    }

    fn tape(&self) -> Tape {
        Tape::open(self.tape_path()).expect("fixture tape opens")
    }

    fn tape_path(&self) -> PathBuf {
        self.0.join(LANE).join("omp")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn append(tape: &Tape, record: &serde_json::Value) {
    let mut bytes = serde_json::to_vec(record).expect("fixture record encodes");
    bytes.push(b'\n');
    tape.tee(&bytes).expect("fixture record is durable");
}

fn prose(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "message_end",
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "stopReason": "stop",
        },
    })
}

fn failure() -> serde_json::Value {
    serde_json::json!({"type": "rpc_frame_error", "error": "failed loudly"})
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn retro(root: &Path, arguments: &[String]) -> Outcome<RetroView> {
    let matches = crate::cli::command()
        .try_get_matches_from(std::iter::once("spice").chain(arguments.iter().map(String::as_str)))
        .expect("retro syntax parses");
    let scan = TailBudget::new(
        SCAN_WINDOW_BYTES,
        SCAN_SOURCE_BYTES,
        SCAN_RECORDS,
        SCAN_PASSES,
        SCAN_DURATION,
    )
    .expect("fixture scan budget is valid");
    match Retro::from_matches(crate::cli::selected_matches(&matches)) {
        Ok(retro) => retro.read(root, &DIALECTS, scan),
        Err(refusal) => Outcome::Refusal(refusal),
    }
}

fn page(outcome: Outcome<RetroView>) -> RetroPage {
    match outcome {
        Outcome::Reply(reply) => match reply.body {
            RetroView::Page(page) => page,
            other => panic!("one session reads as a page: {other}"),
        },
        Outcome::Refusal(refusal) => panic!("page refused: {}", refusal.reason()),
        Outcome::Fault(fault) => panic!("page faulted: {}", fault.detail),
    }
}

fn observations(page: &RetroPage) -> impl Iterator<Item = &Observation> {
    page.rows
        .rows
        .iter()
        .filter_map(|row| match row {
            listen::TranscriptPageRow::Record { record } => Some(&record.observations),
            listen::TranscriptPageRow::Status { .. } => None,
        })
        .flatten()
        .map(|observation| &observation.observation)
}

fn prose_texts(page: &RetroPage) -> Vec<String> {
    observations(page)
        .filter_map(|observation| match observation {
            Observation::Event {
                event: SurfaceEvent::Prose { text, .. },
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The command a page names for what it withheld, if anything.
fn next(page: &RetroPage) -> Option<Vec<String>> {
    let continuation = page
        .rows
        .omission
        .as_ref()
        .map(|omission| &omission.continuation)
        .or_else(|| page.history.as_ref().map(|history| &history.continuation))?;
    match continuation {
        Continuation::Next { command } => Some(command.arguments().to_vec()),
        Continuation::Export { .. } => panic!("fixture rows fit an interactive page"),
    }
}

/// Every page from `arguments` onward, following each named continuation.
fn follow(root: &Path, arguments: Vec<String>) -> Vec<RetroPage> {
    let mut pages = Vec::new();
    let mut arguments = Some(arguments);
    while let Some(current) = arguments.take() {
        assert!(pages.len() < PAGE_CAP, "continuations converge");
        let page = page(retro(root, &current));
        arguments = next(&page);
        pages.push(page);
    }
    pages
}

#[test]
fn continuations_page_every_record_newest_first_exactly_once() {
    let scratch = Scratch::new("paging");
    let tape = scratch.tape();
    for text in TEXTS {
        append(&tape, &prose(text));
    }
    let newest_first = TEXTS.iter().rev().map(|text| (*text).to_owned());

    let row_paged = follow(&scratch.0, words(&["retro", LANE, "--limit", "2"]));
    assert!(row_paged.iter().any(|page| page.rows.omission.is_some()));
    assert_eq!(
        row_paged.iter().flat_map(prose_texts).collect::<Vec<_>>(),
        newest_first.clone().collect::<Vec<_>>()
    );

    let window_paged = follow(&scratch.0, words(&["retro", LANE]));
    assert!(window_paged.iter().any(|page| page.history.is_some()));
    assert_eq!(
        window_paged
            .iter()
            .flat_map(prose_texts)
            .collect::<Vec<_>>(),
        newest_first.collect::<Vec<_>>()
    );
}

#[test]
fn a_row_wider_than_the_page_names_the_byte_limit_that_admits_it() {
    let scratch = Scratch::new("widen");
    append(&scratch.tape(), &prose("only"));
    let narrow = page(retro(
        &scratch.0,
        &words(&["retro", LANE, "--max-bytes", &NARROW_BYTES.to_string()]),
    ));
    let omission = narrow
        .rows
        .omission
        .as_ref()
        .expect("the record is withheld");
    assert!(omission.next_bytes > NARROW_BYTES);
    let continuation = next(&narrow).expect("the omission is resumable");
    assert!(
        continuation
            .windows(2)
            .any(|pair| pair == ["--max-bytes", &omission.next_bytes.to_string()])
    );
    assert_eq!(
        prose_texts(&page(retro(&scratch.0, &continuation))),
        ["only"]
    );
}

#[test]
fn a_kind_filter_selects_across_every_page() {
    let scratch = Scratch::new("filter");
    let tape = scratch.tape();
    for text in TEXTS {
        append(&tape, &prose(text));
        append(&tape, &failure());
    }
    let pages = follow(
        &scratch.0,
        words(&["retro", LANE, "--kind", "failure", "--limit", "2"]),
    );
    let kinds = pages
        .iter()
        .flat_map(observations)
        .map(|observation| match observation {
            Observation::Event { event, .. } => event.kind(),
            other => panic!("filtered pages carry only events: {other}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, vec![EventKind::Failure; TEXTS.len()]);
}

#[test]
fn a_cursor_from_a_replaced_tape_is_refused_as_stale() {
    let scratch = Scratch::new("stale");
    for text in TEXTS {
        append(&scratch.tape(), &prose(text));
    }
    let first = page(retro(&scratch.0, &words(&["retro", LANE, "--limit", "2"])));
    let continuation = next(&first).expect("older records remain");

    fs::remove_dir_all(scratch.tape_path()).expect("fixture tape is removed");
    for text in TEXTS {
        append(&scratch.tape(), &prose(text));
    }
    match retro(&scratch.0, &continuation) {
        Outcome::Refusal(refusal) => assert_eq!(refusal.reason(), "retro::cursor-stale"),
        Outcome::Reply(_) | Outcome::Fault(_) => panic!("a stale cursor is refused"),
    }
}

#[test]
fn export_writes_the_complete_unclipped_transcript_that_a_clipped_page_names() {
    let scratch = Scratch::new("export");
    let tape = scratch.tape();
    let long = "x".repeat(LONG_TEXT_BYTES);
    for text in TEXTS.into_iter().chain([long.as_str()]) {
        append(&tape, &prose(text));
    }

    let clipped = page(retro(&scratch.0, &words(&["retro", LANE, "--limit", "2"])));
    let export = match &clipped
        .clipped
        .as_ref()
        .expect("long text is clipped")
        .continuation
    {
        Continuation::Export { export, then: None } => export.arguments().to_vec(),
        other => panic!("clipped text is recovered by export: {other}"),
    };
    assert_eq!(export.last().map(String::as_str), Some(super::EXPORT_NAME));

    let destination = scratch.0.join("retro.json");
    let mut arguments = export;
    *arguments.last_mut().expect("export names its file") = destination.display().to_string();
    let receipt = match retro(&scratch.0, &arguments) {
        Outcome::Reply(reply) => match reply.body {
            RetroView::Exported(receipt) => receipt,
            other => panic!("export prints only its receipt: {other}"),
        },
        Outcome::Refusal(refusal) => panic!("export refused: {}", refusal.reason()),
        Outcome::Fault(fault) => panic!("export faulted: {}", fault.detail),
    };
    let written = fs::read(&destination).expect("export is written");
    assert_eq!(receipt.digest, Digest::compute(Domain::CliExport, &written));
    let outcome: StreamOutcome = serde_json::from_slice(&written).expect("export decodes");
    assert_eq!(
        written,
        wire::encode(&outcome).expect("outcome encodes"),
        "the export is the canonical encoding of the whole outcome"
    );
    let texts = outcome
        .observations
        .iter()
        .filter_map(|observation| match observation {
            Observation::Event {
                event: SurfaceEvent::Prose { text, .. },
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        texts,
        TEXTS.into_iter().chain([long.as_str()]).collect::<Vec<_>>()
    );
}
