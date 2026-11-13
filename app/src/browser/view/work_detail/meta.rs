//! An opened task's header: its full title, then one meta line holding its
//! track with the current step bold, who holds and reviews it, and how it
//! relates to other tasks, each related id opening that task.

use super::*;
use crate::work::WorkDependency;
use wire::{WorkflowCompletionReview, WorkflowConclusion, WorkflowReview, WorkflowStatus};

/// What activating a related id does: open a task the list shows in place,
/// or replace the selection with one outside it.
struct RelationTargets<'a> {
    selection: &'a TaskOverviewSelection,
    focus: &'a Callback<TaskStamp>,
    navigate: &'a Callback<navigation::Intent>,
}

/// The task's header. Before detail arrives it reads the board row the list
/// already projects.
pub(super) fn render_header(
    render: &WorkDetailRender<'_, '_>,
    stamp: &TaskStamp,
    detail: Option<&TaskDetailState>,
    focus_task: &Callback<TaskStamp>,
) -> Html {
    let projected = projected(render.work, stamp);
    let row = detail
        .and_then(TaskDetailState::answer)
        .map(|answer| &answer.task)
        .or_else(|| projected.map(WorkTask::row));
    let Some(row) = row else {
        return Html::default();
    };
    let statement = &row.statement.body;
    let resolves = |task: &wire::TaskHandle| {
        task.born()
            .is_ok_and(|stamp| selection_has(render.selection, &stamp))
    };
    let targets = RelationTargets {
        selection: render.selection,
        focus: focus_task,
        navigate: render.navigate,
    };
    let track = projected
        .map(WorkTask::track)
        .unwrap_or_else(|| crate::work::retained_track(row));
    let steps = track.into_iter().map(|step| {
        if step.current {
            html! { <strong aria-current="step">{step.label}</strong> }
        } else {
            html! { <span>{step.label}</span> }
        }
    });
    let mut clauses = vec![
        html! { <span data-task-header="claim">{claimant(row, render)}</span> },
        html! { <span data-task-header="review">{review(&row.workflow.body.status)}</span> },
    ];
    clauses.extend(relationships(render.work, projected, statement, &targets));
    html! {
        <header class="work-task-header">
            <h3 class="work-task-title">
                {task_detail::render_task_title(
                    &row.presentation_title,
                    &LineControls::Live { resolves: &resolves },
                )}
            </h3>
            <p class="work-task-meta">
                <span class="work-track" aria-label="Track">{joined(steps, " · ")}</span>
                {" — "}
                {joined(clauses, " · ")}
            </p>
            <p class="work-task-pitch" data-task-header="pitch">{statement.pitch.clone()}</p>
        </header>
    }
}

/// The projected task born at `stamp`, when the board carries it.
pub(super) fn projected<'p, 'work>(
    work: &'p WorkProjection<'work>,
    stamp: &TaskStamp,
) -> Option<&'p WorkTask<'work>> {
    work.tasks()
        .iter()
        .find(|task| task.row().statement.body.born == *stamp)
}

/// `items` in order with `separator` between each pair.
fn joined(items: impl IntoIterator<Item = Html>, separator: &'static str) -> Html {
    html! {
        {for items.into_iter().enumerate().map(|(at, item)| html! {
            <>
                if at > 0 { {separator} }
                {item}
            </>
        })}
    }
}

/// Who holds the task, linked to each participant's lane, or how the last
/// holder let it go.
fn claimant(row: &wire::intent::BoardTask, render: &WorkDetailRender<'_, '_>) -> Html {
    let holders = row
        .active_claims()
        .map(|held| actor_link(&held.claim.body.actor, render))
        .collect::<Vec<_>>();
    if !holders.is_empty() {
        return html! { <>{"claimed by "}{joined(holders, ", ")}</> };
    }
    let Some(held) = row.latest_claim() else {
        return html! { "unclaimed" };
    };
    let verb = match held.claim.body.disposition {
        wire::ClaimDisposition::Active => "claimed by ",
        wire::ClaimDisposition::Freed => "freed by ",
        wire::ClaimDisposition::Lapsed => "lapsed from ",
    };
    html! { <>{verb}{actor_link(&held.claim.body.actor, render)}</> }
}

