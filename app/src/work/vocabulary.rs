//! The one vocabulary every Work surface speaks: where a task stands on its
//! kind's track, and who has it.
//!
//! The board carries no parked standing: `parked_commits` only preserves a
//! released claim's commits while the task stays offered. No task therefore
//! reads as held, and no held group exists.

use std::fmt;

use wire::intent::BoardTask;
use wire::rooms::DesignCycle;
use wire::{Actor, Instant, TaskStamp};

use super::{OperatorWaits, WorkAvailability, WorkProjection, WorkTask};
use crate::runtime::BoardView;

/// Where one Design stands on the Design track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesignPhase {
    /// Gathering input: `answered` of its `questions` current questions have
    /// been weighed.
    Input { answered: usize, questions: usize },
    /// Counted input arrived; a lane may claim it for a research pass.
    Steeped,
    /// A completed research pass waits on the operator and no output was
    /// ever accepted.
    Researched,
    /// An output was accepted: its work runs or settled, or a conclusion
    /// waits on the operator.
    Decided,
    /// The operator concluded it done or collapsed.
    Concluded,
}

/// Where one task stands on its kind's track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkPhase<'a> {
    Design(DesignPhase),
    /// An execution task's flow phase; settled work reads its settlement.
    Execution(&'a str),
}

impl fmt::Display for WorkPhase<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Design(DesignPhase::Input {
                answered,
                questions,
            }) => write!(formatter, "input {answered}/{questions}"),
            Self::Design(DesignPhase::Steeped) => formatter.write_str("steeped"),
            Self::Design(DesignPhase::Researched) => formatter.write_str("researched"),
            Self::Design(DesignPhase::Decided) => formatter.write_str("decided"),
            Self::Design(DesignPhase::Concluded) => formatter.write_str("concluded"),
            Self::Execution(phase) => formatter.write_str(phase),
        }
    }
}

/// One step of a task's track as its opened header reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackStep {
    pub label: String,
    /// Whether the task stands on this step now.
    pub current: bool,
}

/// Who has one task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkHolder<'a> {
    /// Only the operator can move it.
    Yours,
    /// This actor's live claim is working it.
    Working(&'a Actor),
    /// Anyone could pick it up or contribute.
    Ready,
    /// It stands still behind these unsettled prerequisites; empty when only
    /// a resource pool holds it.
    WaitsOn(Vec<&'a TaskStamp>),
    /// It is set aside until this instant, or until woken when the instant
    /// is the indefinite horizon.
    Deferred(&'a Instant),
    /// Its author has not filed it yet.
    Draft,
    /// Settled, named as every surface names a settlement.
    Settled(wire::Settlement),
}

impl WorkHolder<'_> {
    /// The list group this holder belongs to.
    #[must_use]
    pub const fn group(&self) -> HolderGroup {
        match self {
            Self::Yours => HolderGroup::Yours,
            Self::Working(_) => HolderGroup::Working,
            Self::Ready => HolderGroup::Ready,
            Self::WaitsOn(_) => HolderGroup::Waiting,
            Self::Deferred(_) => HolderGroup::Deferred,
            Self::Draft => HolderGroup::Draft,
            Self::Settled(_) => HolderGroup::Finished,
        }
    }

    /// The token's glyph: its shape alone tells the tokens apart.
    #[must_use]
    pub const fn glyph(&self) -> &'static str {
        match self {
            Self::Settled(wire::Settlement::Deleted) => "✕",
            _ => self.group().glyph(),
        }
    }

    /// The token's word.
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Yours => "yours",
            Self::Working(_) => "working",
            Self::Ready => "ready",
            Self::WaitsOn(_) => "waits on",
            Self::Deferred(_) => "deferred",
            Self::Draft => "draft",
            Self::Settled(settlement) => settlement.as_str(),
        }
    }
}

/// What a deferral at the indefinite horizon reads wherever a deferral's
/// time would otherwise show.
pub const UNTIL_WOKEN: &str = "until woken";

/// One group of the Work list, in list order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HolderGroup {
    Yours,
    Working,
    Ready,
    Waiting,
    Deferred,
    Draft,
    Finished,
}

impl HolderGroup {
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            Self::Yours => "◆",
            Self::Working => "●",
            Self::Ready => "○",
            Self::Waiting => "⊘",
            Self::Deferred => "◷",
            Self::Draft => "◌",
            Self::Finished => "✓",
        }
    }

    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Yours => "yours",
            Self::Working => "working",
            Self::Ready => "ready",
            Self::Waiting => "waiting",
            Self::Deferred => "deferred",
            Self::Draft => "draft",
            Self::Finished => "done",
        }
    }
}

/// One nonempty group of a list and its tasks' projection indexes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkGroup {
    pub group: HolderGroup,
    pub tasks: Vec<usize>,
}

