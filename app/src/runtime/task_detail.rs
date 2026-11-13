//! On-demand task detail: which tasks the reader's surfaces show, what each
//! detail reads as, and which shown tasks still owe a read. Demand never
//! sends; the connection's bounded pump takes owed reads as mailbox capacity
//! allows, so any number of shown tasks hydrates without saturating it.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use wire::intent::TaskDetailAnswer;
use wire::{CanonicalUuid, InvitationState, Response, TaskStamp};

use super::{AppRuntime, ReadState};
use crate::socket::Admitted;

/// Capability-scoped state for one on-demand task detail: current task
/// evidence and its ordered durable notes.
pub type TaskDetailState = ReadState<Rc<TaskDetailAnswer>>;

impl TaskDetailState {
    /// Whether this settled state proves the durable task exists: an admitted
    /// answer does, and the resident's unknown-task refusal proves it does
    /// not. Loading and other refusals settle nothing.
    #[must_use]
    pub fn known(&self) -> Option<bool> {
        match self {
            Self::Available(_) | Self::Refreshing(_) => Some(true),
            Self::Unavailable { reason, .. } if reason == "task::detail-unknown" => Some(false),
            Self::Loading | Self::Unavailable { .. } => None,
        }
    }
}

/// Detail states, the tasks surfaces show, and the reads those tasks owe.
#[derive(Default, PartialEq, Eq)]
pub(super) struct TaskDetails {
    states: BTreeMap<TaskStamp, TaskDetailState>,
    /// Tasks some surface currently shows.
    shown: BTreeSet<TaskStamp>,
    /// Tasks whose next read is owed and not yet sent.
    owed: BTreeSet<TaskStamp>,
}

impl TaskDetails {
    /// Tasks some surface currently shows.
    pub(super) const fn shown(&self) -> &BTreeSet<TaskStamp> {
        &self.shown
    }
}

impl AppRuntime {
    /// On-demand detail state retained only for this admitted capability scope.
    #[must_use]
    pub fn task_detail(&self, task: &TaskStamp) -> Option<&TaskDetailState> {
        self.task_details.states.get(task)
    }

    /// Shown task details whose live invitation was issued by any moved
    /// inviter lane.
    pub(super) fn invitation_tasks_from(
        &self,
        inviters: &BTreeSet<CanonicalUuid>,
    ) -> Vec<TaskStamp> {
        self.task_details
            .states
            .iter()
            .filter(|(task, _)| self.task_details.shown.contains(*task))
            .filter(|(_, state)| {
                state.answer().is_some_and(|answer| {
                    answer.task.invitations.iter().any(|invitation| {
                        invitation.state == InvitationState::Invited
                            && inviters.contains(&invitation.inviter)
                    })
                })
            })
            .map(|(task, _)| task.clone())
            .collect()
    }

    /// Replace the tasks the reader's surfaces show. A shown task without an
    /// admitted answer reads as Loading and owes a read unless one is already
    /// owed or in flight; a refused one is asked again. A task no longer shown
    /// draws no further reads, and its unanswered placeholder is dropped.
    /// Returns whether any read is owed.
    pub fn show_task_details(&mut self, tasks: impl IntoIterator<Item = TaskStamp>) -> bool {
        let TaskDetails {
            states,
            shown,
            owed,
        } = &mut self.task_details;
        *shown = tasks.into_iter().collect();
        owed.retain(|task| shown.contains(task));
        states.retain(|task, state| shown.contains(task) || *state != TaskDetailState::Loading);
        for task in shown.iter() {
            if matches!(
                states.get(task),
                None | Some(TaskDetailState::Unavailable { .. })
            ) {
                states.insert(task.clone(), TaskDetailState::Loading);
                owed.insert(task.clone());
            }
        }
        !owed.is_empty()
    }

    /// Ask again for one shown task whose read did not produce an answer.
    /// Returns whether a read became owed.
    pub fn retry_task_detail(&mut self, task: &TaskStamp) -> bool {
        let details = &mut self.task_details;
        if !details.shown.contains(task)
            || details
                .states
                .get(task)
                .and_then(TaskDetailState::answer)
                .is_some()
        {
            return false;
        }
        details
            .states
            .insert(task.clone(), TaskDetailState::Loading);
        details.owed.insert(task.clone())
    }

