//! The Work sidebar: every task in the selection once, grouped by who holds
//! it, each row a native disclosure that opens its task in place.

use super::super::clock::{Age, Countdown, local_full, local_full_millis};
use super::work_detail::{TaskViews, WorkDetailRender, base, context_label};
use super::*;
use crate::runtime::{AppRuntime, TaskDetailState};
use crate::view::{AppRoute, TaskView, WorkOptions, WorkRow};
use crate::work::{
    HolderGroup, OperatorWaits, TaskOverviewContext, TaskOverviewSelection, UNTIL_WOKEN,
    WorkAvailability, WorkHolder, WorkList, WorkScope, WorkTask,
};
use wire::TaskStamp;

/// Which row stands open.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OpenRow {
    /// The first task waiting on the operator, else a lone selected task.
    Default,
    Task(TaskStamp),
    Closed,
}

#[derive(Clone)]
pub(in crate::browser) struct WorkListControls {
    open: UseStateHandle<OpenRow>,
    view: Option<(TaskStamp, TaskView)>,
    options: WorkOptions,
    navigate: Callback<navigation::Intent>,
    finished_open: UseStateHandle<bool>,
    focus: UseStateHandle<Option<TaskStamp>>,
}

/// Row disclosure is presentation state; filter and task view come from the URL.
/// `settled` is whether `selection` is already the one `route` asks for: the
/// panel adopts a new route's selection a render after the route arrives.
#[hook]
pub(in crate::browser) fn use_work_list(
    selection: Option<&TaskOverviewSelection>,
    settled: bool,
    route: &AppRoute,
    navigate: &Callback<navigation::Intent>,
) -> WorkListControls {
    let open = use_state_eq(|| OpenRow::Default);
    let finished_open = use_state_eq(|| false);
    let focus = use_state_eq(|| None::<TaskStamp>);
    // A scope keeps its identity while the board moves its tasks; any other
    // selection is its exact task set.
    let identity = selection.map(|selection| match selection.context() {
        TaskOverviewContext::Scope(_) => (selection.context().clone(), Vec::new()),
        TaskOverviewContext::Task | TaskOverviewContext::Relationships(_) => {
            (selection.context().clone(), selection.tasks().to_vec())
        }
    });
    let listed = identity.is_some();
    {
        let open = open.clone();
        let finished_open = finished_open.clone();
        let focus = focus.clone();
        use_effect_with(identity, move |identity| {
            if focus.is_none() {
                choose(&open, OpenRow::Default);
            }
            // Reveal a direct selection once; a relationship activation has
            // already decided whether its exact row is in Done.
            if !matches!(identity, Some((TaskOverviewContext::Relationships(_), _))) {
                finished_open.set(matches!(identity, Some((TaskOverviewContext::Task, _))));
            }
        });
    }
    {
        // A returned-to entry reopens the row it left open, or stays closed,
        // once its own list is shown: the selection settles a render after
        // the arrival, and that settling resets the row to its default first.
        // A row the list no longer shows opens nothing.
        let open = open.clone();
        use_effect_with(
            (navigation::presentation::generation(), listed && settled),
            move |(_, listed)| {
                if !listed {
                    return;
                }
                if let Some(row) = navigation::presentation::take(|pending| pending.row.take()) {
                    choose(
                        &open,
                        match row {
                            WorkRow::Open(task) => OpenRow::Task(task),
                            WorkRow::Closed => OpenRow::Closed,
                        },
                    );
                }
            },
        );
    }
    {
        let open = open.clone();
        let focus = focus.clone();
        use_effect_with((*focus).clone(), move |task| {
            let Some(task) = task else {
                return;
            };
            choose(&open, OpenRow::Task(task.clone()));
            let row = web_sys::window()
                .and_then(|window| window.document())
                .and_then(|document| {
                    document
                        .query_selector(&format!(".work-row-head[data-task-stamp=\"{task}\"]"))
                        .ok()
                        .flatten()
                });
            if let Some(row) = row.and_then(|row| row.dyn_into::<web_sys::HtmlElement>().ok()) {
                row.scroll_into_view();
                let _ = row.focus();
                focus.set(None);
            }
        });
    }
    WorkListControls {
        open,
        view: route
            .task()
            .and_then(|(task, view)| view.map(|view| (task.clone(), view))),
        options: route.options().cloned().unwrap_or_default(),
        navigate: navigate.clone(),
        focus,
        finished_open,
    }
}

