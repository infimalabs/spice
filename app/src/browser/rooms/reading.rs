//! A task's room read in place, in the pieces its opened row places apart:
//! the conversation newest first, the operator's recorded decisions, and the
//! lanes it involves, for open and settled tasks alike.
//! Showing any of them joins, groups, or wakes nothing.

use wire::TaskStamp;
use yew::prelude::*;

use super::{RoomProjectionStatus, card};
use crate::AppRuntime;
use crate::composer_markdown::{QuoteSources, referenced_quote_agents, referenced_quote_sources};

/// The room's cards newest first, its publications beside the decisions
/// recorded on it and, oldest of all, the steering directive its task's
/// origin names; or the notice that stands in until the room projection is
/// current.
pub(in crate::browser) fn room_cards(
    runtime: &AppRuntime,
    open_task: &Callback<(TaskStamp, web_sys::Element)>,
    room: &TaskStamp,
    origin: Option<&wire::intent::TaskDetailOrigin>,
) -> Result<Vec<Html>, Html> {
    let projection = runtime.room_projection();
    match projection.status() {
        RoomProjectionStatus::Loading => {
            return Err(html! { <p class="room-empty" aria-busy="true">{"Loading room…"}</p> });
        }
        RoomProjectionStatus::RequiresSnapshot => {
            return Err(html! {
                <p class="room-unavailable">{"Room authority unavailable until reconnect."}</p>
            });
        }
        RoomProjectionStatus::Available => {}
    }
    let has_current_listener = projection.has_current_listener(room, runtime.board());
    let mut cards = projection
        .room(room)
        .map(|room| room.in_journal_order())
        .unwrap_or_default()
        .into_iter()
        .map(|(publication, event)| {
            let card = card::render_card(
                runtime,
                None,
                open_task,
                card::ReadRoom {
                    stamp: room,
                    has_current_listener,
                },
                publication,
                event,
            );
            (event.position, card)
        })
        .chain(
            projection
                .preparation()
                .decisions
                .values()
                .filter(|decision| decision.value.decision.controller == *room)
                .map(|decision| {
                    let card =
                        super::approval::render_decision_card(runtime, open_task, room, decision);
                    (decision.position, card)
                }),
        )
        .collect::<Vec<_>>();
    cards.sort_by_key(|(position, _)| std::cmp::Reverse(*position));
    Ok(cards
        .into_iter()
        .map(|(_, card)| card)
        .chain(origin.map(|origin| origin_card(runtime, origin)))
        .collect())
}

/// The directive a task's origin names, read as its room's first card: its
/// own words under its recorded author, each quote in them expanded as a
/// message card expands it. A directive that no longer resolves shows why
/// through the same quote card.
fn origin_card(runtime: &AppRuntime, origin: &wire::intent::TaskDetailOrigin) -> Html {
    let (speaker, words, quoting) = match &origin.directive {
        wire::QuotePresentation::Resolved {
            text,
            attribution,
            quotes,
            ..
        } => {
            let speaker = match &attribution.author {
                wire::QuoteAuthor::Directive { actor } => card::Speaker {
                    name: crate::work::owner_label(actor, None, runtime.principals()),
                    lane: match actor {
                        wire::Actor::Lane(lane) => Some(*lane),
                        _ => None,
                    },
                },
                wire::QuoteAuthor::Agent { lane, .. } => card::Speaker {
                    name: super::conversation::lane_label(runtime, *lane),
                    lane: Some(*lane),
                },
            };
            (Some(speaker), text.clone(), quoting(runtime, quotes))
        }
        unresolved @ (wire::QuotePresentation::Media { target, .. }
        | wire::QuotePresentation::Unavailable { target, .. }) => (
            None,
            format!("[quote]({target})"),
            card::Quoting {
                quotes: vec![unresolved.clone()],
                ..card::Quoting::default()
            },
        ),
    };
    html! {
        <article class="room-card" key="origin" data-room-origin="" tabindex="0">
            {card::render(
                runtime,
                origin.published.get().as_millisecond(),
                card::Face {
                    speaker,
                    receivers: String::new(),
                    badge: Some(card::Badge::Kind {
                        kind: "origin",
                        label: "Origin",
                    }),
                    words,
                    quoting,
                    tail: Html::default(),
                },
                None,
            )}
        </article>
    }
}

/// `quotes` with the people and source cards their bylines can name, as a
/// team's message cards name them.
fn quoting(runtime: &AppRuntime, quotes: &[wire::QuotePresentation]) -> card::Quoting {
    if quotes.is_empty() {
        return card::Quoting::default();
    }
    let teams = runtime
        .lanes()
        .map_or(&[][..], |lanes| lanes.lanes.as_slice());
    let agents = crate::browser::watchband::quote_agents(runtime, teams);
    let mut sources = QuoteSources::new();
    for team in teams {
        for (target, source) in crate::browser::watchband::team_quote_sources(runtime, team, &[]) {
            sources.entry(target).or_insert(source);
        }
    }
    card::Quoting {
        agents: referenced_quote_agents(quotes, &[], &[], &agents),
        sources: referenced_quote_sources(quotes, &[], &sources),
        quotes: quotes.to_vec(),
    }
}

/// `cards` as the room's conversation, or what stands in for it.
pub(in crate::browser) fn render_conversation(cards: Result<&[Html], &Html>) -> Html {
    match cards {
        Err(notice) => notice.clone(),
        Ok([]) => html! { <p class="room-empty">{"No room publications."}</p> },
        Ok(cards) => html! {
            <section class="room-history" aria-label="Room conversation">
                {for cards.iter().cloned()}
            </section>
        },
    }
}

/// A Design's recorded decisions, newest first, as its room shows them.
pub(in crate::browser) fn render_decisions(
    runtime: &AppRuntime,
    open_task: &Callback<(TaskStamp, web_sys::Element)>,
    room: &TaskStamp,
) -> Html {
    let mut decisions = designs_decisions(runtime, room).collect::<Vec<_>>();
    decisions.sort_by_key(|decision| std::cmp::Reverse(decision.position));
    html! {
        <section class="room-history" aria-label="Decisions">
            {for decisions.into_iter().map(|decision| {
                super::approval::render_decision_card(runtime, open_task, room, decision)
            })}
        </section>
    }
}

/// How many decisions the operator recorded on `room`.
pub(in crate::browser) fn decision_count(runtime: &AppRuntime, room: &TaskStamp) -> usize {
    designs_decisions(runtime, room).count()
}

fn designs_decisions<'a>(
    runtime: &'a AppRuntime,
    room: &'a TaskStamp,
) -> impl Iterator<Item = &'a crate::runtime::RoomPositioned<wire::rooms::PreparationDecision>> {
    runtime
        .room_projection()
        .preparation()
        .decisions
        .values()
        .filter(move |decision| decision.value.decision.controller == *room)
}

/// Why each lane is involved in the Design's room, its seated count beside
/// the heading.
pub(in crate::browser) fn render_lanes(runtime: &AppRuntime, room: &TaskStamp) -> Html {
    let block = super::standing::lanes_block(runtime, room);
    html! {
        <section class="work-room-lanes" aria-label="Lanes">
            <h4>{block.summary}</h4>
            {block.body}
        </section>
    }
}
