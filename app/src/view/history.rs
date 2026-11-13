//! The one rule deciding how a route change meets browser history.

use super::{AppRoute, TaskView, WorkPlace};

/// Why the app is moving to another route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NavigationCause {
    /// The reader chose a destination: a scope, task, relationship, task view,
    /// Spice, or a card link.
    Destination,
    /// The reader refined the current view: a scope chip, the filter, or a
    /// graph control.
    Adjustment,
    /// The app corrected the location: canonical spelling, a refused or
    /// missing target, a consumed auth fragment, or a hidden-team reveal.
    Normalization,
}

/// The history entry a route change writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoryEffect {
    /// Add one entry Back returns from.
    Push,
    /// Rewrite the current entry in place.
    Replace,
    /// The route is already current; history and view stay as they are.
    Unchanged,
}

/// Classify one route change. Only a reader destination at another address
/// adds an entry; two routes spelling one address are the same destination.
#[must_use]
pub(crate) fn history_effect(
    current: &AppRoute,
    next: &AppRoute,
    cause: NavigationCause,
) -> HistoryEffect {
    let same = match (current, next) {
        (AppRoute::Work(current), AppRoute::Work(next)) => {
            current.options == next.options
                && match (&current.place, &next.place) {
                    (
                        WorkPlace::Task {
                            task: current,
                            view: current_view,
                        },
                        WorkPlace::Task {
                            task: next,
                            view: next_view,
                        },
                    ) => {
                        current == next
                            && TaskView::routed(*current_view) == TaskView::routed(*next_view)
                    }
                    (current, next) => current == next,
                }
        }
        (current, next) => current == next,
    };
    if same {
        return HistoryEffect::Unchanged;
    }
    match cause {
        NavigationCause::Destination => HistoryEffect::Push,
        NavigationCause::Adjustment | NavigationCause::Normalization => HistoryEffect::Replace,
    }
}
