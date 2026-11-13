//! A room's standing inside its design menu: why each lane is involved and
//! whether it is available. Every row names the admitted source it traces to.
//! Nothing here is a roster of authority, and rendering it mounts no view and
//! wakes no lane.

use wasm_bindgen::JsCast as _;
use wire::TaskStamp;
use wire::intent::BoardTask;
use wire::rooms::{DesignCyclePresentation, DesignNoticeEvidence};
use yew::prelude::*;

use crate::AppRuntime;
use crate::runtime::{ParticipantReason, RoomParticipant, design_next_action};
use crate::watchband;

/// A Design's standing as one line: its pass cycle, who acts next, and the
/// semantic action that moves it forward. A Design without a cycle, claimed,
/// deferred, or settled, reads its lifecycle and next action instead.
pub(in crate::browser) fn render_standing_line(runtime: &AppRuntime, row: &BoardTask) -> Html {
    let room = &row.statement.body.born;
    let target = row.completion.body.target(&row.workflow.body.phase);
    let cycle = runtime
        .board()
        .and_then(|board| board.design_cycles.get(room))
        .zip(row.design_coverage.as_ref())
        .map(|(&cycle, coverage)| {
            DesignCyclePresentation::new(
                cycle,
                row.design_cycle,
                &row.statement.body.handle,
                row.design_availability(),
                coverage,
                &row.statement.body.term,
                target,
            )
        });
    let Some(cycle) = cycle else {
        let settled = row
            .design_standing()
            .is_some_and(|standing| !standing.is_open());
        return html! {
            <p class="work-standing-line" data-room-standing="">
                {row.design_standing().map(|standing| standing.to_string())}
                {" · "}{design_next_action(settled, None, target)}
            </p>
        };
    };
    let action = design_next_action(false, Some(&cycle), target);
    html! {
        <p
            class="work-standing-line"
            data-room-standing=""
            data-design-cycle={cycle.cycle.key()}
            data-cycle-actor={cycle.actor.label()}
        >
            {cycle.cycle.label()}
            {" · next "}<strong>{cycle.actor.label()}</strong>
            {" · "}{action}
        </p>
    }
}

/// Focus the element `selector` names when this page shows it; otherwise
/// let the link open the Design's read view, which does.
pub(super) fn focus_in_place(selector: String) -> Callback<MouseEvent> {
    Callback::from(move |event: MouseEvent| {
        let Some(element) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.query_selector(&selector).ok().flatten())
        else {
            return;
        };
        event.prevent_default();
        element.scroll_into_view();
        if let Ok(element) = element.dyn_into::<web_sys::HtmlElement>() {
            let _ = element.focus();
        }
    })
}

/// Why each lane is involved: the summary counts the involved and seated
/// lanes, and each lane reads as one line.
pub(super) fn lanes_block(runtime: &AppRuntime, room: &TaskStamp) -> super::blocks::RoomBlock {
    let participants = runtime
        .room_projection()
        .participants(room, runtime.board());
    let lanes = participants
        .iter()
        .map(|participant| lane_row(runtime, participant))
        .collect::<Vec<_>>();
    let seated = lanes
        .iter()
        .filter(|lane| lane.availability == "seated")
        .count();
    super::blocks::RoomBlock {
        key: "lanes",
        label: "Lanes",
        summary: html! {
            <>
                {"Lanes"}
                <span class="app-count-badge">{lanes.len()}</span>
                if !lanes.is_empty() {
                    <span class="room-block-meta">{format!("{seated} seated")}</span>
                }
            </>
        },
        body: if lanes.is_empty() {
            html! { <p class="muted">{"No lane is involved."}</p> }
        } else {
            html! {
                <ul class="room-standing-lanes" aria-label="Why each lane is involved">
                    {for lanes.into_iter().map(|lane| lane.html)}
                </ul>
            }
        },
    }
}

/// One lane's roster line beside its availability, so the summary can count
/// seated lanes without resolving each member twice.
struct LaneRow {
    availability: &'static str,
    html: Html,
}

