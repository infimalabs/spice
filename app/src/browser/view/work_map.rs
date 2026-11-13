//! Immediate whole-set graph with optional typed relationship enrichment.

use std::rc::Rc;

use wire::TaskStamp;
use wire::intent::{
    TASK_RELATION_DEPTH_LIMIT, TASK_RELATION_NODE_LIMIT, TASK_RELATION_ROOT_LIMIT,
    TaskRelationTruncation,
};
use yew::prelude::*;

use crate::browser::clock::{TaskTimePresentation, local_compact, task_time};
use crate::browser::navigation;
use crate::view::{AppRoute, WorkLens, WorkOptions};
use crate::work::{
    TaskOverviewSelection, WorkAvailability, WorkMark, WorkProjection, WorkRelationshipDirection,
    lens,
};
use crate::{AppRuntime, TaskRelationsKey, TaskRelationsState};

mod overlay;
mod viewport;

const DEFAULT_NODES: u16 = 48;
const NODE_STEP: u16 = 32;

impl WorkLens {
    const fn context(self) -> &'static str {
        match self {
            Self::Dependency => {
                "Edges point from each prerequisite to the dependent task it blocks."
            }
            Self::Origin => {
                "Task ancestry includes shared parents; non-task ACK and origin identities remain exact."
            }
            Self::Review => "Completed handoffs preserve phase author → reviewer → verdict order.",
            Self::Lifecycle => {
                "Historical task transitions remain ordered by durable fact position."
            }
        }
    }
}

/// URL-owned graph semantics, with a local enrichment budget.
#[derive(Clone)]
pub(in crate::browser) struct WorkMapState {
    options: Rc<WorkOptions>,
    max_nodes: UseStateHandle<u16>,
    navigate: Callback<navigation::Intent>,
}

impl std::ops::Deref for WorkMapState {
    type Target = WorkOptions;

    fn deref(&self) -> &Self::Target {
        &self.options
    }
}

#[hook]
pub(in crate::browser) fn use_work_map(
    route: &AppRoute,
    navigate: &Callback<navigation::Intent>,
) -> WorkMapState {
    let options = use_memo(route.options().cloned().unwrap_or_default(), Clone::clone);
    let max_nodes = use_state(|| DEFAULT_NODES);
    {
        // A returned-to entry keeps the budget the reader raised there; any
        // other arrival starts from the default read.
        let max_nodes = max_nodes.clone();
        use_effect_with(navigation::presentation::generation(), move |_| {
            let nodes = navigation::presentation::take(|pending| pending.nodes.take());
            max_nodes.set(
                nodes
                    .unwrap_or(DEFAULT_NODES)
                    .clamp(DEFAULT_NODES, TASK_RELATION_NODE_LIMIT),
            );
        });
    }
    WorkMapState {
        options,
        max_nodes,
        navigate: navigate.clone(),
    }
}

impl WorkMapState {
    /// Whether done and deleted tasks are drawn and counted.
    pub(in crate::browser) fn show_settled(&self) -> bool {
        self.settled
    }

    pub(in crate::browser) fn request_key(
        &self,
        selection: &TaskOverviewSelection,
    ) -> Option<TaskRelationsKey> {
        let roots = selection
            .tasks()
            .iter()
            .take(TASK_RELATION_ROOT_LIMIT)
            .cloned()
            .collect::<Vec<_>>();
        TaskRelationsKey::new(
            roots,
            self.lens.relation(),
            self.controllers,
            self.depth,
            *self.max_nodes,
        )
    }
}

pub(super) struct WorkMapRender<'a, 'work> {
    pub selection: &'a TaskOverviewSelection,
    pub full_selection: &'a TaskOverviewSelection,
    pub state: &'a WorkMapState,
    pub focus_task: Callback<TaskStamp>,
    pub open_overview: Callback<TaskOverviewSelection>,
    pub work: &'a WorkProjection<'work>,
    pub runtime: Option<&'a AppRuntime>,
    pub retry: Callback<MouseEvent>,
}

/// Activate an existing row directly; a relationship lead first joins the
/// shared list selection, then focuses the row created by that same update.
fn activate_task(
    selection: &TaskOverviewSelection,
    focus: &Callback<TaskStamp>,
    open: &Callback<TaskOverviewSelection>,
) -> Callback<TaskStamp> {
    let selection = selection.clone();
    let focus = focus.clone();
    let open = open.clone();
    Callback::from(move |task: TaskStamp| {
        if !selection.tasks().contains(&task) {
            open.emit(selection.with_related_task(task.clone()));
        }
        focus.emit(task);
    })
}

