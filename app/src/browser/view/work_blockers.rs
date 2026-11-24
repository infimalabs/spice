//! What an opened row waiting on the operator asks of them: each unmet
//! environmental requirement, with its trust grant when the board offers one,
//! the wall only they can move, and the work it holds back.

use super::*;
use crate::runtime::BlockerActionState;
use crate::work::{DependencyWall, OperatorWait, WorkProjection};

pub(super) fn render_operator_act(
    work: &WorkProjection<'_>,
    wait: &OperatorWait<'_>,
    runtime: Option<&AppRuntime>,
    decide: &Callback<task_detail::TrustDecisionRequest>,
    sender: &SenderHandle,
) -> Html {
    let can_decide = runtime.is_some_and(|runtime| {
        runtime.offers(wire::CommandKind::TrustGrant)
            && runtime.offers(wire::CommandKind::TrustRevoke)
    });
    let trust = TrustControls {
        runtime,
        can_decide,
        can_show: runtime.is_some_and(|runtime| runtime.offers(wire::CommandKind::TrustShow)),
        decide,
        show: {
            let sender = sender.clone();
            Callback::from(move |action| admit_action(&sender, action))
        },
    };
    let board = runtime.and_then(AppRuntime::board);
    let tasks = work.tasks();
    let statement = &tasks[wait.task()].row().statement.body;
    let requirements = wait.environment().then(|| {
        statement
            .requirements
            .as_slice()
            .iter()
            .filter(|requirement| requirement_blocked(board, requirement))
    });
    let missing_resources = wait.environment().then(|| {
        statement.resources.iter().filter(|(name, _)| {
            board.is_none_or(|board| board.resource_pool_available(name).is_none())
        })
    });
    let holds = wait
        .holds()
        .iter()
        .map(|&index| tasks[index].row().statement.body.handle.to_string())
        .collect::<Vec<_>>();
    html! {
        <section class="work-act" aria-label="Waiting on you">
            <div class="work-blocker-list">
                {for requirements.into_iter().flatten()
                    .map(|requirement| render_requirement(statement, requirement, &trust))}
                {for missing_resources.into_iter().flatten()
                    .map(|(name, shards)| render_resource_requirement(name, shards))}
                if let Some(wall) = wait.wall() {
                    {render_wall(wall)}
                }
            </div>
            if !holds.is_empty() {
                <p class="work-blocker-holds">
                    {format!("Holds back {} {}: {}", holds.len(), if holds.len() == 1 { "task" } else { "tasks" }, holds.join(", "))}
                </p>
            }
        </section>
    }
}

fn requirement_blocked(
    board: Option<&crate::runtime::BoardView>,
    requirement: &wire::EnvironmentRequirement,
) -> bool {
    match requirement {
        wire::EnvironmentRequirement::ResourcePool { name, .. } => {
            board.is_none_or(|board| board.resource_pool_available(name).is_none())
        }
        _ => board.is_none_or(|board| !board.requirement_satisfied(requirement)),
    }
}

/// Say why a waiting task stands still and why only the operator can move it.
/// Its environment is explained by its requirement cards instead.
fn render_wall(wall: DependencyWall<'_>) -> Html {
    let (kind, title, reason, state) = match wall {
        DependencyWall::Environment(_) => return Html::default(),
        DependencyWall::Gate(_) => (
            "blocked",
            "Blocked",
            "Its prerequisite or controller cannot advance. Change the task or restore its missing authority.",
            "Operator required",
        ),
        DependencyWall::Deferred(_) => (
            "deferred",
            "Deferred",
            "No agent is offered it until it is woken.",
            "Deferred",
        ),
        DependencyWall::Parked(_) => (
            "parked",
            "Parked",
            "It is still a Draft; no agent is offered it until the operator creates it.",
            "Parked",
        ),
        DependencyWall::Cycle(_) => (
            "cycle",
            "Dependency cycle",
            "Its dependencies lead back to itself; no agent can finish the chain until they change.",
            "Cycle",
        ),
        DependencyWall::Unreachable(_) => (
            "unreachable",
            "Closed without completing",
            "It can never complete, so each dependency on it must change.",
            "Unreachable",
        ),
        DependencyWall::Selection(_) => (
            "selection",
            "Awaiting your decision",
            "Choose an outcome for each output.",
            "Operator required",
        ),
    };
    html! {
        <section class="work-blocker-card" data-blocker-kind={kind}>
            <div>
                <strong>{title}</strong>
                <p>{reason}</p>
            </div>
            <span class="work-blocker-state">{state}</span>
        </section>
    }
}

/// What a trust blocker needs to read its differential and rule on it.
struct TrustControls<'a> {
    runtime: Option<&'a AppRuntime>,
    can_decide: bool,
    can_show: bool,
    decide: &'a Callback<task_detail::TrustDecisionRequest>,
    show: Callback<ClientAction, bool>,
}