/// Open or close a row by the reader's choice and remember it on this entry.
fn choose(open: &UseStateHandle<OpenRow>, row: OpenRow) {
    let remembered = match &row {
        OpenRow::Default => None,
        OpenRow::Task(task) => Some(WorkRow::Open(task.clone())),
        OpenRow::Closed => Some(WorkRow::Closed),
    };
    navigation::presentation::record(|current| current.row = remembered);
    open.set(row);
}

pub(super) struct WorkListRender<'a, 'work> {
    pub detail: WorkDetailRender<'a, 'work>,
    pub controls: &'a WorkListControls,
    pub map: &'a super::work_map::WorkMapState,
    pub retry_relations: Callback<MouseEvent>,
    pub open_overview: Callback<TaskOverviewSelection>,
}

impl WorkListControls {
    pub(in crate::browser) fn filter(&self) -> String {
        self.options.filter.clone()
    }

    fn set_filter(&self, filter: String) {
        let mut options = self.options.clone();
        options.filter = filter;
        self.navigate.emit(navigation::Intent::Options(options));
    }
}

pub(in crate::browser) fn graph_selection(
    controls: &WorkListControls,
    selection: &TaskOverviewSelection,
    work: &crate::work::WorkProjection<'_>,
    runtime: Option<&AppRuntime>,
) -> TaskOverviewSelection {
    let holders = work.holders();
    let query = controls.filter();
    let list = work.list(selection.tasks(), &holders, |task| {
        task.matches_filter(&query, lane_names(runtime, task).as_deref())
    });
    work.graph_selection(selection, &list)
}

/// What every row of one render shares.
struct Rows<'r, 'a, 'work> {
    render: &'r WorkDetailRender<'a, 'work>,
    holders: &'r [WorkHolder<'work>],
    waits: &'r OperatorWaits<'work>,
    open: Option<usize>,
    set_open: &'r UseStateHandle<OpenRow>,
    view: &'r Option<(TaskStamp, TaskView)>,
    focus_task: &'r Callback<TaskStamp>,
}