pub(super) fn render(render: WorkMapRender<'_, '_>) -> Html {
    let local = render.work.overview_graph(render.selection);
    let key = render.state.request_key(render.selection);
    let relations = key.as_ref().and_then(|key| {
        render
            .runtime
            .and_then(|runtime| runtime.task_relations(key))
    });
    let answer = relations.and_then(TaskRelationsState::answer);
    let presentation = lens::project(
        &local,
        render.work,
        answer,
        lens::LensView {
            relation: render.state.lens.relation(),
            direction: render.state.direction,
            show_settled: render.state.settled,
            show_controllers: render.state.controllers,
            show_drafts: render.state.drafts,
        },
        render.selection,
        &local_compact,
    );
    let controls = render_controls(
        render.state,
        answer.and_then(|answer| answer.truncation.as_ref()),
    );
    let can_read = render
        .runtime
        .is_some_and(|runtime| runtime.offers(wire::CommandKind::TaskRelations));
    let enrichment_roots = key.as_ref().map_or(0, |key| key.roots().len());
    let activate_task = activate_task(
        render.full_selection,
        &render.focus_task,
        &render.open_overview,
    );

    html! {
        <section
            class="work-map-content work-overview-map"
            aria-labelledby="work-map-diagram-title"
            data-work-map-roots={render.selection.tasks().len().to_string()}
            data-work-map-context={super::work_detail::context_label(render.selection, render.runtime)}
            data-work-map-lens={render.state.lens.key()}
            data-work-map-controllers={render.state.controllers.to_string()}
        >
            <div class="work-map-heading">
                <div>
                    <h3 id="work-map-diagram-title">{format!("{} graph", render.state.lens.label())}</h3>
                </div>
                <span>{presentation.as_ref().map_or_else(
                    || "Graph projection unavailable".to_owned(),
                    |presentation| map_summary(presentation, render.selection.tasks().len()),
                )}</span>
            </div>
            {controls}
            <p class="work-map-context">{render.state.lens.context()}</p>
            // The coverage slot stands whether or not the bound applies, so a
            // board change crossing it never inserts a line above the graph.
            <p class="work-map-bounds" role="status">
                {if render.selection.tasks().len() > enrichment_roots && enrichment_roots > 0 {
                    format!(
                        "Relationship enrichment covers the first {enrichment_roots} of {} roots; settled visibility applies to all roots.",
                        render.selection.tasks().len(),
                    )
                } else {
                    "\u{a0}".to_owned()
                }}
            </p>
            <crate::browser::toast::Notice text={enrichment_problem(can_read, relations)} />
            if render.selection.is_empty() {
                <div class="work-map-empty" role="status">
                    <strong>{"No tasks selected"}</strong>
                </div>
            } else if let Some(presentation) = presentation {
                {render_graph(
                    &presentation,
                    render.work,
                    &activate_task,
                    can_read && matches!(relations, None | Some(TaskRelationsState::Loading)),
                    matches!(relations, Some(TaskRelationsState::Unavailable { .. }))
                        .then(|| render.retry.clone()),
                )}
                {render_relationship_list(&presentation, render.state.lens)}
            } else {
                <div class="work-map-problem" role="alert">
                    <h4>{"Relationship graph unavailable"}</h4>
                    <p>{"The admitted enrichment could not be merged without crossing typed identities."}</p>
                </div>
            }
        </section>
    }
}
fn map_summary(presentation: &lens::Presentation, root_count: usize) -> String {
    let shown_roots = presentation
        .nodes
        .iter()
        .filter(|node| node.selected)
        .count();
    let related = presentation.nodes.len() - shown_roots;
    format!(
        "{shown_roots} of {root_count} {} shown · {related} related {} · {} {}",
        if root_count == 1 { "root" } else { "roots" },
        if related == 1 { "node" } else { "nodes" },
        presentation.edges.len(),
        if presentation.edges.len() == 1 {
            "relationship"
        } else {
            "relationships"
        },
    )
}

