//! The resolution of a completed pass's offered outputs, decided without
//! leaving its Work row.

use super::*;

pub(super) fn render(context: &Context<'_>, offered: &[TaskStamp], detail: Html) -> Html {
    let (design, pass) = (context.design, context.pass);
    let empty = DecisionDraft::new(design.clone(), pass);
    let composer = &context.controls.decision.0;
    let draft = composer.draft(design, pass).unwrap_or(&empty);
    let waiting = draft.is_pending() || matches!(draft.notice(), Some(DraftNotice::Recorded(_)));
    let locked = composer.is_pending() || waiting || !context.interactive();
    let count = |verdict| {
        offered
            .iter()
            .filter(|output| draft.verdict(output) == Some(verdict))
            .count()
    };
    let (accepted, rejected) = (
        count(OutputVerdict::Accepted),
        count(OutputVerdict::Rejected),
    );
    let total = offered.len();
    let tally = if accepted + rejected == 0 {
        "undecided".to_owned()
    } else {
        format!("{accepted} of {total} accepted · {rejected} of {total} rejected")
    };
    let label = if accepted == total {
        "Accept every output".to_owned()
    } else if rejected == total {
        "Reject every output".to_owned()
    } else {
        format!("Record {accepted} accepted, {rejected} rejected")
    };
    let edit = editor(context);
    let resolve = send_prepared(
        context,
        &prepare_submission(context, draft, Choice::Resolve),
    );
    let return_to_room =
        send_prepared(context, &prepare_submission(context, draft, Choice::Return));
    let fact = SequenceCode::new(pass.get()).to_string();
    html! {
        <section class="work-decision" data-pass={fact.clone()} aria-label="Pass decision">
            <header class="work-decision-heading">
                <strong>{"Pass "}<code>{fact.clone()}</code></strong>
                <span data-decision-tally="">{tally}{if draft.comment().trim().is_empty() { "" } else { " · with comment" }}</span>
            </header>
            {render_term_nets(context)}
            <div class="work-decision-outputs">
                <ol aria-label="Offered outputs">
                    {for offered.iter().enumerate().map(|(index, output)| render_output(context, offered.len(), output, index, draft, locked))}
                </ol>
            </div>
            if total > 1 {
                {render_every_output_verdict(offered, locked, &edit)}
            }
            <label class="work-decision-comment">
                <span>{"Comment"}</span>
                <textarea
                    rows="3"
                    value={draft.comment().to_owned()}
                    oninput={draft_text(context, DecisionDraft::set_comment)}
                    disabled={locked}
                    placeholder="Optional on a decision. Required to return to the room."
                />
            </label>
            <p class="work-decision-return-note">{"Returning includes all item comments without accepting or rejecting any output."}</p>
            if waiting {
                <p class="work-decision-sending" role="status" aria-busy="true" data-decision-pending="">
                    {format!("Sending your decision on {fact}… the row moves when the board shows it.")}
                </p>
            }
            {render_notice(draft.notice())}
            <div class="work-decision-actions">
                <button type="button" class="app-compact-control" onclick={return_to_room} disabled={locked || draft.comment().trim().is_empty()}>{"Return to the room"}</button>
                <button type="button" class="app-compact-control work-decision-verdict" onclick={resolve} disabled={locked || accepted + rejected != total}>
                    {if waiting { "Sending…".to_owned() } else { label }}
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

fn render_output(
    context: &Context<'_>,
    total: usize,
    output: &TaskStamp,
    index: usize,
    draft: &DecisionDraft,
    locked: bool,
) -> Html {
    let verdict = |verdict| {
        let output = output.clone();
        let change = editor(context)(Rc::new(move |draft| {
            draft.set_output_verdict(output.clone(), verdict)
        }));
        Callback::from(move |_: MouseEvent| change())
    };
    let comment = {
        let output = output.clone();
        draft_text(context, move |draft, text| {
            draft.set_output_comment(output.clone(), text)
        })
    };
    let task = context.task(output);
    let handle = task.map_or_else(
        || output.to_string(),
        |task| task.statement.body.handle.to_string(),
    );
    let comment_id = yew::AttrValue::from(format!("output-comment-{}-{output}", context.design));
    let identity = task.map_or_else(
        || html! { <code>{output.to_string()}</code> },
        |task| task_handle_link(context.open_task, output, &task.statement.body.handle),
    );
    let title = task.map(|task| task.presentation_title().to_owned());
    html! {
        <li class="work-decision-output" key={output.to_string()} data-approval-output={output.to_string()}>
            <div class="work-decision-output-heading">
                <span>{format!("Output {} of {total}", index + 1)}</span>
                <span>
                    {identity}
                    if let Some(title) = title {
                        {" · "}
                        {crate::browser::task_detail::render_task_title(
                            &title,
                            &crate::composer_markdown::inline::LineControls::Nested,
                        )}
                    }
                </span>
                {task.map_or_else(Html::default, |task| html! { <span>{output_kind(task).into_owned()}</span> })}
            </div>
            <p>{task.map_or("Output unavailable on the current board", |task| task.statement.body.pitch.as_str())}</p>
            <div class="work-decision-output-acceptance" aria-label={format!("Acceptance for {handle}")}>
                {render_term(context, output)}
            </div>
            <div class="work-decision-output-comment">
                <label for={comment_id.clone()}>{"Comment (optional)"}</label>
                <div class="work-decision-output-decision">
                    <textarea id={comment_id} rows="3" value={draft.output_comment(output).to_owned()} oninput={comment} disabled={locked} />
                    <div class="work-decision-output-verdict" role="group" aria-label={format!("Verdict for {handle}")}>
                        <button type="button" class="app-compact-control" aria-pressed={(draft.verdict(output) == Some(OutputVerdict::Accepted)).to_string()} onclick={verdict(OutputVerdict::Accepted)} disabled={locked}>{"Accept"}</button>
                        <button type="button" class="app-compact-control" aria-pressed={(draft.verdict(output) == Some(OutputVerdict::Rejected)).to_string()} onclick={verdict(OutputVerdict::Rejected)} disabled={locked}>{"Reject"}</button>
                    </div>
                </div>
            </div>
        </li>
    }
}

fn render_term(context: &Context<'_>, output: &TaskStamp) -> Html {
    let Some(task) = context.task(output) else {
        return html! {
            <>
                <strong>{"Acceptance · "}<code>{output.to_string()}</code></strong>
                <p>{"Acceptance unavailable on the current board."}</p>
            </>
        };
    };
    let items = wire::StatementItem::each(&task.statement.body.term).collect::<Vec<_>>();
    html! {
        <>
            <strong>{"Acceptance · "}{task_handle_link(context.open_task, output, &task.statement.body.handle)}</strong>
            <ul>
                {for task.statement.body.term.iter().zip(&items).map(|(text, item)| html! {
                    <li>
                        <code>{item.handle(&items).to_owned()}</code>
                        {" "}
                        {crate::browser::task_detail::render_task_text(
                            text,
                            &[],
                            "work-decision-criterion",
                            |_| false,
                        )}
                    </li>
                })}
            </ul>
        </>
    }
}