fn lane_row(runtime: &AppRuntime, participant: &RoomParticipant) -> LaneRow {
    let member = runtime.lanes().and_then(|lanes| {
        lanes
            .lanes
            .iter()
            .flat_map(|team| &team.members)
            .find(|member| member.lane == participant.lane)
    });
    let lane = participant.lane.to_string();
    // A lane no team lists reads by its UUID's first group; the whole UUID
    // stays in the title and `data-lane`.
    let label = member.map_or_else(
        || lane.split('-').next().unwrap_or(&lane).to_owned(),
        |member| {
            watchband::agent_message_label(&member.worktree, member.name.as_deref()).to_owned()
        },
    );
    let availability = match member {
        Some(member) if member.occupant.is_some() => "seated",
        Some(_) => "vacant",
        None => "unknown",
    };
    let href = super::super::watchband::lane_composer_href(runtime.lanes(), participant.lane);
    let publications = participant
        .reasons
        .iter()
        .filter_map(|reason| match reason {
            ParticipantReason::Publication(publication) => Some(publication),
            _ => None,
        })
        .collect::<Vec<_>>();
    let html = html! {
        <li key={lane.clone()} data-lane={lane.clone()}>
            if let Some(href) = href {
                <a
                    class="room-standing-lane"
                    {href}
                    title={lane.clone()}
                    aria-label={format!("Open {label} in Chat")}
                >{label.clone()}</a>
            } else {
                <span class="room-standing-lane" title={lane}>{label}</span>
            }
            <span class="room-standing-availability" data-availability={availability}>{availability}</span>
            if let Some(latest) = publications.last() {
                {reason_chip(
                    "history",
                    &format!("Published {}", publications.len()),
                    &format!("Authored {} room publications; latest {latest}", publications.len()),
                )}
            }
            {for participant.reasons.iter().filter_map(|reason| render_reason(runtime, reason))}
        </li>
    };
    LaneRow { availability, html }
}

/// One non-history reason as a chip naming its source; publication history
/// renders once per lane instead.
fn render_reason(runtime: &AppRuntime, reason: &ParticipantReason) -> Option<Html> {
    let invitation;
    let (kind, label, source) = match reason {
        ParticipantReason::Publication(_) => return None,
        ParticipantReason::Asked {
            evidence,
            questions,
            admission,
        } => {
            let (label, scoring) = invitation_match(admission);
            invitation = label;
            (
                "invitation",
                invitation.as_str(),
                format!(
                    "{}; {scoring}{}",
                    match evidence {
                        DesignNoticeEvidence::Task { task, .. }
                        | DesignNoticeEvidence::TaskClaim { task, .. } => format!(
                            "A Design request asked this lane from retained evidence about {}",
                            task_label(runtime, task)
                        ),
                        DesignNoticeEvidence::Conversation { source } => format!(
                            "A Design request asked this lane from retained conversation evidence at member {}, byte {}",
                            source.locus.member, source.locus.offset
                        ),
                    },
                    asked_questions(questions)
                ),
            )
        }
        ParticipantReason::Work(task) => (
            "work",
            "Working",
            format!("Holds {}", task_label(runtime, task)),
        ),
        ParticipantReason::Stop(key) => (
            "responsibility",
            "Asked to stop",
            format!("Stop request {key} on its current claim"),
        ),
        // The clause reads in full as the room's Left card; the roster keeps
        // only the fact, with the clause in its title.
        ParticipantReason::Left {
            publication,
            clause,
        } => (
            "left",
            "Left",
            format!(
                "Left: {clause}\nLeft at {publication}; only native work and duties keep this lane listening"
            ),
        ),
    };
    Some(reason_chip(kind, label, &source))
}

/// Compact visible score beside its reach, plus complete audit provenance for
/// the chip title.
fn invitation_match(matching: &wire::rooms::DesignNoticeMatch) -> (String, String) {
    let reach = matching.reach();
    (
        format!(
            "Asked · {reach} {score:.3}/{floor:.3}",
            score = matching.score(),
            floor = matching.floor(),
        ),
        format!(
            "{reach} match score {score}, floor {floor}, fit {generation}",
            score = matching.score(),
            floor = matching.floor(),
            generation = matching.generation().as_str(),
        ),
    )
}

/// The questions a Design request asked, in asked order; nothing for a
/// whole-Design ask.
fn asked_questions(questions: &[wire::rooms::StatementItem]) -> String {
    if questions.is_empty() {
        return String::new();
    }
    let listed = questions
        .iter()
        .map(|item| item.handle(questions))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        " on question{} {listed}",
        if questions.len() == 1 { "" } else { "s" }
    )
}

fn reason_chip(kind: &'static str, label: &str, source: &str) -> Html {
    html! {
        <span
            class="room-reason"
            data-reason={kind}
            title={source.to_owned()}
            aria-label={format!("{label}: {source}")}
        >{label.to_owned()}</span>
    }
}

fn task_label(runtime: &AppRuntime, task: &TaskStamp) -> String {
    runtime
        .board()
        .and_then(|board| {
            board
                .tasks
                .iter()
                .find(|entry| entry.statement.body.born == *task)
        })
        .map_or_else(
            || task.to_string(),
            |entry| entry.statement.body.handle.to_string(),
        )
}
