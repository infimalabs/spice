//! One sender-to-destination card family for room SYN publications.
//!
//! A card keeps its publication identity through every echo and move. Being
//! recorded in the room and each actor-addressed route's lifecycle are
//! separate statuses; neither is review or authorization.

use std::collections::BTreeSet;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, HtmlInputElement};
use wire::body::PeerLineAction;
use wire::intent::{
    RoomConcernedDirection, RoomConversationContent, SteerLineAction, SteerLineAnswer,
};
use wire::rooms::{
    PrincipalInputRef, RoomAuthorityStamp, RoomDestination, RoomPublicationContent, RoomSource,
    RoomSourceUnavailable, TranscriptHunk,
};
use wire::{Actor, CanonicalUuid, CommandKind, Disposition};
use yew::prelude::*;

use super::card::{Badge, Face, Speaker};
use crate::AppRuntime;
use crate::browser::clock::{Countdown, local_full};
use crate::browser::sender::{SenderHandle, admit_action};
use crate::client_action::{ClientAction, SteerControlRequest};
use crate::runtime::{
    Conversation, ConversationText, Delivery, PrincipalInboxItem, RoomEvent, RoomEventKind,
};

/// What a card reads, and what its controls act through. A card without
/// controls renders its statuses and nothing that writes.
#[derive(Clone, Copy)]
struct Act<'a> {
    runtime: &'a AppRuntime,
    controls: Option<CardControls<'a>>,
}

/// Where a card's controls send and report their outcomes.
#[derive(Clone, Copy)]
pub(super) struct CardControls<'a> {
    pub(super) report: &'a Callback<String>,
    pub(super) sender: &'a SenderHandle,
}

/// Who spoke one conversation-family event, the words it carried when this
/// browser holds them, and the authority its card's routes read. A lane
/// speaker names its lane; a principal does not.
pub(super) struct Spoken<'a> {
    pub(super) author: String,
    pub(super) lane: Option<CanonicalUuid>,
    pub(super) text: Option<String>,
    conversation: Option<&'a Conversation>,
    packet: Option<&'a [CanonicalUuid]>,
}

/// The speaker and words of one SYN or plain post, or `None` for an event
/// outside this family.
pub(super) fn spoken<'a>(
    runtime: &'a AppRuntime,
    publication: &RoomAuthorityStamp,
    event: &'a RoomEvent,
) -> Option<Spoken<'a>> {
    let projection = runtime.room_projection().conversation();
    let (author, lane, echoed, conversation, packet) = match &event.kind {
        RoomEventKind::Messaged {
            source,
            directed,
            conversation,
            ..
        } => (
            lane_label(runtime, source.head.lane),
            Some(source.head.lane),
            echo(runtime, source),
            conversation
                .as_ref()
                .and_then(|stamp| projection.conversation(stamp)),
            Some(directed.as_slice()),
        ),
        RoomEventKind::Posted { author } => {
            let conversation = projection.conversation(publication)?;
            (
                crate::work::owner_label(author, None, runtime.principals()),
                None,
                conversation.text.clone(),
                Some(conversation),
                None,
            )
        }
        // A plain post is recorded in the room and directs no one.
        RoomEventKind::Published {
            author,
            content: RoomPublicationContent::Authored { text },
        } => (
            crate::work::owner_label(author, None, runtime.principals()),
            None,
            Some(text.clone()),
            None,
            None,
        ),
        _ => return None,
    };
    // A lane's bytes stay in its transcript; absent its echo, the text its
    // own deliveries carried, then the resident's read of that transcript,
    // stands in.
    let text = echoed.or_else(|| {
        packet
            .unwrap_or_default()
            .iter()
            .chain(conversation.map_or(&[][..], |conversation| &conversation.directed))
            .find_map(|directive| projection.delivery(directive))
            .map(|delivery| delivery.value.text.clone())
            .or_else(|| {
                conversation.and_then(|conversation| {
                    conversation
                        .publication
                        .principal_routes()
                        .find_map(|(principal, _)| {
                            let input = conversation.publication.principal_input(principal)?;
                            projection
                                .delivery(&input.directive)
                                .map(|delivery| delivery.value.text.clone())
                        })
                })
            })
            .or_else(|| {
                conversation.and_then(|conversation| {
                    match runtime.conversation_text(conversation.publication.publication()) {
                        Some(ConversationText::Read {
                            content: RoomConversationContent::Text { text },
                            ..
                        }) => Some(text.clone()),
                        _ => None,
                    }
                })
            })
    });
    Some(Spoken {
        author,
        lane,
        text,
        conversation,
        packet,
    })
}

