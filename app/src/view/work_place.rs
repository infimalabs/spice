//! The destinations a Work address names beside its scopes: one task with an
//! optional view, and one explicit ordered selection.

use std::collections::BTreeSet;

use wire::TaskStamp;

use super::WorkRoute;
use super::work_options::{WorkOptions, decode_component, encode_component};

const TASK_PREFIX: &str = "/work/task/";
const SELECTION_PATH: &str = "/work/selection";
const TASK_KEY: &str = "task";
const RELATED_KEY: &str = "related";

/// A view one opened task's links line expands in place of its conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskView {
    /// The room: a Design's standing line and lanes over its conversation.
    /// Its address is the task's base address.
    Room,
    /// A Design's questions, each answered or open.
    Questions,
    Description,
    /// An execution task's acceptance criteria.
    Acceptance,
    /// Every fact about the task.
    Facts,
    /// A Design's proposal versions newest first.
    History,
}

impl TaskView {
    const ALL: [Self; 6] = [
        Self::Room,
        Self::Questions,
        Self::Description,
        Self::Acceptance,
        Self::Facts,
        Self::History,
    ];

    /// The view's address segment.
    #[must_use]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Room => "room",
            Self::Questions => "questions",
            Self::Description => "description",
            Self::Acceptance => "acceptance",
            Self::Facts => "facts",
            Self::History => "history",
        }
    }

    pub(super) fn parse(segment: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|view| view.label() == segment)
    }

    /// The route's view, with the room folded into the base address.
    pub(super) fn routed(view: Option<Self>) -> Option<Self> {
        view.filter(|view| *view != Self::Room)
    }
}

/// One explicit Work selection: a complete, stable, deduplicated root order
/// and, when a relationship lead extended it, that lead. The root set is
/// never recomputed or truncated to a relationship-enrichment bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkSelection {
    tasks: Vec<TaskStamp>,
    related: Option<TaskStamp>,
}

impl WorkSelection {
    /// Admit `tasks` in first-seen order; a lead absent from them joins last.
    /// An empty selection is no destination.
    #[must_use]
    pub(crate) fn new(
        tasks: impl IntoIterator<Item = TaskStamp>,
        related: Option<TaskStamp>,
    ) -> Option<Self> {
        let mut seen = BTreeSet::new();
        let tasks = tasks
            .into_iter()
            .chain(related.clone())
            .filter(|task| seen.insert(task.clone()))
            .collect::<Vec<_>>();
        (!tasks.is_empty()).then_some(Self { tasks, related })
    }

    #[must_use]
    pub(crate) fn tasks(&self) -> &[TaskStamp] {
        &self.tasks
    }

    #[must_use]
    pub(crate) const fn related(&self) -> Option<&TaskStamp> {
        self.related.as_ref()
    }

    /// This selection as the overview reads it.
    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    #[must_use]
    pub(crate) fn to_overview(&self) -> crate::work::TaskOverviewSelection {
        use crate::work::{TaskOverviewContext, TaskOverviewSelection};
        let context = self.related.clone().map_or(
            TaskOverviewContext::Task,
            TaskOverviewContext::Relationships,
        );
        TaskOverviewSelection::new(self.tasks.iter().cloned(), context)
    }
}

/// The place one Work address opens over Chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkPlace {
    /// A scope whose membership the current board resolves.
    Scope(WorkRoute),
    /// One task, at its base address or one expanded view.
    Task {
        task: TaskStamp,
        view: Option<TaskView>,
    },
    Selection(WorkSelection),
}

/// One Work address: its place and the reader's overview adjustments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkLocation {
    pub(crate) place: WorkPlace,
    pub(crate) options: WorkOptions,
}

impl WorkLocation {
    /// Parse one Work path and its fragment query, returning the parts no
    /// Work address carries. A malformed path or a selection with no valid
    /// root is no location.
    pub(super) fn parse(path: &str, query: &str) -> Option<(Self, Vec<String>)> {
        let mut ignored = Vec::new();
        let mut place = if let Some(task) = path.strip_prefix(TASK_PREFIX) {
            let (task, view) = match task.split_once('/') {
                Some((task, view)) => (task, Some(view)),
                None => (task, None),
            };
            let view = view.and_then(|segment| {
                let view = TaskView::parse(segment);
                if view.is_none() {
                    ignored.push(format!("/{segment}"));
                }
                view
            });
            Some(WorkPlace::Task {
                task: task.parse().ok()?,
                view: TaskView::routed(view),
            })
        } else if path == SELECTION_PATH {
            None
        } else {
            Some(WorkPlace::Scope(WorkRoute::parse(path)?))
        };

        let mut options = WorkOptions::default();
        let mut tasks = Vec::new();
        let mut related = None;
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let admitted = pair.split_once('=').and_then(|(key, value)| {
                let (key, value) = (decode_component(key)?, decode_component(value)?);
                match key.as_str() {
                    TASK_KEY if place.is_none() => {
                        tasks.push(value.parse().ok()?);
                    }
                    RELATED_KEY if place.is_none() && related.is_none() => {
                        related = Some(value.parse().ok()?);
                    }
                    _ => return options.admit(&key, &value).then_some(()),
                }
                Some(())
            });
            if admitted.is_none() {
                ignored.push(pair.to_owned());
            }
        }
        if place.is_none() {
            place = Some(WorkPlace::Selection(WorkSelection::new(tasks, related)?));
        }
        Some((
            Self {
                place: place?,
                options,
            },
            ignored,
        ))
    }

    /// The canonical path and non-default fragment query.
    pub(super) fn path(&self) -> String {
        let mut pairs = Vec::new();
        let mut path = match &self.place {
            WorkPlace::Scope(route) => route.path(),
            WorkPlace::Task { task, view } => match TaskView::routed(*view) {
                Some(view) => format!("{TASK_PREFIX}{task}/{}", view.label()),
                None => format!("{TASK_PREFIX}{task}"),
            },
            WorkPlace::Selection(selection) => {
                pairs.extend(
                    selection
                        .tasks()
                        .iter()
                        .map(|task| (TASK_KEY, task.to_string())),
                );
                pairs.extend(
                    selection
                        .related()
                        .map(|task| (RELATED_KEY, task.to_string())),
                );
                SELECTION_PATH.to_owned()
            }
        };
        self.options.write_query(&mut pairs);
        for (index, (key, value)) in pairs.iter().enumerate() {
            path.push(if index == 0 { '?' } else { '&' });
            path.push_str(key);
            path.push('=');
            path.push_str(&encode_component(value));
        }
        path
    }
}
