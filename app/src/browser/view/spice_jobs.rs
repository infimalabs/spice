//! One bounded job list and inspector over every lane's jobs.

use super::super::clock::Age;
use super::*;
use crate::watchband::Standing;
use wire::intent::{LaneJobKind, LaneJobPhase, LaneJobRef, LaneJobsAnswer};

mod inspector;

const HEADING: &str = "spice-jobs-heading";
const REFRESH_MILLIS: u32 = 3_000;
const KINDS: [(LaneJobKind, &str, &str, &str); 4] = [
    (LaneJobKind::Command, "▶", "▷", "commands"),
    (LaneJobKind::Service, "◆", "◇", "services"),
    (LaneJobKind::Watch, "●", "○", "watches"),
    (LaneJobKind::WatchedService, "◈", "◇", "watched services"),
];

#[derive(Clone, PartialEq)]
struct Selection {
    target: LaneJobRef,
    label: AttrValue,
    accent: AttrValue,
    admitted: Option<crate::socket::SessionScope>,
}

struct Row {
    selection: Selection,
    command: AttrValue,
    kind: LaneJobKind,
    phase: LaneJobPhase,
    started: Option<i64>,
}

struct Catalog {
    scope: Option<crate::socket::SessionScope>,
    result: Result<Rc<Vec<Row>>, String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Kind(LaneJobKind),
    Unheard,
}

impl Filter {
    fn key(self) -> String {
        match self {
            Self::All => String::new(),
            Self::Kind(kind) => kind.as_str().to_owned(),
            Self::Unheard => "unheard".to_owned(),
        }
    }

    fn from_key(key: &str) -> Self {
        if key == "unheard" {
            return Self::Unheard;
        }
        KINDS
            .iter()
            .find(|(kind, ..)| kind.as_str() == key)
            .map_or(Self::All, |(kind, ..)| Self::Kind(*kind))
    }
}

#[derive(Properties, PartialEq)]
struct JobsProps {
    standings: Option<Rc<Vec<Standing>>>,
    ready: bool,
    inspect: bool,
    can_stop: bool,
    scope: Option<crate::socket::SessionScope>,
    on_action: Callback<ClientAction, bool>,
}

pub(super) fn render(
    standings: Option<Rc<Vec<Standing>>>,
    connection: &Connection,
    runtime: Option<&AppRuntime>,
    sender: &SenderHandle,
) -> Html {
    let offered =
        |kind| connection.is_online() && runtime.is_some_and(|runtime| runtime.offers(kind));
    let ready = offered(CommandKind::LaneJobsRead);
    let inspect = offered(CommandKind::LaneJobRead);
    let can_stop = offered(CommandKind::LaneJobStop);
    let scope = runtime.map(|runtime| runtime.principal().scope());
    let sender = sender.clone();
    let on_action = Callback::from(move |action| admit_action(&sender, action));
    html! { <Jobs {standings} {ready} {inspect} {can_stop} {scope} {on_action} /> }
}

