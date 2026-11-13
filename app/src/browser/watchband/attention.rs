//! What one agent needs from the operator now, listed in its lane menu.
//!
//! A team's items are computed once and each lane menu lists only its own
//! member's. With nothing due the list renders nothing.

use gloo_timers::callback::Timeout;

use super::super::clock::now_millis;
use super::*;
use crate::runtime::ConsentRuling;
use crate::watchband::{AttentionItem, AttentionSubject, TaskAttention, attention_at};

#[derive(Properties, PartialEq)]
struct AttentionBandProps {
    /// Every candidate item, including ones not yet due.
    items: Rc<Vec<AttentionItem>>,
    /// Render one due item with its control.
    view: Callback<AttentionItem, Html>,
}

/// The due items, re-rendered when the next time-dependent item falls due.
#[function_component(AttentionBand)]
fn attention_band(props: &AttentionBandProps) -> Html {
    let tick = use_state(|| 0_u64);
    let now = now_millis();
    let (due, next) = attention_at(&props.items, now);
    {
        let tick = tick.clone();
        use_effect_with((next, *tick), move |(next, generation)| {
            let generation = *generation;
            let timeout = next.map(|next| {
                // One frame past the deadline, so the re-render finds it due.
                let delay = next.saturating_sub(now_millis()).saturating_add(16);
                Timeout::new(u32::try_from(delay).unwrap_or(u32::MAX), move || {
                    tick.set(generation.wrapping_add(1));
                })
            });
            move || drop(timeout)
        });
    }
    if due.is_empty() {
        return Html::default();
    }
    html! {
        <ul class="agent-menu-attention" aria-label="Needs you">
            {for due.into_iter().map(|item| props.view.emit(item.clone()))}
        </ul>
    }
}

/// One team's attention items and the renderer owning their controls.
pub(super) struct TeamAttention {
    items: Vec<AttentionItem>,
    view: Callback<AttentionItem, Html>,
}

impl TeamAttention {
    /// Every member's candidate items, dropping consent requests already ruled.
    pub(super) fn new(
        runtime: &AppRuntime,
        team: CanonicalUuid,
        standings: &[Standing],
        work: Option<&crate::work::WorkProjection<'_>>,
        sender: &SenderHandle,
        select: &Callback<wire::TaskStamp>,
        navigate: &Callback<navigation::Intent>,
    ) -> Self {
        let mut items =
            watchband::team_attention(standings, work, runtime.inbox(), runtime.consent_queue());
        items.retain(|item| match &item.subject {
            AttentionSubject::Consent(pending) => !matches!(
                runtime.consent_ruling(pending.request),
                Some(ConsentRuling::Ruled(_))
            ),
            _ => true,
        });
        Self {
            items,
            view: item_view(runtime, team, sender, select, navigate),
        }
    }

    /// The list one member's lane menu shows.
    pub(super) fn lane(&self, lane: CanonicalUuid) -> Html {
        let items = self
            .items
            .iter()
            .filter(|item| item.lane == lane)
            .cloned()
            .collect::<Vec<_>>();
        html! { <AttentionBand items={Rc::new(items)} view={self.view.clone()} /> }
    }
}