pub(super) fn render(render: WorkListRender<'_, '_>) -> sidebar::Content {
    let detail = &render.detail;
    let work = detail.work;
    let selection = detail.selection;
    let holders = work.holders();
    let query = render.controls.filter();
    let list = work.list(selection.tasks(), &holders, |task| {
        task.matches_filter(&query, lane_names(detail.runtime, task).as_deref())
    });
    let retained = work.retained_direct_task(selection, &list);
    let graph_selection = work.graph_selection(selection, &list);
    let waits = OperatorWaits::new(work);
    let terminal = work
        .tasks()
        .iter()
        .filter(|task| task.availability() == crate::work::WorkAvailability::Terminal)
        .map(|task| task.row().statement.body.born.clone())
        .collect();
    let focus_task = focus_task_callback(
        &render.controls.open,
        &render.controls.focus,
        &render.controls.finished_open,
        terminal,
    );
    let rows = Rows {
        render: detail,
        holders: &holders,
        waits: &waits,
        open: open_row(&render.controls.open, &list, selection, work),
        set_open: &render.controls.open,
        view: &render.controls.view,
        focus_task: &focus_task,
    };
    let yours = list
        .groups
        .first()
        .is_some_and(|group| group.group == HolderGroup::Yours)
        || !list.outside.is_empty();
    let clear = {
        let controls = render.controls.clone();
        Callback::from(move |_: MouseEvent| controls.set_filter(String::new()))
    };
    let listed = retained.is_some() || !list.groups.is_empty() || !list.outside.is_empty();
    let finished = list
        .groups
        .iter()
        .find(|group| group.group == HolderGroup::Finished);
    let body = html! {
        <div class="work-surface">
            {super::work_map::render(super::work_map::WorkMapRender {
                selection: &graph_selection,
                full_selection: selection,
                state: render.map,
                focus_task: focus_task.clone(),
                open_overview: render.open_overview.clone(),
                work,
                runtime: detail.runtime,
                retry: render.retry_relations,
            })}
            <div class="work-list" data-task-count={graph_selection.tasks().len().to_string()}>
            if selection.is_empty() {
                <div class="work-list-empty">
                    <p class="work-list-empty-title">{"No open work"}</p>
                </div>
            } else {
                if listed {
                    <div class="work-columns" aria-hidden="true">
                        <span></span>
                        <span>{"id"}</span>
                        <span>{"title"}</span>
                        <span>{"phase"}</span>
                        <span>{"holder"}</span>
                        <span>{"age"}</span>
                    </div>
                }
                if !list.outside.is_empty() {
                    {render_group(&rows, HolderGroup::Yours, &list.outside, Some("outside this filter"))}
                }
                if let Some(task) = retained {
                    {render_retained_group(&rows, task)}
                }
                if !yours {
                    <p class="work-group-heading work-nothing-yours">{"✓ nothing waits on you"}</p>
                }
                {for list.groups.iter()
                    .filter(|group| group.group != HolderGroup::Finished)
                    .map(|group| render_group(&rows, group.group, &group.tasks, None))}
                if let Some(group) = finished {
                    {render_finished_group(
                        &rows,
                        &group.tasks,
                        *render.controls.finished_open,
                        &render.controls.finished_open,
                    )}
                }
                if list.groups.is_empty() && list.outside.is_empty() && retained.is_none() {
                    <div class="work-list-empty work-no-match">
                        <p class="work-list-empty-title">{format!("No open work matches “{}”.", query.trim())}</p>
                        <button type="button" class="app-compact-control" onclick={clear}>{"Clear filter"}</button>
                    </div>
                }
            }
            </div>
        </div>
    };
    sidebar::Content {
        label: "Work".into(),
        heading: render_header(detail, render.controls, render.map.show_settled()),
        width: sidebar::Width::Large,
        base: base(detail),
        body,
        anchor: None,
    }
}

/// The scope chips over the whole board, or the one context chip a narrower
/// selection wears in their place, then the filter. A chip counts settled
/// tasks only while the map's Settled toggle draws them.
fn render_header(
    render: &WorkDetailRender<'_, '_>,
    controls: &WorkListControls,
    show_settled: bool,
) -> Html {
    let context = render.selection.context();
    let chip = |label: &'static str, scope: WorkScope| {
        let current = *context == TaskOverviewContext::Scope(scope.clone());
        let count = if show_settled {
            render.work.scope_overview_task_count(&scope)
        } else {
            render
                .work
                .tasks()
                .iter()
                .filter(|task| {
                    task.availability() != WorkAvailability::Terminal
                        && render.work.in_scope(task, &scope)
                })
                .count()
        };
        let navigate = render.navigate.clone();
        let open = Callback::from(move |_: MouseEvent| {
            navigate.emit(navigation::Intent::RefineScope(scope.clone()));
        });
        html! {
            <button
                type="button"
                class="work-scope-chip"
                data-scope={label}
                aria-current={current.then_some("true")}
                onclick={open}
            >
                {label}
                <span class="work-scope-count">{count}</span>
            </button>
        }
    };
    let scopes = match context {
        TaskOverviewContext::Scope(
            WorkScope::All | WorkScope::AllOpen | WorkScope::Planning { topic: None },
        ) => html! {
            <>
                {chip("all", WorkScope::All)}
                {chip("planning", WorkScope::planning())}
                {chip("execution", WorkScope::AllOpen)}
            </>
        },
        _ => {
            let label = context_label(render.selection, render.runtime);
            let navigate = render.navigate.clone();
            let remove = Callback::from(move |_: MouseEvent| {
                navigate.emit(navigation::Intent::RefineScope(WorkScope::All));
            });
            html! {
                <span class="work-context-chip">
                    <span>{label.clone()}</span>
                    <button
                        type="button"
                        aria-label={format!("Remove {label}; show all work")}
                        onclick={remove}
                    >{"×"}</button>
                </span>
            }
        }
    };
    let oninput = {
        let controls = controls.clone();
        Callback::from(move |event: InputEvent| {
            if let Some(input) = event.target_dyn_into::<web_sys::HtmlInputElement>() {
                controls.set_filter(input.value());
            }
        })
    };
    html! {
        <div class="work-list-header">
            <nav class="work-scope" aria-label="Work scope">{scopes}</nav>
            <input
                type="search"
                class="work-filter"
                placeholder="Filter id, title, lane"
                aria-label="Filter work by id, title, or lane"
                value={controls.filter()}
                {oninput}
            />
        </div>
    }
}

