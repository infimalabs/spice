//! A Design's recorded decisions and the operator's decision surface for its
//! completed pass, read from the Design's controlled work. Resolutions,
//! conclusions, collapses, and returns are previewed before one atomic
//! `task decide`.

use std::borrow::Cow;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use wire::intent::{BoardTask, TextAnswer};
use wire::rooms::{DecisionOutcome, OutputVerdict, PreparationDecision};
use wire::{CommandKind, Seq, SequenceCode, TaskStamp};
use yew::prelude::*;

use super::standing::focus_in_place;
use super::{Controls, OpenTask, card, task_handle_link};
use crate::AppRuntime;
use crate::browser::sender::{SenderHandle, admit_action};
use crate::client_action::{ClientAction, PreparationRequest};
use crate::preparation_decision::{
    Choice, DecisionComposer, DecisionDraft, DraftNotice, DraftPhase, Submission,
};
use crate::runtime::{PreparationProjection, RoomPositioned};

use crate::decision_history as history;

mod conclusion;
mod work;

fn current_task<'a>(runtime: &'a AppRuntime, stamp: &TaskStamp) -> Option<&'a BoardTask> {
    match history::task_evidence(runtime.board(), runtime.task_detail(stamp), stamp) {
        history::TaskEvidence::Available(task) => Some(task),
        history::TaskEvidence::Loading
        | history::TaskEvidence::Missing
        | history::TaskEvidence::Refused { .. } => None,
    }
}

/// The decision owed on the Design's completed pass, edited inside its
/// opened Work row: a resolution of its offered outputs, or, when it offers
/// none, a conclusion, collapse, or return.
pub(in crate::browser) fn render_pending_decision(
    runtime: &AppRuntime,
    controls: &Controls,
    sender: &SenderHandle,
    open_task: &OpenTask,
    design: &TaskStamp,
    detail: Html,
) -> Option<Html> {
    let board = runtime.board()?;
    let controller = board.row(design).filter(|task| {
        task.design_standing()
            .is_some_and(|standing| standing.is_open())
    })?;
    let pass = controller.design_cycle.pass?;
    let context = Context {
        runtime,
        controls,
        sender,
        open_task,
        preparation: runtime.room_projection().preparation(),
        design,
        pass,
        tasks: &board.tasks,
        controller,
    };
    let offered = crate::preparation_decision::offered_outputs(design, &board.tasks)
        .map(|task| task.statement.body.born.clone())
        .collect::<Vec<_>>();
    Some(if offered.is_empty() {
        conclusion::render(&context, detail)
    } else {
        work::render(&context, &offered, detail)
    })
}

type DraftChange = Rc<dyn Fn(&mut DecisionDraft)>;
type DraftCommand = Box<dyn Fn()>;

/// This page's per-Design decision drafts. One pending decision globally
/// locks submission until its response settles.
#[derive(Clone, Default, PartialEq)]
pub(super) struct Composer(DecisionComposer);

/// One edit to one Design pass's draft. A late response cannot settle another.
pub(super) struct ComposerEdit {
    design: TaskStamp,
    pass: Seq,
    settles: bool,
    change: Box<dyn FnOnce(&mut DecisionDraft)>,
}

impl Reducible for Composer {
    type Action = ComposerEdit;

    fn reduce(self: Rc<Self>, action: ComposerEdit) -> Rc<Self> {
        let mut composer = Rc::unwrap_or_clone(self).0;
        composer.apply(action.design, action.pass, action.settles, action.change);
        Rc::new(Self(composer))
    }
}

struct Context<'a> {
    runtime: &'a AppRuntime,
    controls: &'a Controls,
    sender: &'a SenderHandle,
    open_task: &'a OpenTask,
    preparation: &'a PreparationProjection,
    design: &'a TaskStamp,
    /// The completed pass awaiting decision, which scopes the draft.
    pass: Seq,
    tasks: &'a [BoardTask],
    /// Current open Design row whose completed pass awaits decision.
    controller: &'a BoardTask,
}

impl<'a> Context<'a> {
    fn task(&self, stamp: &TaskStamp) -> Option<&'a BoardTask> {
        current_task(self.runtime, stamp)
    }

    fn interactive(&self) -> bool {
        self.runtime.principal().is_principal()
            && self.runtime.offers(CommandKind::PreparationDecide)
    }

    /// The Design room's head as this page has folded it.
    fn room(&self) -> Option<Seq> {
        self.runtime
            .room_projection()
            .room(self.design)
            .and_then(crate::runtime::RoomState::head)
    }

    /// The code the next decision under `controller` takes: one past every
    /// decision this page has seen for it.
    fn next_decision(&self, controller: &TaskStamp) -> SequenceCode {
        SequenceCode::new(
            self.preparation
                .decisions
                .keys()
                .filter(|decision| decision.controller == *controller)
                .map(|decision| decision.decision.get())
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        )
    }
}