#[function_component(Jobs)]
fn jobs_view(props: &JobsProps) -> Html {
    let data = use_state(|| None::<Catalog>);
    let selected = use_state(|| None::<Selection>);
    let filter = use_state(|| Filter::All);
    let query = use_state(String::new);
    let stage = use_node_ref();
    let last = use_mut_ref(|| None::<LaneJobRef>);
    let request = use_catalog_refresh(props, &data, &stage);
    let catalog = data.as_ref().filter(|catalog| catalog.scope == props.scope);
    let rows = catalog.and_then(|catalog| catalog.result.as_ref().ok());
    let jobs = rows.map_or(&[][..], |rows| rows.as_slice()).iter();
    let saved = selected
        .as_ref()
        .filter(|selected| selected.admitted == props.scope);
    use_presentation(props.scope, rows, &selected, &filter, &query);
    let live =
        saved.and_then(|selected| jobs.clone().find(|row| same_job(&row.selection, selected)));
    let current = live.map(|row| &row.selection).or(saved);
    let visible = jobs
        .clone()
        .filter(|row| matches_filter(row, *filter, query.trim()));
    let count = visible.clone().count();
    let position = current.and_then(|selected| {
        visible
            .clone()
            .position(|row| same_job(&row.selection, selected))
    });
    let in_list = rows.map(|_| live.is_some());
    use_job_focus(current, &stage, &last);
    let on_select = {
        let selected = selected.clone();
        Callback::from(move |selection| selected.set(Some(selection)))
    };
    let on_back = {
        let selected = selected.clone();
        Callback::from(move |()| selected.set(None))
    };
    let on_nav = navigation(rows.cloned(), &selected, *filter, (*query).clone());
    let oninput = {
        let query = query.clone();
        Callback::from(move |event: InputEvent| {
            query.set(
                event
                    .target_unchecked_into::<web_sys::HtmlInputElement>()
                    .value(),
            );
        })
    };
    let reset = {
        let filter = filter.clone();
        let query = query.clone();
        Callback::from(move |_| {
            filter.set(Filter::All);
            query.set(String::new());
        })
    };
    let on_scroll_key = Callback::from(chain_scroll_key);
    html! {
        <section class="app-sheet-section app-spice-jobs" aria-labelledby={HEADING}
            data-state={if rows.is_some() { "ready" } else { "loading" }}>
            <div class="section-heading">
                <h2 id={HEADING}><button type="button" class="app-job-reset" onclick={reset}
                    title="Show all jobs; clear filters">{"Jobs"}</button></h2>
                <input class="app-job-search" type="search" placeholder="Find job…"
                    aria-label="Find jobs by agent, command, or job ID" value={(*query).clone()} {oninput} />
                {census(jobs.clone(), &filter)}
            </div>
            <div class="app-job-stage" ref={stage} tabindex="-1" data-view={if current.is_some() { "detail" } else { "list" }}
                onkeydown={on_scroll_key}>
                if let Some(selection) = current {
                    <inspector::Inspector key={row_id(&selection.target)} selection={selection.clone()} {position} {count} {in_list}
                        ready={props.inspect} can_stop={props.can_stop} scope={props.scope}
                        on_action={props.on_action.clone()} {on_back} {on_nav} />
                } else if let Some(Err(error)) = catalog.map(|catalog| &catalog.result) {
                    <div class="app-job-catalog-error"><p class="app-job-error" role="alert">{error.clone()}</p>
                        <button type="button" onclick={request.reform(|_| ())} disabled={!props.ready}>{"Refresh jobs"}</button></div>
                } else if rows.is_none() {
                    <p class="app-empty-state">{if props.ready { "Reading keeper jobs…" } else { "Job management is unavailable on this connection." }}</p>
                } else if count == 0 {
                    <p class="app-empty-state">{if jobs.clone().next().is_none() { "No jobs running or awaiting delivery." }
                        else { "No matching jobs. Click Jobs to clear filters." }}</p>
                } else {
                    <ol class="app-spice-job-board" aria-label="Agent jobs">
                        {for visible.map(|row| render_job(row, &on_select, props.inspect))}
                    </ol>
                }
            </div>
        </section>
    }
}

/// A returned-to entry's read applies within the session that made it: the
/// same narrowing, and its job once the current read lists it. What the
/// reader narrows and opens here is remembered on the entry in turn.
#[hook]
fn use_presentation(
    scope: Option<crate::socket::SessionScope>,
    rows: Option<&Rc<Vec<Row>>>,
    selected: &UseStateHandle<Option<Selection>>,
    filter: &UseStateHandle<Filter>,
    query: &UseStateHandle<String>,
) {
    let reopening = use_state(|| None::<String>);
    {
        let filter = filter.clone();
        let query = query.clone();
        let reopening = reopening.clone();
        use_effect_with(scope, move |scope| {
            let Some(scope) = scope else {
                return;
            };
            let Some(view) = navigation::presentation::take(|pending| pending.jobs.take()) else {
                return;
            };
            if view.session != scope.epoch().to_string() {
                return;
            }
            filter.set(Filter::from_key(&view.filter));
            query.set(view.query);
            reopening.set(view.target);
        });
    }
    {
        let rows = rows.cloned();
        let selected = selected.clone();
        let reopening = reopening.clone();
        use_effect_with(
            (
                rows.as_ref().map(|rows| Rc::as_ptr(rows) as usize),
                (*reopening).clone(),
            ),
            move |(_, target)| {
                let (Some(rows), Some(target)) = (rows, target) else {
                    return;
                };
                if let Some(row) = rows
                    .iter()
                    .find(|row| row_id(&row.selection.target) == *target)
                {
                    selected.set(Some(row.selection.clone()));
                }
                reopening.set(None);
            },
        );
    }
    let saved = selected
        .as_ref()
        .filter(|selected| selected.admitted == scope);
    use_effect_with(
        (
            scope.map(|scope| scope.epoch().to_string()),
            saved.map(|selected| row_id(&selected.target)),
            filter.key(),
            (**query).clone(),
        ),
        |(session, target, filter, query)| {
            let Some(session) = session.clone() else {
                return;
            };
            let view = (target.is_some() || !filter.is_empty() || !query.is_empty()).then(|| {
                crate::view::JobsView {
                    session,
                    target: target.clone(),
                    filter: filter.clone(),
                    query: query.clone(),
                }
            });
            navigation::presentation::record(move |current| current.jobs = view);
        },
    );
}

