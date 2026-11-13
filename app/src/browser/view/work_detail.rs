//! One opened task read in place inside its Work list row: its title and
//! meta line, then its conversation or an expanded view beside its compact
//! facts, then the links line that switches the view.

mod meta;
mod views;

use super::super::clock::{TaskTimePresentation, task_time};
use super::super::watchband::lane_composer_href;
use super::*;
use crate::composer_markdown::inline::LineControls;
use crate::runtime::LaneView;
use crate::work::{
    TaskOverviewBase, TaskOverviewSelection, UNTIL_WOKEN, WorkProjection, WorkScope, WorkTask,
};
pub(super) use views::TaskViews;
use wire::TaskStamp;
pub(super) struct WorkDetailRender<'a, 'work> {
    pub work: &'a WorkProjection<'work>,
    pub lanes: Option<&'a LaneView>,
    pub navigate: &'a Callback<navigation::Intent>,
    pub creator_messages: &'a [Rc<wire::AgentMessage>],
    pub selection: &'a TaskOverviewSelection,
    pub runtime: Option<&'a AppRuntime>,
    pub room_controls: &'a super::super::rooms::Controls,
    pub sender: &'a SenderHandle,
    pub draft_controls: &'a super::super::drafts::Controls,
    pub connected: bool,
    pub can_note: bool,
    pub can_close: bool,
    pub can_conclude: bool,
    pub notes: &'a NoteComposers,
    pub submit_note: Callback<(TaskStamp, SubmitEvent)>,
    pub decide_trust: Callback<task_detail::TrustDecisionRequest>,
    pub request_task: Callback<TaskStamp>,
    /// Opens a task's sheet from a card, returning focus to the activating element.
    pub open_message_task: Callback<(TaskStamp, web_sys::Element)>,
}

/// One opened task: its header, then its body. `focus_task` opens a related
/// task listed in the same selection; `views` is the view its links line
/// expanded.
pub(super) fn render_task(
    render: &WorkDetailRender<'_, '_>,
    stamp: &TaskStamp,
    focus_task: &Callback<TaskStamp>,
    views: &TaskViews,
) -> Html {
    let detail = render
        .runtime
        .and_then(|runtime| runtime.task_detail(stamp));
    let body = render_detail_body(render, stamp, detail, views);
    let decision = render.runtime.and_then(|runtime| {
        super::super::rooms::render_pending_decision(
            runtime,
            render.room_controls,
            render.sender,
            &render.open_message_task,
            stamp,
            body.clone(),
        )
    });
    html! {
        <>
            {meta::render_header(render, stamp, detail, focus_task)}
            {decision.unwrap_or(body)}
        </>
    }
}

/// The base of the team, Design namespace or Drafts the overview shows.
pub(super) fn base(render: &WorkDetailRender<'_, '_>) -> Option<sidebar::Base> {
    match render.work.overview_base(render.selection) {
        TaskOverviewBase::Page => None,
        TaskOverviewBase::Design => Some(sidebar::Base::Accent(crate::watchband::DESIGN_BASE)),
        TaskOverviewBase::Drafts => Some(sidebar::Base::Neutral),
        TaskOverviewBase::Team(team) => render
            .lanes?
            .lanes
            .iter()
            .find(|row| row.lane == team)
            .map(|row| sidebar::Base::Accent(row.accent(0))),
    }
}

/// The selection's heading, naming a team from its lanes.
pub(super) fn context_label(
    selection: &TaskOverviewSelection,
    runtime: Option<&AppRuntime>,
) -> String {
    let lanes = runtime
        .and_then(AppRuntime::lanes)
        .map_or(&[][..], |view| &view.lanes);
    selection
        .context()
        .label(|team| crate::watchband::team_label_in(lanes, team))
}

