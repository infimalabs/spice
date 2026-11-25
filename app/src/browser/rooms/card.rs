//! One room card face for every surface, shaped as a team message card's: the
//! words lead with a signoff naming the age, the speaker in the accent their
//! lane wears in a team, the flow to any receivers, and the badges that
//! classify what was said; the statuses or sources the publication carries
//! follow. A surface places the face in its own card and never nests a second
//! card around it or shows a publication key.

use std::rc::Rc;

use wire::CanonicalUuid;
use wire::rooms::RoomAuthorityStamp;
use yew::prelude::*;

use super::{contributions, conversation};
use crate::AppRuntime;
use crate::browser::clock::Age;
use crate::runtime::RoomEvent;
use crate::watchband;

/// Who said one card's words. A lane wears its team accent; the operator and
/// a lane this browser has not loaded keep the card's own accent.
pub(super) struct Speaker {
    pub(super) name: String,
    pub(super) lane: Option<CanonicalUuid>,
}

/// What classifies one card, beside its speaker.
pub(super) enum Badge {
    /// A room-only kind, such as a typed contribution or a recorded room
    /// event: a stable key and the words that name it.
    Kind {
        kind: &'static str,
        label: &'static str,
    },
    /// An operator post no current or later lane authority proves was heard.
    Unheard,
}

/// The quote links one card's words carry, as the resident presented them,
/// with everyone and every source card their bylines can name. Most cards
/// carry none.
#[derive(Default)]
pub(super) struct Quoting {
    pub(super) quotes: Vec<wire::QuotePresentation>,
    pub(super) agents: crate::composer_markdown::QuoteAgents,
    pub(super) sources: crate::composer_markdown::QuoteSources,
}

/// One card's signoff-led words and what follows them.
pub(super) struct Face {
    pub(super) speaker: Option<Speaker>,
    /// The receivers after the flow arrow; empty when the card directs no one.
    pub(super) receivers: String,
    pub(super) badge: Option<Badge>,
    pub(super) words: String,
    pub(super) quoting: Quoting,
    pub(super) tail: Html,
}

/// Where a surface anchors its cards: the card's element id, its age link's
/// accessible label, and the corner island of card actions, which leads the
/// words so they flow around it as a team card's prose does.
pub(super) struct Anchor<'a> {
    pub(super) id: &'a str,
    pub(super) label: String,
    pub(super) island: Html,
}

/// The room a post is read in, and whether a current listener already hears
/// it there, which decides whether the post reads as unheard.
#[derive(Clone, Copy)]
pub(super) struct ReadRoom<'a> {
    pub(super) stamp: &'a wire::TaskStamp,
    pub(super) has_current_listener: bool,
}

/// One room event as a card in a surface's list.
pub(super) fn render_card(
    runtime: &AppRuntime,
    controls: Option<conversation::CardControls<'_>>,
    open_task: &super::OpenTask,
    room: ReadRoom<'_>,
    publication: &RoomAuthorityStamp,
    event: &RoomEvent,
) -> Html {
    html! {
        <article
            class="room-card"
            key={publication.to_string()}
            data-room-position={event.position.to_string()}
            tabindex="0"
        >
            {render_face(runtime, controls, open_task, room, publication, event, None)}
        </article>
    }
}

/// One room event's face, for a surface that supplies its own card.
pub(super) fn render_face(
    runtime: &AppRuntime,
    controls: Option<conversation::CardControls<'_>>,
    open_task: &super::OpenTask,
    room: ReadRoom<'_>,
    publication: &RoomAuthorityStamp,
    event: &RoomEvent,
    anchor: Option<Anchor<'_>>,
) -> Html {
    let ReadRoom {
        stamp: room,
        has_current_listener,
    } = room;
    let unheard = runtime
        .room_projection()
        .unheard_post(room, publication, has_current_listener);
    let face = conversation::face(runtime, controls, publication, event, unheard)
        .or_else(|| contributions::face(runtime, open_task, room, publication, event))
        .unwrap_or_else(|| super::summary_face(runtime, event));
    render(runtime, publication.micros() / 1_000, face, anchor)
}

