//! One spelling of a task's workflow steps for every task read surface.

use wire::TaskStamp;
use wire::intent::{BoardAnswer, TaskDetailStep, TaskStep};

/// A task's current handle on `board`, or its bare stamp when the board does
/// not carry it.
pub(super) fn stamp_name(board: &BoardAnswer, stamp: &TaskStamp) -> String {
    board
        .tasks
        .iter()
        .find(|task| task.statement.body.born == *stamp)
        .map_or_else(
            || stamp.to_string(),
            |task| task.statement.body.handle.to_string(),
        )
}

/// The step's position, commit time, author as `author` spells it, and what
/// it recorded.
pub(super) fn step_heading(step: &TaskDetailStep, author: &dyn std::fmt::Display) -> String {
    let recorded = match &step.step {
        TaskStep::Done { phase, .. } => format!("done {phase}"),
        TaskStep::Phased { phase } => format!("phased {phase}"),
        TaskStep::Reviewed { verdict, .. } => format!(
            "reviewed {}",
            match verdict {
                wire::body::Verdict::Clean => "clean",
                wire::body::Verdict::Finding => "finding",
            }
        ),
        TaskStep::Concluded { conclusion } => format!(
            "concluded {}",
            match conclusion {
                wire::body::DesignConclusion::Done {} => wire::Settlement::Done,
                wire::body::DesignConclusion::Collapsed { .. } => wire::Settlement::Collapsed,
            }
        ),
    };
    format!("{} {} {author} {recorded}", step.position, step.at)
}

/// A completion's validation, a verdict's finding or note, or a collapse's
/// reason.
pub(super) fn step_text(step: &TaskDetailStep) -> Option<&str> {
    match &step.step {
        TaskStep::Done { validation, .. } => Some(validation),
        TaskStep::Phased { .. } => None,
        TaskStep::Reviewed { text, .. } => text.as_deref(),
        TaskStep::Concluded { conclusion } => match conclusion {
            wire::body::DesignConclusion::Done {} => None,
            wire::body::DesignConclusion::Collapsed { reason } => Some(reason),
        },
    }
}

/// The corrections a verdict linked, spelled by `name`.
pub(super) fn step_followups(
    step: &TaskDetailStep,
    name: impl Fn(&TaskStamp) -> String,
) -> Option<String> {
    let TaskStep::Reviewed { followups, .. } = &step.step else {
        return None;
    };
    (!followups.is_empty()).then(|| {
        let names = followups.iter().map(name).collect::<Vec<_>>();
        format!("followups {}", names.join(", "))
    })
}

/// The no-construction disposition a completion recorded, its covering task
/// spelled by `name`.
pub(super) fn step_disposition(
    step: &TaskDetailStep,
    name: impl Fn(&TaskStamp) -> String,
) -> Option<String> {
    let TaskStep::Done {
        disposition: Some(disposition),
        ..
    } = &step.step
    else {
        return None;
    };
    let kind = disposition.kind();
    Some(match disposition.cover() {
        None => format!("disposition {kind}"),
        Some(wire::body::DispositionCover::Task(task)) => {
            format!("disposition {kind} by {}", name(task))
        }
        Some(wire::body::DispositionCover::Commit(commit)) => {
            format!("disposition {kind} by commit {commit}")
        }
    })
}

/// The step as one document annotation: its heading, text, and corrections.
///
/// Exporting and applying a ledger both spell steps here, so applying an
/// exported ledger recognizes these annotations instead of appending them as
/// notes.
pub(super) fn step_annotation(
    step: &TaskDetailStep,
    name: impl Fn(&TaskStamp) -> String,
) -> String {
    let mut annotation = format!("workflow {}", step_heading(step, &step.author));
    if let Some(text) = step_text(step) {
        annotation.push_str("\n\n");
        annotation.push_str(text);
    }
    if let Some(disposition) = step_disposition(step, &name) {
        annotation.push_str("\n\n");
        annotation.push_str(&disposition);
    }
    if let Some(followups) = step_followups(step, name) {
        annotation.push_str("\n\n");
        annotation.push_str(&followups);
    }
    annotation
}
