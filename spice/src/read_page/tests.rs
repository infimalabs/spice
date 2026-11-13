use super::*;
use serde::Serialize;
use wire::output::Exhausted;

const PATH: [&str; 2] = ["session", "turns"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Row {
    ts: String,
    text: String,
}

fn row(ts: &str, text: &str) -> Row {
    Row {
        ts: ts.to_owned(),
        text: text.to_owned(),
    }
}

fn page(arguments: &[String]) -> Result<Page, Refusal> {
    let matches = crate::cli::command()
        .try_get_matches_from(std::iter::once("spice").chain(arguments.iter().map(String::as_str)))
        .expect("session read syntax parses");
    Page::from_matches(crate::cli::selected_matches(&matches), &PATH)
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// One timestamped scan as a history view runs it: newest records at or
/// before the page's end, as many as its frontier retains.
fn scan(page: Page, history: &[Row]) -> Bounded<Row> {
    let end = page.end(None).expect("cursor admits");
    let matched = history
        .iter()
        .rev()
        .filter(|row| end.as_deref().is_none_or(|end| row.ts.as_str() <= end))
        .cloned()
        .collect::<Vec<_>>();
    let population = matched.len();
    let retained = matched
        .into_iter()
        .take(page.frontier().expect("cursor admits"))
        .collect();
    match page
        .newest(retained, population, |row| &row.ts)
        .expect("page admits")
    {
        Paged::Shown(bounded) => bounded,
        Paged::Exported(_) => panic!("an interactive read shows its page"),
    }
}

#[test]
fn following_continuations_reads_every_record_once_newest_first() {
    // Shared instants straddle page boundaries, which the `TS#N` cursor
    // must resume inside without repeating or skipping a record. Rows carry
    // the canonical instant form every transcript fold emits.
    let history = [
        row("2026-01-01T00:00:01.000Z", "a"),
        row("2026-01-01T00:00:02.000Z", "b"),
        row("2026-01-01T00:00:02.000Z", "c"),
        row("2026-01-01T00:00:02.000Z", "d"),
        row("2026-01-01T00:00:02.000Z", "e"),
        row("2026-01-01T00:00:03.000Z", "f"),
        row("2026-01-01T00:00:04.000Z", "g"),
    ];
    let mut arguments = words(&["session", "turns", "fixture.jsonl", "--limit", "2"]);
    let mut shown = Vec::new();
    for _ in 0..history.len() {
        let bounded = scan(page(&arguments).expect("bounds admit"), &history);
        shown.extend(bounded.rows.iter().map(|row| row.text.clone()));
        let Some(omission) = bounded.omission else {
            break;
        };
        assert_eq!(omission.withheld, history.len() - shown.len());
        arguments = omission.continuation.arguments().to_vec();
    }
    assert_eq!(shown, ["g", "f", "e", "d", "c", "b", "a"]);
}

#[test]
fn an_oversized_record_continues_under_a_widened_byte_limit() {
    let history = [
        row(
            "2026-01-01T00:00:01.000Z",
            &"x".repeat(INTERACTIVE.default_limits().bytes),
        ),
        row("2026-01-01T00:00:02.000Z", "newest"),
    ];
    let first = scan(
        page(&words(&["session", "turns", "fixture.jsonl"])).expect("defaults admit"),
        &history,
    );
    assert_eq!(first.rows, [history[1].clone()]);
    let omission = first.omission.expect("the oversized record is withheld");
    assert_eq!(omission.exhausted, Exhausted::Bytes);
    let resumed = scan(
        page(omission.continuation.arguments()).expect("the widened limit admits"),
        &history,
    );
    assert_eq!(resumed.rows, [history[0].clone()]);
    assert!(resumed.omission.is_none());
}

#[test]
fn a_record_beyond_the_interactive_maximum_continues_only_as_an_export() {
    let history = [row(
        "2026-01-01T00:00:01Z",
        &"x".repeat(INTERACTIVE.maximum().bytes),
    )];
    let bounded = scan(
        page(&words(&["session", "turns", "fixture.jsonl"])).expect("defaults admit"),
        &history,
    );
    assert!(bounded.rows.is_empty());
    let continuation = bounded
        .omission
        .expect("the record is withheld")
        .continuation;
    assert!(
        continuation
            .arguments()
            .windows(2)
            .any(|pair| pair[0] == "--export"),
        "{continuation}"
    );
}

#[test]
fn bounds_beyond_the_surface_maximum_are_refused() {
    for (flag, maximum) in [
        ("--limit", INTERACTIVE.maximum().rows),
        ("--max-bytes", INTERACTIVE.maximum().bytes),
        ("--max-text", INTERACTIVE.maximum().text),
    ] {
        let over = (maximum + 1).to_string();
        let refused = page(&words(&["session", "turns", "fixture.jsonl", flag, &over]))
            .err()
            .expect("a bound over the maximum refuses");
        assert_eq!(refused.reason(), "session::bound-exceeded", "{flag}");
        let at = maximum.to_string();
        assert!(
            page(&words(&["session", "turns", "fixture.jsonl", flag, &at])).is_ok(),
            "{flag} at its maximum admits"
        );
    }
}
