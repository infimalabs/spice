//! Typed Design contributions read in place in the room history, in journal
//! order beside ordinary conversation. Each entry names its lane, time, vote,
//! the question item it weighs as a link into the Contract list, text, and
//! every cited source as an exact link or the typed reason the resident
//! cannot resolve it; a cited journal fact also names its kind and the task
//! it concerns. Contributions are evidence for the operator's Design
//! judgment, never authority; reading them writes nothing.

use wire::TaskStamp;
use wire::intent::RoomFactSummary;
use wire::rooms::{RoomAuthorityStamp, RoomContributionKind, RoomSource, RoomSourceView};
use yew::prelude::*;

use super::card::{Badge, Face, Speaker};
use super::standing::focus_in_place;
use crate::AppRuntime;
use crate::browser::view::chrome::CopyButton;
use crate::runtime::{Contribution, ContributionRead, RoomEvent, RoomEventKind};

/// Words naming one contribution kind as the vote it casts, or a question.
pub(super) const fn kind_label(kind: RoomContributionKind) -> &'static str {
    match kind {
        RoomContributionKind::Charge(charge) => charge.vote_label(),
        RoomContributionKind::Question => "question",
    }
}

/// One contribution event's card face, or `None` for any other event.
pub(super) fn face(
    runtime: &AppRuntime,
    open_task: &super::OpenTask,
    room: &TaskStamp,
    publication: &RoomAuthorityStamp,
    event: &RoomEvent,
) -> Option<Face> {
    let RoomEventKind::Contributed {
        lane,
        contribution,
        question,
        text,
        sources,
    } = &event.kind
    else {
        return None;
    };
    Some(entry_face(
        runtime,
        open_task,
        room,
        &Contribution {
            publication,
            position: event.position,
            lane: *lane,
            contribution: *contribution,
            question: question.as_ref(),
            text,
            sources,
        },
    ))
}

/// The badge one contribution kind wears beside its author.
pub(super) const fn badge(kind: RoomContributionKind) -> Badge {
    Badge::Kind {
        kind: kind.as_str(),
        label: kind_label(kind),
    }
}

fn entry_face(
    runtime: &AppRuntime,
    open_task: &super::OpenTask,
    room: &TaskStamp,
    entry: &Contribution<'_>,
) -> Face {
    Face {
        speaker: Some(Speaker {
            name: super::conversation::lane_label(runtime, entry.lane),
            lane: Some(entry.lane),
        }),
        receivers: String::new(),
        badge: Some(badge(entry.contribution)),
        words: entry.text.to_owned(),
        quoting: super::card::Quoting::default(),
        tail: html! {
            <>
                {render_question(runtime, room, entry)}
                {render_sources(runtime, open_task, entry.publication, entry.sources)}
            </>
        },
    }
}

/// The question item a contribution weighs, as a link carrying the item's
/// handle that focuses the item in the Design's Contract list; the item's
/// text is the link's title once the resident reads it, and the title says
/// so while the read is pending, failed, or the statement it witnessed has
/// been revised. Nothing for a question naming no item.
fn render_question(runtime: &AppRuntime, room: &TaskStamp, entry: &Contribution<'_>) -> Html {
    let Some(item) = entry.question else {
        return Html::default();
    };
    let handle = item.handle(&[]);
    let title = match runtime.contribution_read(entry.publication) {
        Some(ContributionRead::Read {
            question: Some(text),
            ..
        }) => text.clone(),
        Some(ContributionRead::Read { question: None, .. }) => {
            format!("Question {handle} of an earlier statement")
        }
        _ => format!("Question {handle}"),
    };
    let selector = format!("[data-task-contract=\"{room}\"] [data-contract-question=\"{item}\"]");
    html! {
        <p class="room-contribution-question" data-question-item={item.to_string()}>
            <a
                href={crate::view::design_href(room)}
                title={title}
                onclick={focus_in_place(selector)}
            >{format!("Q {handle}")}</a>
        </p>
    }
}

/// How one source currently reads.
#[derive(Clone, Copy)]
enum SourceState<'a> {
    /// The source yields its bytes; a journal fact also says what it is.
    Available(Option<&'a RoomFactSummary>),
    Unavailable(&'a wire::rooms::RoomSourceUnavailable),
    /// The resident has not answered yet.
    Resolving,
    /// The resident refused or failed the read.
    Unresolved,
}

/// What `facts` says the journal fact `source` cites is, if it is one.
fn fact_summary<'a>(
    facts: &'a [RoomFactSummary],
    source: &RoomSource,
) -> Option<&'a RoomFactSummary> {
    let RoomSource::Journal { fact } = source else {
        return None;
    };
    facts.iter().find(|summary| summary.fact == *fact)
}

