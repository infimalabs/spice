//! Browser callbacks, activity cards, attachments, and safe Markdown for lane messages.

use wasm_bindgen::JsCast;
use web_sys::DragEvent;
use yew::functional::UseReducerDispatcher;

use crate::composer_drafts::QuoteDestination;

use super::*;
mod draft_images;
mod drag;
mod embedded;
mod media;
mod message_badges;
mod message_boundary;
mod message_controls;
mod message_render;
mod recall;
use draft_images::{add_image_files, has_image};
pub(in crate::browser) use draft_images::{drop_images, paste_images, render_image_indicators};
pub(super) use drag::primary_composer_dragging;
pub(in crate::browser) use drag::{PrimaryComposerDragState, use_primary_composer_drag_state};
use message_boundary::render as render_agent_boundary;
use message_render::render_agent_markdown;

pub(super) fn keyboard_submit(
    lane: CanonicalUuid,
    target: Option<SubmissionTarget>,
    lane_key: String,
    sender: SenderHandle,
    drafts: UseReducerHandle<ComposerDrafts>,
    submissions: SubmissionHandle,
    broadcast_targets: Vec<BroadcastTarget>,
) -> Callback<KeyboardEvent> {
    Callback::from(move |event: KeyboardEvent| {
        if event.key() == "Enter" && (event.meta_key() || event.ctrl_key()) {
            event.prevent_default();
            if event.shift_key() {
                if event.repeat() {
                    return;
                }
                let source_is_primary = event
                    .target()
                    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                    .is_some_and(|target| target.has_attribute("data-primary-composer-target-id"));
                if source_is_primary
                    && submit_broadcast(
                        ComposerSource::Lane(lane),
                        Composition::Primary,
                        &broadcast_targets,
                        &sender,
                        &drafts,
                        &submissions,
                    )
                {
                    focus_after_render(primary_textarea_id(&lane_key));
                }
                return;
            }
            let Ok(fanout) = random_uuid() else {
                return;
            };
            if submit_draft(
                fanout,
                lane,
                target,
                &lane_key,
                &sender,
                &drafts,
                &submissions,
            ) {
                focus_after_render(primary_textarea_id(&lane_key));
            }
        }
    })
}
#[derive(Properties, PartialEq)]
pub(in crate::browser) struct ComposerTextareaProps {
    pub(in crate::browser) id: String,
    pub(in crate::browser) lane: String,
    pub(in crate::browser) drafts: UseReducerDispatcher<ComposerDrafts>,
    pub(in crate::browser) label: String,
    pub(in crate::browser) body: String,
    pub(in crate::browser) placeholder: String,
    pub(in crate::browser) rows: &'static str,
    pub(in crate::browser) primary_target: Option<String>,
    pub(in crate::browser) quote_identity: Option<String>,
    pub(in crate::browser) broadcast_help: Option<String>,
    pub(in crate::browser) onkeydown: Callback<KeyboardEvent>,
    pub(in crate::browser) onpaste: Callback<Event>,
    /// Sent content awaiting durable acceptance: shown, never edited.
    #[prop_or_default]
    pub(in crate::browser) locked: bool,
}

#[hook]
fn use_composer_diagnostics(root: NodeRef, trace_instance: u64, trace_kind: &'static str) {
    use_effect_with((), move |()| {
        let registration = (trace_instance != 0)
            .then(|| root.cast::<web_sys::HtmlTextAreaElement>())
            .flatten()
            .map(|textarea| {
                crate::browser::diagnostics::instant("composer.textarea.mount", || {
                    crate::browser::diagnostics::Metadata {
                        instance: Some(trace_instance),
                        kind: Some(trace_kind),
                        focused: textarea.matches(":focus").ok(),
                        selection_start: textarea.selection_start().ok().flatten(),
                        selection_end: textarea.selection_end().ok().flatten(),
                        ..crate::browser::diagnostics::Metadata::default()
                    }
                });
                let composing = Rc::new(Cell::new(false));
                let listeners = [
                    "beforeinput",
                    "input",
                    "compositionstart",
                    "compositionupdate",
                    "compositionend",
                    "select",
                ]
                .into_iter()
                .map(|event_name| {
                    let callback_textarea = textarea.clone();
                    let composing = composing.clone();
                    let callback = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
                        if event_name == "compositionstart" {
                            composing.set(true);
                        } else if event_name == "compositionend" {
                            composing.set(false);
                        }
                        crate::browser::diagnostics::instant("composer.input", || {
                            crate::browser::diagnostics::Metadata {
                                instance: Some(trace_instance),
                                kind: Some(trace_kind),
                                event: Some(event_name),
                                focused: callback_textarea.matches(":focus").ok(),
                                composing: Some(composing.get()),
                                selection_start: callback_textarea.selection_start().ok().flatten(),
                                selection_end: callback_textarea.selection_end().ok().flatten(),
                                ..crate::browser::diagnostics::Metadata::default()
                            }
                        });
                    });
                    textarea
                        .add_event_listener_with_callback(
                            event_name,
                            callback.as_ref().unchecked_ref(),
                        )
                        .expect("composer textarea accepts its diagnostics listener");
                    (event_name, callback)
                })
                .collect::<Vec<_>>();
                (textarea, listeners)
            });
        move || {
            let Some((textarea, listeners)) = registration else {
                return;
            };
            for (event_name, callback) in listeners {
                let _ = textarea.remove_event_listener_with_callback(
                    event_name,
                    callback.as_ref().unchecked_ref(),
                );
            }
            crate::browser::diagnostics::instant("composer.textarea.unmount", || {
                crate::browser::diagnostics::Metadata {
                    instance: Some(trace_instance),
                    kind: Some(trace_kind),
                    focused: textarea.matches(":focus").ok(),
                    selection_start: textarea.selection_start().ok().flatten(),
                    selection_end: textarea.selection_end().ok().flatten(),
                    ..crate::browser::diagnostics::Metadata::default()
                }
            });
        }
    });
}