/// Where review stands: a clean verdict, or the workflow's review spelling.
/// Seated reviewers show as the task's claimants.
fn review(status: &WorkflowStatus) -> Html {
    match status {
        WorkflowStatus::Pending(WorkflowReview::Clean)
        | WorkflowStatus::Settled {
            conclusion:
                WorkflowConclusion::Positive {
                    review: WorkflowCompletionReview::Clean,
                },
            ..
        } => html! { "review ✓ clean" },
        WorkflowStatus::Pending(WorkflowReview::Unassigned)
        | WorkflowStatus::Settled {
            conclusion:
                WorkflowConclusion::Positive {
                    review: WorkflowCompletionReview::Unassigned,
                },
            ..
        } => html! { "review unassigned" },
        WorkflowStatus::Settled { .. } => html! { "closed" },
    }
}

/// One clause per relationship kind the task has: unsettled prerequisites it
/// waits on, settled ones it came after, the Design that released it, the
/// outputs it released, and the tasks it unblocks. A task the projection does
/// not carry reads its statement's prerequisites.
fn relationships(
    work: &WorkProjection<'_>,
    task: Option<&WorkTask<'_>>,
    statement: &wire::intent::BoardStatement,
    targets: &RelationTargets<'_>,
) -> Vec<Html> {
    let clause = |word: &'static str, tasks: Vec<&TaskStamp>| {
        (!tasks.is_empty()).then(|| {
            let links = tasks
                .into_iter()
                .map(|born| render_task_link(work, born, targets));
            html! {
                <span class="work-relation" data-relation={word}>
                    {word}{" "}{joined(links, ", ")}
                </span>
            }
        })
    };
    let clauses = match task {
        None => {
            let mut waiting = Vec::new();
            let mut satisfied = Vec::new();
            let mut unresolved = Vec::new();
            for prerequisite in &statement.dependencies {
                match work.prerequisite_satisfied(prerequisite) {
                    Some(false) => waiting.push(prerequisite),
                    Some(true) => satisfied.push(prerequisite),
                    None => unresolved.push(prerequisite),
                }
            }
            vec![
                clause("waits on", waiting),
                clause("after", satisfied),
                clause("prerequisites", unresolved),
            ]
        }
        Some(task) => {
            let born = |index: usize| &work.tasks()[index].row().statement.body.born;
            let prerequisites = |released: bool, satisfied: Option<bool>| {
                task.dependencies()
                    .iter()
                    .filter(|edge| edge.released() == released)
                    .filter(|edge| satisfied.is_none_or(|satisfied| edge.satisfied() == satisfied))
                    .map(WorkDependency::born)
                    .collect()
            };
            let dependents = |released: bool| {
                task.dependents()
                    .iter()
                    .filter(|edge| edge.released() == released)
                    .map(|edge| born(edge.task()))
                    .collect()
            };
            vec![
                clause("waits on", prerequisites(false, Some(false))),
                clause("after", prerequisites(false, Some(true))),
                clause("released by", dependents(true)),
                clause("released", prerequisites(true, None)),
                clause("unblocks", dependents(false)),
            ]
        }
    };
    let clauses = clauses.into_iter().flatten().collect::<Vec<_>>();
    if clauses.is_empty() {
        vec![html! { <span class="work-relation">{"no relationships"}</span> }]
    } else {
        clauses
    }
}

/// One related task's id: a task the list shows opens in place, and one
/// outside the selection replaces the selection with it.
fn render_task_link(
    work: &WorkProjection<'_>,
    born: &TaskStamp,
    targets: &RelationTargets<'_>,
) -> Html {
    let row = projected(work, born).map(WorkTask::row);
    let selected = selection_has(targets.selection, born);
    let target = born.clone();
    let activate = if selected {
        let focus = targets.focus.clone();
        Callback::from(move |_| focus.emit(target.clone()))
    } else {
        let navigate = targets.navigate.clone();
        Callback::from(move |_| navigate.emit(navigation::Intent::Task(target.clone())))
    };
    let handle = row.map_or_else(
        || born.to_string(),
        |row| row.statement.body.handle.to_string(),
    );
    let kind = row.map(|row| {
        if row.design_standing().is_some() {
            "design"
        } else {
            "execution"
        }
    });
    let title = row.map(|row| row.presentation_title.clone());
    html! {
        <button
            type="button"
            class="app-fact-link work-relation-link"
            data-kind={kind}
            data-task-stamp={born.to_string()}
            {title}
            aria-label={(!selected).then(|| format!("Open {handle}, outside this list"))}
            onclick={activate}
        >{handle}</button>
    }
}