fn render_detail_body(
    render: &WorkDetailRender<'_, '_>,
    selected: &TaskStamp,
    detail: Option<&TaskDetailState>,
    views: &TaskViews,
) -> Html {
    match detail {
        None | Some(TaskDetailState::Loading) => html! {
            <div class="work-detail-placeholder" aria-busy="true"></div>
        },
        Some(TaskDetailState::Unavailable { reason, detail }) => {
            let retry = {
                let request = render.request_task.clone();
                let task = selected.clone();
                Callback::from(move |_| request.emit(task.clone()))
            };
            let problem = detail.as_ref().map_or_else(
                || format!("Task detail unavailable: {reason}"),
                |detail| format!("Task detail unavailable: {reason}. {detail}"),
            );
            html! {
                <div class="work-detail-placeholder">
                    <crate::browser::toast::Notice text={Some(AttrValue::from(problem))} />
                    <button type="button" class="app-compact-control" onclick={retry}>{"Retry detail"}</button>
                </div>
            }
        }
        Some(TaskDetailState::Available(answer) | TaskDetailState::Refreshing(answer)) => {
            views::render_available(render, answer, views)
        }
    }
}

/// Every scalar fact about the task, one `<div>` each: state, lease, origin,
/// claim context, and repository boundary.
fn facts(render: &WorkDetailRender<'_, '_>, answer: &wire::intent::TaskDetailAnswer) -> Vec<Html> {
    let row = &answer.task;
    let statement = &row.statement.body;
    let workflow = &row.workflow.body;
    let completion = workflow.status.completion();
    let mut facts = vec![
        topic_fact(render, &statement.topic),
        fact("Status", row.status_word().to_owned()),
    ];
    facts.extend(
        statement
            .priority
            .map(|priority| fact("Priority", priority.to_string())),
    );
    facts.extend(
        statement
            .taste
            .as_ref()
            .map(|hold| fact("Taste review", taste_review_text(hold))),
    );
    facts.extend([
        super::super::deferral::fact(
            render.runtime,
            render.sender,
            row,
            deadline_value(workflow.deferral.as_ref()),
        ),
        instant_fact("Completion", completion.as_ref()),
        fact("Round", round_text(row)),
    ]);
    if row.claims.is_empty() {
        facts.push(fact("Claim fact", ABSENT.to_owned()));
    }
    for held in &row.claims {
        let lease = held.lease.as_ref();
        facts.extend([
            fact("Claim fact", sequence(held.claim.position)),
            instant_fact("Lease start", lease.map(|lease| &lease.body.start)),
            html_fact(
                "Lease expiry",
                deadline_value(lease.map(|lease| &lease.body.expiry)),
            ),
        ]);
    }
    facts.extend(invitation_lines(row, render));
    facts.extend([
        origin_fact(render, &statement.provenance),
        fact("Origin thread", row.origin_thread.clone()),
        creator_team_fact(row.creator_team, render),
        fact("Topic fact", sequence(statement.topic_origin)),
        fact("Statement revision", sequence(statement.revision)),
        fact("Workflow fact", sequence(row.workflow.position)),
    ]);
    claim_context_facts(&mut facts, row, render);
    git_context_facts(&mut facts, row);
    facts
}

/// The current round in one line: its phase's participant target and the
/// participants that already arrived.
fn round_text(row: &wire::intent::BoardTask) -> String {
    let completion = &row.completion.body;
    format!(
        "target {} · arrived {}",
        completion.target(&row.workflow.body.phase),
        completion.arrivals.len()
    )
}

/// One claim and its lease per claim the task retains, holder first.
fn claim_lines(row: &wire::intent::BoardTask, render: &WorkDetailRender<'_, '_>) -> Vec<Html> {
    if row.claims.is_empty() {
        return vec![html_fact("Claim", html! { ABSENT })];
    }
    row.claims
        .iter()
        .flat_map(|held| {
            let claim = &held.claim;
            let lease = held.lease.as_ref().map_or_else(
                || html! { ABSENT },
                |lease| {
                    html! {
                        <>
                            {task_time(&lease.body.start, TaskTimePresentation::Elapsed)}
                            {" → "}
                            {task_time(&lease.body.expiry, TaskTimePresentation::Remaining)}
                        </>
                    }
                },
            );
            [
                html_fact(
                    "Claim",
                    html! { <>{sequence(claim.position)}{" · "}{actor_link(&claim.body.actor, render)}</> },
                ),
                html_fact("Lease", lease),
            ]
        })
        .collect()
}