#[hook]
fn use_catalog_refresh(
    props: &JobsProps,
    data: &UseStateHandle<Option<Catalog>>,
    stage: &NodeRef,
) -> Callback<()> {
    let nonce = use_mut_ref(|| 0_u64);
    let busy = use_mut_ref(|| false);
    let request = catalog_request(props, data, &nonce, &busy);
    let latest_request = use_mut_ref(|| request.clone());
    *latest_request.borrow_mut() = request.clone();
    {
        let nonce = nonce.clone();
        let busy = busy.clone();
        let request = request.clone();
        let ready = props.ready;
        use_effect_with((props.ready, props.scope), move |_| {
            *nonce.borrow_mut() += 1;
            *busy.borrow_mut() = false;
            if ready {
                request.emit(());
            }
            move || {
                *nonce.borrow_mut() += 1;
            }
        });
    }
    {
        let latest_request = latest_request.clone();
        let stage = stage.clone();
        let ready = props.ready;
        use_effect_with((props.ready, props.scope), move |_| {
            let timer = ready.then(|| {
                gloo_timers::callback::Interval::new(REFRESH_MILLIS, move || {
                    if stage
                        .cast::<web_sys::HtmlElement>()
                        .is_some_and(|stage| stage.get_bounding_client_rect().width() > 0.0)
                    {
                        let request = latest_request.borrow().clone();
                        request.emit(());
                    }
                })
            });
            move || drop(timer)
        });
    }
    request
}

fn catalog_request(
    props: &JobsProps,
    data: &UseStateHandle<Option<Catalog>>,
    nonce: &Rc<RefCell<u64>>,
    busy: &Rc<RefCell<bool>>,
) -> Callback<()> {
    let ready = props.ready;
    let scope = props.scope;
    let standings = props.standings.clone();
    let on_action = props.on_action.clone();
    let data = data.clone();
    let nonce = nonce.clone();
    let busy = busy.clone();
    Callback::from(move |()| {
        if !ready || *busy.borrow() {
            return;
        }
        *busy.borrow_mut() = true;
        *nonce.borrow_mut() += 1;
        let sent = *nonce.borrow();
        let respond: crate::client_action::JobsResponse = {
            let nonce = nonce.clone();
            let busy = busy.clone();
            let data = data.clone();
            let standings = standings.clone();
            Rc::new(move |result| {
                if *nonce.borrow() != sent {
                    return;
                }
                *busy.borrow_mut() = false;
                data.set(Some(Catalog {
                    scope,
                    result: result.map(|answer| {
                        Rc::new(project(
                            answer,
                            standings.as_deref().map(Vec::as_slice).unwrap_or_default(),
                            scope,
                        ))
                    }),
                }));
            })
        };
        if !on_action.emit(ClientAction::Jobs {
            respond: respond.clone(),
        }) {
            respond(Err(
                "The job catalog request could not be queued.".to_owned()
            ));
        }
    })
}

