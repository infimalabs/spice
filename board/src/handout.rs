use std::fmt::Write;

use crate::graph::{ReviewEdge, View, render};
use crate::task::Entry;

/// Compose every named diagram into one stable shareable document.
#[must_use]
pub fn compose(entries: &[Entry], reviews: &[ReviewEdge]) -> String {
    let mut handout = String::from("# Board handout\n\n");
    for view in View::ALL {
        let diagram = render(view, entries, reviews);
        writeln!(handout, "## {}\n", view.as_str()).expect("String writes are infallible");
        handout.push_str("```text\n");
        handout.push_str(diagram.text());
        handout.push_str("```\n\n");
    }
    handout
}