/// The open row's projection index: a chosen task while the list shows it,
/// else the default.
fn open_row(
    open: &OpenRow,
    list: &WorkList,
    selection: &TaskOverviewSelection,
    work: &crate::work::WorkProjection<'_>,
) -> Option<usize> {
    let listed = || {
        list.outside
            .iter()
            .chain(list.groups.iter().flat_map(|group| &group.tasks))
            .copied()
    };
    let born = |index: usize| &work.tasks()[index].row().statement.body.born;
    match open {
        OpenRow::Closed => None,
        OpenRow::Task(task) => listed().find(|&index| born(index) == task),
        OpenRow::Default => list
            .outside
            .first()
            .or_else(|| {
                list.groups
                    .first()
                    .filter(|group| group.group == HolderGroup::Yours)
                    .and_then(|group| group.tasks.first())
            })
            .copied()
            .or_else(|| match (selection.context(), selection.tasks()) {
                (TaskOverviewContext::Task, [only]) => listed().find(|&index| born(index) == only),
                _ => None,
            }),
    }
}

/// Opening a related task the list shows opens its row and brings it into
/// view.
fn focus_task_callback(
    open: &UseStateHandle<OpenRow>,
    focus: &UseStateHandle<Option<TaskStamp>>,
    finished_open: &UseStateHandle<bool>,
    terminal: std::collections::BTreeSet<TaskStamp>,
) -> Callback<TaskStamp> {
    let open = open.clone();
    let focus = focus.clone();
    let finished_open = finished_open.clone();
    Callback::from(move |task: TaskStamp| {
        if terminal.contains(&task) {
            finished_open.set(true);
        }
        choose(&open, OpenRow::Task(task.clone()));
        focus.set(Some(task));
    })
}

fn render_group(
    rows: &Rows<'_, '_, '_>,
    group: HolderGroup,
    tasks: &[usize],
    note: Option<&str>,
) -> Html {
    let heading = note.map_or_else(
        || format!("{} {} · {}", group.glyph(), group.word(), tasks.len()),
        |note| format!("{} {note} · {}", group.glyph(), tasks.len()),
    );
    html! {
        <section class="work-group" data-holder={group.word()}>
            <h3 class="work-group-heading">{heading}</h3>
            <ul>
                {for tasks.iter().map(|&index| render_row(rows, index))}
            </ul>
        </section>
    }
}
fn render_finished_group(
    rows: &Rows<'_, '_, '_>,
    tasks: &[usize],
    expanded: bool,
    set_expanded: &UseStateHandle<bool>,
) -> Html {
    let toggle = {
        let set_expanded = set_expanded.clone();
        Callback::from(move |_: MouseEvent| set_expanded.set(!expanded))
    };
    let list_id = "work-finished-rows";
    html! {
        <section class="work-group" data-holder={HolderGroup::Finished.word()}>
            <button
                type="button"
                class="work-group-heading work-finished-toggle"
                aria-expanded={expanded.to_string()}
                aria-controls={list_id}
                onclick={toggle}
            >
                <span aria-hidden="true">{if expanded { "▾" } else { "▸" }}</span>
                {format!("{} {} · {}", HolderGroup::Finished.glyph(), HolderGroup::Finished.word(), tasks.len())}
            </button>
            if expanded {
                <ul id={list_id}>
                    {for tasks.iter().map(|&index| render_row(rows, index))}
                </ul>
            }
        </section>
    }
}