/// One task set as the Work list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkList {
    /// Each nonempty group in group order, its tasks in the set's order.
    pub groups: Vec<WorkGroup>,
    /// Tasks waiting on the operator that the filter rejects: listed apart,
    /// never hidden.
    pub outside: Vec<usize>,
}

impl<'a> WorkTask<'a> {
    /// Where this task stands on its kind's track; `None` once withdrawn,
    /// which no track step names.
    #[must_use]
    pub const fn phase(&self) -> Option<WorkPhase<'a>> {
        self.phase
    }

    /// Every step of this task's track, using its projected phase.
    #[must_use]
    pub fn track(&self) -> Vec<TrackStep> {
        track(self.row, self.phase)
    }

    /// Whether `query` names this task by its handle, its title, or `lane`,
    /// the label of the lane holding it, ignoring case. An empty query
    /// matches every task.
    #[must_use]
    pub fn matches_filter(&self, query: &str, lane: Option<&str>) -> bool {
        let query = query.trim().to_lowercase();
        let names = |text: &str| text.to_lowercase().contains(&query);
        query.is_empty()
            || names(&self.row.statement.body.handle.to_string())
            || names(&self.row.presentation_title)
            || lane.is_some_and(names)
    }

    fn holder(&self, operator: bool) -> WorkHolder<'a> {
        match self.availability {
            WorkAvailability::Terminal => WorkHolder::Settled(
                self.row
                    .settlement()
                    .expect("terminal work carries its settlement"),
            ),
            WorkAvailability::Draft => WorkHolder::Draft,
            _ if operator => WorkHolder::Yours,
            WorkAvailability::Selection | WorkAvailability::Held => WorkHolder::Yours,
            WorkAvailability::Deferred => WorkHolder::Deferred(
                self.row
                    .workflow
                    .body
                    .deferral
                    .as_ref()
                    .expect("deferred work carries its deferral"),
            ),
            _ if self.awaiting_selection => WorkHolder::Yours,
            WorkAvailability::InFlight => WorkHolder::Working(
                self.working_claimant()
                    .expect("in-flight work carries its active claim"),
            ),
            WorkAvailability::Ready
            | WorkAvailability::Review
            | WorkAvailability::Stale
            | WorkAvailability::Passive => WorkHolder::Ready,
            WorkAvailability::Waiting | WorkAvailability::Blocked => WorkHolder::WaitsOn(
                self.dependencies
                    .iter()
                    .filter(|dependency| !dependency.satisfied())
                    .map(super::WorkDependency::born)
                    .collect(),
            ),
        }
    }
}

/// A retained task's track, derived from its own admitted lifecycle rather
/// than requiring a row in the current board window.
#[cfg(any(target_arch = "wasm32", test))]
#[must_use]
pub(crate) fn retained_track(row: &BoardTask) -> Vec<TrackStep> {
    track(row, phase(None, row, &[]))
}

fn track(row: &BoardTask, phase: Option<WorkPhase<'_>>) -> Vec<TrackStep> {
    let Some(phase) = phase else {
        return Vec::new();
    };
    let step = |label: String, current: bool| TrackStep { label, current };
    match phase {
        WorkPhase::Design(current) => [
            DesignPhase::Input {
                answered: 0,
                questions: 0,
            },
            DesignPhase::Steeped,
            DesignPhase::Researched,
            DesignPhase::Decided,
            DesignPhase::Concluded,
        ]
        .into_iter()
        .map(|candidate| {
            let at = std::mem::discriminant(&candidate) == std::mem::discriminant(&current);
            match candidate {
                DesignPhase::Input { .. } if !at => step("input".to_owned(), false),
                _ if at => step(WorkPhase::Design(current).to_string(), true),
                _ => step(WorkPhase::Design(candidate).to_string(), false),
            }
        })
        .collect(),
        WorkPhase::Execution(current) => {
            // The track runs over the substantive route: draft is intake, not a
            // step, and the track ends where the task ends: `done` while live,
            // and the settlement word once settled, so a closed task never
            // reads done.
            let terminal = row
                .settlement()
                .map_or("done", |settlement| settlement.as_str());
            let flow = &row.workflow.body.flow;
            flow.iter()
                .filter(|phase| **phase != wire::Phase::Draft)
                .map(|phase| phase.as_str())
                .filter(|phase| *phase != "done")
                .chain([terminal])
                .map(|label| step(label.to_owned(), label == current))
                .collect()
        }
    }
}