/// One recorded decision as its room conversation shows it: what the
/// operator decided, its verdict counts, and each offered output's handle
/// opening that task's sheet.
pub(super) fn render_decision_card(
    runtime: &AppRuntime,
    open_task: &OpenTask,
    room: &TaskStamp,
    positioned: &RoomPositioned<PreparationDecision>,
) -> Html {
    let code = SequenceCode::new(positioned.position.get()).to_string();
    html! {
        <article class="room-card" key={format!("decision-{code}")} tabindex="0">
            {render_decision_face(runtime, open_task, room, positioned)}
        </article>
    }
}

fn decision_notes(runtime: &AppRuntime, open_task: &OpenTask, outcome: &DecisionOutcome) -> Html {
    match outcome {
        DecisionOutcome::Returned { reason } | DecisionOutcome::Collapsed { reason } => {
            html! { <p class="room-decision-note">{reason.clone()}</p> }
        }
        DecisionOutcome::Resolved {
            outputs, comment, ..
        } => {
            let output_notes = outputs
                .iter()
                .filter_map(|output| {
                    let comment = output.comment.as_ref()?;
                    let identity = current_task(runtime, &output.output).map_or_else(
                        || html! { <code>{output.output.to_string()}</code> },
                        |task| {
                            task_handle_link(
                                open_task,
                                &task.statement.body.born,
                                &task.statement.body.handle,
                            )
                        },
                    );
                    Some(html! { <li>{identity}{": "}{comment.clone()}</li> })
                })
                .collect::<Vec<_>>();
            if output_notes.is_empty() && comment.is_none() {
                Html::default()
            } else {
                html! {
                    <div class="room-decision-note">
                        if !output_notes.is_empty() {
                            <ul>{for output_notes}</ul>
                        }
                        if let Some(comment) = comment {
                            <p>{comment.clone()}</p>
                        }
                    </div>
                }
            }
        }
        DecisionOutcome::Concluded {} => Html::default(),
    }
}

fn render_history_output(
    runtime: &AppRuntime,
    open_task: &OpenTask,
    room: &TaskStamp,
    decision: &PreparationDecision,
    stamp: &TaskStamp,
    selector: String,
) -> Html {
    let row = current_task(runtime, stamp);
    let identity = row.map_or_else(
        || html! { <code>{stamp.to_string()}</code> },
        |task| {
            task_handle_link(
                open_task,
                &task.statement.body.born,
                &task.statement.body.handle,
            )
        },
    );
    let title = row.map(|task| {
        html! {
            <a href={crate::view::design_href(room)} onclick={focus_in_place(selector)}>
                {task.statement.body.pitch.clone()}
            </a>
        }
    });
    let verdict =
        history::output_decision(decision, stamp).map_or("pending", |output| {
            match output.verdict {
                OutputVerdict::Accepted => "accepted",
                OutputVerdict::Rejected => "rejected",
            }
        });
    let state = history::output_state(runtime.board(), runtime.task_detail(stamp), stamp);
    let state_key = state.key();
    let standing = match state {
        history::OutputState::Open { phase } => html! {
            <span class="room-decision-output-state">{"○ "}{phase}</span>
        },
        history::OutputState::Settled { state, commit } => html! {
            <span class="room-decision-output-state">
                {if state == "deleted" { "✕ " } else { "✓ " }}{state}
                if let Some(commit) = commit {
                    {" "}<code>{commit}</code>
                }
            </span>
        },
        history::OutputState::Resolving | history::OutputState::Missing => html! {
            <span class="room-decision-output-state">{state_key}</span>
        },
    };
    html! {
        <li data-output-state={state_key}>
            {identity}{" · "}<span data-output-verdict={verdict}>{verdict}</span>{" · "}{standing}
            if let Some(title) = title {
                {" — "}{title}
            }
        </li>
    }
}