fn project(
    answer: LaneJobsAnswer,
    standings: &[Standing],
    admitted: Option<crate::socket::SessionScope>,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for lane in answer.lanes {
        let standing = standings.iter().find(|standing| {
            standing.worktree == lane.worktree
                && lane
                    .name
                    .as_ref()
                    .is_none_or(|name| &standing.label == name)
        });
        let label: AttrValue = lane
            .name
            .or_else(|| standing.map(|standing| standing.label.clone()))
            .unwrap_or_else(|| {
                std::path::Path::new(&lane.worktree)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(&lane.worktree)
                    .to_owned()
            })
            .into();
        let accent: AttrValue = standing
            .map(|standing| standing.accent.to_string())
            .unwrap_or_default()
            .into();
        for job in lane.jobs {
            rows.push(Row {
                selection: Selection {
                    target: LaneJobRef {
                        scope: lane.scope,
                        id: job.id,
                    },
                    label: label.clone(),
                    accent: accent.clone(),
                    admitted,
                },
                command: job.command.into(),
                kind: job.kind,
                phase: job.phase,
                started: job
                    .started_millis
                    .and_then(|millis| i64::try_from(millis).ok()),
            });
        }
    }
    rows.sort_by(|left, right| {
        left.started
            .unwrap_or(i64::MAX)
            .cmp(&right.started.unwrap_or(i64::MAX))
            .then_with(|| left.selection.target.id.cmp(&right.selection.target.id))
    });
    rows
}

fn navigation(
    rows: Option<Rc<Vec<Row>>>,
    selected: &UseStateHandle<Option<Selection>>,
    filter: Filter,
    query: String,
) -> Callback<i32> {
    let selected = selected.clone();
    Callback::from(move |direction: i32| {
        let Some(rows) = &rows else {
            return;
        };
        let visible = rows
            .iter()
            .filter(|row| matches_filter(row, filter, query.trim()));
        let count = visible.clone().count();
        if count == 0 {
            return;
        }
        let position = selected.as_ref().and_then(|selected| {
            visible
                .clone()
                .position(|row| same_job(&row.selection, selected))
        });
        let index = match (position, direction < 0) {
            (Some(index), true) => {
                if index == 0 {
                    count - 1
                } else {
                    index - 1
                }
            }
            (Some(index), false) => (index + 1) % count,
            (None, true) => count - 1,
            (None, false) => 0,
        };
        if let Some(row) = visible.clone().nth(index) {
            selected.set(Some(row.selection.clone()));
        }
    })
}

/// Native wheel and touch chaining cross a nested scrollport boundary. Browsers
/// do not consistently do the same for scroll keys, so pass only a key that
/// reached the active Jobs viewport's edge to the containing menu.
fn chain_scroll_key(event: KeyboardEvent) {
    if event.alt_key() || event.ctrl_key() || event.meta_key() {
        return;
    }
    let key = event.key();
    let direction = match key.as_str() {
        "ArrowUp" | "PageUp" => -1,
        "ArrowDown" | "PageDown" => 1,
        _ => return,
    };
    let Some(stage) = event
        .current_target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    let selector = if stage.get_attribute("data-view").as_deref() == Some("detail") {
        ".app-job-evidence"
    } else {
        ".app-spice-job-board"
    };
    let Ok(Some(inner)) = stage.query_selector(selector) else {
        return;
    };
    let target_is_inside = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|target| target.closest(selector).ok().flatten())
        .is_some();
    let at_boundary = !target_is_inside
        || direction < 0 && inner.scroll_top() <= 0
        || direction > 0 && inner.scroll_top() + inner.client_height() >= inner.scroll_height() - 1;
    if !at_boundary {
        return;
    }
    let Ok(Some(outer)) = stage.closest(".app-sidebar-frame") else {
        return;
    };
    let step = if key.starts_with("Page") {
        inner.client_height().max(1)
    } else {
        40
    };
    let current = outer.scroll_top();
    let maximum = (outer.scroll_height() - outer.client_height()).max(0);
    let next = (current + direction * step).clamp(0, maximum);
    if next != current {
        event.prevent_default();
        outer.set_scroll_top(next);
    }
}

fn matches_filter(row: &Row, filter: Filter, query: &str) -> bool {
    let matches = match filter {
        Filter::All => true,
        Filter::Kind(kind) => row.kind == kind,
        Filter::Unheard => row.phase == LaneJobPhase::Ended,
    };
    matches
        && (query.is_empty()
            || [
                row.selection.label.as_str(),
                row.command.as_str(),
                row.selection.target.id.as_str(),
            ]
            .iter()
            .any(|text| {
                text.as_bytes()
                    .windows(query.len())
                    .any(|part| part.eq_ignore_ascii_case(query.as_bytes()))
            }))
}

fn same_job(left: &Selection, right: &Selection) -> bool {
    left.target == right.target
}
fn row_id(target: &LaneJobRef) -> String {
    format!(
        "job-{}-{}-{}",
        target.scope.topology, target.scope.lane, target.id
    )
}