    /// Read a shown task again after an accepted command changed it, keeping
    /// any admitted answer on screen meanwhile. A read already in flight may
    /// predate the change, so this one is owed after it settles. A task no
    /// surface shows is forgotten without creating new demand.
    pub fn reread_task_detail(&mut self, task: &TaskStamp) {
        let details = &mut self.task_details;
        if !details.shown.contains(task) {
            details.states.remove(task);
            details.owed.remove(task);
            return;
        }
        details
            .states
            .entry(task.clone())
            .and_modify(TaskDetailState::refresh)
            .or_insert(TaskDetailState::Loading);
        details.owed.insert(task.clone());
    }

    /// Mark one task a fact changed. A shown task keeps its admitted answer
    /// while it owes a replacement read; a task no surface shows is forgotten,
    /// so showing it again reads it fresh. Returns whether a read became owed.
    pub fn invalidate_task_detail(&mut self, task: &TaskStamp) -> bool {
        let details = &mut self.task_details;
        if !details.shown.contains(task) {
            details.states.remove(task);
            return false;
        }
        let Some(state) = details.states.get_mut(task) else {
            return false;
        };
        state.refresh();
        details.owed.insert(task.clone());
        true
    }

    /// Take up to `limit` owed reads in identity order, skipping any task
    /// whose earlier read is still in flight. A taken task owes nothing more
    /// until it is restored or invalidated again.
    pub fn take_task_detail_reads(
        &mut self,
        limit: usize,
        in_flight: impl Fn(&TaskStamp) -> bool,
    ) -> Vec<TaskStamp> {
        let owed = &mut self.task_details.owed;
        let taken = owed
            .iter()
            .filter(|task| !in_flight(task))
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        for task in &taken {
            owed.remove(task);
        }
        taken
    }

    /// Owe a taken read again because the connection could not send it.
    pub fn restore_task_detail_read(&mut self, task: TaskStamp) {
        self.task_details.owed.insert(task);
    }

    /// Apply one correlated detail response without crossing capability scope.
    /// A response whose surface closed while it was in flight forgets that
    /// task's detail, so showing it again reads current detail instead of
    /// keeping a refresh nothing will answer.
    pub fn apply_task_detail(
        &mut self,
        task: &TaskStamp,
        admitted: &Admitted<Response<TaskDetailAnswer>>,
    ) {
        if admitted.scope() != self.accepted.scope() {
            return;
        }
        if !self.task_details.shown.contains(task) {
            self.task_details.states.remove(task);
            return;
        }
        let state = TaskDetailState::settle(
            &admitted.get().outcome,
            |answer| (answer.task.statement.body.born == *task).then(|| Rc::new(answer.clone())),
            (
                "task::detail-identity-mismatch",
                "the detail answer named a different durable task",
            ),
            "task::detail-refused",
        );
        self.task_details.states.insert(task.clone(), state);
    }

    /// A read in flight at a dropped connection never answers, and facts
    /// published while it was down were never streamed: every shown task owes
    /// a fresh read, and details no surface shows are forgotten.
    pub(super) fn reconnect_task_details(&mut self) {
        let TaskDetails {
            states,
            shown,
            owed,
        } = &mut self.task_details;
        states.retain(|task, _| shown.contains(task));
        states.values_mut().for_each(TaskDetailState::refresh);
        *owed = states.keys().cloned().collect();
    }
}

#[cfg(test)]
mod tests {
    use super::TaskDetailState;

    #[test]
    fn only_the_resident_unknown_refusal_settles_a_task_as_missing() {
        let unavailable = |reason: &str| TaskDetailState::Unavailable {
            reason: reason.to_owned(),
            detail: None,
        };
        assert_eq!(unavailable("task::detail-unknown").known(), Some(false));
        for transient in [
            "task::detail-refused",
            "task::detail-identity-mismatch",
            "connection::closed",
        ] {
            assert_eq!(unavailable(transient).known(), None, "{transient}");
        }
        assert_eq!(TaskDetailState::Loading.known(), None);
    }
}
