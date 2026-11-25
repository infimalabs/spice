//! Read-only decision-history labels derived from durable decision and task
//! authority. Browser history and room cards use this one vocabulary.

use std::collections::BTreeSet;

use wire::TaskStamp;
use wire::intent::BoardTask;
use wire::rooms::{DecisionOutcome, OutputDecision, OutputVerdict, PreparationDecision};

use crate::runtime::{BoardView, PreparationProjection, TaskDetailState};

const COMMIT_ABBREVIATION: usize = 9;

/// Operator-facing disposition of a decision.
#[cfg(target_arch = "wasm32")]
pub(crate) fn disposition(outcome: &DecisionOutcome) -> &'static str {
    match outcome {
        DecisionOutcome::Returned { .. } => "returned",
        DecisionOutcome::Concluded {} => "concluded",
        DecisionOutcome::Collapsed { .. } => "collapsed",
        DecisionOutcome::Resolved { outputs, .. }
            if outputs
                .iter()
                .any(|output| output.verdict == OutputVerdict::Accepted) =>
        {
            "accepted"
        }
        DecisionOutcome::Resolved { .. } => "rejected",
    }
}

/// Recorded verdict for one offered output, if its decision resolved it.
#[cfg(target_arch = "wasm32")]
pub(crate) fn output_decision<'a>(
    decision: &'a PreparationDecision,
    output: &TaskStamp,
) -> Option<&'a OutputDecision> {
    let DecisionOutcome::Resolved { outputs, .. } = &decision.outcome else {
        return None;
    };
    outputs.iter().find(|decision| decision.output == *output)
}

/// Current board/detail standing of one decided output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OutputState {
    /// The output still exists and can advance through this phase.
    Open { phase: String },
    /// The output settled; a completion boundary may identify its integration.
    Settled {
        state: &'static str,
        commit: Option<String>,
    },
    /// The board does not carry this identity and its task detail read has
    /// not settled it yet.
    Resolving,
    /// The resident's task detail read refused this durable identity as unknown.
    Missing,
}

impl OutputState {
    pub(crate) const fn key(&self) -> &'static str {
        match self {
            Self::Open { .. } => "open",
            Self::Settled { state, .. } => state,
            Self::Resolving => "resolving",
            Self::Missing => "missing",
        }
    }
}

/// Evidence for one exact task identity through the board's bounded rows and
/// the targeted detail read used when that identity falls outside them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TaskEvidence<'a> {
    Available(&'a BoardTask),
    Loading,
    Missing,
    Refused {
        reason: &'a str,
        detail: Option<&'a str>,
    },
}

/// Resolve one exact task without widening the board's recent-finished window.
pub(crate) fn task_evidence<'a>(
    board: Option<&'a BoardView>,
    detail: Option<&'a TaskDetailState>,
    task: &TaskStamp,
) -> TaskEvidence<'a> {
    if let Some(row) = board.and_then(|board| board.row(task)) {
        return TaskEvidence::Available(row);
    }
    match detail {
        Some(TaskDetailState::Available(answer) | TaskDetailState::Refreshing(answer)) => {
            TaskEvidence::Available(&answer.task)
        }
        Some(TaskDetailState::Unavailable { reason, .. }) if reason == "task::detail-unknown" => {
            TaskEvidence::Missing
        }
        Some(TaskDetailState::Unavailable { reason, detail }) => TaskEvidence::Refused {
            reason,
            detail: detail.as_deref(),
        },
        None | Some(TaskDetailState::Loading) => TaskEvidence::Loading,
    }
}

/// Targeted details for selected tasks, their decisions' off-board outputs,
/// and every accepted output whose complete evidence is needed even while the
/// board carries its standing.
pub(crate) fn detail_demand(
    selected: &[TaskStamp],
    preparation: &PreparationProjection,
    board: Option<&BoardView>,
) -> BTreeSet<TaskStamp> {
    let mut demand = selected.iter().cloned().collect::<BTreeSet<_>>();
    let Some(board) = board else {
        return demand;
    };
    let designs = selected.iter().cloned().collect::<BTreeSet<_>>();
    let owed = preparation
        .decisions
        .values()
        .filter(|decision| designs.contains(&decision.value.decision.controller))
        .flat_map(|decision| &decision.value.basis.outputs)
        .map(|revision| &revision.task)
        .filter(|task| board.row(task).is_none());
    demand.extend(owed.cloned());
    demand.extend(accepted_output_tasks(preparation, &designs).cloned());
    demand
}

