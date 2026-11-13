use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{self, Write};

use crate::task::Entry;
use wire::{Actor, Phase, Seq, TaskHandle, TaskStamp, body};

/// Named deterministic diagrams the board exposes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum View {
    Origin,
    Dependencies,
    Review,
    Workflow,
    Claims,
}

impl View {
    /// Every named view in handout order.
    pub const ALL: [Self; 5] = [
        Self::Origin,
        Self::Dependencies,
        Self::Review,
        Self::Workflow,
        Self::Claims,
    ];

    /// Stable view name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Origin => "origin",
            Self::Dependencies => "dependencies",
            Self::Review => "review",
            Self::Workflow => "workflow",
            Self::Claims => "claims",
        }
    }
}

/// One rendered board diagram.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagram {
    view: View,
    text: String,
}

/// One completed durable review, joined to the phase it followed and that
/// phase's author when the journal records one. A flow that begins with review
/// has no completed phase before its first review, so that edge names none.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ReviewEdge {
    task: TaskHandle,
    implemented: Option<(Phase, Actor)>,
    reviewer: Actor,
    verdict: body::Verdict,
    reviewed: Seq,
}

impl ReviewEdge {
    /// Build one already-validated historical review relation.
    #[must_use]
    pub const fn new(
        task: TaskHandle,
        implemented: Option<(Phase, Actor)>,
        reviewer: Actor,
        verdict: body::Verdict,
        reviewed: Seq,
    ) -> Self {
        Self {
            task,
            implemented,
            reviewer,
            verdict,
            reviewed,
        }
    }
}

/// A completed review contradicts its authenticated fact actor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewHistoryError {
    task: TaskStamp,
    body: Actor,
    fact: Actor,
    reviewed: Seq,
}

impl fmt::Display for ReviewHistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            task,
            body,
            fact,
            reviewed,
        } = self;
        write!(
            formatter,
            "task {task} review at {reviewed} names reviewer {body} under fact actor {fact}"
        )
    }
}

impl Error for ReviewHistoryError {}

/// Join completed review facts to the most recent completed phase author, when
/// one precedes the review.
///
/// Only tasks present in `entries` contribute edges, preserving the board's
/// actor-filtered visibility boundary. Input is sorted by durable position so
/// callers cannot change history by changing collection order.
pub fn review_history<'a>(
    facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
    entries: &[Entry],
) -> Result<Vec<ReviewEdge>, ReviewHistoryError> {
    let handles = entries
        .iter()
        .map(|entry| (entry.born().clone(), entry.handle().clone()))
        .collect::<BTreeMap<_, _>>();
    let mut ordered = facts.into_iter().collect::<Vec<_>>();
    ordered.sort_by_key(|envelope| envelope.seq);
    let mut phases = BTreeMap::<TaskStamp, Phase>::new();
    let mut authors = BTreeMap::<TaskStamp, (Phase, Actor)>::new();
    let mut reviews = Vec::new();
    for envelope in ordered {
        match envelope.fact.kind() {
            wire::FactKind::TaskAdded => {
                let added = envelope
                    .fact
                    .read::<body::TaskAdded>()
                    .expect("registered task-added fact decodes");
                if handles.contains_key(&added.task) {
                    phases.insert(added.task, added.phase);
                }
            }
            wire::FactKind::TaskDone => {
                let done = envelope
                    .fact
                    .read::<body::TaskDone>()
                    .expect("registered task-done fact decodes");
                if let Some(phase) = phases.get(&done.task).cloned() {
                    authors.insert(done.task, (phase, envelope.actor.clone()));
                }
            }
            wire::FactKind::TaskPhased => {
                let phased = envelope
                    .fact
                    .read::<body::TaskPhased>()
                    .expect("registered task-phased fact decodes");
                if handles.contains_key(&phased.task) {
                    phases.insert(phased.task, phased.phase);
                }
            }
            wire::FactKind::TaskReviewed => {
                let reviewed = envelope
                    .fact
                    .read::<body::TaskReviewed>()
                    .expect("registered task-reviewed fact decodes");
                let Some(task) = handles.get(&reviewed.task) else {
                    continue;
                };
                if reviewed.reviewer != envelope.actor {
                    return Err(ReviewHistoryError {
                        task: reviewed.task,
                        body: reviewed.reviewer,
                        fact: envelope.actor.clone(),
                        reviewed: envelope.seq,
                    });
                }
                reviews.push(ReviewEdge::new(
                    task.clone(),
                    authors.get(&reviewed.task).cloned(),
                    reviewed.reviewer,
                    reviewed.verdict,
                    envelope.seq,
                ));
            }
            _ => {}
        }
    }
    reviews.sort();
    Ok(reviews)
}