/// One row per invitation into the task, in publication order: its key,
/// inviter and recipient, bound phase, and state. A live one names the two
/// answers open to the recipient and when it lapses on its own; a joined one
/// has its partner claim on the claim lines above.
fn invitation_lines(row: &wire::intent::BoardTask, render: &WorkDetailRender<'_, '_>) -> Vec<Html> {
    row.invitations
        .iter()
        .map(|invitation| {
            let state = invitation.state.as_str();
            let live = (invitation.state == wire::InvitationState::Invited).then(|| {
                html! {
                    <>
                        {" · ACK joins · NAK declines · lapses "}
                        {task_time(&invitation.expires, TaskTimePresentation::Remaining)}
                    </>
                }
            });
            html! {
                <div>
                    <dt>{"Invitation"}</dt>
                    <dd data-invitation-key={invitation.key.clone()} data-invitation-state={state}>
                        {invitation.key.clone()}
                        {" · "}
                        {actor_link(&wire::Actor::Lane(invitation.inviter), render)}
                        {" → "}
                        {actor_link(&wire::Actor::Lane(invitation.recipient), render)}
                        {" · "}
                        {invitation.phase.to_string()}
                        {" · "}
                        {state}
                        {live}
                    </dd>
                </div>
            }
        })
        .collect()
}

/// The facts beside the conversation, one short line each: topic, status,
/// deferral with its controls, claim, lease, origin, creator, and repository.
fn compact_facts(
    render: &WorkDetailRender<'_, '_>,
    answer: &wire::intent::TaskDetailAnswer,
) -> Vec<Html> {
    let row = &answer.task;
    let statement = &row.statement.body;
    let workflow = &row.workflow.body;
    let claims = claim_lines(row, render);
    let messages = render.creator_messages.iter().map(AsRef::as_ref);
    let creator = crate::view::creator_context_href(&statement.creation, messages).map_or_else(
        || creator_team(row.creator_team, render),
        |href| task_detail::creator_context_link("work-context-link", href, statement, None),
    );
    let repo = row.git_context.as_ref().map_or_else(
        || ABSENT.to_owned(),
        |context| {
            format!(
                "{} · {} local",
                context.boundary.head, context.boundary.local_commits
            )
        },
    );
    let mut facts = vec![
        topic_fact(render, &statement.topic),
        fact("Status", row.status_word().to_owned()),
        super::super::deferral::fact(
            render.runtime,
            render.sender,
            row,
            deadline_value(workflow.deferral.as_ref()),
        ),
        fact("Round", round_text(row)),
    ];
    facts.extend(claims);
    facts.extend(invitation_lines(row, render));
    facts.extend([
        fact(
            "Topic",
            format!(
                "{} · rev {}",
                sequence(statement.topic_origin),
                sequence(statement.revision)
            ),
        ),
        origin_fact(render, &statement.provenance),
        html_fact("Creator", creator),
        fact("Repo", repo),
    ]);
    facts
}

/// The settlement and disposition records over the one composer that
/// publishes into the task's room, so a submitted observation lands directly
/// above the conversation it joins. Finished work, a settled task or a
/// concluded Design, reads its records with no composer that could write to it.
fn render_composer(
    render: &WorkDetailRender<'_, '_>,
    answer: &wire::intent::TaskDetailAnswer,
) -> Html {
    let statement = &answer.task.statement.body;
    let live = answer.task.workflow.body.status.is_pending();
    let note_id = format!("work-detail-note-{}", statement.born);
    html! {
        <>
            {task_detail::render_settlement_record(
                crate::work::SettlementRecord::of(answer),
                |actor| actor_name(actor, render),
                |task| selection_has(render.selection, task),
            )}
            {task_detail::render_disposition_record(
                answer.disposition.as_ref(),
                |actor| actor_name(actor, render),
                |task| {
                    meta::projected(render.work, task).map_or_else(
                        || task.to_string(),
                        |row| row.row().statement.body.handle.to_string(),
                    )
                },
                |task| selection_has(render.selection, task),
            )}
            if live {
                {task_detail::render_note_composer(
                    note_id.clone(),
                    statement.handle.as_str(),
                    &statement.born,
                    task_detail::NoteAccess {
                        connected: render.connected,
                        can_note: render.can_note,
                        closure: task_detail::closure(
                            render.can_close,
                            render.can_conclude,
                            &answer.task,
                        )
                        .as_ref(),
                    },
                    &render.notes.shown(&statement.born),
                    task_detail::NoteCallbacks {
                        input: render.notes.draft_input(&statement.born),
                        submit: &render.submit_note,
                        open: render.notes.opener(&statement.born, note_id),
                    },
                )}
            }
        </>

    }
}