/// Accepted outputs of the mounted Designs. They always owe targeted detail:
/// board rows provide immediate standing, while detail supplies evidence the
/// bounded board contract does not carry.
pub(crate) fn accepted_output_tasks<'a>(
    preparation: &'a PreparationProjection,
    designs: &'a BTreeSet<TaskStamp>,
) -> impl Iterator<Item = &'a TaskStamp> + 'a {
    preparation
        .decisions
        .values()
        .filter(|decision| designs.contains(&decision.value.decision.controller))
        .flat_map(|decision| decision.value.outcome.accepted_outputs())
}

/// Derive output standing from the board, else task detail. Leaving the open
/// board never reads as missing: only the resident's unknown-task refusal does.
pub(crate) fn output_state(
    board: Option<&BoardView>,
    detail: Option<&TaskDetailState>,
    task: &TaskStamp,
) -> OutputState {
    let row = match task_evidence(board, detail, task) {
        TaskEvidence::Available(row) => row,
        TaskEvidence::Missing => return OutputState::Missing,
        TaskEvidence::Loading | TaskEvidence::Refused { .. } => {
            return OutputState::Resolving;
        }
    };
    if row.workflow.body.status.is_pending() {
        return OutputState::Open {
            phase: row.workflow.body.phase.as_str().to_owned(),
        };
    }
    let state = row.status_word();
    let commit = row
        .git_context
        .as_ref()
        .map(|context| abbreviate_commit(&context.boundary.integrated));
    OutputState::Settled { state, commit }
}

