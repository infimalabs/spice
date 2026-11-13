//! An opened task's body: the links line that chooses its view, then its
//! compact facts as one flow header, then its conversation, or the view the
//! links line expanded with a crumb back to it. The links line leads so no
//! selection resizes anything above it.

use super::*;
use crate::browser::rooms;

use crate::view::TaskView;

/// The view one opened task's links line expanded, if any, and the callback
/// that changes it; `None` returns to the conversation.
pub(in crate::browser) struct TaskViews {
    pub(in crate::browser) shown: Option<TaskView>,
    pub(in crate::browser) show: Callback<Option<TaskView>>,
}

/// The room's cards newest first, or the notice standing in for them.
type Cards = Result<Vec<Html>, Html>;

/// How many prose lines a description carries; blank lines separate, they do
/// not count.
fn description_lines(description: Option<&str>) -> usize {
    description.map_or(0, |text| {
        text.lines().filter(|line| !line.trim().is_empty()).count()
    })
}

pub(super) fn render_available(
    render: &WorkDetailRender<'_, '_>,
    answer: &wire::intent::TaskDetailAnswer,
    views: &TaskViews,
) -> Html {
    let statement = &answer.task.statement.body;
    let cards = render.runtime.map(|runtime| {
        rooms::room_cards(
            runtime,
            &render.open_message_task,
            &statement.born,
            answer.origin.as_ref(),
        )
    });
    let full = facts(render, answer);
    let links = links(render, answer, cards.as_ref(), full.len());
    let main = match views.shown {
        None => default_main(render, answer, cards.as_ref()),
        Some(view) => html! {
            <>
                <nav class="work-view-crumb" aria-label="Task view">
                    <button
                        type="button"
                        class="work-view-back"
                        onclick={views.show.reform(|_: MouseEvent| None)}
                    >{format!("← {}", statement.handle)}</button>
                    <span aria-current="page">{format!("/ {}", view.label())}</span>
                </nav>
                {expanded(render, answer, view, cards.as_ref(), full)}
            </>
        },
    };
    html! {
        <>
            <nav class="work-task-links" aria-label="Task views">
                {for links.into_iter().map(|(view, count)| render_link(view, count, views))}
            </nav>
            // The compact facts flow as one header between the views and
            // the conversation. The full grid replaces it while shown, so
            // each fact's controls appear once.
            if views.shown != Some(TaskView::Facts) {
                <dl class="work-facts" aria-label="Facts">
                    {for compact_facts(render, answer)}
                </dl>
            }
            // An image address resolves only in the detail its task's own
            // address renders, never in a row another selection still shows.
            <div
                class="work-task-main"
                data-task-view={views.shown.map(TaskView::label)}
                data-image-owner-task={statement.born.to_string()}
                data-image-owner-addressed={
                    (render.selection.sole_task() == Some(&statement.born)).then_some("true")
                }
            >
                {main}
            </div>
        </>
    }
}

/// Each view the task has, with what it counts: the room's cards once they
/// are current, a Design's questions or another task's acceptance, the
/// description's lines, every fact, and a Design's decisions.
fn links(
    render: &WorkDetailRender<'_, '_>,
    answer: &wire::intent::TaskDetailAnswer,
    cards: Option<&Cards>,
    facts: usize,
) -> Vec<(TaskView, Option<usize>)> {
    let statement = &answer.task.statement.body;
    let design = answer.task.design_standing().is_some();
    let mut links = Vec::new();
    if let Some(cards) = cards {
        links.push((TaskView::Room, cards.as_ref().ok().map(Vec::len)));
    }
    links.extend([
        (
            if design {
                TaskView::Questions
            } else {
                TaskView::Acceptance
            },
            Some(statement.term.len()),
        ),
        (
            TaskView::Description,
            Some(description_lines(statement.description.as_deref())),
        ),
        (TaskView::Facts, Some(facts)),
    ]);
    if let Some(runtime) = render.runtime.filter(|_| design) {
        links.push((
            TaskView::History,
            Some(rooms::decision_count(runtime, &statement.born)),
        ));
    }
    links
}

/// Select a destination; selecting the current view is a navigation no-op.
fn render_link(view: TaskView, count: Option<usize>, views: &TaskViews) -> Html {
    let pressed = views.shown.unwrap_or(TaskView::Room) == view;
    let select = views.show.reform(move |_: MouseEvent| Some(view));
    html! {
        <button
            type="button"
            class="work-task-link"
            data-task-view-link={view.label()}
            aria-pressed={pressed.to_string()}
            onclick={select}
        >
            {view.label()}
            if let Some(count) = count {
                <span class="work-task-link-count">{count}</span>
            }
        </button>
    }
}