fn census<'a>(
    jobs: impl Iterator<Item = &'a Row> + Clone,
    filter: &UseStateHandle<Filter>,
) -> Html {
    let unheard = jobs
        .clone()
        .filter(|row| row.phase == LaneJobPhase::Ended)
        .count();
    let toggle = |next| {
        let filter = filter.clone();
        Callback::from(move |_| filter.set(if *filter == next { Filter::All } else { next }))
    };
    html! {
        <span class="section-count app-spice-job-census" aria-label="Filter jobs">
            {for KINDS.iter().map(|(kind, running, _, plural)| {
                let count = jobs.clone().filter(|row| row.kind == *kind).count();
                html! { <button type="button" data-job-kind={kind.as_str()} onclick={toggle(Filter::Kind(*kind))}
                    aria-pressed={(**filter == Filter::Kind(*kind)).to_string()} title={format!("{count} {plural}; click to filter")}>
                    <span aria-hidden="true">{*running}</span><span class="visually-hidden">{format!("{plural}: ")}</span>{count}
                </button> }
            })}
            <button type="button" data-job-standing="unheard" onclick={toggle(Filter::Unheard)}
                aria-pressed={(**filter == Filter::Unheard).to_string()} title="Filter ended jobs awaiting agent delivery">{unheard}{" unheard"}</button>
        </span>
    }
}

fn render_job(row: &Row, on_select: &Callback<Selection>, ready: bool) -> Html {
    let selected = row.selection.clone();
    let id = row_id(&selected.target);
    let on_select = on_select.clone();
    let onclick = Callback::from(move |_| on_select.emit(selected.clone()));
    let (_, running, unheard, _) = KINDS
        .iter()
        .find(|(kind, ..)| *kind == row.kind)
        .expect("every job kind has a glyph");
    html! {
        <li key={id.clone()} class="app-spice-job" data-accent={row.selection.accent.clone()}
            data-job={row.selection.target.id.clone()} data-job-kind={row.kind.as_str()}
            data-job-standing={if row.phase == LaneJobPhase::Ended { "unheard" } else { "running" }}>
            <button {id} type="button" class="app-job-open" {onclick} disabled={!ready}
                data-sidebar-focus-key={format!("job:{}", row.selection.target.id)}
                title={row.command.clone()} aria-label={format!("Inspect {} job {}", row.selection.label, row.selection.target.id)}>
                <span class="app-spice-job-glyph" aria-hidden="true">{if row.phase == LaneJobPhase::Ended { *unheard } else { *running }}</span>
                <span class="app-spice-job-agent">{row.selection.label.clone()}</span>
                {match row.started {
                    Some(authored) => html! { <Age {authored} class="app-spice-job-age" /> },
                    None => html! { <span class="app-spice-job-age">{"?"}</span> },
                }}
                <code class="app-spice-job-command">{row.command.clone()}</code>
            </button>
        </li>
    }
}

/// Move focus across a selection change: opening a job focuses its Back
/// control, leaving one returns to its row. Mounting moves nothing, so opening
/// the menu never pulls focus or scrolls the panel to the job stage.
#[hook]
fn use_job_focus(
    current: Option<&Selection>,
    stage: &NodeRef,
    last: &Rc<std::cell::RefCell<Option<LaneJobRef>>>,
) {
    let stage = stage.clone();
    let last = last.clone();
    let mounted = use_mut_ref(|| false);
    use_effect_with(
        current.map(|selection| selection.target.clone()),
        move |target| {
            if let Some(target) = target {
                *last.borrow_mut() = Some(target.clone());
            }
            let settled = std::mem::replace(&mut *mounted.borrow_mut(), true);
            let focus = settled
                .then(|| {
                    match target {
                        Some(_) => web_sys::window()
                            .and_then(|window| window.document())
                            .and_then(|document| document.get_element_by_id("spice-job-back"))
                            .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok()),
                        None => last.borrow().as_ref().and_then(|target| {
                            web_sys::window()?
                                .document()?
                                .get_element_by_id(&row_id(target))?
                                .dyn_into::<web_sys::HtmlElement>()
                                .ok()
                        }),
                    }
                    .or_else(|| stage.cast::<web_sys::HtmlElement>())
                })
                .flatten();
            if let Some(focus) = focus {
                let _ = focus.focus();
            }
            || ()
        },
    );
}
