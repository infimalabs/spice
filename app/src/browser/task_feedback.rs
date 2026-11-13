//! Out-of-flow feedback for retained task detail and room composer state.

use super::*;

#[derive(Clone, PartialEq, Eq)]
enum PendingFeedback {
    NotePosting { task: TaskStamp, subject: String },
    Closing { task: TaskStamp, subject: String },
    Concluding { task: TaskStamp, subject: String },
    Collapsing { task: TaskStamp, subject: String },
    Deleting { task: TaskStamp, subject: String },
    Reconnecting { task: TaskStamp },
}

impl PendingFeedback {
    fn task(&self) -> &TaskStamp {
        match self {
            Self::NotePosting { task, .. }
            | Self::Closing { task, .. }
            | Self::Concluding { task, .. }
            | Self::Collapsing { task, .. }
            | Self::Deleting { task, .. }
            | Self::Reconnecting { task } => task,
        }
    }

    fn message(&self) -> AttrValue {
        match self {
            Self::NotePosting { subject, .. } => {
                format!("Publishing observation in {subject}'s room…").into()
            }
            Self::Closing { subject, .. } => format!("Closing {subject}…").into(),
            Self::Concluding { subject, .. } => format!("Concluding {subject}…").into(),
            Self::Collapsing { subject, .. } => format!("Collapsing {subject}…").into(),
            Self::Deleting { subject, .. } => format!("Deleting {subject}…").into(),
            Self::Reconnecting { .. } => {
                "Showing admitted detail while reconnecting. Updates and observations will resume when spice reconnects.".into()
            }
        }
    }

    fn completion(&self) -> Option<AttrValue> {
        match self {
            Self::NotePosting { subject, .. } => {
                Some(format!("Observation published in {subject}'s room.").into())
            }
            Self::Closing { subject, .. } => Some(settled(subject, wire::Settlement::Closed)),
            Self::Concluding { subject, .. } => Some(settled(subject, wire::Settlement::Done)),
            Self::Collapsing { subject, .. } => Some(settled(subject, wire::Settlement::Collapsed)),
            Self::Deleting { subject, .. } => Some(settled(subject, wire::Settlement::Deleted)),
            Self::Reconnecting { .. } => None,
        }
    }
}

/// The settle feedback names the outcome by the one word every surface
/// spells a settlement with.
fn settled(subject: &str, settlement: wire::Settlement) -> AttrValue {
    format!("{subject} is {settlement}.").into()
}

#[derive(Clone, PartialEq, Eq)]
struct SettledFeedback {
    task: TaskStamp,
    message: String,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Feedback {
    pending: Option<PendingFeedback>,
    settled: Option<SettledFeedback>,
}

fn task_subject(runtime: Option<&AppRuntime>, task: &TaskStamp) -> String {
    runtime
        .and_then(|runtime| runtime.task_detail(task))
        .and_then(TaskDetailState::answer)
        .map_or_else(
            || task.to_string(),
            |detail| detail.task.statement.body.handle.to_string(),
        )
}

fn feedback(state: &AppState) -> Feedback {
    let surface = state.surface.read();
    let runtime = surface.as_ref();
    let mut note_pending = None;
    let mut note_settled = None;
    for (task, composer) in state.task_detail.notes.published.iter() {
        if note_pending.is_none() {
            note_pending = match &composer.submission {
                NoteSubmission::Posting => Some(PendingFeedback::NotePosting {
                    task: task.clone(),
                    subject: task_subject(runtime, task),
                }),
                NoteSubmission::Closing => Some(PendingFeedback::Closing {
                    task: task.clone(),
                    subject: task_subject(runtime, task),
                }),
                NoteSubmission::Concluding => Some(PendingFeedback::Concluding {
                    task: task.clone(),
                    subject: task_subject(runtime, task),
                }),
                NoteSubmission::Collapsing => Some(PendingFeedback::Collapsing {
                    task: task.clone(),
                    subject: task_subject(runtime, task),
                }),
                NoteSubmission::Deleting => Some(PendingFeedback::Deleting {
                    task: task.clone(),
                    subject: task_subject(runtime, task),
                }),
                NoteSubmission::Idle | NoteSubmission::Failed(_) => None,
            };
        }
        if note_settled.is_none() {
            note_settled = composer
                .notice
                .is_some()
                .then(|| SettledFeedback {
                    task: task.clone(),
                    message: composer.notice.to_string(),
                })
                .or_else(|| match &composer.submission {
                    NoteSubmission::Failed(issue) => Some(SettledFeedback {
                        task: task.clone(),
                        message: issue.clone(),
                    }),
                    NoteSubmission::Idle
                    | NoteSubmission::Posting
                    | NoteSubmission::Closing
                    | NoteSubmission::Concluding
                    | NoteSubmission::Collapsing
                    | NoteSubmission::Deleting => None,
                });
        }
        if note_pending.is_some() && note_settled.is_some() {
            break;
        }
    }
    let surface_pending = state
        .sidebar
        .content
        .as_ref()
        .as_ref()
        .and_then(|content| content.tasks())
        .into_iter()
        .flat_map(|selection| selection.tasks())
        .find_map(|task| {
            let detail = runtime.and_then(|runtime| runtime.task_detail(task));
            (matches!(&*state.connection, Connection::Offline(_))
                && detail.and_then(TaskDetailState::answer).is_some())
            .then(|| PendingFeedback::Reconnecting { task: task.clone() })
        });
    Feedback {
        pending: note_pending.or(surface_pending),
        settled: note_settled,
    }
}

/// Keep pending task feedback visible until the state that caused it settles.
#[hook]
pub(super) fn use_feedback(state: &AppState) {
    let feedback = feedback(state);
    let previous = use_mut_ref(Feedback::default);
    let toaster = toast::use_task_toaster();
    use_effect_with(feedback, move |current| {
        let prior = previous.borrow().clone();
        if let Some(pending) = &current.pending {
            if prior.pending.as_ref() != Some(pending) {
                toaster.pending(pending.message());
            }
        } else if let Some(settled) = &current.settled
            && (prior.settled.as_ref() != Some(settled)
                || prior
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.task() == &settled.task))
        {
            toaster.settled(settled.message.clone());
        } else if let Some(pending) = &prior.pending {
            if let Some(message) = pending.completion() {
                toaster.settled(message);
            } else {
                toaster.clear();
            }
        }
        *previous.borrow_mut() = current.clone();
    });
}