#[function_component(ComposerTextarea)]
pub(in crate::browser) fn composer_textarea(properties: &ComposerTextareaProps) -> Html {
    let root = use_node_ref();
    let trace_instance = *use_mut_ref(crate::browser::diagnostics::instance).borrow();
    let trace_kind = if properties.primary_target.is_some() {
        "primary"
    } else {
        "quote"
    };
    let recall = use_mut_ref(crate::composer_history::Recall::default);
    {
        let root = root.clone();
        let lane = properties.lane.clone();
        let recall = recall.clone();
        use_effect_with(
            (properties.onkeydown.clone(), lane),
            move |(onkeydown, lane)| {
                let registration = root.cast::<web_sys::HtmlTextAreaElement>().map(|textarea| {
                    recall::listen(
                        textarea,
                        (*lane).clone(),
                        onkeydown.clone(),
                        recall.clone(),
                        trace_instance,
                        trace_kind,
                    )
                });
                move || drop(registration)
            },
        );
    }
    use_composer_diagnostics(root.clone(), trace_instance, trace_kind);
    {
        let root = root.clone();
        let recall = recall.clone();
        let drafts = properties.drafts.clone();
        let lane = properties.lane.clone();
        let quote = properties.quote_identity.clone();
        // Typed text lives in the element, not the store, until it is sent. The
        // element is keyed by the body it is drawn from, so it writes that body
        // once as it mounts; a value re-applied on every render would overwrite
        // typing. A lane moved into another team is rebuilt there from the
        // store, so the text an element leaves behind is handed back as it goes.
        use_effect_with(properties.body.clone(), move |drawn| {
            let textarea = root.cast::<web_sys::HtmlTextAreaElement>();
            recall.borrow_mut().edge_reset();
            if let Some(textarea) = &textarea {
                textarea.set_value(drawn);
                announce_value(textarea);
            }
            let drawn = drawn.clone();
            move || {
                let Some(live) = textarea.map(|textarea| textarea.value()) else {
                    return;
                };
                if live != drawn {
                    drafts.dispatch(ComposerAction::KeepDetached {
                        lane,
                        quote,
                        drawn,
                        live,
                    });
                }
            }
        });
    }
    html! {
        <textarea
            ref={root}
            id={properties.id.clone()}
            aria-label={properties.label.clone()}
            data-composer-lane={properties.lane.clone()}
            data-primary-composer-target-id={properties.primary_target.clone()}
            data-quote-draft-id={properties.quote_identity.clone()}
            aria-keyshortcuts={properties.primary_target.as_ref().map(|_| "Control+Shift+Enter Meta+Shift+Enter")}
            aria-describedby={properties.broadcast_help.clone()}
            rows={properties.rows}
            placeholder={properties.placeholder.clone()}
            readonly={properties.locked}
            aria-busy={properties.locked.then_some("true")}
            onpaste={properties.onpaste.clone()}
        ></textarea>
    }
}

/// Announce a value the app wrote into a composer as the `input` event typing
/// would raise, so whatever follows drafting hears every change one way.
pub(super) fn announce_value(textarea: &web_sys::HtmlTextAreaElement) {
    let init = web_sys::EventInit::new();
    init.set_bubbles(true);
    if let Ok(event) = web_sys::Event::new_with_event_init_dict("input", &init) {
        let _ = textarea.dispatch_event(&event);
    }
}

/// Route target prefix of every member's primary composer.
pub(in crate::browser) const PRIMARY_TEXTAREA_PREFIX: &str = "composer-primary-";

pub(in crate::browser) fn primary_textarea_id(lane_key: &str) -> String {
    format!("{PRIMARY_TEXTAREA_PREFIX}{lane_key}")
}

/// The Chat route to one lane's primary composer, when Chat seats the lane in
/// a team; an independent seat or a lane this browser has not loaded has none.
pub(in crate::browser) fn lane_composer_href(
    lanes: Option<&crate::runtime::LaneView>,
    lane: CanonicalUuid,
) -> Option<String> {
    let (_team, member) = lanes?.chat_target_for_lane(lane)?;
    let composer = primary_textarea_id(&member.to_string());
    Some(crate::view::card_href(&composer))
}

pub(in crate::browser) fn quote_textarea_id(lane_key: &str, identity: &str) -> String {
    format!("composer-quote-{lane_key}-{identity}")
}