fn render_sources(
    runtime: &AppRuntime,
    open_task: &super::OpenTask,
    publication: &RoomAuthorityStamp,
    cited: &[RoomSource],
) -> Html {
    if cited.is_empty() {
        return html! { <p class="room-contribution-sources-none">{"No sources cited."}</p> };
    }
    let (busy, failure, rows) = match runtime.contribution_read(publication) {
        Some(ContributionRead::Read {
            sources: views,
            facts,
            ..
        }) => (
            false,
            None,
            views
                .iter()
                .map(|view| match view {
                    RoomSourceView::Available { source } => {
                        (source, SourceState::Available(fact_summary(facts, source)))
                    }
                    RoomSourceView::Unavailable {
                        source,
                        unavailable,
                    } => (source, SourceState::Unavailable(unavailable)),
                })
                .collect::<Vec<_>>(),
        ),
        Some(ContributionRead::Failed(reason)) => (
            false,
            Some(reason.as_str()),
            cited
                .iter()
                .map(|source| (source, SourceState::Unresolved))
                .collect(),
        ),
        Some(ContributionRead::Loading) | None => (
            true,
            None,
            cited
                .iter()
                .map(|source| (source, SourceState::Resolving))
                .collect(),
        ),
    };
    html! {
        <div class="room-contribution-sources">
            if let Some(reason) = failure {
                <p class="room-contribution-sources-failed" role="status">
                    {format!("Sources could not be resolved: {reason}")}
                </p>
            }
            <ul aria-label="Sources" aria-busy={busy.to_string()}>
                {for rows.into_iter().map(|(source, state)| render_source(runtime, open_task, source, state))}
            </ul>
        </div>
    }
}

/// One cited source beside how it reads. The state takes only the width its
/// row leaves and truncates, so resolution changes its words in place and
/// never moves the card; the full words stay in its title.
fn render_source(
    runtime: &AppRuntime,
    open_task: &super::OpenTask,
    source: &RoomSource,
    state: SourceState<'_>,
) -> Html {
    let (state_key, title, status) = match state {
        SourceState::Available(Some(summary)) => {
            let kind = summary.kind.as_str();
            match &summary.task {
                Some(task) => (
                    "available",
                    format!("{kind} · {}", task.handle),
                    html! {
                        <>
                            {kind}{" · "}
                            {super::task_handle_link(open_task, &task.born, &task.handle)}
                        </>
                    },
                ),
                None => ("available", kind.to_owned(), html! { {kind} }),
            }
        }
        SourceState::Available(None) => {
            ("available", "available".to_owned(), html! { "available" })
        }
        SourceState::Unavailable(unavailable) => {
            let status = format!(
                "unavailable: {}",
                super::conversation::unavailable_reason(unavailable)
            );
            ("unavailable", status.clone(), html! { {status} })
        }
        SourceState::Resolving => ("resolving", "resolving…".to_owned(), html! { "resolving…" }),
        SourceState::Unresolved => (
            "unresolved",
            "not resolved".to_owned(),
            html! { "not resolved" },
        ),
    };
    let (kind, link) = match source {
        RoomSource::Transcript(hunk) => {
            let label = format!(
                "Transcript · {} · member {} offset {} event {} · bytes {}..{}",
                super::conversation::lane_label(runtime, hunk.head.lane),
                hunk.locus.member,
                hunk.locus.offset,
                hunk.locus.event,
                hunk.text.start,
                hunk.text.end,
            );
            let href = matches!(state, SourceState::Available(_))
                .then(|| super::conversation::transcript_card_href(runtime, hunk))
                .flatten();
            let link = href.map_or_else(
                || html! { <span>{label.clone()}</span> },
                |href| html! { <a href={href}>{label.clone()}</a> },
            );
            ("transcript", link)
        }
        RoomSource::Journal { fact } => (
            "journal",
            html! {
                <CopyButton
                    class="app-compact-control"
                    text={AttrValue::from(fact.to_string())}
                    subject="journal fact"
                    label={AttrValue::from(format!("Copy journal fact {fact}"))}
                >{format!("Journal fact {fact}")}</CopyButton>
            },
        ),
        RoomSource::Artifact { digest } => (
            "artifact",
            html! {
                <CopyButton
                    class="app-compact-control"
                    text={AttrValue::from(digest.to_string())}
                    subject="artifact digest"
                    label={AttrValue::from(format!("Copy artifact digest {digest}"))}
                >{"Artifact "}<code>{digest.to_string()}</code></CopyButton>
            },
        ),
        RoomSource::Contribution { publication, .. } => (
            "contribution",
            html! {
                <CopyButton
                    class="app-compact-control"
                    text={AttrValue::from(serde_json::to_string(source).unwrap_or_default())}
                    subject="contribution source"
                    label={AttrValue::from(format!("Copy contribution source {publication}"))}
                >{"Contribution "}<code>{publication.to_string()}</code></CopyButton>
            },
        ),
        RoomSource::Git(git) => {
            let key = git.lines.map_or_else(
                || format!("{}:{}", git.commit, git.path),
                |lines| format!("{}:{}:{lines}", git.commit, git.path),
            );
            (
                "git",
                html! {
                    <CopyButton
                        class="app-compact-control"
                        text={AttrValue::from(key)}
                        subject="repository source"
                        label={AttrValue::from(format!("Copy repository source {git}"))}
                    >{"Source "}<code>{git.to_string()}</code></CopyButton>
                },
            )
        }
    };
    html! {
        <li data-source-kind={kind} data-source-state={state_key}>
            {link}
            <span class="room-contribution-source-state" {title}>{status}</span>
        </li>
    }
}