/// What a fact reads when its value is absent.
const ABSENT: &str = "—";

fn fact(label: &'static str, value: String) -> Html {
    html! { <div><dt>{label}</dt><dd>{value}</dd></div> }
}

fn html_fact(label: &'static str, value: Html) -> Html {
    html! { <div><dt>{label}</dt><dd>{value}</dd></div> }
}

/// Where each taste held the statement, with the matched text the operator
/// reads beside it.
fn taste_review_text(hold: &wire::TasteHold) -> String {
    hold.matches
        .iter()
        .map(|found| {
            let place = format!(
                "{} line {}: {}",
                found.field.as_str(),
                found.line,
                found.taste
            );
            match &found.finding {
                Some(finding) => format!("{place} ({:?})", finding.trigger),
                None => place,
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The task's origin key; the request a steering origin names leads its room
/// instead. A parent task's origin opens that task's sheet; no other origin
/// has one.
fn origin_fact(render: &WorkDetailRender<'_, '_>, origin: &wire::TaskProvenance) -> Html {
    let value = match origin {
        wire::TaskProvenance::Task(parent) => {
            let navigate = render.navigate.clone();
            let parent = parent.clone();
            html! {
                <button
                    type="button"
                    class="app-fact-link"
                    aria-label={format!("Open origin task {parent}")}
                    onclick={Callback::from(move |_| {
                        navigate.emit(navigation::Intent::Task(parent.clone()));
                    })}
                >{origin.to_string()}</button>
            }
        }
        _ => html! { {origin.to_string()} },
    };
    html_fact("Origin", value)
}

/// The topic fact: the exact topic opening its Work scope, or `Unplaced`
/// with any stored relative tail opening the unplaced group.
fn topic_fact(render: &WorkDetailRender<'_, '_>, topic: &wire::TaskTopic) -> Html {
    let navigate = render.navigate.clone();
    let (scope, topic_label) = match &topic.path {
        Some(path) => (WorkScope::Topic(path.to_string()), path.to_string()),
        None => (
            WorkScope::Unplaced,
            topic.tail().map_or_else(
                || "Unplaced".to_owned(),
                |tail| format!("Unplaced · {}", tail.as_str()),
            ),
        ),
    };
    html! {
        <div>
            <dt>{"Topic"}</dt>
            <dd title={topic_label.clone()}>
                <button
                    type="button"
                    class="app-fact-link"
                    data-task-topic-link={topic_label.clone()}
                    aria-label={format!("Show {topic_label} in task overview")}
                    onclick={Callback::from(move |_| {
                        navigate.emit(navigation::Intent::Work(scope.clone()));
                    })}
                >{topic_label}</button>
            </dd>
        </div>
    }
}

fn creator_team_fact(team: Option<wire::CanonicalUuid>, render: &WorkDetailRender<'_, '_>) -> Html {
    html_fact("Creator team", creator_team(team, render))
}

/// The creator team by the name the reader knows, linked to the Chat
/// composer that reaches it.
fn creator_team(team: Option<wire::CanonicalUuid>, render: &WorkDetailRender<'_, '_>) -> Html {
    let Some(team) = team else {
        return html! { ABSENT };
    };
    let lanes = render.lanes.map_or(&[][..], |view| &view.lanes);
    let href = render
        .lanes
        .and_then(|view| view.chat_target_for_team(team))
        .and_then(|(_team, member)| lane_composer_href(render.lanes, member));
    identity_link(
        crate::watchband::team_label_in(lanes, team),
        team.to_string(),
        href,
    )
}

/// A name the reader knows, its durable spelling on hover, linked to the Chat
/// composer that reaches it when one exists.
fn identity_link(display: String, identity: String, href: Option<String>) -> Html {
    let Some(href) = href else {
        return html! { <span title={identity}>{display}</span> };
    };
    html! {
        <a
            class="app-fact-link"
            title={identity}
            {href}
            aria-label={format!("Open {display} in Chat")}
        >{display}</a>
    }
}

fn instant_fact(label: &'static str, instant: Option<&wire::Instant>) -> Html {
    html_fact(
        label,
        instant.map_or_else(
            || html! { ABSENT },
            |instant| task_time(instant, TaskTimePresentation::Elapsed),
        ),
    )
}

/// A deferral's time: the time remaining, or the hold's own words at the
/// indefinite horizon, where no time wakes it.
fn deadline_value(instant: Option<&wire::Instant>) -> Html {
    match instant {
        None => html! { ABSENT },
        Some(instant) if instant.is_indefinite() => html! { UNTIL_WOKEN },
        Some(instant) => task_time(instant, TaskTimePresentation::Remaining),
    }
}

/// One actor by the name the reader knows, linked to its lane's Chat composer.
fn actor_link(actor: &wire::Actor, render: &WorkDetailRender<'_, '_>) -> Html {
    let href = match actor {
        wire::Actor::Lane(lane) => lane_composer_href(render.lanes, *lane),
        wire::Actor::Spice | wire::Actor::Principal(_) => None,
    };
    identity_link(actor_name(actor, render), actor.to_string(), href)
}

pub(super) fn actor_name(actor: &wire::Actor, render: &WorkDetailRender<'_, '_>) -> String {
    let member = match actor {
        wire::Actor::Lane(lane) => render.lanes.and_then(|lanes| lanes.member(*lane)),
        wire::Actor::Spice | wire::Actor::Principal(_) => None,
    };
    crate::work::owner_label(
        actor,
        member,
        render.runtime.and_then(AppRuntime::principals),
    )
}

fn selection_has(selection: &TaskOverviewSelection, born: &TaskStamp) -> bool {
    selection.tasks().contains(born)
}

/// Where the claim was won: its fact, its lane by name, that lane's occupant
/// and resident generation relative to the current ones, and its transcript.
fn claim_context_facts(
    facts: &mut Vec<Html>,
    row: &wire::intent::BoardTask,
    render: &WorkDetailRender<'_, '_>,
) {
    let Some(context) = &row.claim_context else {
        return;
    };
    let recovery = match &context.recovery {
        wire::intent::TaskTranscriptRecovery::Transcript { session } => {
            format!("Transcript session {session}")
        }
        wire::intent::TaskTranscriptRecovery::Unavailable { reason } => {
            format!("Transcript unavailable: {reason}")
        }
    };
    let lane = wire::Actor::Lane(context.lane);
    let seated = render
        .lanes
        .and_then(|lanes| lanes.member(context.lane))
        .map(|member| member.occupant == Some(context.occupant));
    let occupant = match seated {
        Some(true) => format!("{}, seated now", actor_name(&lane, render)),
        Some(false) => format!("{}, an earlier occupant", actor_name(&lane, render)),
        None => format!("{} occupant", actor_name(&lane, render)),
    };
    let epoch = match render.lanes.map(|lanes| lanes.epoch == context.epoch) {
        Some(true) => "Current resident".to_owned(),
        Some(false) => "An earlier resident".to_owned(),
        None => format!("Resident {}", crate::watchband::short(context.epoch)),
    };
    facts.extend([
        fact("Claimed fact", sequence(context.claimed)),
        html_fact("Claim lane", actor_link(&lane, render)),
        html_fact(
            "Occupant",
            html! { <span title={context.occupant.to_string()}>{occupant}</span> },
        ),
        html_fact(
            "Resident epoch",
            html! { <span title={context.epoch.to_string()}>{epoch}</span> },
        ),
        fact("Recovery", recovery),
    ]);
}

fn git_context_facts(facts: &mut Vec<Html>, row: &wire::intent::BoardTask) {
    let Some(context) = &row.git_context else {
        return;
    };
    facts.extend([
        fact("Repository lifecycle", context.kind.as_str().to_owned()),
        fact("Repository fact", sequence(context.fact)),
        fact("Head", context.boundary.head.clone()),
        fact("Integrated", context.boundary.integrated.clone()),
        fact(
            "Upstream",
            context
                .boundary
                .upstream
                .clone()
                .unwrap_or_else(|| ABSENT.to_owned()),
        ),
        fact("Local commits", context.boundary.local_commits.to_string()),
    ]);
}

/// A journal position in the condensed base52 spelling every surface reads.
fn sequence(position: wire::Seq) -> String {
    wire::SequenceCode::new(position.get()).to_string()
}