/// The real problem behind absent enrichment, if any. Loading and refreshing
/// over retained evidence are routine and say nothing.
fn enrichment_problem(can_read: bool, relations: Option<&TaskRelationsState>) -> Option<AttrValue> {
    if !can_read {
        return Some(AttrValue::from(
            "Typed enrichment is unavailable to this capability; the admitted-board graph remains current.",
        ));
    }
    let Some(TaskRelationsState::Unavailable { reason, detail }) = relations else {
        return None;
    };
    Some(
        detail
            .as_ref()
            .map_or_else(
                || format!("Typed enrichment unavailable: {reason}"),
                |detail| format!("Typed enrichment unavailable: {reason}. {detail}"),
            )
            .into(),
    )
}

/// Draw the graph inside its fixed frame, busy until the first enrichment read
/// answers. Unavailable enrichment is absent content, so its Retry rests inside
/// the graph's own reserved viewport rather than in flow.
fn render_graph(
    presentation: &lens::Presentation,
    work: &WorkProjection<'_>,
    activate: &Callback<TaskStamp>,
    pending: bool,
    retry: Option<Callback<MouseEvent>>,
) -> Html {
    html! {
        <viewport::WorkMapViewport
            source={AttrValue::from(presentation.source.clone())}
            targets={map_targets(presentation, work)}
            select={activate.clone()}
            {pending}
            {retry}
        />
    }
}

fn render_controls(state: &WorkMapState, truncation: Option<&TaskRelationTruncation>) -> Html {
    html! {
        <>
            <div class="work-map-primary-controls">
                <section class="work-map-lenses app-control-rail" aria-label="Relationship lens">
                    {lens_button(state, WorkLens::Dependency)}
                    {lens_button(state, WorkLens::Origin)}
                    {lens_button(state, WorkLens::Review)}
                    {lens_button(state, WorkLens::Lifecycle)}
                </section>
                if state.lens == WorkLens::Dependency {
                    <div class="work-map-control-group work-map-direction app-control-rail" aria-label="Dependency direction">
                        {direction_button(state, WorkRelationshipDirection::Prerequisites, "Prerequisites")}
                        {direction_button(state, WorkRelationshipDirection::Dependents, "Dependents")}
                    </div>
                }
            </div>
            <section class="work-map-controls" aria-label="Map controls">
                <div class="work-map-control-group app-control-rail work-map-depth" aria-label="Relationship depth">
                    <button type="button" aria-label="Decrease map depth" disabled={state.depth == 0} onclick={depth_callback(state.clone(), false)}>{"−"}</button>
                    <span>{format!("Depth {}", state.depth)}</span>
                    <button type="button" aria-label="Increase map depth" disabled={state.depth >= TASK_RELATION_DEPTH_LIMIT} onclick={depth_callback(state.clone(), true)}>{"+"}</button>
                </div>
                {controllers_button(state)}
                {drafts_button(state)}
                {settled_button(state)}
                {render_budget(truncation, state)}
            </section>
        </>
    }
}

fn lens_button(state: &WorkMapState, lens: WorkLens) -> Html {
    let active = state.lens == lens;
    let change = {
        let state = state.clone();
        Callback::from(move |_| update(&state, |next| next.lens = lens))
    };
    html! {
        <button type="button" aria-pressed={active.to_string()} class={active.then_some("active")} onclick={change}>
            {lens.label()}
        </button>
    }
}
/// The default-on overlay for preparation controller edges.
fn controllers_button(state: &WorkMapState) -> Html {
    let active = state.controllers;
    let change = {
        let state = state.clone();
        Callback::from(move |_| update(&state, |next| next.controllers = !next.controllers))
    };
    html! {
        <div class="work-map-control-group app-control-rail work-map-toggle" aria-label="Controller relationships">
            <button type="button" aria-pressed={active.to_string()} class={active.then_some("active")} onclick={change}>
                {"Controllers"}
            </button>
        </div>
    }
}

/// Draft visibility filters the graph without changing its read or selection.
fn drafts_button(state: &WorkMapState) -> Html {
    let active = state.drafts;
    let change = {
        let state = state.clone();
        Callback::from(move |_| update(&state, |next| next.drafts = !next.drafts))
    };
    html! {
        <div class="work-map-control-group app-control-rail work-map-toggle" aria-label="Draft tasks">
            <button type="button" aria-pressed={active.to_string()} class={active.then_some("active")} onclick={change}>
                {"Drafts"}
            </button>
        </div>
    }
}