/// One exact task no longer carried by the board's bounded finished rows. Its
/// targeted detail answer remains the row authority.
fn render_retained_group(rows: &Rows<'_, '_, '_>, task: &TaskStamp) -> Html {
    let detail = rows
        .render
        .runtime
        .and_then(|runtime| runtime.task_detail(task));
    let (glyph, label) = retained_standing(detail);
    html! {
        <section class="work-group" data-holder="history">
            <p class="work-group-heading">{format!("{glyph} {label} · 1")}</p>
            <ul>{render_retained_row(rows, task)}</ul>
        </section>
    }
}

fn retained_standing(detail: Option<&TaskDetailState>) -> (&'static str, &'static str) {
    match detail {
        Some(TaskDetailState::Available(answer) | TaskDetailState::Refreshing(answer))
            if answer.task.workflow.body.status.completion().is_some() =>
        {
            (HolderGroup::Finished.glyph(), "history")
        }
        Some(TaskDetailState::Unavailable { reason, .. }) if reason == "task::detail-unknown" => {
            ("◌", "lookup")
        }
        None | Some(TaskDetailState::Loading | TaskDetailState::Unavailable { .. }) => {
            ("◌", "history")
        }
        Some(TaskDetailState::Available(_) | TaskDetailState::Refreshing(_)) => ("◌", "history"),
    }
}

/// Title and holder words for a retained row whose detail has no answer:
/// still loading, or unavailable for `reason`.
fn unanswered_words(unavailable: Option<&str>) -> (&'static str, &'static str) {
    match unavailable {
        Some("task::detail-unknown") => ("Task not found", "unknown"),
        Some(_) => ("Task unavailable", "unavailable"),
        None => ("Loading task", "resolving"),
    }
}

fn render_retained_row(rows: &Rows<'_, '_, '_>, task: &TaskStamp) -> Html {
    let render = rows.render;
    let detail = render.runtime.and_then(|runtime| runtime.task_detail(task));
    let answer = detail.and_then(TaskDetailState::answer);
    let handle = answer.map_or_else(
        || task.to_string(),
        |answer| answer.task.statement.body.handle.to_string(),
    );
    let unanswered = |unavailable| {
        let (title, holder) = unanswered_words(unavailable);
        (title.to_owned(), holder)
    };
    let (title, holder) = match detail {
        Some(TaskDetailState::Available(answer) | TaskDetailState::Refreshing(answer)) => (
            answer.task.presentation_title.clone(),
            answer.task.status_word(),
        ),
        Some(TaskDetailState::Unavailable { reason, .. }) => unanswered(Some(reason.as_str())),
        None | Some(TaskDetailState::Loading) => unanswered(None),
    };
    let phase = answer.map_or("—", |answer| answer.task.workflow.body.phase.as_str());
    let kind = answer.map_or("execution", |answer| {
        if answer.task.design_standing().is_some() {
            "design"
        } else {
            "execution"
        }
    });
    let stamp = task.to_string();
    let born = task.micros() / 1_000;
    let open = match &**rows.set_open {
        OpenRow::Default => true,
        OpenRow::Task(open) => open == task,
        OpenRow::Closed => false,
    };
    let toggle = {
        let set_open = rows.set_open.clone();
        let task = task.clone();
        Callback::from(move |_: MouseEvent| {
            choose(
                &set_open,
                if open {
                    OpenRow::Closed
                } else {
                    OpenRow::Task(task.clone())
                },
            );
        })
    };
    let head_id = format!("work-row-{stamp}");
    let body_id = format!("work-row-body-{stamp}");
    html! {
        <li
            key={stamp.clone()}
            class={classes!("work-row", open.then_some("open"))}
            data-holder="history"
            data-task-source="retained"
        >
            <button
                type="button"
                id={head_id.clone()}
                class="work-row-head"
                data-task-stamp={stamp.clone()}
                data-sidebar-focus-key={format!("work-row:{stamp}")}
                aria-expanded={open.to_string()}
                aria-controls={open.then(|| body_id.clone())}
                onclick={toggle}
            >
                <span class="work-row-glyph" aria-hidden="true">{retained_standing(detail).0}</span>
                <span class="work-row-id" data-kind={kind}>{handle}</span>
                <span class="work-row-title">
                    {task_detail::render_task_title(
                        &title,
                        &crate::composer_markdown::inline::LineControls::Nested,
                    )}
                </span>
                <span class="work-row-phase">{phase}</span>
                <span class="work-row-holder">{holder}</span>
                <Age
                    authored={born}
                    class="work-row-age"
                    title={local_full_millis(born)}
                />
            </button>
            if open {
                <div
                    id={body_id}
                    class="work-row-body"
                    role="region"
                    aria-labelledby={head_id}
                    onclick={task_detail::task_text_chip_click(rows.focus_task)}
                    onkeydown={task_detail::task_text_chip_keydown(rows.focus_task)}
                >
                    {super::work_detail::render_task(
                        render,
                        task,
                        rows.focus_task,
                        &task_views(rows.view, task, render.navigate),
                    )}
                </div>
            }
        </li>
    }
}

