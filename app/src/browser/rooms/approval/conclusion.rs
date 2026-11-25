//! A completed pass that offers no output: the operator concludes the
//! Design done, collapses it, or returns the pass, inside the opened Work row.

use super::*;

pub(super) fn render(context: &Context<'_>, detail: Html) -> Html {
    let (design, pass) = (context.design, context.pass);
    let empty = DecisionDraft::new(design.clone(), pass);
    let composer = &context.controls.decision.0;
    let draft = composer.draft(design, pass).unwrap_or(&empty);
    let waiting = draft.is_pending() || matches!(draft.notice(), Some(DraftNotice::Recorded(_)));
    let locked = composer.is_pending() || waiting || !context.interactive();
    let reasoned = !draft.comment().trim().is_empty();
    let send = |choice| send_prepared(context, &prepare_submission(context, draft, choice));
    let fact = SequenceCode::new(pass.get()).to_string();
    html! {
        <section
            class="work-decision work-conclusion"
            data-pass={fact.clone()}
            aria-label="Pass decision"
        >
            <header class="work-decision-heading">
                <strong>{"Pass "}<code>{fact.clone()}</code></strong>
                <span data-decision-tally="">{"no offered output · conclude, collapse, or return"}</span>
            </header>
            {render_term_nets(context)}
            <p class="work-decision-summary">
                {"Done keeps accepted work released, closes the Design and its room, and leaves its history readable. Collapse records why the premise failed."}
            </p>
            <div class="work-conclusion-evidence">
                {render_conclusion_evidence(context)}
                {render_collapse_evidence(context)}
            </div>
            {render_witnesses(context)}
            <label class="work-decision-comment">
                <span>{"Reason"}</span>
                <textarea
                    rows="2"
                    value={draft.comment().to_owned()}
                    oninput={draft_text(context, DecisionDraft::set_comment)}
                    disabled={locked}
                    placeholder="Required to return or collapse. Concluding carries no comment."
                />
            </label>
            if waiting {
                <p class="work-decision-sending" role="status" aria-busy="true" data-decision-pending="">
                    {format!("Sending your decision on {fact}… the row moves when the board shows it.")}
                </p>
            }
            {render_notice(draft.notice())}
            <div class="work-decision-actions">
                <button
                    type="button"
                    class="app-compact-control"
                    onclick={send(Choice::Return)}
                    disabled={locked || !reasoned}
                >
                    {"Return to the room"}
                </button>
                <button
                    type="button"
                    class="app-compact-control"
                    onclick={send(Choice::Collapse)}
                    disabled={locked || !reasoned}
                >
                    {"Collapse the Design"}
                </button>
                <button
                    type="button"
                    class="app-compact-control work-decision-verdict"
                    onclick={send(Choice::Conclude)}
                    disabled={locked || reasoned}
                >
                    {if waiting { "Sending…" } else { "Conclude as done" }}
                </button>
            </div>
            <div class="work-decision-links">
                <details class="work-decision-context">
                    <summary>{"Room · questions · description · facts"}</summary>
                    {detail}
                </details>
            </div>
        </section>
    }
}

fn render_conclusion_evidence(context: &Context<'_>) -> Html {
    let mut rows = Vec::new();
    for decision in context.preparation.decisions.values().filter(|decision| {
        decision.value.decision.controller == *context.design
            && matches!(decision.value.outcome, DecisionOutcome::Resolved { .. })
    }) {
        let DecisionOutcome::Resolved { outputs, comment } = &decision.value.outcome else {
            continue;
        };
        let accepted = outputs
            .iter()
            .filter(|output| output.verdict == OutputVerdict::Accepted)
            .collect::<Vec<_>>();
        if accepted.is_empty() {
            continue;
        }
        let decision_fact = SequenceCode::new(decision.position.get()).to_string();
        rows.push(html! {
            <li class="work-conclusion-evidence-row">
                <code>{format!("decision {}", decision.value.decision.decision)}</code>
                <span>{comment.as_deref().unwrap_or("Accepted without an overall comment")}</span>
                <span class="work-conclusion-standing">{format!("accepted · decision {decision_fact}")}</span>
            </li>
        });
        for output in accepted {
            rows.push(render_released_output(context, &output.output));
        }
    }
    if rows.is_empty() {
        rows.push(html! {
            <li class="work-conclusion-evidence-row">
                <span>{"No released output remains to settle."}</span>
            </li>
        });
    }
    html! {
        <>
            <strong>{"What this conclusion rests on"}</strong>
            <ol class="work-conclusion-evidence-list">{for rows}</ol>
        </>
    }
}