pub(in crate::browser) fn agent_message_source_id(message: &wire::AgentMessage) -> String {
    format!(
        "message-{}",
        crate::runtime::agent_message_card_identity(message)
    )
}
/// Retained source cards for quote targets this open team can navigate to.
///
/// History arrives in stages. Rebuilding this projection from the resident
/// snapshot lets existing quote cards acquire names and links as their source
/// cards arrive, without treating an absent source as a permanent failure.
/// A direction the team shows as its own card is that directive's source, so
/// its age links there wherever it is quoted. The accepted steering key gives
/// a provisional card its final id before the transcript delivery lands, so
/// reconciliation does not rewrite the body HTML or replace visible media.
pub(in crate::browser) fn team_quote_sources(
    runtime: &AppRuntime,
    team: &crate::runtime::LaneRow,
    directions: &[wire::AgentMessage],
) -> crate::composer_markdown::QuoteSources {
    let mut sources = crate::composer_markdown::QuoteSources::new();
    for card in directions {
        let card_id = agent_message_source_id(card);
        for context in &card.directive_contexts {
            let source = crate::composer_markdown::QuoteSource {
                card_id: Some(card_id.clone()),
                ..crate::composer_markdown::QuoteSource::directive(context)
            };
            for media_quote in &context.media_quotes {
                sources.insert(media_quote.target.to_string(), source.clone());
            }
            sources.insert(context.quote_target.to_string(), source);
        }
    }
    for message in runtime.messages().iter().rev() {
        let Some((position, member)) = team
            .members
            .iter()
            .enumerate()
            .find(|(_, member)| member.lane == message.lane)
        else {
            continue;
        };
        let card_id = agent_message_source_id(message);
        let display_millis = message.display_millis();
        let mut insert_agent_source = |target: &wire::QuoteTarget| {
            sources.entry(target.to_string()).or_insert_with(|| {
                crate::composer_markdown::QuoteSource {
                    card_id: Some(card_id.clone()),
                    display_millis,
                    agent: Some(crate::composer_markdown::QuoteAgent {
                        accent: team.accent(position),
                        label: watchband::agent_message_label(
                            &member.worktree,
                            member.name.as_deref(),
                        )
                        .to_owned(),
                    }),
                }
            });
        };
        if let Some(target) = &message.quote_target {
            insert_agent_source(target);
        }
        for media_quote in &message.media_quotes {
            insert_agent_source(&media_quote.target);
        }
        for context in &message.directive_contexts {
            let source = crate::composer_markdown::QuoteSource::directive(context);
            sources
                .entry(context.quote_target.to_string())
                .or_insert_with(|| source.clone());
            for media_quote in &context.media_quotes {
                sources
                    .entry(media_quote.target.to_string())
                    .or_insert_with(|| source.clone());
            }
        }
    }
    sources
}