/// The view-only control that shows or hides settled tasks, selected roots
/// included but never the one task its selection addresses; it changes no
/// request or list selection.
fn settled_button(state: &WorkMapState) -> Html {
    let active = state.settled;
    let change = {
        let state = state.clone();
        Callback::from(move |_| update(&state, |next| next.settled = !next.settled))
    };
    html! {
        <div class="work-map-control-group app-control-rail work-map-toggle" aria-label="Settled tasks">
            <button type="button" aria-pressed={active.to_string()} class={active.then_some("active")} onclick={change}>
                {"Settled"}
            </button>
        </div>
    }
}

fn direction_button(
    state: &WorkMapState,
    direction: WorkRelationshipDirection,
    label: &'static str,
) -> Html {
    let active = if direction == WorkRelationshipDirection::Prerequisites {
        state.direction.includes_prerequisites()
    } else {
        state.direction.includes_dependents()
    };
    let change = {
        let state = state.clone();
        Callback::from(move |_| {
            update(&state, |next| {
                next.direction = next.direction.toggled(direction)
            })
        })
    };
    html! {
        <button type="button" aria-pressed={active.to_string()} class={active.then_some("active")} onclick={change}>{label}</button>
    }
}

fn depth_callback(state: WorkMapState, increase: bool) -> Callback<MouseEvent> {
    Callback::from(move |_| {
        update(&state, |next| {
            next.depth = if increase {
                next.depth.saturating_add(1).min(TASK_RELATION_DEPTH_LIMIT)
            } else {
                next.depth.saturating_sub(1)
            };
        });
    })
}

fn map_targets(
    presentation: &lens::Presentation,
    work: &WorkProjection<'_>,
) -> Rc<Vec<overlay::MapNodeTarget>> {
    Rc::new(
        presentation
            .nodes
            .iter()
            .filter_map(|node| {
                let task = node.task.as_ref()?;
                let tone = task_tone(work, task, node.status.as_ref());
                Some(overlay::MapNodeTarget {
                    id: node.id.clone(),
                    task: task.clone(),
                    label: node.label.clone(),
                    selected: node.selected,
                    availability: tone.map(|tone| tone.availability),
                    disposition: tone.and_then(|tone| tone.disposition),
                    mark: tone.and_then(|tone| tone.mark),
                    state: tone.map(|tone| tone.label),
                })
            })
            .collect(),
    )
}

/// The tone one task wears wherever a surface colours it by lifecycle: the
/// work map's cards and a Design's pips.
#[derive(Clone, Copy)]
pub(in crate::browser) struct TaskTone {
    pub(in crate::browser) availability: &'static str,
    pub(in crate::browser) disposition: Option<&'static str>,
    /// The tab mark this task carries, so the graph names what the tab marks.
    pub(in crate::browser) mark: Option<&'static str>,
    pub(in crate::browser) label: &'static str,
}

/// A board row is authoritative; a task the board lacks takes its settled
/// status from the typed enrichment.
pub(in crate::browser) fn task_tone(
    work: &WorkProjection<'_>,
    task: &TaskStamp,
    status: Option<&wire::WorkflowStatusKind>,
) -> Option<TaskTone> {
    let Some(task) = work
        .tasks()
        .iter()
        .find(|candidate| candidate.row().statement.body.born == *task)
    else {
        return match status? {
            wire::WorkflowStatusKind::Completed => Some(settled_tone(false)),
            wire::WorkflowStatusKind::Deleted => Some(settled_tone(true)),
            wire::WorkflowStatusKind::Pending => None,
        };
    };
    let availability = task.availability();
    if task.pass_ready() {
        return Some(TaskTone {
            availability: availability.key(),
            disposition: None,
            mark: Some(WorkMark::Pass.key()),
            label: "Ready for a research pass",
        });
    }
    let label = match availability {
        WorkAvailability::InFlight => "In flight",
        WorkAvailability::Ready => "Ready",
        WorkAvailability::Review => "Review",
        WorkAvailability::Stale => "Stale",
        WorkAvailability::Blocked => "Blocked",
        WorkAvailability::Waiting => "Waiting",
        WorkAvailability::Passive => "Passive",
        WorkAvailability::Selection => {
            return Some(TaskTone {
                availability: availability.key(),
                disposition: None,
                mark: Some(WorkMark::Decision.key()),
                label: "Awaiting decision",
            });
        }
        WorkAvailability::Held => "Held",
        WorkAvailability::Deferred => "Deferred",
        WorkAvailability::Draft => "Draft",
        WorkAvailability::Terminal => {
            return Some(settled_tone(task.row().workflow.body.status.is_deleted()));
        }
    };
    Some(TaskTone {
        availability: availability.key(),
        disposition: None,
        mark: None,
        label,
    })
}