impl<'a> WorkProjection<'a> {
    /// Every task's holder, by projection index. Operator waits take precedence,
    /// so a gate only a human can repair reads yours whatever the task's own
    /// standing.
    #[must_use]
    pub fn holders(&self) -> Vec<WorkHolder<'a>> {
        let mut operator = vec![false; self.tasks.len()];
        for wait in OperatorWaits::new(self).waits() {
            operator[wait.task()] = true;
        }
        self.tasks
            .iter()
            .zip(operator)
            .map(|(task, operator)| task.holder(operator))
            .collect()
    }

    /// `tasks` grouped by `holders`, keeping only those `keep` admits. A task
    /// waiting on the operator that `keep` rejects moves to
    /// [`WorkList::outside`], so a filter never hides one. Stamps the
    /// projection does not carry are skipped.
    #[must_use]
    pub fn list(
        &self,
        tasks: &[TaskStamp],
        holders: &[WorkHolder<'_>],
        keep: impl Fn(&WorkTask<'a>) -> bool,
    ) -> WorkList {
        let by_born = self
            .tasks
            .iter()
            .enumerate()
            .map(|(index, task)| (&task.row.statement.body.born, index))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut list = WorkList {
            groups: Vec::new(),
            outside: Vec::new(),
        };
        for &index in tasks.iter().filter_map(|stamp| by_born.get(stamp)) {
            let group = holders[index].group();
            if !keep(&self.tasks[index]) {
                if group == HolderGroup::Yours {
                    list.outside.push(index);
                }
                continue;
            }
            match list
                .groups
                .binary_search_by(|candidate| candidate.group.cmp(&group))
            {
                Ok(at) => list.groups[at].tasks.push(index),
                Err(at) => list.groups.insert(
                    at,
                    WorkGroup {
                        group,
                        tasks: vec![index],
                    },
                ),
            }
        }
        list
    }

    /// The graph follows the listed rows, plus an exact task whose identity
    /// lies outside the board window. Filters still govern current rows.
    #[must_use]
    pub fn graph_selection(
        &self,
        selection: &super::TaskOverviewSelection,
        list: &WorkList,
    ) -> super::TaskOverviewSelection {
        let shown = list
            .outside
            .iter()
            .chain(list.groups.iter().flat_map(|group| &group.tasks))
            .map(|&index| &self.tasks[index].row.statement.body.born)
            .collect::<std::collections::BTreeSet<_>>();
        let retained = selection.sole_task().is_some_and(|task| {
            !self
                .tasks
                .iter()
                .any(|row| row.row.statement.body.born == *task)
        });
        super::TaskOverviewSelection::new(
            selection
                .tasks()
                .iter()
                .filter(|task| retained || shown.contains(task))
                .cloned(),
            selection.context().clone(),
        )
    }

    /// Explicit standing of a prerequisite carried by this board. Board
    /// absence alone says nothing when reading a historical task.
    #[must_use]
    pub fn prerequisite_satisfied(&self, prerequisite: &TaskStamp) -> Option<bool> {
        self.tasks
            .iter()
            .find(|task| task.row.statement.body.born == *prerequisite)
            .map(|task| task.row.workflow.body.status.completion().is_some())
    }
}

/// Where `row` stands on its kind's track, given the projection indexes of
/// the outputs it released.
pub(super) fn phase<'a>(
    board: Option<&BoardView>,
    row: &'a BoardTask,
    released: &[usize],
) -> Option<WorkPhase<'a>> {
    let status = &row.workflow.body.status;
    if status.is_deleted() {
        return None;
    }
    let Some(standing) = row.design_standing() else {
        return Some(WorkPhase::Execution(if status.is_pending() {
            row.workflow.body.phase.as_str()
        } else {
            row.status_word()
        }));
    };
    if !standing.is_open() {
        return Some(WorkPhase::Design(DesignPhase::Concluded));
    }
    let facts = row.design_cycle;
    let cycle = board
        .and_then(|board| board.design_cycles.get(&row.statement.body.born))
        .copied()
        .unwrap_or_else(|| {
            facts.cycle(board.is_some_and(|board| {
                released
                    .iter()
                    .any(|&output| board.tasks[output].workflow.body.status.is_pending())
            }))
        });
    // Released outputs and settled released work exist only once a decision
    // accepted an output; from then on the Design stays decided.
    if !released.is_empty()
        || facts.settled.is_some()
        || cycle == DesignCycle::WaitingOnSelectedWork
    {
        return Some(WorkPhase::Design(DesignPhase::Decided));
    }
    Some(WorkPhase::Design(match cycle {
        DesignCycle::GatheringInput => {
            let (answered, questions) = row.design_coverage.as_ref().map_or_else(
                || (0, row.statement.body.term.len()),
                |coverage| {
                    (
                        coverage.covered.len(),
                        coverage.covered.len() + coverage.missing.len(),
                    )
                },
            );
            DesignPhase::Input {
                answered,
                questions,
            }
        }
        DesignCycle::ReadyForPass => DesignPhase::Steeped,
        DesignCycle::AwaitingDecision => DesignPhase::Researched,
        DesignCycle::WaitingOnSelectedWork => DesignPhase::Decided,
    }))
}

#[cfg(test)]
mod tests;