fn render_requirement(
    statement: &wire::intent::BoardStatement,
    requirement: &wire::EnvironmentRequirement,
    trust: &TrustControls<'_>,
) -> Html {
    match requirement {
        wire::EnvironmentRequirement::TrustGrant { value } => {
            render_trust_requirement(statement, value, trust)
        }
        wire::EnvironmentRequirement::JournalSchema { value } => html! {
            <section class="work-blocker-card" data-blocker-kind="journal-schema">
                <div>
                    <strong>{"Journal schema upgrade required"}</strong>
                    <p>{format!("Requires schema {value} or newer. Only the operator can upgrade the journal; upgrade actions are not available in the app yet.")}</p>
                </div>
                <span class="work-blocker-state">{"Upgrade required"}</span>
            </section>
        },
        wire::EnvironmentRequirement::ApplianceVersion { value } => html! {
            <section class="work-blocker-card" data-blocker-kind="appliance-version">
                <div>
                    <strong>{"Newer appliance required"}</strong>
                    <p>{format!("Requires a version later than {value}. Only the operator can install it; reload and upgrade actions are not available in the app yet.")}</p>
                </div>
                <span class="work-blocker-state">{"Upgrade required"}</span>
            </section>
        },
        wire::EnvironmentRequirement::ResourcePool { name, shards } => {
            render_resource_requirement(name, shards.get())
        }
    }
}

fn render_resource_requirement(name: &wire::ResourcePoolName, shards: u32) -> Html {
    html! {
        <section class="work-blocker-card" data-blocker-kind="resource-pool">
            <div>
                <strong>{"Resource pool not configured"}</strong>
                <p>{format!("Requires {shards} shard(s) from `{name}`. Configure that named lock pool before this task can be allocated.")}</p>
            </div>
            <span class="work-blocker-state">{"Configuration required"}</span>
        </section>
    }
}