/// The decision card's face, for a surface that supplies its own card.
pub(super) fn render_decision_face(
    runtime: &AppRuntime,
    open_task: &OpenTask,
    room: &TaskStamp,
    positioned: &RoomPositioned<PreparationDecision>,
) -> Html {
    let preparation = runtime.room_projection().preparation();
    let decision = &positioned.value;
    let record = preparation.decision_records.get(&decision.decision);
    let speaker = record.map(|record| card::Speaker {
        name: crate::work::owner_label(&record.author, None, runtime.principals()),
        lane: None,
    });
    let fact = SequenceCode::new(positioned.position.get()).to_string();
    let disposition = history::disposition(&decision.outcome);
    let design_pitch =
        current_task(runtime, room).map_or("", |task| task.statement.body.pitch.as_str());
    let outputs = decision.basis.outputs.iter().map(|revision| {
        render_history_output(
            runtime,
            open_task,
            room,
            decision,
            &revision.task,
            format!("[data-approval-output=\"{}\"]", revision.task),
        )
    });
    let tail = html! {
        <div class="room-decision" data-decision-card={fact.clone()}>
            <p class="room-decision-standing">
                <code>{fact}</code>
                {" · "}{disposition}{" by decision "}{decision.decision.decision.to_string()}
                <span class="room-decision-badges" role="group" aria-label="Decision">
                    {render_verdict_badges(&decision.outcome)}
                </span>
            </p>
            {decision_notes(runtime, open_task, &decision.outcome)}
            if !decision.basis.outputs.is_empty() {
                <ul class="room-decision-outputs" aria-label="Offered outputs">{for outputs}</ul>
            }
        </div>
    };
    card::render(
        runtime,
        decision_display_millis(preparation, positioned),
        card::Face {
            speaker,
            receivers: String::new(),
            badge: Some(card::Badge::Kind {
                kind: "decision",
                label: "Decision",
            }),
            words: format!("{disposition} · {design_pitch}"),
            quoting: card::Quoting::default(),
            tail,
        },
        None,
    )
}

/// Journal time shown by a decision card and used to interleave it with
/// other activity; a decision without its record shows its Design's birth.
pub(super) fn decision_display_millis(
    preparation: &PreparationProjection,
    positioned: &RoomPositioned<PreparationDecision>,
) -> i64 {
    let decision = &positioned.value.decision;
    preparation.decision_records.get(decision).map_or_else(
        || decision.controller.micros() / 1_000,
        |record| record.at.get().as_millisecond(),
    )
}

/// A resolution's accepted and rejected output counts as tab-style badges.
fn render_verdict_badges(outcome: &DecisionOutcome) -> Html {
    let DecisionOutcome::Resolved { outputs, .. } = outcome else {
        return Html::default();
    };
    let badge = |verdict: &'static str, label: &'static str, verdict_kind| {
        let count = outputs
            .iter()
            .filter(|output| output.verdict == verdict_kind)
            .count();
        html! {
            <span class="room-decision-badge" data-verdict={verdict}>
                {label}
                <span class="app-count-badge">{count}</span>
            </span>
        }
    };
    html! {
        <>
            {badge("accepted", "Accepted", OutputVerdict::Accepted)}
            {badge("rejected", "Rejected", OutputVerdict::Rejected)}
        </>
    }
}

fn editor(context: &Context<'_>) -> impl Fn(DraftChange) -> DraftCommand {
    let (composer, design, pass) = (
        context.controls.decision.clone(),
        context.design.clone(),
        context.pass,
    );
    move |change: DraftChange| {
        let (composer, design) = (composer.clone(), design.clone());
        Box::new(move || {
            let change = Rc::clone(&change);
            composer.dispatch(ComposerEdit {
                design: design.clone(),
                pass,
                settles: false,
                change: Box::new(move |draft| change(draft)),
            });
        }) as DraftCommand
    }
}

/// One control per verdict that marks every offered output at once. It sits
/// after the output list, beside the pass-wide comment and decision actions,
/// so the operator who read the list need not scroll back or visit each line.
fn render_every_output_verdict(
    outputs: &[TaskStamp],
    locked: bool,
    edit: &impl Fn(DraftChange) -> DraftCommand,
) -> Html {
    let all = |verdict: OutputVerdict| {
        let outputs = outputs.to_vec();
        let apply = edit(Rc::new(move |draft| {
            draft.set_every_output_verdict(outputs.iter().cloned(), verdict);
        }));
        Callback::from(move |_: MouseEvent| apply())
    };
    html! {
        <div
            class="room-inline-actions room-output-verdict-all"
            role="group"
            aria-label="Verdict for every output"
        >
            <button type="button" class="app-compact-control" disabled={locked} onclick={all(OutputVerdict::Accepted)}>
                {"Accept all"}
            </button>
            <button type="button" class="app-compact-control" disabled={locked} onclick={all(OutputVerdict::Rejected)}>
                {"Reject all"}
            </button>
        </div>
    }
}

fn output_kind(task: &BoardTask) -> Cow<'static, str> {
    match task.statement.body.intent {
        Some(wire::TaskIntent::Plan) => Cow::Borrowed("Plan"),
        Some(wire::TaskIntent::Design | wire::TaskIntent::OpportunisticDesign) => {
            Cow::Borrowed("Design")
        }
        None => {
            let mut label = String::from("Task");
            for (index, phase) in task.workflow.body.flow.iter().enumerate() {
                label.push_str(if index == 0 { " · " } else { " → " });
                label.push_str(phase.as_str());
            }
            Cow::Owned(label)
        }
    }
}