/// Settled tasks read as quiet provenance: done is neutral, deleted keeps its
/// own tone.
fn settled_tone(deleted: bool) -> TaskTone {
    let (label, disposition) = if deleted {
        ("Deleted", Some("deleted"))
    } else {
        ("Done", None)
    };
    TaskTone {
        availability: WorkAvailability::Terminal.key(),
        disposition,
        mark: None,
        label,
    }
}

/// The lens's evidence below the graph. Task nodes are already the graph's own
/// interactive nodes, so only evidence the graph draws as non-task facts is
/// listed; task-to-task relationship lenses have none and list nothing.
fn render_relationship_list(presentation: &lens::Presentation, active_lens: WorkLens) -> Html {
    let (title, note) = match active_lens {
        WorkLens::Dependency => return Html::default(),
        WorkLens::Origin => (
            "Lineage evidence",
            "ACK and origin identities stay explicit",
        ),
        WorkLens::Review => (
            "Review chain evidence",
            "Reviews and verdicts remain in graph order",
        ),
        WorkLens::Lifecycle => (
            "Ordered lifecycle evidence",
            "Durable lifecycle facts remain in source order",
        ),
    };
    let facts = presentation
        .nodes
        .iter()
        .filter(|node| node.task.is_none())
        .map(|node| {
            html! {
                <article key={node.id.clone()} class="work-map-fact">
                    <span>{node.kind}</span>
                    <strong>{node.label.clone()}</strong>
                    <small>
                        if let Some(at) = &node.at {
                            {task_time(at, TaskTimePresentation::Elapsed)}
                            {" · "}
                        }
                        {node.detail.clone()}
                    </small>
                </article>
            }
        })
        .collect::<Vec<_>>();
    html! {
        <section class="work-map-list" aria-labelledby="work-map-list-title">
            <div class="work-map-heading">
                <h3 id="work-map-list-title">{title}</h3>
                <span>{note}</span>
            </div>
            <div class="work-map-list-scroll" tabindex="0" role="region" aria-labelledby="work-map-list-title">
                if facts.is_empty() {
                    <p class="work-map-context">{"No evidence beyond the tasks the graph shows."}</p>
                } else {
                    <div class="work-map-fact-list">{facts}</div>
                }
            </div>
        </section>
    }
}

/// The enrichment node budget and, for a truncated read, how many
/// relationships lie outside it, with the one control that raises the budget.
/// Both hold their slot in every state, so a read truncating or expanding
/// never moves the graph.
fn render_budget(truncation: Option<&TaskRelationTruncation>, state: &WorkMapState) -> Html {
    let omitted = truncation.map(|truncation| {
        u32::try_from(truncation.frontier.len())
            .unwrap_or(u32::MAX)
            .saturating_add(truncation.omitted)
    });
    let at_bound = *state.max_nodes >= TASK_RELATION_NODE_LIMIT;
    let next = state
        .max_nodes
        .saturating_add(NODE_STEP)
        .min(TASK_RELATION_NODE_LIMIT);
    let status = match omitted {
        Some(omitted) if at_bound => format!(
            "{omitted} relationships outside the {}-node bound",
            *state.max_nodes
        ),
        Some(omitted) => format!(
            "{omitted} relationships outside this {}-node read",
            *state.max_nodes
        ),
        None => format!("{}-node limit", *state.max_nodes),
    };
    let expand = {
        let budget = state.max_nodes.clone();
        Callback::from(move |_| {
            navigation::presentation::record(|current| current.nodes = Some(next));
            budget.set(next);
        })
    };
    html! {
        <div class="work-map-budget">
            <span id="work-map-budget-status" class="work-map-bounds" role="status">{status}</span>
            <button
                type="button"
                class="app-compact-control"
                aria-label={format!("Raise the node budget to {next}")}
                aria-describedby="work-map-budget-status"
                disabled={omitted.is_none() || at_bound}
                onclick={expand}
            >{"More nodes"}</button>
        </div>
    }
}

fn update(state: &WorkMapState, change: impl FnOnce(&mut WorkOptions)) {
    let mut next = (*state.options).clone();
    change(&mut next);
    state.navigate.emit(navigation::Intent::Options(next));
}