fn render_released_output(context: &Context<'_>, output: &TaskStamp) -> Html {
    let task = match history::task_evidence(
        context.runtime.board(),
        context.runtime.task_detail(output),
        output,
    ) {
        history::TaskEvidence::Available(task) => task,
        history::TaskEvidence::Loading => {
            return render_evidence_notice(
                output,
                "loading",
                "Loading released output evidence…".to_owned(),
            );
        }
        history::TaskEvidence::Missing => {
            return render_evidence_notice(
                output,
                "missing",
                "Released output does not exist.".to_owned(),
            );
        }
        history::TaskEvidence::Refused { reason, detail } => {
            let message = detail.map_or_else(
                || format!("Released output evidence refused: {reason}."),
                |detail| format!("Released output evidence refused: {reason}: {detail}"),
            );
            return render_evidence_notice(output, "refused", message);
        }
    };
    let boundary = task.git_context.as_ref().map(|git| {
        let head = short_oid(&git.boundary.head);
        let integrated = short_oid(&git.boundary.integrated);
        if head == integrated {
            head
        } else {
            format!("{head} → {integrated}")
        }
    });

    html! {
        <li class="work-conclusion-evidence-row">
            {task_handle_link(context.open_task, output, &task.statement.body.handle)}
            <span>
                {task.statement.body.pitch.clone()}
                if let Some(boundary) = boundary {
                    <code>{format!(" · {boundary}")}</code>
                }
                {render_review(context, output)}
            </span>
            <span class="work-conclusion-standing">{task.workflow.body.status.to_string()}</span>
        </li>
    }
}

fn render_review(context: &Context<'_>, output: &TaskStamp) -> Html {
    use crate::runtime::TaskDetailState;
    use wire::body::{TaskCompletionStatus, Verdict};

    let answer = match context.runtime.task_detail(output) {
        Some(TaskDetailState::Available(answer) | TaskDetailState::Refreshing(answer)) => answer,
        Some(TaskDetailState::Unavailable { reason, detail }) => {
            return html! {
                <span class="work-conclusion-review" data-review-state="unavailable">
                    {format!("Review evidence unavailable: {reason}")}
                    if let Some(detail) = detail {
                        {format!(" · {detail}")}
                    }
                </span>
            };
        }
        Some(TaskDetailState::Loading) | None => {
            return html! { <span class="work-conclusion-review" data-review-state="loading">{"Loading review evidence…"}</span> };
        }
    };
    let Some(review) = &answer.review else {
        return html! { <span class="work-conclusion-review" data-review-state="absent">{"No recorded review."}</span> };
    };
    let (state, label, verdict) = match (review.verdict, review.status) {
        (Verdict::Clean, TaskCompletionStatus::Completed) => {
            ("final-clean", "final review ", "clean")
        }
        (Verdict::Clean, TaskCompletionStatus::Pending) => {
            ("intermediate-clean", "intermediate review ", "clean")
        }
        (Verdict::Finding, _) => ("finding", "review ", "finding"),
    };
    let reviewer = crate::work::principal_label(&review.reviewer, context.runtime.principals());
    html! {
        <span class="work-conclusion-review" data-review-state={state}>
            {label}
            <code>{SequenceCode::new(review.position.get()).to_string()}</code>
            {format!(" · {verdict} · {} · by {reviewer}", review.status)}
            if let Some(text) = &review.text {
                <span class="work-conclusion-review-note">{text.clone()}</span>
            }
        </span>
    }
}

fn render_evidence_notice(output: &TaskStamp, state: &'static str, message: String) -> Html {
    html! {
        <li class="work-conclusion-evidence-row" data-evidence-state={state}>
            <code>{output.to_string()}</code>
            <span>{message}</span>
        </li>
    }
}

fn render_collapse_evidence(context: &Context<'_>) -> Html {
    let controlled = context.controller.controlled.as_slice();
    html! {
        <>
            <strong>{"What collapse leaves undone"}</strong>
            if controlled.is_empty() {
                <p class="work-conclusion-empty">{"No pending controlled task remains."}</p>
            } else {
                <ol class="work-conclusion-evidence-list">
                    {for controlled.iter().map(|handle| {
                        let task = context.tasks.iter().find(|task| task.statement.body.handle == *handle);
                        html! {
                            <li class="work-conclusion-evidence-row">
                                <code>{handle.to_string()}</code>
                                <span>{task.map_or("Pending controlled work", |task| task.statement.body.pitch.as_str())}</span>
                                <span class="work-conclusion-standing">{"left undone"}</span>
                            </li>
                        }
                    })}
                </ol>
            }
        </>
    }
}

fn render_witnesses(context: &Context<'_>) -> Html {
    let controller = context.controller;
    let claim = controller
        .latest_claim()
        .map(|held| &held.claim)
        .map_or_else(
            || "none".to_owned(),
            |claim| {
                let disposition = match claim.body.disposition {
                    wire::ClaimDisposition::Active => "active",
                    wire::ClaimDisposition::Freed => "freed",
                    wire::ClaimDisposition::Lapsed => "lapsed",
                };
                format!(
                    "{} · {disposition}",
                    SequenceCode::new(claim.position.get())
                )
            },
        );
    html! {
        <aside class="work-conclusion-witnesses" aria-label="What the decision checks">
            <strong>{"Decision witnesses"}</strong>
            <dl class="work-facts work-conclusion-witness-facts">
                <div>
                    <dt>{"statement"}</dt>
                    <dd><code>{SequenceCode::new(controller.statement.position.get()).to_string()}</code></dd>
                </div>
                <div>
                    <dt>{"workflow"}</dt>
                    <dd><code>{SequenceCode::new(controller.workflow.position.get()).to_string()}</code></dd>
                </div>
                <div>
                    <dt>{"claim"}</dt>
                    <dd><code>{claim}</code></dd>
                </div>
            </dl>
            <p>{"The decision checks these heads and the room head it read. A moved head refuses it; only committed board authority moves the row."}</p>
        </aside>
    }
}

fn short_oid(oid: &str) -> String {
    oid.chars().take(9).collect()
}