/// One room publication's card face, or `None` for an event outside this
/// family. Its controls' outcomes and refusals go to their `report`; without
/// controls the face only reads.
pub(super) fn face(
    runtime: &AppRuntime,
    controls: Option<CardControls<'_>>,
    publication: &RoomAuthorityStamp,
    event: &RoomEvent,
    unheard: bool,
) -> Option<Face> {
    let act = Act { runtime, controls };
    let Spoken {
        author,
        lane,
        text,
        conversation,
        packet,
    } = spoken(runtime, publication, event)?;
    let routes = conversation.map_or_else(
        || {
            packet
                .unwrap_or_default()
                .iter()
                .map(|directive| directive_route(act, *directive, None, ""))
                .collect()
        },
        |conversation| routes(act, conversation),
    );
    let receivers = routes
        .iter()
        .map(|route| route.destination.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Some(Face {
        speaker: Some(Speaker { name: author, lane }),
        receivers,
        badge: unheard.then_some(Badge::Unheard),
        words: text.unwrap_or_else(|| "Source remains in the author's lane transcript.".to_owned()),
        quoting: super::card::Quoting::default(),
        tail: html! {
            <ul class="room-flow-statuses" aria-label="Delivery status">
                <li class="room-chip" data-status="recorded">{"Recorded in room"}</li>
                {for routes.into_iter().map(|route| route.html)}
            </ul>
        },
    })
}

/// This principal's actor-addressed inputs, each retaining its publication
/// source, native steering standing and standard line controls.
pub(super) fn render_inbox(
    runtime: &AppRuntime,
    report: &Callback<String>,
    sender: &SenderHandle,
    drafts: &UseReducerHandle<crate::browser::ComposerDrafts>,
    agents: &crate::browser::view::AgentControls,
    open_task: &Callback<(wire::TaskStamp, web_sys::Element)>,
) -> (usize, Html) {
    let act = Act {
        runtime,
        controls: Some(CardControls { report, sender }),
    };
    let open = runtime.principal_inbox();
    let count = open.len();
    let transcript_sources = open
        .iter()
        .filter_map(|entry| entry.conversation)
        .filter_map(|conversation| match conversation.publication.source() {
            RoomSource::Transcript(hunk) => transcript_message(runtime, hunk)
                .map(crate::browser::watchband::agent_message_source_id),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let transcript_cards =
        crate::browser::watchband::inbox_activity_cards(runtime, &transcript_sources);
    let body = html! {
        <>
            if open.is_empty() {
                <p class="room-empty">{"No principal input awaits action."}</p>
            }
            {for open.into_iter().map(|entry| {
                let directive = entry.input.directive;
                let root_route = entry.conversation.is_some_and(|conversation| {
                    conversation
                        .publication
                        .principal_input(entry.input.principal)
                        .is_some_and(|root| root.directive == directive)
                });
                let retained_card = root_route
                    .then_some(entry.conversation)
                    .flatten()
                    .and_then(|conversation| match conversation.publication.source() {
                        RoomSource::Transcript(hunk) => transcript_message(runtime, hunk),
                        _ => None,
                    })
                    .and_then(|message| {
                        transcript_cards
                            .get(&crate::browser::watchband::agent_message_source_id(message))
                            .cloned()
                    })
                    .and_then(|(team_lane, card)| {
                        crate::browser::watchband::render_inbox_agent_card(
                            team_lane,
                            card,
                            drafts,
                            open_task,
                            agents,
                        )
                    });
                render_inbox_item(act, entry, root_route, retained_card)
            })}
        </>
    };
    (count, body)
}

fn render_inbox_item(
    act: Act<'_>,
    entry: PrincipalInboxItem<'_>,
    root_route: bool,
    retained_card: Option<Html>,
) -> Html {
    let runtime = act.runtime;
    let directive = entry.input.directive;
    let delivery = &entry.delivery.value;
    let conversation = entry.conversation;
    let summary = format!(
        "SYN {} from {}",
        delivery.key,
        actor_label(runtime, &delivery.sender),
    );
    let sender_card =
        conversation.and_then(|conversation| match conversation.publication.source() {
            RoomSource::Transcript(hunk) => transcript_card_href(runtime, hunk),
            _ => None,
        });
    let message_body = retained_card.unwrap_or_else(|| {
        let words = if root_route {
            conversation.map_or_else(
                || delivery.text.clone(),
                |conversation| inbox_words(runtime, conversation),
            )
        } else {
            delivery.text.clone()
        };
        super::card::render_words(runtime, &words, None, None)
    });
    let (concern, direction) = if root_route {
        principal_concern(
            runtime,
            conversation.expect("a root route has its publication"),
            &entry.input,
        )
    } else {
        (
            delivery.line.as_ref().map_or_else(
                || "Direct principal input".to_owned(),
                |line| format!("On line {}", line.root),
            ),
            None,
        )
    };
    let citation = if root_route {
        conversation
            .expect("a root route has its publication")
            .publication
            .publication()
            .to_string()
    } else {
        delivery
            .line
            .as_ref()
            .map_or(directive, |line| line.root)
            .to_string()
    };
    html! {
        <article
            class="room-card room-flow-card"
            key={directive.to_string()}
            data-input={directive.to_string()}
            data-admitted={entry.delivery.position.to_string()}
            tabindex="0"
        >
            <header>
                <strong class="room-flow-route">
                    if let Some(href) = sender_card {
                        <a href={href}>{summary}</a>
                    } else {
                        {summary}
                    }
                </strong>
                <span>{concern}</span>
                <code>{citation}</code>
            </header>
            if let Some(direction) = direction {
                <blockquote class="room-flow-concern">
                    {render_direction(runtime, direction)}
                </blockquote>
            }
            {message_body}
            <ul class="room-flow-statuses" aria-label="Line status">
                {delivery_status_item(
                    act,
                    directive,
                    Some(&entry.input),
                    delivery,
                    &principal_label(runtime, entry.input.principal),
                )}
            </ul>
            <p class="room-flow-guide">
                {"ACK reacts or continues, NAK declines, and FIN explicitly closes the line."}
            </p>
        </article>
    }
}

fn principal_concern<'a>(
    runtime: &'a AppRuntime,
    conversation: &'a Conversation,
    input: &PrincipalInputRef,
) -> (String, Option<&'a RoomConcernedDirection>) {
    let route = conversation.publication.routes().iter().find(|route| {
        route.destination
            == (RoomDestination::Principal {
                principal: input.principal,
            })
    });
    let key = route.and_then(|route| route.direction.as_deref());
    let direction = match (
        key,
        runtime.conversation_text(conversation.publication.publication()),
    ) {
        (
            Some(key),
            Some(ConversationText::Read {
                direction: Some(direction),
                recipient: Some(Actor::Principal(recipient)),
                ..
            }),
        ) if direction.key == key && *recipient == input.principal => Some(direction.as_ref()),
        _ => None,
    };
    if let Some(key) = key {
        return (format!("About your direction {key}"), direction);
    }
    if let Some(context) = route.and_then(|route| route.context.as_ref()) {
        return (
            format!("About {}", super::anchor_label(runtime, context)),
            None,
        );
    }
    ("Direct principal input".to_owned(), None)
}

/// The principal's own direction as sent: its text with every `attachment:`
/// reference expanded to the image the principal attached.
fn render_direction(runtime: &AppRuntime, direction: &RoomConcernedDirection) -> Html {
    let expanded = crate::composer_markdown::expand_media_payloads(
        &direction.text,
        &direction.attachments,
        &[],
        &[],
        &[],
    );
    let words = expanded
        .as_ref()
        .map_or(direction.text.as_str(), |expanded| &expanded.markdown);
    super::card::render_words(runtime, words, None, None)
}

/// What one inbox publication says: the principal's own text, else the
/// resident's read of the authoring lane's transcript, else that lane's echo
/// while the read runs.
fn inbox_words(runtime: &AppRuntime, conversation: &Conversation) -> String {
    if let Some(text) = &conversation.text {
        return text.clone();
    }
    match runtime.conversation_text(conversation.publication.publication()) {
        Some(ConversationText::Read {
            content: RoomConversationContent::Text { text },
            ..
        }) => text.clone(),
        Some(ConversationText::Read {
            content: RoomConversationContent::Linked { source },
            ..
        }) => format!("Linked source: {source:?}"),
        Some(ConversationText::Read {
            content: RoomConversationContent::Unavailable { unavailable },
            ..
        }) => format!("Text unavailable: {}", unavailable_reason(unavailable)),
        Some(ConversationText::Failed(reason)) => format!("Text could not be read: {reason}"),
        Some(ConversationText::Loading) | None => match conversation.publication.source() {
            RoomSource::Transcript(hunk) => echo(runtime, hunk),
            _ => None,
        }
        .unwrap_or_else(|| "Reading the author's transcript…".to_owned()),
    }
}

pub(super) fn unavailable_reason(unavailable: &RoomSourceUnavailable) -> String {
    match unavailable {
        RoomSourceUnavailable::Missing {} => "the source is missing".to_owned(),
        RoomSourceUnavailable::Hidden {} => "the source is hidden".to_owned(),
        RoomSourceUnavailable::Changed {} => "the source no longer holds these bytes".to_owned(),
        RoomSourceUnavailable::Unreadable {} => "the source cannot be read now".to_owned(),
    }
}

/// One route's destination and lifecycle status.
struct Route {
    destination: String,
    html: Html,
}

fn routes(act: Act<'_>, conversation: &Conversation) -> Vec<Route> {
    let mut directed = conversation.directed.iter();
    conversation
        .publication
        .routes()
        .iter()
        .map(|route| match &route.destination {
            RoomDestination::Lane { lane } => match directed.next() {
                Some(directive) => {
                    delivery_route(act, *directive, None, lane_label(act.runtime, *lane), "")
                }
                None => unavailable_route(lane_label(act.runtime, *lane)),
            },
            RoomDestination::Principal { principal } => {
                let destination = principal_label(act.runtime, *principal);
                match conversation.publication.principal_input(*principal) {
                    Some(input) => {
                        delivery_route(act, input.directive, Some(&input), destination, "")
                    }
                    None => unavailable_route(destination),
                }
            }
            RoomDestination::Task { task } => {
                let destination = format!("#{}", super::anchor_label(act.runtime, task));
                Route {
                    html: html! {
                        <li class="room-chip" data-status="room">
                            {format!("{destination}: recorded there; no directed recipient")}
                        </li>
                    },
                    destination,
                }
            }
        })
        .collect()
}

fn unavailable_route(destination: String) -> Route {
    Route {
        html: html! {
            <li class="room-chip" data-status="unavailable">
                {format!("{destination}: input unavailable")}
            </li>
        },
        destination,
    }
}

fn directive_route(
    act: Act<'_>,
    directive: CanonicalUuid,
    input: Option<&PrincipalInputRef>,
    lead: &str,
) -> Route {
    let destination = act
        .runtime
        .room_projection()
        .conversation()
        .delivery(&directive)
        .map_or_else(
            || "unknown recipient".to_owned(),
            |delivery| actor_label(act.runtime, &delivery.value.recipient),
        );
    delivery_route(act, directive, input, destination, lead)
}

/// One actor-addressed steering input, whether its recipient is a lane or a
/// principal endpoint.
fn delivery_route(
    act: Act<'_>,
    directive: CanonicalUuid,
    input: Option<&PrincipalInputRef>,
    destination: String,
    lead: &str,
) -> Route {
    let delivery = act
        .runtime
        .room_projection()
        .conversation()
        .delivery(&directive)
        .map(|delivery| &delivery.value);
    let Some(delivery) = delivery else {
        return unavailable_route(destination);
    };
    Route {
        html: delivery_status_item(
            act,
            directive,
            input,
            delivery,
            &format!("{lead}{destination}"),
        ),
        destination,
    }
}

fn delivery_status_item(
    act: Act<'_>,
    directive: CanonicalUuid,
    input: Option<&PrincipalInputRef>,
    delivery: &Delivery,
    destination: &str,
) -> Html {
    let (status, label) = delivery_status(act.runtime, delivery);
    let exact = local_full(&delivery.expires);
    let clock = (delivery.disposition == Disposition::Live).then(|| {
        html! {
            <Countdown
                expiry={delivery.expires.get().as_millisecond()}
                class={classes!("room-input-time")}
                title={exact.clone()}
            />
        }
    });
    let control = match (delivery.disposition, input, act.controls) {
        (Disposition::Live, Some(input), Some(controls)) => {
            line_controls(act.runtime, controls, input)
        }
        (Disposition::Lapsed, Some(_), Some(controls)) => {
            recover_control(act.runtime, controls, directive)
        }
        _ => Html::default(),
    };
    let child = delivery.line_body.and_then(|child| {
        act.runtime
            .room_projection()
            .conversation()
            .delivery(&child)
            .map(|delivery| {
                html! {
                    <blockquote class="room-flow-line-body" data-line-body={child.to_string()}>
                        {super::card::render_words(
                            act.runtime,
                            &delivery.value.text,
                            None,
                            None,
                        )}
                    </blockquote>
                }
            })
    });
    let text = format!("{destination}: {label}");
    html! {
        <li
            class="room-chip"
            data-status={status}
            data-steering-key={delivery.key.clone()}
            data-input={directive.to_string()}
            title={format!("{text}; expiry {exact}")}
        >
            <span>{text}{clock}</span>
            {child}
            <div class="room-flow-slot">{control}</div>
        </li>
    }
}

fn delivery_status(runtime: &AppRuntime, delivery: &Delivery) -> (&'static str, &'static str) {
    match delivery.disposition {
        Disposition::Lapsed => return ("expired", "lapsed"),
        Disposition::DeadLettered => return ("unavailable", "undeliverable"),
        Disposition::Canceled => return ("cancelled", "cancelled"),
        Disposition::Live | Disposition::Acked | Disposition::Declined => {}
    }
    if let Some(action) = delivery.line_action {
        return match action {
            PeerLineAction::Reaction => ("acked", "seen"),
            PeerLineAction::Continue => ("acked", "continued"),
            PeerLineAction::Finish => ("acked", "closed by FIN"),
            PeerLineAction::Decline => ("declined", "declined by NAK"),
            PeerLineAction::AutomaticFinish => ("acked", "closed at clean checkpoint"),
            PeerLineAction::Unanswered => ("expired", "ended unanswered"),
        };
    }
    let member = delivery
        .recipient
        .lane()
        .and_then(|lane| runtime.lanes().and_then(|lanes| lanes.member(lane)));
    match delivery.disposition {
        Disposition::Live if delivery.queued => ("unheard", "queued for vacant lane · unheard"),
        Disposition::Live if delivery.recipient.lane().is_some() && member.is_none() => {
            ("unavailable", "lane unavailable · pending")
        }
        Disposition::Live if member.is_some_and(|member| member.occupant.is_none()) => {
            ("vacant", "lane vacant · pending")
        }
        Disposition::Live => ("pending", "pending"),
        Disposition::Acked => ("acked", "acked"),
        Disposition::Declined => ("declined", "declined"),
        Disposition::Lapsed | Disposition::DeadLettered | Disposition::Canceled => {
            unreachable!("terminal dispositions returned above")
        }
    }
}

/// ACK, NAK and explicit FIN operate on the exact principal input. Blank ACK
/// is a reaction; nonblank text is the raw line body.
fn line_controls(
    runtime: &AppRuntime,
    controls: CardControls<'_>,
    input: &PrincipalInputRef,
) -> Html {
    let text = NodeRef::default();
    let enabled = runtime.offers(CommandKind::SteerLineControl);
    let control = |action: SteerLineAction| {
        let act = Owned::from(controls);
        let text = text.clone();
        let input = input.clone();
        move |event: &Event| {
            act.send(
                event,
                SteerControlRequest::line(input.clone(), action, input_value(&text)),
            );
        }
    };
    let ack = control(SteerLineAction::Ack);
    let nak = control(SteerLineAction::Nak);
    let fin = control(SteerLineAction::Fin);
    let prompt = "Line body (optional)";
    html! {
        <form class="room-flow-control" onsubmit={Callback::from(move |event: SubmitEvent| {
            event.prevent_default();
            ack(&event);
        })}>
            <input ref={text} aria-label={prompt} placeholder={prompt} disabled={!enabled} />
            <button type="submit" disabled={!enabled}>{"ACK"}</button>
            <button type="button" disabled={!enabled} onclick={Callback::from(move |event: MouseEvent| nak(&event))}>{"NAK"}</button>
            <button type="button" disabled={!enabled} onclick={Callback::from(move |event: MouseEvent| fin(&event))}>{"FIN"}</button>
        </form>
    }
}

fn recover_control(
    runtime: &AppRuntime,
    controls: CardControls<'_>,
    directive: CanonicalUuid,
) -> Html {
    let enabled = runtime.offers(CommandKind::SteerRecover);
    let act = Owned::from(controls);
    let onclick = Callback::from(move |event: MouseEvent| {
        act.send(
            &event,
            SteerControlRequest::Recover(wire::intent::DirectiveRef { directive }),
        );
    });
    html! {
        <button type="button" class="room-flow-control" disabled={!enabled} {onclick}>{"Recover"}</button>
    }
}

/// Control context a callback can own.
#[derive(Clone)]
struct Owned {
    report: Callback<String>,
    sender: SenderHandle,
}

impl From<CardControls<'_>> for Owned {
    fn from(controls: CardControls<'_>) -> Self {
        Self {
            report: controls.report.clone(),
            sender: controls.sender.clone(),
        }
    }
}

impl Owned {
    /// Send one control and report its outcome where its surface reports.
    /// Keyboard focus moves to the acting card first, so a retiring input never
    /// strands it.
    fn send(&self, event: &Event, request: SteerControlRequest) {
        if let Some(card) = acting_card(event) {
            let _ = card.focus();
        }
        let line_report = self.report.clone();
        let line_respond = Rc::new(move |answer: Result<SteerLineAnswer, String>| {
            line_report.emit(match answer {
                Ok(answer) => line_notice(&answer),
                Err(error) => error,
            });
        });
        let directive_report = self.report.clone();
        let directive_respond = Rc::new(
            move |answer: Result<wire::intent::DirectiveAnswer, String>| {
                directive_report.emit(match answer {
                    Ok(_) => "Steering input recovered.".to_owned(),
                    Err(error) => error,
                });
            },
        );
        let _ = admit_action(
            &self.sender,
            ClientAction::SteerControl {
                request,
                line_respond,
                directive_respond,
            },
        );
    }
}

fn line_notice(answer: &SteerLineAnswer) -> String {
    let effect = match answer.effect.action {
        None => "input acknowledged; line already closed",
        Some(PeerLineAction::Reaction) => "input seen",
        Some(PeerLineAction::Continue) => "line continued",
        Some(PeerLineAction::Finish) => "line closed by FIN",
        Some(PeerLineAction::Decline) => "line declined by NAK",
        Some(PeerLineAction::AutomaticFinish) => "line closed at clean checkpoint",
        Some(PeerLineAction::Unanswered) => "line ended unanswered",
    };
    let body = match answer.body {
        Some(wire::outbox::BodyFate::Sent) => "; body sent",
        Some(wire::outbox::BodyFate::Unsent) => "; body unsent",
        None => match &answer.effect.body {
            Some(wire::outbox::LineBody::Admitted(_)) => "; body sent",
            Some(wire::outbox::LineBody::Refused { .. }) => "; body unsent",
            None => "",
        },
    };
    format!("{effect}{body}.")
}

/// The card a control acts on, or the inbox holding it, since a settled
/// item leaves the inbox.
fn acting_card(event: &Event) -> Option<HtmlElement> {
    let target = event.target()?.dyn_into::<Element>().ok()?;
    target
        .closest(".room-card")
        .ok()
        .flatten()
        .or_else(|| target.closest("[data-room-inbox]").ok().flatten())?
        .dyn_into::<HtmlElement>()
        .ok()
}

fn input_value(node: &NodeRef) -> String {
    node.cast::<HtmlInputElement>()
        .map(|input| input.value())
        .unwrap_or_default()
}

pub(super) fn actor_label(runtime: &AppRuntime, actor: &Actor) -> String {
    match actor {
        Actor::Principal(principal) => principal_label(runtime, *principal),
        Actor::Lane(lane) => lane_label(runtime, *lane),
        Actor::Spice => "Spice".to_owned(),
    }
}

fn principal_label(runtime: &AppRuntime, principal: CanonicalUuid) -> String {
    let actor = Actor::Principal(principal);
    crate::principal_inbox::label(
        principal,
        runtime
            .principals()
            .and_then(|principals| principals.identity(&actor)),
    )
}

pub(super) fn lane_label(runtime: &AppRuntime, lane: CanonicalUuid) -> String {
    runtime
        .lanes()
        .and_then(|lanes| lanes.member(lane))
        .map_or_else(
            || format!("lane {}", &lane.to_string()[..8]),
            |member| {
                crate::watchband::agent_message_label(&member.worktree, member.name.as_deref())
                    .to_owned()
            },
        )
}

/// The loaded agent message one transcript hunk was contributed from.
fn transcript_message<'a>(
    runtime: &'a AppRuntime,
    hunk: &TranscriptHunk,
) -> Option<&'a wire::AgentMessage> {
    runtime
        .messages()
        .iter()
        .find(|message| message.lane == hunk.head.lane && message.locus == hunk.locus)
        .map(Rc::as_ref)
}

/// The card route of the loaded message one transcript hunk came from.
pub(super) fn transcript_card_href(runtime: &AppRuntime, hunk: &TranscriptHunk) -> Option<String> {
    transcript_message(runtime, hunk).map(|message| {
        crate::view::card_href(&crate::browser::watchband::agent_message_source_id(message))
    })
}

/// The exact packet body retained beside its presented source card.
fn echo(runtime: &AppRuntime, hunk: &TranscriptHunk) -> Option<String> {
    let message = transcript_message(runtime, hunk)?;
    crate::runtime::room_message_body(&message.sends, hunk.text).map(str::to_owned)
}
