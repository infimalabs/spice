//! Design reading, pass decisions, and the connected principal's inbox.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wire::TaskStamp;
use yew::prelude::*;

mod card;
mod conversation;

use super::sender::SenderHandle;
use crate::AppRuntime;
use crate::runtime::{RoomAnchorAuthority, RoomEventKind, RoomProjectionStatus};

mod approval;
mod blocks;
mod contributions;
mod reading;
mod standing;
pub(super) use approval::render_pending_decision;
pub(super) use reading::{
    decision_count, render_conversation, render_decisions, render_lanes, room_cards,
};
pub(super) use standing::render_standing_line;

/// Opens one task's sheet, returning focus to the activating element.
type OpenTask = Callback<(TaskStamp, web_sys::Element)>;

/// A task's handle as a link that opens that task's sheet.
fn task_handle_link(open_task: &OpenTask, born: &TaskStamp, handle: &wire::TaskHandle) -> Html {
    let handle = handle.to_string();
    let open = {
        let (open_task, born) = (open_task.clone(), born.clone());
        Callback::from(move |event: MouseEvent| {
            if let Some(target) = event
                .current_target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            {
                open_task.emit((born.clone(), target));
            }
        })
    };
    html! {
        <button
            type="button"
            class="app-fact-link"
            aria-label={format!("Open task {handle}")}
            onclick={open}
        ><code>{handle}</code></button>
    }
}

#[derive(Clone)]
pub(super) struct Controls {
    /// The outcome of the last inbox control, shown beside the inbox.
    inbox_notice: UseStateHandle<Option<String>>,
    /// The operator's draft decision for each Design's completed pass.
    decision: UseReducerHandle<approval::Composer>,
    /// Designs whose decision awaits the resident's answer.
    deciding: Rc<RefCell<BTreeSet<wire::TaskStamp>>>,
}

#[hook]
pub(super) fn use_controls() -> Controls {
    let deciding = use_mut_ref(BTreeSet::new);
    Controls {
        inbox_notice: use_state(|| None),
        decision: use_reducer(approval::Composer::default),
        deciding,
    }
}

impl Controls {
    /// The inbox's notice as a callback for its controls.
    fn inbox_reporter(&self) -> Callback<String> {
        let notice = self.inbox_notice.clone();
        Callback::from(move |text| notice.set(Some(text)))
    }
}

/// The connected principal's personal inbox, beneath their identity. Its
/// heading follows that principal's recorded profile on every render, so a
/// profile edit retitles the same surface without remounting its items.
pub(super) fn render_inbox(
    runtime: Option<&AppRuntime>,
    controls: &Controls,
    sender: &SenderHandle,
    drafts: &UseReducerHandle<crate::browser::ComposerDrafts>,
    agents: &super::view::AgentControls,
    open_task: &Callback<(wire::TaskStamp, web_sys::Element)>,
) -> Html {
    let principal = runtime.and_then(AppRuntime::own_identity);
    let (count, body) = match runtime {
        None => (
            None,
            html! { <p class="room-unavailable">{"Unavailable"}</p> },
        ),
        Some(runtime) => match runtime.room_projection().status() {
            RoomProjectionStatus::Loading => (
                None,
                html! { <p class="room-empty" aria-busy="true">{"Loading inbox…"}</p> },
            ),
            RoomProjectionStatus::RequiresSnapshot => (
                None,
                html! {
                    <p class="room-unavailable">{"Inbox unavailable until reconnect."}</p>
                },
            ),
            RoomProjectionStatus::Available => {
                let (count, items) = conversation::render_inbox(
                    runtime,
                    &controls.inbox_reporter(),
                    sender,
                    drafts,
                    agents,
                    open_task,
                );
                (Some(count), items)
            }
        },
    };
    html! {
        <section
            class="room-inbox"
            data-room-inbox=""
            aria-labelledby="spice-principal-inbox-heading"
            tabindex="-1"
        >
            <header>
                <h3 id="spice-principal-inbox-heading">
                    {crate::principal_inbox::heading(principal)}
                </h3>
                if let Some(count) = count {
                    <span>{count}</span>
                }
            </header>
            <div class="room-inbox-scroll">
                if let Some(notice) = (*controls.inbox_notice).clone() {
                    <p class="room-notice" role="status">{notice}</p>
                }
                {body}
            </div>
        </section>
    }
}

/// A recorded room event outside the conversation and contribution families,
/// as a card face: its kind as the badge and its summary as the words.
fn summary_face(runtime: &AppRuntime, event: &crate::runtime::RoomEvent) -> card::Face {
    let (label, words) = event_summary(runtime, event);
    card::Face {
        speaker: None,
        receivers: String::new(),
        badge: Some(card::Badge::Kind {
            kind: "event",
            label,
        }),
        words,
        quoting: card::Quoting::default(),
        tail: Html::default(),
    }
}

/// The kind and summary of one event outside the conversation card family.
fn event_summary(
    runtime: &AppRuntime,
    event: &crate::runtime::RoomEvent,
) -> (&'static str, String) {
    match &event.kind {
        RoomEventKind::Published {
            content: wire::rooms::RoomPublicationContent::Linked { link },
            ..
        } => ("Source", format!("Linked source: {link:?}")),
        // Rendered by the conversation card family whenever its authority
        // is loaded.
        RoomEventKind::Published {
            content: wire::rooms::RoomPublicationContent::Authored { .. },
            ..
        }
        | RoomEventKind::Messaged { .. }
        | RoomEventKind::Posted { .. } => (
            "Conversation",
            "Conversation authority is unavailable.".to_owned(),
        ),
        RoomEventKind::Left { lane, clause } => (
            "Left",
            format!(
                "{} left: {clause}",
                conversation::lane_label(runtime, *lane)
            ),
        ),
        RoomEventKind::Contributed {
            lane,
            contribution,
            text,
            ..
        } => (
            contributions::kind_label(*contribution),
            format!("{}: {text}", conversation::lane_label(runtime, *lane)),
        ),
    }
}

fn anchor_label(runtime: &AppRuntime, room: &TaskStamp) -> String {
    match runtime
        .room_projection()
        .anchor_authority(room, runtime.board())
    {
        RoomAnchorAuthority::Available(handle) => handle.to_string(),
        RoomAnchorAuthority::Settled { handle, standing } => {
            format!("{handle} · {}", standing.as_str())
        }
        RoomAnchorAuthority::Unavailable => format!("{room} · unavailable"),
        RoomAnchorAuthority::Missing => format!("{room} · missing"),
    }
}