pub(in crate::browser) fn focus_after_render(id: String) {
    spawn_local(async move {
        TimeoutFuture::new(0).await;
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let Some(textarea) = document
            .get_element_by_id(&id)
            .and_then(|element| element.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        else {
            return;
        };
        let options = web_sys::FocusOptions::new();
        options.set_prevent_scroll(true);
        let _ = textarea.focus_with_options(&options);
    });
}

#[derive(Properties)]
pub(super) struct AgentMessageCardProps {
    pub(super) accent: usize,
    pub(super) member: crate::runtime::LaneMember,
    pub(super) quote_agents: Rc<crate::composer_markdown::QuoteAgents>,
    pub(super) quote_sources: Rc<crate::composer_markdown::QuoteSources>,
    pub(super) quote_tasks: Rc<crate::runtime::QuoteTasks>,
    pub(super) quote_lane: CanonicalUuid,
    pub(super) message: Rc<wire::AgentMessage>,
    pub(super) quote_dispatcher: UseReducerDispatcher<ComposerDrafts>,
    pub(super) open_task: Callback<(wire::TaskStamp, web_sys::Element)>,
    pub(super) speech_queue: Callback<SpeechRequest>,
    pub(super) speech_owner: Option<SpeechOwner>,
    pub(super) lane: CanonicalUuid,
    pub(super) order: u64,
    pub(super) layout: Option<CardLayout>,
    /// Later close images crystallized into this card, oldest first.
    pub(super) companions: Rc<[Rc<wire::AgentMessage>]>,
}

impl PartialEq for AgentMessageCardProps {
    fn eq(&self, other: &Self) -> bool {
        self.accent == other.accent
            && self.member.lane == other.member.lane
            && self.member.name == other.member.name
            && self.member.worktree == other.member.worktree
            && self.quote_agents == other.quote_agents
            && self.quote_sources == other.quote_sources
            && self.quote_tasks == other.quote_tasks
            && self.quote_lane == other.quote_lane
            && (Rc::ptr_eq(&self.message, &other.message) || self.message == other.message)
            && self.quote_dispatcher == other.quote_dispatcher
            && self.open_task == other.open_task
            && self.speech_queue == other.speech_queue
            && self.speech_owner == other.speech_owner
            && self.lane == other.lane
            && self.order == other.order
            && self.layout == other.layout
            && self.companions == other.companions
    }
}

/// Keep stable message DOM and parsed Markdown out of unrelated surface updates.
#[function_component(AgentMessageCard)]
pub(super) fn agent_message_card(props: &AgentMessageCardProps) -> Html {
    let lane = props.lane.to_string();
    let card_key = agent_message_source_id(&props.message);
    let worktree = props.member.worktree.clone();
    let playback = props.member.speech.clone();
    let message_key = card_key.clone();
    let speech_queue = props.speech_queue.clone();
    let speak = Callback::from(move |(control, text): (SpeechControl, String)| {
        if let Some(text) = crate::narration::prepared_utterance(&text) {
            speech_queue.emit(SpeechRequest::message(
                worktree.clone(),
                playback.clone(),
                lane.clone(),
                message_key.clone(),
                control,
                text,
            ));
        }
    });
    let correspondent = agent_message_correspondent(props, card_key, &speak);
    let crystal = use_node_ref();
    super::mosaic_view::use_crystal_arrangement(crystal.clone(), props.companions.clone());
    if props.layout.is_none() {
        return embedded::render(props, &correspondent);
    }
    render_agent_message_card(props, &correspondent, &crystal)
}

fn render_agent_message_card(
    props: &AgentMessageCardProps,
    correspondent: &view::chrome::Correspondent,
    crystal: &NodeRef,
) -> Html {
    let member = &props.member;
    let quote_lane = props.quote_lane;
    let message = &props.message;
    let layout = props
        .layout
        .as_ref()
        .expect("mosaic message cards carry a layout");
    let AgentMessagePresentation {
        source_id,
        attribution,
        footer_label,
        is_boundary,
        rendered_html,
        unlinked_media,
        media_description,
        image_only,
        lone_tool_image,
        single_image_cover,
        card_label,
        classifications,
    } = agent_message_presentation(props, correspondent);
    let crystallized = lone_tool_image && !props.companions.is_empty();
    let mut card_label = if crystallized {
        format!(
            "{} tool images from {attribution}",
            1 + props.companions.len()
        )
    } else {
        card_label
    };
    let speaking = props.speech_owner.is_some();
    // The operator's direction wears the neutral accent the operator speaks
    // in; its quote wears the receiving member's.
    let operator = message.activity == wire::AgentActivityKind::Direction;
    let provisional = message.directive_contexts.iter().any(|context| {
        context
            .delivery
            .as_ref()
            .is_some_and(|delivery| delivery.provisional)
    });
    if provisional {
        card_label.push_str("; accepted, transcript delivery pending");
    }
    let accent = (!operator).then(|| correspondent.accent.to_string());
    let (message_click, message_keydown) = message_card_events(props, &source_id);
    html! {
        <article
            key={source_id.clone()}
            id={source_id.clone()}
            class={classes!(
                "mosaic-message-card",
                is_boundary.then_some("mosaic-compaction-card"),
                (!is_boundary && image_only).then_some("mosaic-message-card--image-only"),
                single_image_cover.then_some("mosaic-message-card--image-cover"),
                crystallized.then_some("mosaic-message-card--crystal"),
                provisional.then_some("mosaic-message-card--provisional"),
                message.final_answer.then_some("final"),
                speaking.then_some("now-playing"),
                layout.lifecycle.as_str(),
            )}
            aria-label={card_label}
            tabindex="-1"
            data-card-kind={if is_boundary {
                watchband::agent_activity_name(message.activity)
            } else {
                "agent-message"
            }}
            data-message-role={if operator { "operator" } else { "agent" }}
            data-mosaic-card-key={source_id}
            onclick={message_click}
            onkeydown={message_keydown}
            onfocusout={Callback::from(overlay::dismiss_disclosure_on_focusout)}
            data-layout-phase={if layout.committed { "committed" } else { "measuring" }}
            data-agent-lane={member.lane.to_string()}
            data-agent-activity={watchband::agent_activity_name(message.activity)}
            data-direction-delivery={provisional.then_some("provisional")}
            data-message-ack-count={classifications.acknowledgements.to_string()}
            data-message-taste-count={classifications.tastes.to_string()}
            data-message-broadcast-count={classifications.broadcasts.to_string()}
            data-message-review-count={classifications.reviews.to_string()}
            data-message-pending-count={classifications.pending.to_string()}
            data-message-decline-count={classifications.declines.to_string()}
            data-speech-active={speaking.to_string()}
            data-producer-target-id={quote_lane.to_string()}
            data-accent={accent.clone()}
            data-accent-slot={accent}
            data-state={layout.lifecycle.as_str()}
            data-track={layout.track.to_string()}
            data-span={layout.span.to_string()}
            data-base={layout.base.to_string()}
            data-row={layout.row.to_string()}
            data-rows={layout.rows.to_string()}
            data-measure-span={layout.measure_span.to_string()}
        >
            if operator && !is_boundary {
                <span class="message-delivery-rail" aria-hidden="true"></span>
            }
            if is_boundary {
                {render_agent_boundary(message, &footer_label)}
            } else {
                <div class="mosaic-message-content">
                    {render_normal_face(message, NormalFace {
                        rendered_html,
                        unlinked_media,
                        media_description: media_description.as_deref(),
                        lone_tool_image,
                        crystal: crystallized.then_some((&props.companions[..], crystal)),
                        footer_label: &footer_label,
                        attribution: &attribution,
                    })}
                </div>
            }
        </article>
    }
}

fn message_card_events(
    props: &AgentMessageCardProps,
    source_id: &str,
) -> (Callback<MouseEvent>, Callback<KeyboardEvent>) {
    let display_millis = props.message.display_millis();
    let click = message_card_click(
        props.open_task.clone(),
        props.quote_dispatcher.clone(),
        props.quote_lane,
        source_id.to_owned(),
        props.order,
        display_millis,
    );
    let keydown = message_card_keydown(props.open_task.clone());
    (click, keydown)
}

fn message_card_click(
    open_task: Callback<(wire::TaskStamp, web_sys::Element)>,
    dispatcher: UseReducerDispatcher<ComposerDrafts>,
    lane: CanonicalUuid,
    source_id: String,
    order: u64,
    display_millis: i64,
) -> Callback<MouseEvent> {
    Callback::from(move |event: MouseEvent| {
        if let Some(action) =
            media_quote_action(event.target(), lane, &source_id, order, display_millis)
        {
            event.prevent_default();
            event.stop_propagation();
            overlay::dismiss_disclosure_from_click(&event);
            dispatch_quote(&dispatcher, action);
            return;
        }
        let Some((chip, task)) = message_task_from_target(event.target()) else {
            return;
        };
        event.prevent_default();
        event.stop_propagation();
        open_task.emit((task, chip));
    })
}

fn message_card_keydown(
    open_task: Callback<(wire::TaskStamp, web_sys::Element)>,
) -> Callback<KeyboardEvent> {
    Callback::from(move |event: KeyboardEvent| {
        if event.key() == "Escape" {
            overlay::dismiss_disclosure_on_escape(event);
            return;
        }
        if event.key() != "Enter" && event.key() != " " {
            return;
        }
        let Some((chip, task)) = message_task_from_target(event.target()) else {
            return;
        };
        event.prevent_default();
        event.stop_propagation();
        open_task.emit((task, chip));
    })
}

/// The quote an image's Quote or Quote-into-new-Draft control under `target`
/// adds, for the card `source_id` in `lane`.
fn media_quote_action(
    target: Option<web_sys::EventTarget>,
    lane: CanonicalUuid,
    source_id: &str,
    order: u64,
    display_millis: i64,
) -> Option<ComposerAction> {
    let control = target
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())?
        .closest("[data-image-quote-action]")
        .ok()??;
    let destination = match control.get_attribute("data-image-quote-action")?.as_str() {
        "draft" => QuoteDestination::NewDraft,
        _ => QuoteDestination::Composer,
    };
    let target = control
        .get_attribute("data-image-quote-target")?
        .parse()
        .ok()?;
    Some(ComposerAction::AddQuote {
        lane: lane.to_string(),
        destination,
        target,
        source_id: source_id.to_owned(),
        order,
        display_millis,
        preview: control.get_attribute("data-image-quote-name")?,
    })
}