/// The conversation column: a draft's editor, the room beneath its one Observe
/// composer, then the task's attachments.
fn default_main(
    render: &WorkDetailRender<'_, '_>,
    answer: &wire::intent::TaskDetailAnswer,
    cards: Option<&Cards>,
) -> Html {
    let row = &answer.task;
    let room = &row.statement.body.born;
    let board_row = meta::projected(render.work, room).map_or(row, WorkTask::row);
    html! {
        <>
            {super::super::super::drafts::render_editor(
                render.runtime,
                render.draft_controls,
                render.sender,
                board_row,
                &answer.media,
            )}
            <section class="work-room" data-task-room={room.to_string()}>
                <header class="work-room-heading">
                    <h4>{"Room"}</h4>
                    if let Some(Ok(cards)) = cards {
                        <span>{format!("{} · newest first", cards.len())}</span>
                    }
                    if row.design_standing().is_some() {
                        <a class="app-compact-control" href={crate::view::design_href(room)}>
                            {"Permalink"}
                        </a>
                    }
                </header>
                {render_composer(render, answer)}
                {conversation(cards)}
            </section>
            {task_detail::render_media(&answer.media)}
        </>
    }
}

fn conversation(cards: Option<&Cards>) -> Html {
    cards.map_or_else(Html::default, |cards| {
        rooms::render_conversation(cards.as_ref().map(Vec::as_slice))
    })
}

/// The view `view` expands in place of the conversation.
fn expanded(
    render: &WorkDetailRender<'_, '_>,
    answer: &wire::intent::TaskDetailAnswer,
    view: TaskView,
    cards: Option<&Cards>,
    full: Vec<Html>,
) -> Html {
    let row = &answer.task;
    let statement = &row.statement.body;
    let resolves = |task: &TaskStamp| selection_has(render.selection, task);
    let absent = |text: &'static str| html! { <p class="muted">{text}</p> };
    match view {
        TaskView::Room => {
            let design = render.runtime.filter(|_| row.design_standing().is_some());
            html! {
                <>
                    if let Some(runtime) = design {
                        {rooms::render_standing_line(
                            runtime,
                            runtime
                                .board()
                                .and_then(|board| board.row(&statement.born))
                                .unwrap_or(row),
                        )}
                        {rooms::render_lanes(runtime, &statement.born)}
                    }
                    {conversation(cards)}
                </>
            }
        }
        TaskView::Questions => questions(row),
        TaskView::Description if description_lines(statement.description.as_deref()) == 0 => {
            absent("No description was recorded.")
        }
        TaskView::Description => {
            task_detail::render_description(statement, &answer.description_quotes, resolves)
        }
        TaskView::Acceptance if statement.term.is_empty() => {
            absent("No acceptance criteria were recorded.")
        }
        TaskView::Acceptance => task_detail::render_term(statement, resolves),
        TaskView::Facts => html! { <dl class="app-fact-grid">{for full}</dl> },
        TaskView::History => match render.runtime {
            Some(runtime) if rooms::decision_count(runtime, &statement.born) > 0 => {
                rooms::render_decisions(runtime, &render.open_message_task, &statement.born)
            }
            _ => absent("No decisions were recorded."),
        },
    }
}

/// A Design's questions, one line each, answered once any charge weighed it
/// and open until then.
fn questions(row: &wire::intent::BoardTask) -> Html {
    let term = &row.statement.body.term;
    if term.is_empty() {
        return html! { <p class="muted">{"No open questions were recorded."}</p> };
    }
    let covered = row
        .design_coverage
        .as_ref()
        .map_or(&[][..], |coverage| coverage.covered.as_slice());
    let items = wire::rooms::StatementItem::each(term).collect::<Vec<_>>();
    html! {
        // A contribution's question link focuses its question here.
        <ul class="work-questions" data-task-contract={row.statement.body.born.to_string()}>
            {for term.iter().zip(&items).map(|(question, item)| {
                let answered = covered.contains(item);
                html! {
                    <li
                        data-contract-question={item.to_string()}
                        data-answered={answered.to_string()}
                        tabindex="-1"
                    >
                        <span class="work-question-standing">
                            {if answered { "answered" } else { "open" }}
                        </span>
                        <code class="work-question-handle">{item.handle(&items).to_owned()}</code>
                        <span>{question.clone()}</span>
                    </li>
                }
            })}
        </ul>
    }
}