fn render_trust_requirement(
    statement: &wire::intent::BoardStatement,
    digest: &wire::Digest,
    controls: &TrustControls<'_>,
) -> Html {
    use crate::client_action::TrustDecision;

    let TrustControls {
        runtime,
        can_decide,
        can_show,
        decide,
        show,
    } = controls;
    let (can_decide, decide) = (*can_decide, *decide);
    let action_state = runtime.and_then(|runtime| runtime.blocker_action(digest));
    let board = runtime.and_then(AppRuntime::board);
    let trust = board.and_then(|board| board.trust_head(digest));
    let (ready, explanation, unavailable_label) = match trust {
        None => (
            true,
            "Review this exact executable configuration. The resident will capture and verify its producing worktree when you decide.".to_owned(),
            "",
        ),
        Some(head) if head.state == wire::TrustState::Pended => (
            true,
            "Review this exact executable configuration and choose whether it may run. The decision rules on the recorded configuration, whatever any worktree holds now.".to_owned(),
            "",
        ),
        Some(head) if head.state == wire::TrustState::Revoked => (
            false,
            "This exact executable configuration was rejected and remains blocked.".to_owned(),
            "Rejected",
        ),
        Some(_) => (
            false,
            "This exact executable configuration is already approved; the blocker snapshot is refreshing.".to_owned(),
            "Approved",
        ),
    };
    let pending = matches!(action_state, Some(BlockerActionState::Pending(_)));
    let settled = matches!(action_state, Some(BlockerActionState::Settled(_)));
    let action_label = match action_state {
        Some(BlockerActionState::Pending(TrustDecision::Approve)) => "Approving…",
        Some(BlockerActionState::Pending(TrustDecision::Reject)) => "Rejecting…",
        Some(BlockerActionState::Settled(TrustDecision::Approve)) => "Approved",
        Some(BlockerActionState::Settled(TrustDecision::Reject)) => "Rejected",
        Some(BlockerActionState::Failed(TrustDecision::Approve, _)) => "Approval failed",
        Some(BlockerActionState::Failed(TrustDecision::Reject, _)) => "Rejection failed",
        None => "",
    };
    let request = |decision| task_detail::TrustDecisionRequest {
        task: statement.born.clone(),
        revision: statement.revision,
        digest: digest.clone(),
        decision,
    };
    let approve = {
        let decide = decide.clone();
        let request = request(TrustDecision::Approve);
        Callback::from(move |_| decide.emit(request.clone()))
    };
    let reject = {
        let decide = decide.clone();
        let request = request(TrustDecision::Reject);
        Callback::from(move |_| decide.emit(request.clone()))
    };
    html! {
        <section class="work-blocker-card work-blocker-card--action" data-blocker-kind="trust-decision">
            <div>
                <strong>{"Executable trust decision required"}</strong>
                <p>{explanation}</p>
                <code title={digest.to_string()}>{digest.to_string()}</code>
                if trust.is_some() && *can_show {
                    <TrustDiff
                        digest={digest.clone()}
                        granted={board.map(crate::runtime::BoardView::granted_trust).unwrap_or_default()}
                        on_action={show.clone()}
                    />
                }
                if let Some(BlockerActionState::Failed(_, issue)) = action_state {
                    <p class="work-blocker-issue" role="alert">{issue.clone()}</p>
                }
                <span class="visually-hidden" role="status">
                    {(pending || settled).then_some(action_label)}
                </span>
            </div>
            if ready {
                <div class="work-blocker-actions" aria-label="Executable trust decision">
                    <button
                        type="button"
                        class="app-compact-control"
                        disabled={!can_decide || pending || settled}
                        onclick={reject}
                    >{"Reject"}</button>
                    <button
                        type="button"
                        class="app-compact-control"
                        disabled={!can_decide || pending || settled}
                        onclick={approve}
                    >{"Approve"}</button>
                </div>
            } else {
                <span class="work-blocker-state">{unavailable_label}</span>
            }
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct TrustDiffProps {
    digest: wire::Digest,
    /// Every granted digest, so the differential is reread whenever the
    /// grant it compares against may have moved.
    granted: Vec<wire::Digest>,
    on_action: Callback<ClientAction, bool>,
}

/// The recorded configuration for one digest against the latest other grant,
/// reread when the digest or the set of grants changes.
#[function_component(TrustDiff)]
fn trust_diff(props: &TrustDiffProps) -> Html {
    let answer = use_state(|| None::<Result<wire::intent::TrustShowAnswer, String>>);
    {
        let answer = answer.clone();
        let on_action = props.on_action.clone();
        let read = (props.digest.clone(), props.granted.clone());
        use_effect_with(read, move |(digest, _)| {
            answer.set(None);
            let respond = {
                let answer = answer.clone();
                Rc::new(move |result| answer.set(Some(result)))
            };
            if !on_action.emit(ClientAction::TrustShow {
                digest: digest.clone(),
                respond,
            }) {
                answer.set(Some(Err(
                    "The configuration diff could not be requested.".to_owned()
                )));
            }
        });
    }
    let answer = match answer.as_ref() {
        None => return html! { <p class="trust-diff-status">{"Loading configuration…"}</p> },
        Some(Err(issue)) => {
            return html! { <p class="work-blocker-issue" role="alert">{issue.clone()}</p> };
        }
        Some(Ok(answer)) => answer,
    };
    let basis = match &answer.baseline {
        Some(baseline) if baseline.recorded => {
            format!(
                "Changes against approved {}",
                short_digest(&baseline.digest)
            )
        }
        Some(baseline) => format!(
            "Full text; approved {} recorded no text to compare",
            short_digest(&baseline.digest)
        ),
        None => "Full text; no other configuration is approved".to_owned(),
    };
    html! {
        <div class="trust-diff" aria-label="Configuration changes">
            <p class="trust-diff-status">{basis}</p>
            if let Some(documents) = &answer.documents {
                {for documents.iter().map(render_trust_document)}
            } else {
                <p class="trust-diff-status">{"This request recorded no configuration text."}</p>
            }
        </div>
    }
}

fn render_trust_document(document: &wire::intent::TrustDocumentDiff) -> Html {
    use wire::intent::TrustDiffLine;

    let line = |change, marker, text: &str, ending: wire::intent::TrustLineEnding| {
        html! {
            <span data-change={change}>
                {format!("{marker}{text}")}
                if let Some(note) = ending.note() {
                    <em class="trust-diff-ending">{format!(" ⏎ {note}")}</em>
                }
            </span>
        }
    };
    html! {
        <figure class="trust-diff-document">
            <figcaption>
                {document.source.as_str()}
                if !document.present {
                    {" (absent)"}
                }
            </figcaption>
            if !document.lines.is_empty() {
                <pre>
                    {for document.lines.iter().map(|change| match change {
                        TrustDiffLine::Kept { text, ending } => line("kept", ' ', text, *ending),
                        TrustDiffLine::Added { text, ending } => line("added", '+', text, *ending),
                        TrustDiffLine::Removed { text, ending } => {
                            line("removed", '-', text, *ending)
                        }
                        TrustDiffLine::Elided { count } => html! {
                            <span data-change="elided">{format!("⋯ {count} unchanged")}</span>
                        },
                    })}
                </pre>
            }
        </figure>
    }
}

fn short_digest(digest: &wire::Digest) -> String {
    digest.to_string().chars().take(19).collect()
}