fn render_row(rows: &Rows<'_, '_, '_>, index: usize) -> Html {
    let render = rows.render;
    let task = &render.work.tasks()[index];
    let row = task.row();
    let statement = &row.statement.body;
    let stamp = statement.born.to_string();
    let holder = &rows.holders[index];
    let open = rows.open == Some(index);
    let toggle = {
        let set_open = rows.set_open.clone();
        let task = statement.born.clone();
        Callback::from(move |_: MouseEvent| {
            choose(
                &set_open,
                if open {
                    OpenRow::Closed
                } else {
                    OpenRow::Task(task.clone())
                },
            );
        })
    };
    let head_id = format!("work-row-{stamp}");
    let body_id = format!("work-row-body-{stamp}");
    let kind = if row.design_standing().is_some() {
        "design"
    } else {
        "execution"
    };
    let born = statement.born.micros() / 1_000;
    let phase = task
        .phase()
        .map_or_else(|| "—".to_owned(), |phase| phase.to_string());
    let wait = open
        .then(|| rows.waits.waits().iter().find(|wait| wait.task() == index))
        .flatten();
    html! {
        <li
            key={stamp.clone()}
            class={classes!("work-row", open.then_some("open"))}
            data-holder={holder.group().word()}
        >
            <button
                type="button"
                id={head_id.clone()}
                class="work-row-head"
                data-task-stamp={stamp.clone()}
                data-sidebar-focus-key={format!("work-row:{stamp}")}
                aria-expanded={open.to_string()}
                aria-controls={open.then(|| body_id.clone())}
                onclick={toggle}
            >
                <span class="work-row-glyph" aria-hidden="true">{holder.glyph()}</span>
                <span class="work-row-id" data-kind={kind}>{statement.handle.to_string()}</span>
                <span class="work-row-title">
                    {task_detail::render_task_title(
                        &row.presentation_title,
                        &crate::composer_markdown::inline::LineControls::Nested,
                    )}
                </span>
                <span class="work-row-phase">
                    <span>{phase}</span>
                    {render_ownership(row.ownership.as_ref())}
                </span>
                <span class="work-row-holder">{render_holder(render, task, holder)}</span>
                <Age
                    authored={born}
                    class="work-row-age"
                    title={local_full_millis(born)}
                />
            </button>
            if open {
                <div
                    id={body_id}
                    class="work-row-body"
                    role="region"
                    aria-labelledby={head_id}
                    onclick={task_detail::task_text_chip_click(rows.focus_task)}
                    onkeydown={task_detail::task_text_chip_keydown(rows.focus_task)}
                >
                    if let Some(wait) = wait {
                        {super::work_blockers::render_operator_act(render.work, wait, render.runtime, &render.decide_trust)}
                    }
                    {super::work_detail::render_task(
                        render,
                        &statement.born,
                        rows.focus_task,
                        &task_views(rows.view, &statement.born, render.navigate),
                    )}
                </div>
            }
        </li>
    }
}