/// Add one quote shard where its `ComposerAction::AddQuote` sends it. A shard
/// kept in its own composer takes focus there; the global creator focuses
/// its shard once the anchored composer has opened.
pub(in crate::browser) fn dispatch_quote(
    dispatcher: &UseReducerDispatcher<ComposerDrafts>,
    action: ComposerAction,
) {
    let focus = match &action {
        ComposerAction::AddQuote {
            lane,
            destination: QuoteDestination::Composer,
            target,
            ..
        } => Some(quote_textarea_id(lane, &target.to_string())),
        _ => None,
    };
    dispatcher.dispatch(action);
    if let Some(id) = focus {
        focus_after_render(id);
    }
}
fn message_task_from_target(
    target: Option<web_sys::EventTarget>,
) -> Option<(web_sys::Element, wire::TaskStamp)> {
    let chip = target
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())?
        .closest(".message-task-chip")
        .ok()??;
    let task = chip
        .get_attribute("data-task-handle")?
        .parse::<wire::TaskHandle>()
        .ok()?
        .born()
        .ok()?;
    Some((chip, task))
}

/// What a card's normal face shows besides its producing message.
struct NormalFace<'a> {
    rendered_html: Html,
    unlinked_media: Vec<&'a wire::Media>,
    media_description: Option<&'a str>,
    lone_tool_image: bool,
    /// The companions and cell host of a crystallized run.
    crystal: Option<(&'a [Rc<wire::AgentMessage>], &'a NodeRef)>,
    footer_label: &'a str,
    attribution: &'a str,
}