pub(super) fn render(
    runtime: &AppRuntime,
    authored_millis: i64,
    face: Face,
    anchor: Option<Anchor<'_>>,
) -> Html {
    let Face {
        speaker,
        receivers,
        badge,
        words,
        quoting,
        tail,
    } = face;
    let (href, label, island) = anchor.map_or_else(
        || (None, AttrValue::default(), Html::default()),
        |anchor| {
            (
                Some(AttrValue::from(crate::view::card_href(anchor.id))),
                AttrValue::from(anchor.label),
                anchor.island,
            )
        },
    );
    let signoff = html! {
        <>
            <Age authored={authored_millis} class="relative-age" {href} {label} />
            {for speaker.map(|speaker| render_speaker(runtime, speaker))}
            if !receivers.is_empty() {
                <span class="message-quote-direction" aria-label="to">{"→"}</span>
                <span class="message-quote-recipients">{receivers}</span>
            }
            if let Some(badge) = badge {
                <span class="message-classification-group" role="group" aria-label="Message classifications">
                    {render_badge(badge)}
                </span>
            }
        </>
    };
    html! {
        <div class="message-flow room-card-face">
            {quoted_words(runtime, &words, &quoting, Some(signoff), Some(island))}
            {tail}
        </div>
    }
}

/// Authored room words as Markdown through the renderer every message card
/// uses; a card's signoff leads them and its corner island floats beside them.
pub(super) fn render_words(
    runtime: &AppRuntime,
    words: &str,
    signoff: Option<Html>,
    corner: Option<Html>,
) -> Html {
    quoted_words(runtime, words, &Quoting::default(), signoff, corner)
}

fn quoted_words(
    runtime: &AppRuntime,
    words: &str,
    quoting: &Quoting,
    signoff: Option<Html>,
    corner: Option<Html>,
) -> Html {
    let rendered = crate::composer_markdown::render_room_card(
        words,
        &quoting.quotes,
        &quoting.agents,
        &quoting.sources,
    );
    let quote_tasks = runtime
        .board()
        .map(|board| Rc::clone(&board.quote_tasks))
        .unwrap_or_default();
    html! {
        <crate::browser::mermaid::MarkdownBody
            html={AttrValue::from(rendered.html)}
            sources={Rc::new(rendered.diagrams.into_iter().map(AttrValue::from).collect::<Vec<_>>())}
            final_answer=true
            {quote_tasks}
            {signoff}
            {corner}
        />
    }
}

/// The speaker in its lane's accent, linked to that lane's Chat composer when
/// Chat seats it; the operator and an unseated lane stay a plain label.
fn render_speaker(runtime: &AppRuntime, speaker: Speaker) -> Html {
    let lanes = runtime.lanes();
    let accent = speaker
        .lane
        .and_then(|lane| lanes.and_then(|lanes| watchband::lane_accent(&lanes.lanes, lane)));
    let href = speaker
        .lane
        .and_then(|lane| super::super::watchband::lane_composer_href(lanes, lane));
    html! {
        <span class="message-agent-name" data-accent={accent.map(|accent| accent.to_string())}>
            if let Some(href) = href {
                <a
                    class="message-agent-label"
                    {href}
                    aria-label={format!("Open {} in Chat", speaker.name)}
                >{speaker.name}</a>
            } else {
                <span class="message-agent-label">{speaker.name}</span>
            }
        </span>
    }
}

fn render_badge(badge: Badge) -> Html {
    match badge {
        Badge::Kind { kind, label } => html! {
            <span class="message-room-pill" data-message-classification={kind}>
                {label}
            </span>
        },
        Badge::Unheard => {
            const EXPLANATION: &str = "Unheard: no lane listens to this room yet; starting or directing a lane will have this post read.";
            html! {
                <span
                    class="message-room-pill"
                    data-message-classification="unheard"
                    title={EXPLANATION}
                    aria-label={EXPLANATION}
                >
                    {"Unheard"}
                </span>
            }
        }
    }
}