/// Each input event's value applied to the open pass's draft through `set`.
fn draft_text(
    context: &Context<'_>,
    set: impl Fn(&mut DecisionDraft, String) + Clone + 'static,
) -> Callback<InputEvent> {
    let (composer, design, pass) = (
        context.controls.decision.clone(),
        context.design.clone(),
        context.pass,
    );
    Callback::from(move |event: InputEvent| {
        let Some(value) = event.target().and_then(|target| {
            target
                .dyn_ref::<HtmlInputElement>()
                .map(HtmlInputElement::value)
                .or_else(|| {
                    target
                        .dyn_ref::<HtmlTextAreaElement>()
                        .map(HtmlTextAreaElement::value)
                })
        }) else {
            return;
        };
        let set = set.clone();
        composer.dispatch(ComposerEdit {
            design: design.clone(),
            pass,
            settles: false,
            change: Box::new(move |draft| set(draft, value)),
        });
    })
}

/// Preview `choice` against the Design's current heads, room, and offered
/// work. Every decision surface shares this authority capture.
fn prepare_submission(
    context: &Context<'_>,
    draft: &DecisionDraft,
    choice: Choice,
) -> DecisionDraft {
    let mut prepared = draft.clone();
    prepared.preview(
        context.controller,
        context.room(),
        context.next_decision(context.design),
        context.tasks,
        choice,
    );
    prepared
}

/// Send one already-previewed choice. The shared composer owns pending,
/// refusal retention, and committed-authority movement for every Design.
fn send_prepared(context: &Context<'_>, prepared: &DecisionDraft) -> Callback<MouseEvent> {
    let composer = context.controls.decision.clone();
    let deciding = Rc::clone(&context.controls.deciding);
    let prepared = prepared.clone();
    let send = if let DraftPhase::Previewing(submission) = prepared.phase() {
        Some(submit(
            context.controls,
            context.sender,
            prepared.design(),
            prepared.pass(),
            submission,
        ))
    } else {
        None
    };
    Callback::from(move |event: MouseEvent| {
        if !deciding.borrow().is_empty() {
            return;
        }
        let prepared = prepared.clone();
        composer.dispatch(ComposerEdit {
            design: prepared.design().clone(),
            pass: prepared.pass(),
            settles: false,
            change: Box::new(move |draft| {
                if !draft.is_pending() {
                    *draft = prepared;
                }
            }),
        });
        if let Some(send) = &send {
            send.emit(event);
        }
    })
}

fn submit(
    controls: &Controls,
    sender: &SenderHandle,
    design: &TaskStamp,
    pass: Seq,
    submission: &Submission,
) -> Callback<MouseEvent> {
    let (composer, deciding, sender) = (
        controls.decision.clone(),
        Rc::clone(&controls.deciding),
        sender.clone(),
    );
    let (design, submission) = (design.clone(), submission.clone());
    Callback::from(move |_: MouseEvent| {
        let mut in_flight = deciding.borrow_mut();
        if !in_flight.is_empty() {
            return;
        }
        in_flight.insert(design.clone());
        drop(in_flight);
        composer.dispatch(ComposerEdit {
            design: design.clone(),
            pass,
            settles: false,
            change: Box::new(|draft| {
                draft.begin_submit();
            }),
        });
        let settle = {
            let (composer, deciding, design) =
                (composer.clone(), Rc::clone(&deciding), design.clone());
            move |outcome: Result<TextAnswer, String>| {
                deciding.borrow_mut().remove(&design);
                composer.dispatch(ComposerEdit {
                    design: design.clone(),
                    pass,
                    settles: true,
                    change: Box::new(move |draft| draft.settle(outcome)),
                });
            }
        };
        let action = ClientAction::Preparation {
            request: PreparationRequest::Decide {
                request: Box::new(submission.request.clone()),
                expected: submission.expected.clone(),
            },
            respond: Rc::new(settle.clone()),
        };
        if !admit_action(&sender, action) {
            settle(Err("The decision could not be queued.".to_owned()));
        }
    })
}

fn render_notice(notice: Option<&DraftNotice>) -> Html {
    match notice {
        None => Html::default(),
        Some(DraftNotice::Refused(reason)) => html! {
            <p class="room-notice room-decision-refusal" role="alert" data-decision-refusal="">
                {reason.clone()}
            </p>
        },
        Some(DraftNotice::Recorded(answer)) => html! {
            <p class="room-notice" role="status" data-decision-recorded="">
                {format!("{answer}. Standing updates only when committed authority arrives.")}
            </p>
        },
    }
}