/// The card's normal face. A lone tool image carries its attribution over the
/// image's top-left corner the way prose leads its words; opening the image is
/// a click on the image itself.
fn render_normal_face(message: &wire::AgentMessage, face: NormalFace<'_>) -> Html {
    let media = match face.crystal {
        Some((companions, crystal)) => media::render_crystal_media(message, companions, crystal),
        None => media::render_agent_message_media(
            face.unlinked_media,
            &message.media,
            face.media_description,
            &message.media_quotes,
        ),
    };
    // A lone tool image carries no badges or card actions, so it has no
    // controls to draw.
    if face.lone_tool_image {
        return html! {
            <>
                <div class="message-body markdown-body">
                    <p class="message-signoff-line">
                        <span class="message-signoff">
                            {render_agent_message_signoff(message, face.footer_label, face.attribution)}
                        </span>
                    </p>
                </div>
                {media}
            </>
        };
    }
    html! {
        <div class="message-flow">
            {face.rendered_html}
            {media}
        </div>
    }
}

struct AgentMessagePresentation<'a> {
    source_id: String,
    attribution: String,
    footer_label: String,
    is_boundary: bool,
    rendered_html: Html,
    unlinked_media: Vec<&'a wire::Media>,
    /// What a tool card says about its images in place of its preview line.
    media_description: Option<String>,
    image_only: bool,
    /// A tool card whose whole face is one image, which fills its committed card.
    lone_tool_image: bool,
    /// An image-only card with exactly one non-diagram image.
    single_image_cover: bool,
    card_label: String,
    classifications: ClassificationMetrics,
}

#[derive(Default)]
struct ClassificationMetrics {
    acknowledgements: usize,
    tastes: usize,
    reviews: usize,
    broadcasts: usize,
    pending: usize,
    declines: usize,
}

impl ClassificationMetrics {
    fn from_message(message: &wire::AgentMessage) -> Self {
        let badge_count = |badge| {
            message
                .directive_contexts
                .iter()
                .filter(|context| context.badge == Some(badge))
                .count()
        };
        let mut metrics = Self {
            tastes: badge_count(wire::DirectiveBadge::Taste),
            reviews: badge_count(wire::DirectiveBadge::Review),
            broadcasts: usize::from(crate::watchband::is_fanout_delivery(message)),
            ..Self::default()
        };
        match message.acknowledgement {
            Some(wire::MessageAcknowledgement::Acknowledged) => metrics.acknowledgements = 1,
            Some(wire::MessageAcknowledgement::Pending) => metrics.pending = 1,
            Some(wire::MessageAcknowledgement::Declined) => metrics.declines = 1,
            None => {}
        }
        metrics
    }
}

fn agent_message_card_label(
    message: &wire::AgentMessage,
    media_description: Option<&str>,
    attribution: &str,
    producer_label: &str,
) -> String {
    match message.activity {
        wire::AgentActivityKind::Turn => unreachable!("turn events are not message cards"),
        wire::AgentActivityKind::OutboxTransport => {
            unreachable!("outbox transports are not message cards")
        }
        wire::AgentActivityKind::Failure => format!("Provider failure from {attribution}"),
        wire::AgentActivityKind::Compaction => {
            format!("Context compacted by {producer_label}")
        }
        wire::AgentActivityKind::Session => format!("Fresh session started by {producer_label}"),
        wire::AgentActivityKind::SignOut => format!("Account signed out on {attribution}"),
        wire::AgentActivityKind::Tool => media_description.map_or_else(
            || format!("Tool result from {attribution}"),
            |description| format!("Tool result from {attribution}: {description}"),
        ),
        wire::AgentActivityKind::Reasoning => format!("Thinking from {attribution}"),
        wire::AgentActivityKind::Prose => format!("Agent message from {attribution}"),
        wire::AgentActivityKind::Direction => format!("Operator direction to {attribution}"),
    }
}

fn agent_message_presentation<'a>(
    props: &'a AgentMessageCardProps,
    correspondent: &view::chrome::Correspondent,
) -> AgentMessagePresentation<'a> {
    let member = &props.member;
    let message = &props.message;
    let quote_tasks = props.quote_tasks.clone();
    let attribution = watchband::agent_message_attribution(
        message.author,
        member.lane,
        &member.worktree,
        member.name.as_deref(),
    );
    let footer_label =
        watchband::agent_message_label(&member.worktree, member.name.as_deref()).to_owned();
    let is_boundary = message.activity.is_boundary();
    let classifications = ClassificationMetrics::from_message(message);
    // A tool card is its image. The resident's preview line stays on the wire
    // for the ticker and the hook, but here it becomes the image's description
    // rather than visible words, and the resident withholds it altogether when
    // a result carried only images.
    let is_tool = message.activity == wire::AgentActivityKind::Tool;
    let media_description = (is_tool && !message.text.trim().is_empty())
        .then(|| watchband::tool_media_description(&message.text));
    let is_prose = message.activity == wire::AgentActivityKind::Prose;
    let signed_body = matches!(
        message.activity,
        wire::AgentActivityKind::Prose | wire::AgentActivityKind::Failure
    );
    let (shown_text, rendered_markdown) =
        render_agent_markdown(props, is_tool, is_prose, signed_body);
    let card_label = agent_message_card_label(
        message,
        media_description.as_deref(),
        &attribution,
        &footer_label,
    );
    let unlinked_media = message
        .media
        .iter()
        .filter(|media| {
            !media
                .is_embedded_in(&shown_text)
                .expect("message media passed admission before browser rendering")
        })
        .collect::<Vec<_>>();
    let image_only = rendered_markdown.image_only
        || (!unlinked_media.is_empty() && shown_text.trim().is_empty());
    let single_image_cover = image_only
        && rendered_markdown.diagrams.is_empty()
        && rendered_markdown.image_count + unlinked_media.len() == 1;
    let lone_tool_image = is_tool && single_image_cover && unlinked_media.len() == 1;
    let control_text = if is_tool { "" } else { message.text.as_str() };
    let message_controls::MessageChrome {
        signoff,
        controls,
        corner,
    } = message_controls::render_message_chrome(
        props,
        control_text,
        signed_body,
        lone_tool_image,
        correspondent,
        &footer_label,
        &attribution,
    );
    AgentMessagePresentation {
        source_id: agent_message_source_id(message),
        attribution,
        footer_label,
        is_boundary,
        rendered_html: html! {
            <super::super::mermaid::MarkdownBody
                html={AttrValue::from(rendered_markdown.html)}
                sources={Rc::new(rendered_markdown.diagrams.into_iter().map(AttrValue::from).collect::<Vec<_>>())}
                final_answer={message.final_answer}
                {quote_tasks}
                {signoff}
                {controls}
                {corner}
            />
        },
        unlinked_media,
        media_description,
        image_only,
        lone_tool_image,
        card_label,
        classifications,
        single_image_cover,
    }
}

