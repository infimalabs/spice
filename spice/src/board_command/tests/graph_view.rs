//! `board graph VIEW` pages a large diagram and names its file export;
//! complete snapshots never stream to standard output.

use super::*;
use wire::output::{INTERACTIVE, Request};

const TASKS: usize = 25;

fn refused(outcome: Result<Destination, Outcome<CommandAnswer>>, reason: &str) -> String {
    match outcome {
        Err(Outcome::Refusal(refusal)) if refusal.reason() == reason => refusal
            .diagnostic()
            .and_then(|diagnostic| diagnostic.detail())
            .unwrap_or_default()
            .to_owned(),
        other => panic!("expected refusal {reason}, got {other:?}"),
    }
}

#[test]
fn a_large_graph_pages_its_first_lines_and_names_the_complete_file_export() {
    let (root, bus, epoch, _) = fixture("graph-view", 1);
    let long = "é".repeat(400);
    for index in 0..TASKS {
        add(
            &bus,
            epoch,
            Actor::TEST_PRINCIPAL,
            &format!("GRAPH-{index:05}"),
            &format!("Graph row {index}"),
            Some("todo"),
        );
    }
    let graph = issue(
        &bus,
        epoch,
        Actor::TEST_PRINCIPAL,
        CommandKind::BoardGraph,
        Vec::new(),
        &BoardGraph {
            view: "workflow".to_owned(),
            all: false,
            at: None,
        },
    );
    let Outcome::Reply(Reply {
        body: CommandAnswer::Text(answer),
        ..
    }) = graph
    else {
        panic!("graph was not a text snapshot: {graph:?}");
    };
    // One oversized diagram line is clipped rather than closing the page.
    let text = answer.text.replacen('\n', &format!("\n{long}\n"), 1);
    let budget = INTERACTIVE
        .budget(Request::default())
        .expect("default budget");
    let page = graph_page("workflow", &text, budget, true).expect("graph pages");

    let shown = INTERACTIVE.default_limits().rows;
    assert_eq!(page.lines.rows.len(), shown);
    assert_eq!(page.lines.rows[0].text, "é".repeat(120));
    assert_eq!(
        page.lines.rows[0]
            .withheld_bytes
            .map(std::num::NonZero::get),
        Some(long.len() - 240)
    );
    assert_eq!(page.lines.rows[1].text, text.lines().nth(2).expect("row"));
    let omission = page.lines.omission.as_ref().expect("most lines withheld");
    assert_eq!(
        (omission.population, omission.shown, omission.withheld),
        (TASKS + 1, shown, TASKS + 1 - shown)
    );
    assert_eq!(
        omission.continuation.arguments(),
        [
            "board",
            "graph",
            "workflow",
            "--all",
            "--output",
            "board-graph-workflow.txt"
        ]
    );
    clean(&root, bus);
}

#[test]
fn complete_snapshots_refuse_standard_output_and_name_the_file_command() {
    let graph = refused(
        page_or_file("review", Request::default(), Some("-".to_owned()), false),
        "board::export-stdout",
    );
    assert!(
        graph.contains("spice board graph review --output board-graph-review.txt"),
        "{graph}"
    );
    let handout = refused(handout_destination("-"), "board::export-stdout");
    assert!(
        handout.contains("spice board handout board-handout.md"),
        "{handout}"
    );

    // A file export is complete, so it admits no page limits.
    let limited = Request {
        rows: Some(5),
        ..Request::default()
    };
    refused(
        page_or_file("review", limited, Some("graph.txt".to_owned()), false),
        "board::export-bounded",
    );
    assert_eq!(
        page_or_file(
            "review",
            Request::default(),
            Some("graph.txt".to_owned()),
            false,
        ),
        Ok(Destination::File("graph.txt".to_owned()))
    );
    assert_eq!(
        handout_destination("handout.md"),
        Ok(Destination::File("handout.md".to_owned()))
    );
}