impl Diagram {
    /// Diagram identity.
    #[must_use]
    pub const fn view(&self) -> View {
        self.view
    }

    /// Deterministic textual rendering.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Render one named view from every current head and durable review relation.
#[must_use]
pub fn render(view: View, entries: &[Entry], reviews: &[ReviewEdge]) -> Diagram {
    render_scope(view, entries, reviews, false)
}

/// Render one named view from live task heads and their direct relations.
///
/// Dependency names still resolve through settled entries, but those entries
/// contribute no unrelated rows of their own.
#[must_use]
pub fn render_live(view: View, entries: &[Entry], reviews: &[ReviewEdge]) -> Diagram {
    render_scope(view, entries, reviews, true)
}

fn render_scope(view: View, entries: &[Entry], reviews: &[ReviewEdge], live_only: bool) -> Diagram {
    let mut ordered: Vec<&Entry> = entries.iter().collect();
    ordered.sort_by(|left, right| left.handle().cmp(right.handle()));
    let handles = entries
        .iter()
        .map(|entry| (entry.born(), entry.handle()))
        .collect::<BTreeMap<_, _>>();
    let live_handles = live_only.then(|| {
        entries
            .iter()
            .filter(|entry| live(entry))
            .map(|entry| entry.handle().clone())
            .collect::<BTreeSet<_>>()
    });
    let visible = |entry: &&Entry| !live_only || live(entry);

    let mut text = String::new();
    writeln!(text, "graph {}", view.as_str()).expect("String writes are infallible");
    match view {
        View::Origin => {
            for entry in ordered.into_iter().filter(visible) {
                let statement = entry.statement();
                writeln!(text, "{} -> {}", statement.provenance, entry.handle())
                    .expect("String writes are infallible");
            }
        }
        View::Dependencies => {
            let designs = entries
                .iter()
                .filter(|entry| {
                    entry
                        .statement()
                        .intent
                        .is_some_and(wire::TaskIntent::is_design)
                })
                .map(Entry::born)
                .collect::<BTreeSet<_>>();
            let name = |stamp: &TaskStamp| {
                handles
                    .get(stamp)
                    .map_or_else(|| stamp.as_str().to_owned(), ToString::to_string)
            };
            for entry in ordered.into_iter().filter(visible) {
                for edge in entry.dependencies() {
                    writeln!(text, "{} -> {}", name(&edge.task), name(&edge.after))
                        .expect("String writes are infallible");
                }
                // This virtual lifecycle relation presents the Design's wait
                // without adding an ordinary dependency to either statement.
                if let Some(design) = wire::intent::released_construction_root(
                    entry.born(),
                    entry.workflow(),
                    entry.statement().preparation.as_ref(),
                )
                .filter(|design| designs.contains(design))
                {
                    writeln!(text, "{} -> {}", name(design), entry.handle())
                        .expect("String writes are infallible");
                }
            }
        }
        View::Review => {
            let mut reviews = reviews.to_vec();
            reviews.sort();
            for edge in reviews {
                if live_handles
                    .as_ref()
                    .is_some_and(|handles| !handles.contains(&edge.task))
                {
                    continue;
                }
                let verdict = verdict(edge.verdict);
                match &edge.implemented {
                    Some((phase, author)) => writeln!(
                        text,
                        "{} -> {author} [{} {phase} {verdict}]",
                        edge.reviewer, edge.task,
                    ),
                    None => writeln!(
                        text,
                        "{} -> {} [no completed phase {verdict}]",
                        edge.reviewer, edge.task,
                    ),
                }
                .expect("String writes are infallible");
            }
        }
        View::Workflow => {
            for entry in ordered.into_iter().filter(visible) {
                writeln!(
                    text,
                    "{}: {} [{}]",
                    entry.handle(),
                    entry.workflow().phase,
                    entry.workflow().status.as_str()
                )
                .expect("String writes are infallible");
            }
        }
        View::Claims => {
            for entry in ordered.into_iter().filter(visible) {
                for claim in entry.claims().iter().map(crate::TaskClaim::claim) {
                    writeln!(
                        text,
                        "{}: {} [{}]",
                        entry.handle(),
                        claim.actor,
                        claim.disposition.as_str()
                    )
                    .expect("String writes are infallible");
                }
            }
        }
    }

    Diagram { view, text }
}

fn live(entry: &Entry) -> bool {
    entry.workflow().status.is_pending()
}

const fn verdict(verdict: body::Verdict) -> &'static str {
    match verdict {
        body::Verdict::Clean => "clean",
        body::Verdict::Finding => "finding",
    }
}

#[cfg(test)]
mod tests;