fn agent_message_correspondent(
    props: &AgentMessageCardProps,
    message_key: String,
    speak: &Callback<(SpeechControl, String)>,
) -> view::chrome::Correspondent {
    let message = &props.message;
    let order = props.order;
    let quote = message.quote_target.clone().map(|target| {
        let lane = props.quote_lane.to_string();
        let source_id = agent_message_source_id(message);
        let dispatcher = props.quote_dispatcher.clone();
        let display_millis = message.display_millis();
        let preview =
            watchband::quote_label(watchband::first_nonquoted_prose(&message.text).as_deref())
                .to_owned();
        Callback::from(move |destination| {
            dispatch_quote(
                &dispatcher,
                ComposerAction::AddQuote {
                    lane: lane.clone(),
                    destination,
                    target: target.clone(),
                    source_id: source_id.clone(),
                    order,
                    display_millis,
                    preview: preview.clone(),
                },
            );
        })
    });
    view::chrome::Correspondent {
        accent: props.accent,
        speak: speak.clone(),
        speech_owner: props.speech_owner.clone(),
        message_key,
        quote,
    }
}

/// A prose card leads its words the way a quote leads its own: live age,
/// producer, then one ordered ACK/NAK → FINAL → role classification group.
fn render_agent_message_signoff(
    message: &wire::AgentMessage,
    producer_label: &str,
    attribution: &str,
) -> Html {
    html! {
        <>
            {render_message_age(message, attribution)}
            {render_producer_name(message, producer_label, attribution)}
            {message_badges::render(message)}
        </>
    }
}

fn render_producer_name(
    message: &wire::AgentMessage,
    producer_label: &str,
    attribution: &str,
) -> Html {
    html! {
        <view::chrome::CopyButton
            class="message-agent-name"
            text={AttrValue::from(message.author.to_string())}
            subject="agent id"
            title={AttrValue::from(format!("Copy agent id\n{}", message.author))}
            label={AttrValue::from(format!("Copy agent id for {attribution}"))}
        >
            <span class="message-agent-label">{producer_label.to_owned()}</span>
        </view::chrome::CopyButton>
    }
}

fn render_message_age(message: &wire::AgentMessage, attribution: &str) -> Html {
    let locus = format!(
        "tape {}:{}+{}#{}",
        message.locus.member, message.locus.offset, message.locus.length, message.locus.event,
    );
    html! {
        <Age
            authored={message.display_millis()}
            class="relative-age"
            title={locus}
            href={crate::view::card_href(&agent_message_source_id(message))}
            label={format!("Link to message from {attribution}")}
        />
    }
}

/// The card's controls: a corner island emitted before the body so its words
/// flow around it. A signed body's producer metadata lives in its leading
/// signoff, so its island keeps only actions; other cards keep metadata here.
fn render_message_controls(
    message: &wire::AgentMessage,
    shown_text: std::borrow::Cow<'_, str>,
    correspondent: &view::chrome::Correspondent,
    signed: bool,
    producer_label: &str,
    attribution: &str,
) -> Html {
    let shown_text = match shown_text {
        std::borrow::Cow::Borrowed(text) => AttrValue::from(text.to_owned()),
        std::borrow::Cow::Owned(text) => AttrValue::from(text),
    };
    let has_actions = !shown_text.trim().is_empty();
    html! {
        <div
            class="message-controls"
            role="group"
            aria-label={format!("Message controls for {attribution}")}
        >
            <div class="message-controls-meta">
                if !signed {
                    {render_message_age(message, attribution)}
                }
            </div>
            <div class="message-controls-actions">
                if !signed {
                    <span class="message-controls-identity">
                        {message_badges::render_final(message)}
                        {render_producer_name(message, producer_label, attribution)}
                    </span>
                }
                if has_actions {
                    {view::chrome::render_card_actions(shown_text.clone(), correspondent)}
                }
            </div>
        </div>
    }
}