fn abbreviate_commit(commit: &str) -> String {
    commit.chars().take(COMMIT_ABBREVIATION).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RoomPositioned;
    use crate::work::tests::{board, stamp, task};
    use std::rc::Rc;
    use wire::body::GitBoundary;
    use wire::intent::{TaskDetailAnswer, TaskGitContext, TaskGitContextKind};
    use wire::rooms::{DecisionBasis, DecisionRef, OutputRevision, RoomSource};

    fn settled_at() -> wire::Instant {
        "2026-11-01T00:00:00.000Z".parse().expect("fixture instant")
    }

    fn seq(value: u64) -> wire::Seq {
        wire::Seq::new(value).expect("fixture sequence")
    }

    fn done_task(id: i64, integrated: &str) -> BoardTask {
        let mut done = task(id, "/app");
        done.workflow.body.status = wire::WorkflowStatus::completed(settled_at());
        done.git_context = Some(TaskGitContext {
            fact: seq(3),
            kind: TaskGitContextKind::Completion,
            boundary: GitBoundary {
                head: integrated.to_owned(),
                integrated: integrated.to_owned(),
                upstream: None,
                upstream_head: None,
                local_commits: 1,
            },
        });
        done
    }

    fn refused(reason: &str) -> TaskDetailState {
        TaskDetailState::Unavailable {
            reason: reason.to_owned(),
            detail: None,
        }
    }

    #[test]
    fn output_evidence_uses_targeted_detail_beyond_the_finished_window() {
        let open = task(1, "/app");
        let open_stamp = open.statement.body.born.clone();
        let on_board = done_task(2, "abcdef0123456789");
        let on_board_stamp = on_board.statement.body.born.clone();
        let off_board = done_task(3, "fedcba9876543210");
        let off_board_stamp = off_board.statement.body.born.clone();
        let mut view = board(vec![open]);
        view.finished.push(on_board);
        view.finished
            .extend((4..23).map(|id| done_task(id, &format!("{id:016x}"))));
        let view = Some(&view);

        assert!(matches!(
            task_evidence(view, None, &off_board_stamp),
            TaskEvidence::Loading
        ));
        assert_eq!(
            task_evidence(
                view,
                Some(&refused("task::detail-refused")),
                &off_board_stamp,
            ),
            TaskEvidence::Refused {
                reason: "task::detail-refused",
                detail: None,
            }
        );

        assert_eq!(
            output_state(view, None, &open_stamp),
            OutputState::Open {
                phase: "todo".to_owned()
            }
        );
        assert_eq!(
            output_state(view, None, &on_board_stamp),
            OutputState::Settled {
                state: "done",
                commit: Some("abcdef012".to_owned())
            }
        );
        for unsettled in [
            None,
            Some(TaskDetailState::Loading),
            Some(refused("task::detail-refused")),
        ] {
            assert_eq!(
                output_state(view, unsettled.as_ref(), &off_board_stamp),
                OutputState::Resolving,
                "{unsettled:?}"
            );
        }
        let answered = TaskDetailState::Available(Rc::new(TaskDetailAnswer {
            task: off_board,
            media: Vec::new(),
            closure: None,
            deletion: None,
            disposition: None,
            review: None,
            pass: None,
            origin: None,
            description_quotes: Vec::new(),
        }));
        assert!(matches!(
            task_evidence(view, Some(&answered), &off_board_stamp),
            TaskEvidence::Available(task)
                if task.statement.body.born == off_board_stamp
        ));
        assert_eq!(
            output_state(view, Some(&answered), &off_board_stamp),
            OutputState::Settled {
                state: "done",
                commit: Some("fedcba987".to_owned())
            }
        );
        let missing = output_state(
            view,
            Some(&refused("task::detail-unknown")),
            &off_board_stamp,
        );
        assert_eq!(missing, OutputState::Missing);
        assert_eq!(
            task_evidence(
                view,
                Some(&refused("task::detail-unknown")),
                &off_board_stamp,
            ),
            TaskEvidence::Missing
        );
        // Only a settled unknown wears the key the danger tone selects.
        assert_ne!(OutputState::Resolving.key(), missing.key());
    }

    #[test]
    fn only_shown_designs_owe_unresolved_and_accepted_output_details() {
        let carried = task(12, "/app");
        let carried_stamp = carried.statement.body.born.clone();
        let view = board(vec![carried]);
        let revision = |task: TaskStamp| OutputRevision {
            task,
            statement: seq(1),
            workflow: seq(2),
            dependencies: Vec::new(),
        };
        let verdict = |output: TaskStamp, verdict| OutputDecision {
            output,
            verdict,
            comment: None,
        };
        let decision = |controller: TaskStamp, outputs: Vec<OutputDecision>| {
            let decision = DecisionRef {
                controller,
                decision: wire::SequenceCode::new(1),
            };
            let positioned = RoomPositioned {
                position: seq(2),
                value: PreparationDecision {
                    decision: decision.clone(),
                    source: RoomSource::Journal {
                        fact: wire::SequenceCode::new(2),
                    },
                    basis: DecisionBasis {
                        statement: seq(1),
                        room: None,
                        outputs: outputs
                            .iter()
                            .map(|output| revision(output.output.clone()))
                            .collect(),
                    },
                    outcome: DecisionOutcome::Resolved {
                        outputs,
                        comment: None,
                    },
                },
            };
            (decision, positioned)
        };
        let (selected_design, mounted_design, unmounted_design) = (stamp(10), stamp(11), stamp(16));
        let mut preparation = PreparationProjection::default();
        preparation.decisions.extend([
            decision(
                selected_design.clone(),
                vec![
                    verdict(carried_stamp.clone(), OutputVerdict::Accepted),
                    verdict(stamp(13), OutputVerdict::Accepted),
                    verdict(stamp(14), OutputVerdict::Rejected),
                ],
            ),
            decision(
                mounted_design.clone(),
                vec![verdict(stamp(15), OutputVerdict::Rejected)],
            ),
            decision(
                unmounted_design,
                vec![verdict(stamp(17), OutputVerdict::Accepted)],
            ),
        ]);
        let designs = BTreeSet::from([selected_design.clone()]);

        let accepted = accepted_output_tasks(&preparation, &designs)
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(accepted, [carried_stamp.clone(), stamp(13)]);

        assert!(
            detail_demand(&[], &preparation, Some(&view)).is_empty(),
            "Designs no surface shows owe no task detail reads"
        );
        let plain = stamp(20);
        assert_eq!(
            detail_demand(
                &[
                    selected_design.clone(),
                    mounted_design.clone(),
                    plain.clone()
                ],
                &preparation,
                Some(&view),
            ),
            BTreeSet::from([
                selected_design,
                mounted_design,
                plain,
                carried_stamp,
                stamp(13),
                stamp(14),
                stamp(15),
            ])
        );
    }
}