fn render_ownership(ownership: Option<&wire::intent::TaskOwnership>) -> Html {
    let Some(ownership) = ownership else {
        return Html::default();
    };
    let (label, mut title) = if ownership.reserved {
        ("Reserved".to_owned(), "Reserved for teams: ".to_owned())
    } else if ownership.teams.is_empty() {
        (
            "No owner".to_owned(),
            "No team currently owns this work".to_owned(),
        )
    } else {
        let count = ownership.teams.len();
        (
            format!("{count} {}", if count == 1 { "team" } else { "teams" }),
            "Automatic owners: ".to_owned(),
        )
    };
    for (index, team) in ownership.teams.iter().enumerate() {
        if index != 0 {
            title.push_str(", ");
        }
        std::fmt::write(&mut title, format_args!("{team}")).expect("writing to String");
    }
    html! {
        <span class="work-row-ownership" {title} data-reserved={ownership.reserved.to_string()}>
            {label}
        </span>
    }
}

/// The view `task`'s links line expanded, and the callback that changes it.
fn task_views(
    view: &Option<(TaskStamp, TaskView)>,
    task: &TaskStamp,
    navigate: &Callback<navigation::Intent>,
) -> TaskViews {
    let shown = view
        .as_ref()
        .filter(|(shown, _)| shown == task)
        .map(|(_, shown)| *shown);
    let show = {
        let navigate = navigate.clone();
        let task = task.clone();
        Callback::from(move |shown: Option<TaskView>| {
            navigate.emit(navigation::Intent::TaskView(task.clone(), shown));
        })
    };
    TaskViews { shown, show }
}

/// Who has the task, in the list's own words.
fn render_holder(
    render: &WorkDetailRender<'_, '_>,
    task: &WorkTask<'_>,
    holder: &WorkHolder<'_>,
) -> Html {
    match holder {
        WorkHolder::Yours => html! { "you" },
        WorkHolder::Working(_) => {
            let names = lane_names(render.runtime, task).unwrap_or_default();
            match task.round() {
                Some((arrived, target)) => html! {
                    <span title={format!("{arrived} of {target} participants arrived")}>
                        {format!("{names} {arrived}/{target}")}
                    </span>
                },
                None => html! { {names} },
            }
        }
        WorkHolder::Ready => html! { "—" },
        WorkHolder::WaitsOn(prerequisites) => {
            let handles = prerequisites
                .iter()
                .map(|prerequisite| {
                    render
                        .work
                        .tasks()
                        .iter()
                        .find(|candidate| candidate.row().statement.body.born == **prerequisite)
                        .map_or_else(
                            || prerequisite.to_string(),
                            |candidate| candidate.row().statement.body.handle.to_string(),
                        )
                })
                .collect::<Vec<_>>()
                .join(", ");
            html! { <span title={format!("waits on {handles}")}>{handles.clone()}</span> }
        }
        WorkHolder::Deferred(until) if until.is_indefinite() => html! { {UNTIL_WOKEN} },
        WorkHolder::Deferred(until) => html! {
            <Countdown
                expiry={until.get().as_millisecond()}
                class="work-row-countdown"
                title={local_full(until)}
            />
        },
        WorkHolder::Draft | WorkHolder::Settled(_) => {
            html! { {holder.word()} }
        }
    }
}

/// The lanes working the task, by the names the reader knows them.
fn lane_names(runtime: Option<&AppRuntime>, task: &WorkTask<'_>) -> Option<String> {
    let names = task
        .participants()
        .map(|actor| {
            let member = match actor {
                wire::Actor::Lane(lane) => runtime
                    .and_then(AppRuntime::lanes)
                    .and_then(|lanes| lanes.member(*lane)),
                wire::Actor::Spice | wire::Actor::Principal(_) => None,
            };
            crate::work::owner_label(actor, member, runtime.and_then(AppRuntime::principals))
        })
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| names.join(", "))
}