/// The quote shards above one composer. A lane composer passes its status; a
/// design composer has none, so its shards carry no lane status. A locked
/// stack shows sent shards with no control that edits or removes them. The
/// global creator renders those same full-size shards as description-only
/// composers inside its fixed watchband.
pub(in crate::browser) fn render_quote_shards(
    lane_key: &str,
    draft: &ComposerDraft,
    status: Option<crate::runtime::LaneStatus>,
    drafts: &UseReducerHandle<ComposerDrafts>,
    target_submit: Callback<KeyboardEvent>,
    placeholder: &str,
    locked: bool,
) -> Html {
    let status_key = status.map(crate::runtime::LaneStatus::key);
    let description_only = lane_key == crate::composer_drafts::NEW_DRAFT_KEY;
    let stack_label = description_only.then_some("Quoted long-description composers");
    let textarea_label = if description_only {
        "Long description for quoted source"
    } else {
        "Reply to quoted message"
    };
    let remove_label = if description_only {
        "Remove quoted long-description composer"
    } else {
        "Remove quoted composer shard"
    };
    html! {
        <div
            class={classes!(
                "composer-quote-stack",
                description_only.then_some("draft-quote-sections"),
            )}
            aria-label={stack_label}
            data-composer-quote-stack-target-id={lane_key.to_owned()}
        >
            {for draft.quotes.iter().map(|quote| {
                let remove_key = lane_key.to_owned();
                let remove_drafts = drafts.clone();
                let remove_identity = quote.identity.clone();
                let remove = Callback::from(move |_| {
                    remove_drafts.dispatch(ComposerAction::RemoveQuote {
                        lane: remove_key.clone(),
                        identity: remove_identity.clone(),
                    });
                });
                let shown = quote.preview.as_str();
                let textarea_id = quote_textarea_id(lane_key, &quote.identity);
                let onpaste = if locked {
                    Callback::noop()
                } else {
                    paste_images(lane_key.to_owned(), Some(quote.identity.clone()), drafts.clone())
                };
                let href = crate::view::card_href(&quote.source_id);
                html! {
                    <article
                        key={quote.identity.clone()}
                        class="mosaic-composer composer-shard composer-shard--quote"
                        title={shown.to_owned()}
                        data-shard-kind="quote"
                        data-shard-target-id={lane_key.to_owned()}
                        data-quote-identity={quote.identity.clone()}
                        data-quote-target-id={lane_key.to_owned()}
                    >
                        <header class="app-glass-control-surface composer-shard-heading">
                            <a
                                class={classes!(
                                    "composer-shard-link",
                                    "composer-shard-age",
                                    status_key,
                                )}
                                {href}
                                aria-label={format!("Jump to quoted source: {shown}")}
                                data-lane-status={status_key}
                            >{lease_age(
                                Some(quote.display_millis),
                                "composer-shard-age-label",
                                "—",
                            )}</a>
                            <strong class="composer-shard-prose" title={shown.to_owned()}>{shown}</strong>
                            {render_image_indicators(
                                lane_key,
                                &quote.attachments,
                                Some(&quote.identity),
                                drafts,
                                locked,
                            )}
                            <button
                                type="button"
                                aria-label={remove_label}
                                disabled={locked}
                                onclick={remove}
                            >{"×"}</button>
                        </header>
                        <ComposerTextarea
                            key={quote.body.clone()}
                            id={textarea_id}
                            lane={lane_key.to_owned()}
                            drafts={drafts.dispatcher()}
                            label={textarea_label}
                            body={quote.body.clone()}
                            placeholder={placeholder.to_owned()}
                            rows="2"
                            primary_target={None::<String>}
                            quote_identity={Some(quote.identity.clone())}
                            broadcast_help={None::<String>}
                            onkeydown={target_submit.clone()}
                            {onpaste}
                            {locked}
                        />
                    </article>
                }
            })}
        </div>
    }
}

pub(super) fn drop_on_lane(
    lane: CanonicalUuid,
    target: Option<SubmissionTarget>,
    lane_key: String,
    sender: SenderHandle,
    drafts: UseReducerHandle<ComposerDrafts>,
) -> Callback<DragEvent> {
    Callback::from(move |event: DragEvent| {
        event.prevent_default();
        let Some(transfer) = event.data_transfer() else {
            return;
        };
        if let Some(files) = transfer.files()
            && has_image(&files)
        {
            add_image_files(&drafts, &lane_key, None, &files);
            return;
        }
        let Ok(text) = transfer.get_data("text/plain") else {
            return;
        };
        let Some(SubmissionTarget::Resident { occupant, epoch }) = target else {
            return;
        };
        let message = crate::composer_markdown::block_quote(&text);
        if message.is_empty() {
            return;
        }
        let (Some(id), Some(fanout)) = (action_identity(&sender), action_identity(&sender)) else {
            return;
        };
        if !admit_action(
            &sender,
            ClientAction::Steer {
                id,
                fanout,
                lane,
                occupant,
                epoch,
                message,
                attachments: Vec::new(),
            },
        ) {
            transfer.set_drop_effect("none");
        }
    })
}