/// One item renderer that owns every handle its controls need.
fn item_view(
    runtime: &AppRuntime,
    team: CanonicalUuid,
    sender: &SenderHandle,
    select: &Callback<wire::TaskStamp>,
    navigate: &Callback<navigation::Intent>,
) -> Callback<AttentionItem, Html> {
    let decide = runtime.offers(CommandKind::ConsentDecide);
    let rulings = runtime
        .consent_queue()
        .into_iter()
        .flat_map(|queue| &queue.pending)
        .filter_map(|pending| {
            runtime
                .consent_ruling(pending.request)
                .map(|ruling| (pending.request, ruling.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let sender = sender.clone();
    let select = select.clone();
    let navigate = navigate.clone();
    Callback::from(move |item: AttentionItem| {
        let body = match &item.subject {
            AttentionSubject::Consent(pending) => render_consent(
                pending,
                decide.then_some(&sender),
                rulings.get(&pending.request),
            ),
            AttentionSubject::Task {
                task,
                handle,
                title,
                reason,
            } => render_task(task, handle, title, *reason, &select),
            AttentionSubject::Lease {
                task,
                handle,
                expires_at,
            } => render_lease(task.as_ref(), handle, *expires_at, &select),
            AttentionSubject::Direction {
                directive,
                pending,
                oldest,
                preview,
            } => render_direction(
                &item, *directive, *pending, *oldest, preview, team, &navigate,
            ),
        };
        html! {
            <li
                key={item.key()}
                class="agent-menu-attention-item"
                data-attention={attention_kind(&item.subject)}
                data-agent-lane={item.lane.to_string()}
            >{body}</li>
        }
    })
}

const fn attention_kind(subject: &AttentionSubject) -> &'static str {
    match subject {
        AttentionSubject::Consent(_) => "consent",
        AttentionSubject::Task { .. } => "task",
        AttentionSubject::Lease { .. } => "lease",
        AttentionSubject::Direction { .. } => "direction",
    }
}

fn render_consent(
    pending: &wire::intent::ConsentPendingAnswer,
    sender: Option<&SenderHandle>,
    ruling: Option<&ConsentRuling>,
) -> Html {
    let sending = matches!(ruling, Some(ConsentRuling::Sending(_)));
    // The ruling's progress and outcome report through the root toast.
    let rule = |answer: wire::Answer, label: &'static str| {
        let sender = sender.cloned();
        let request = pending.request;
        let onclick = Callback::from(move |event: MouseEvent| {
            event.stop_propagation();
            if let Some(sender) = &sender {
                let _admitted =
                    admit_action(sender, ClientAction::ConsentRuling { request, answer });
            }
        });
        html! {
            <button
                type="button"
                class="agent-menu-attention-rule"
                data-answer={label.to_lowercase()}
                disabled={sending}
                {onclick}
            >{label}</button>
        }
    };
    html! {
        <span
            class="agent-menu-attention-chip"
            title={format!("Asks for {} under rule {}", pending.operation, pending.rule.as_str())}
        >
            <span class="agent-menu-attention-copy">
                <strong>{"Consent"}</strong>
                {" · "}
                {pending.operation.clone()}
            </span>
            if sender.is_some() {
                {rule(wire::Answer::Granted, "Allow")}
                {rule(wire::Answer::Denied, "Deny")}
            }
        </span>
    }
}

fn render_task(
    task: &wire::TaskStamp,
    handle: &str,
    title: &str,
    reason: TaskAttention,
    select: &Callback<wire::TaskStamp>,
) -> Html {
    let label = reason.label();
    let title = crate::composer_markdown::inline::plain_text(title);
    html! {
        <button
            type="button"
            class="agent-menu-attention-chip"
            data-task-attention={reason.key()}
            aria-label={format!("Open task {handle}: {title}")}
            title={format!("{label}: {handle} · {title}")}
            onclick={open_task(task, select)}
        >
            <span class="agent-menu-attention-copy">
                <strong>{label}</strong>
                {" · "}
                {handle.to_owned()}
            </span>
        </button>
    }
}

fn render_lease(
    task: Option<&wire::TaskStamp>,
    handle: &str,
    expiry: i64,
    select: &Callback<wire::TaskStamp>,
) -> Html {
    let copy = html! {
        <>
            <span class="agent-menu-attention-copy">
                <strong>{"Lease"}</strong>
                {" · "}
                {handle.to_owned()}
            </span>
            <Countdown
                {expiry}
                class={classes!("agent-menu-attention-age")}
                title={format!("Lease expiry for {handle}")}
            />
        </>
    };
    match task {
        Some(task) => html! {
            <button
                type="button"
                class="agent-menu-attention-chip"
                aria-label={format!("Open task {handle}, whose lease is expiring")}
                onclick={open_task(task, select)}
            >{copy}</button>
        },
        None => html! { <span class="agent-menu-attention-chip">{copy}</span> },
    }
}

/// Open the task's detail, closing the menu it was chosen from.
fn open_task(task: &wire::TaskStamp, select: &Callback<wire::TaskStamp>) -> Callback<MouseEvent> {
    let task = task.clone();
    let select = select.clone();
    Callback::from(move |event: MouseEvent| {
        agent_menu::close(&event);
        select.emit(task.clone());
    })
}

fn render_direction(
    item: &AttentionItem,
    _directive: CanonicalUuid,
    pending: usize,
    oldest: i64,
    preview: &str,
    team: CanonicalUuid,
    navigate: &Callback<navigation::Intent>,
) -> Html {
    let waiting = if pending == 1 {
        "1 waiting".to_owned()
    } else {
        format!("{pending} waiting")
    };
    let navigate = navigate.clone();
    let open = Callback::from(move |event: MouseEvent| {
        agent_menu::close(&event);
        navigate.emit(navigation::Intent::Work(crate::work::WorkScope::Team {
            team,
        }));
    });
    html! {
        <button
            type="button"
            class="agent-menu-attention-chip"
            aria-label={format!("Open {}'s oldest waiting direction in task overview", item.label)}
            title={crate::composer_markdown::inline::plain_text(preview)}
            onclick={open}
        >
            <span class="agent-menu-attention-copy">
                <strong>{"Direction"}</strong>
                {" · "}
                {waiting}
            </span>
            <Age authored={oldest} class={classes!("agent-menu-attention-age")} />
        </button>
    }
}
