//! One Design's current term-net evidence for `task show`.

use super::*;

/// One Design's bounded term evidence from the same local journal snapshot
/// surface used by its room detail. Non-Designs carry no maturity read.
pub(super) fn task_show_maturity(
    repository: Host<'_>,
    task: &BoardTask,
) -> Result<Option<wire::rooms::DesignMaturity>, Outcome<CommandAnswer>> {
    if task.statement.body.intent != Some(wire::TaskIntent::Design) {
        return Ok(None);
    }
    let state = repository
        .admit_state()
        .map_err(|error| repository_state_failure(&error))?;
    let unreadable = |error: &dyn std::fmt::Display| {
        store_fault(
            "task::maturity-unreadable",
            format!(
                "cannot read term evidence for {}: {error}",
                task.statement.body.handle
            ),
        )
    };
    let reader =
        journal::JournalReader::open(state.journal()).map_err(|error| unreadable(&error))?;
    reader
        .read(|read| read.design_maturity(&task.statement.body.born))
        .map(Some)
        .map_err(|error| unreadable(&error))
}

pub(super) fn design_net_lines(
    maturity: &wire::rooms::DesignMaturity,
    statement: &wire::intent::BoardStatement,
) -> Vec<String> {
    let nets = maturity.items.nets(&statement.term);
    let standings = maturity.items.standings(&statement.term);
    let current = nets
        .terms
        .iter()
        .map(|term| term.item.clone())
        .collect::<Vec<_>>();
    let mut lines = vec![format!("term evidence regime {}", nets.regime().as_str())];
    for (term, standing) in nets.terms.iter().zip(standings) {
        let unavailable = if term.truncated {
            format!(
                "; more than {} lanes charged this term; exact counts unavailable",
                wire::rooms::DESIGN_MATURITY_LANE_LIMIT
            )
        } else {
            String::new()
        };
        lines.push(format!(
            "  - {} {} · {}+/{}- net {:+}{unavailable}",
            term.item.handle(&current),
            standing.state.label(),
            term.positive,
            term.negative,
            term.net(),
        ));
    }
    let marker = if nets.inexact() { " (inexact)" } else { "" };
    let agreement = nets
        .agreement()
        .map_or_else(|| "n/a".to_owned(), |(a, b)| format!("{a}/{b}"));
    lines.push(format!(
        "term total {:+}{marker}; agreement {agreement}{marker}; cancellation {}{marker}; \
         zero-net terms {}{marker}",
        nets.total(),
        nets.cancellation(),
        nets.zero_nets(),
    ));
    if let Some(advice) = nets.advice() {
        lines.push(format!("term guidance {advice}"));
    }
    lines
}
