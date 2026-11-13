//! One operator decision over one Design's completed pass, read from its
//! controlled graph: resolve each offered output, conclude or collapse a pass
//! that offers none, or return the pass with material feedback; preview the
//! effect; then submit once.
//!
//! The offered work is every pending, acceptance-bearing Draft the Design
//! controls. No output verdict is assumed: every offered output stays
//! undecided until the operator marks it, singly or all at once, and a
//! resolution with an undecided output refuses to preview rather than
//! deciding for them.
//!
//! The draft never changes rendered authority. A successful command answer
//! only reports that the write landed; the Design's standing moves when the
//! committed decision and refreshed board heads arrive.

use std::collections::{BTreeMap, btree_map::Entry};

use wire::intent::{BoardClaim, BoardTask, TextAnswer};
use wire::rooms::{
    DecisionBasis, DecisionOutcome, DecisionRef, OutputDecision, OutputRevision, OutputVerdict,
    PreparationDecide, PreparationDecision, ProposalRef, RoomSource,
};
use wire::{Seq, SequenceCode, TaskStamp, Witness};

/// One decision command and the authority heads its preview observed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submission {
    pub request: PreparationDecide,
    pub expected: Vec<Witness>,
}

/// Where the decision draft stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftPhase {
    Editing,
    Previewing(Submission),
    Pending(Submission),
}

/// Last local or resident outcome, never projected authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftNotice {
    Refused(String),
    Recorded(String),
}

/// The decision the operator previews.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Accept or reject every offered output.
    Resolve,
    /// Conclude the Design done.
    Conclude,
    /// Collapse the Design, giving the comment as its reason.
    Collapse,
    /// Return the pass, giving the comment as its feedback.
    Return,
}

/// Browser-local decision state for one Design's completed pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionDraft {
    design: TaskStamp,
    pass: Seq,
    comment: String,
    verdicts: BTreeMap<TaskStamp, OutputVerdict>,
    output_comments: BTreeMap<TaskStamp, String>,
    phase: DraftPhase,
    notice: Option<DraftNotice>,
}

/// Browser-local decision drafts, one per Design, each scoped to the
/// completed pass it decides.
///
/// Settled drafts remain beside their Designs while the operator edits
/// another. A later pass of the same Design replaces a settled draft, so an
/// earlier pass's recorded answer never locks it. At most one draft owns the
/// pending slot, and only that Design's response may settle it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecisionComposer {
    drafts: BTreeMap<TaskStamp, DecisionDraft>,
    pending: Option<TaskStamp>,
}

impl DecisionComposer {
    /// The draft for `design`'s completed `pass`, if one was begun.
    #[must_use]
    pub fn draft(&self, design: &TaskStamp, pass: Seq) -> Option<&DecisionDraft> {
        self.drafts.get(design).filter(|draft| draft.pass == pass)
    }

    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Apply one edit to the draft for `design`'s completed `pass` without
    /// displacing the one in-flight draft, which only an edit for its own
    /// pass reaches. A settled draft for another pass is replaced.
    pub fn apply(
        &mut self,
        design: TaskStamp,
        pass: Seq,
        settles: bool,
        change: impl FnOnce(&mut DecisionDraft),
    ) {
        let owns_pending = self.pending.as_ref() == Some(&design)
            && self
                .drafts
                .get(&design)
                .is_some_and(|draft| draft.pass == pass);
        if (settles && !owns_pending) || (self.pending.is_some() && !owns_pending) {
            return;
        }
        let draft = match self.drafts.entry(design) {
            Entry::Occupied(entry) => {
                let draft = entry.into_mut();
                if draft.pass != pass {
                    *draft = DecisionDraft::new(draft.design.clone(), pass);
                }
                draft
            }
            Entry::Vacant(entry) => {
                let design = entry.key().clone();
                entry.insert(DecisionDraft::new(design, pass))
            }
        };
        change(draft);
        if draft.is_pending() && self.pending.is_none() {
            self.pending = Some(draft.design().clone());
        } else if owns_pending && !draft.is_pending() {
            self.pending = None;
        }
    }
}

impl DecisionDraft {
    #[must_use]
    pub const fn new(design: TaskStamp, pass: Seq) -> Self {
        Self {
            design,
            pass,
            comment: String::new(),
            verdicts: BTreeMap::new(),
            output_comments: BTreeMap::new(),
            phase: DraftPhase::Editing,
            notice: None,
        }
    }

    #[must_use]
    pub const fn design(&self) -> &TaskStamp {
        &self.design
    }

    /// The completed pass this draft decides.
    #[must_use]
    pub const fn pass(&self) -> Seq {
        self.pass
    }

    /// The verdict the operator gave `output`, if any.
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn verdict(&self, output: &TaskStamp) -> Option<OutputVerdict> {
        self.verdicts.get(output).copied()
    }

    #[must_use]
    pub fn output_comment(&self, output: &TaskStamp) -> &str {
        self.output_comments.get(output).map_or("", String::as_str)
    }

    /// The overall comment: optional on a resolution, the feedback of a
    /// return, and the reason of a collapse.
    #[must_use]
    pub fn comment(&self) -> &str {
        &self.comment
    }

    #[must_use]
    pub const fn phase(&self) -> &DraftPhase {
        &self.phase
    }

    #[must_use]
    pub const fn notice(&self) -> Option<&DraftNotice> {
        self.notice.as_ref()
    }

    #[must_use]
    pub const fn is_pending(&self) -> bool {
        matches!(self.phase, DraftPhase::Pending(_))
    }

    pub fn set_output_verdict(&mut self, output: TaskStamp, verdict: OutputVerdict) {
        self.edit(|draft| {
            draft.verdicts.insert(output, verdict);
        });
    }

    /// Give every listed output the same verdict in one edit.
    pub fn set_every_output_verdict(
        &mut self,
        outputs: impl IntoIterator<Item = TaskStamp>,
        verdict: OutputVerdict,
    ) {
        self.edit(|draft| {
            for output in outputs {
                draft.verdicts.insert(output, verdict);
            }
        });
    }

    pub fn set_output_comment(&mut self, output: TaskStamp, comment: String) {
        self.edit(|draft| {
            draft.output_comments.insert(output, comment);
        });
    }

    pub fn set_comment(&mut self, comment: String) {
        self.edit(|draft| draft.comment = comment);
    }

    /// Preview `choice` against the Design's current heads, its room head,
    /// and the controlled work `tasks` offers.
    pub fn preview(
        &mut self,
        design: &BoardTask,
        room: Option<Seq>,
        next_decision: SequenceCode,
        tasks: &[BoardTask],
        choice: Choice,
    ) {
        if self.is_pending() {
            return;
        }
        match self.submission(design, room, next_decision, tasks, choice) {
            Ok(submission) => {
                self.phase = DraftPhase::Previewing(submission);
                self.notice = None;
            }
            Err(reason) => {
                self.phase = DraftPhase::Editing;
                self.notice = Some(DraftNotice::Refused(reason));
            }
        }
    }

    /// Take the preview once. The caller's in-flight guard closes the event-loop
    /// gap before this pending state redraws.
    pub fn begin_submit(&mut self) -> Option<Submission> {
        if !matches!(self.phase, DraftPhase::Previewing(_)) {
            return None;
        }
        let DraftPhase::Previewing(submission) =
            std::mem::replace(&mut self.phase, DraftPhase::Editing)
        else {
            unreachable!("checked previewing above");
        };
        self.phase = DraftPhase::Pending(submission.clone());
        self.notice = None;
        Some(submission)
    }

    /// Settle only an in-flight request. Success leaves standing to committed
    /// room authority; refusal keeps every choice for revision and re-preview.
    pub fn settle(&mut self, outcome: Result<TextAnswer, String>) {
        if !self.is_pending() {
            return;
        }
        self.phase = DraftPhase::Editing;
        self.notice = Some(match outcome {
            Ok(answer) => DraftNotice::Recorded(answer.text),
            Err(reason) => DraftNotice::Refused(reason),
        });
    }

    fn edit(&mut self, change: impl FnOnce(&mut Self)) {
        if self.is_pending() {
            return;
        }
        self.phase = DraftPhase::Editing;
        self.notice = None;
        change(self);
    }

    fn submission(
        &self,
        design: &BoardTask,
        room: Option<Seq>,
        next_decision: SequenceCode,
        tasks: &[BoardTask],
        choice: Choice,
    ) -> Result<Submission, String> {
        let design_stamp = &design.statement.body.born;
        if self.design != *design_stamp {
            return Err(
                "preparation::design-mismatch: preview the Design this draft names".to_owned(),
            );
        }
        if !design
            .design_standing()
            .is_some_and(|standing| standing.is_open())
        {
            return Err("preparation::design-not-open: decide an open Design".to_owned());
        }
        let Some(pass) = design.design_cycle.pass else {
            return Err("preparation::pass-open: decide after the Design's pass ends".to_owned());
        };
        let offered = offered_outputs(design_stamp, tasks).collect::<Vec<_>>();
        let basis = DecisionBasis {
            statement: design.statement.position,
            room,
            outputs: offered.iter().map(|task| revision(task)).collect(),
        };
        let outcome = self.decision_outcome(&basis.outputs, choice)?;
        let decision = DecisionRef {
            controller: design_stamp.clone(),
            decision: next_decision,
        };
        let expected = decision_witnesses(design, &decision, &basis, &outcome, tasks)?;
        Ok(Submission {
            request: PreparationDecide {
                decision: PreparationDecision {
                    decision,
                    source: RoomSource::Journal {
                        fact: SequenceCode::new(pass.get()),
                    },
                    basis,
                    outcome,
                },
            },
            expected,
        })
    }

    fn decision_outcome(
        &self,
        offered: &[OutputRevision],
        choice: Choice,
    ) -> Result<DecisionOutcome, String> {
        let comment = self.comment.trim();
        match choice {
            Choice::Return if comment.is_empty() => Err(
                "preparation::return-reason-missing: give the feedback this pass returns with"
                    .to_owned(),
            ),
            Choice::Return => Ok(DecisionOutcome::Returned {
                reason: self.return_reason(offered, comment),
            }),
            Choice::Collapse if comment.is_empty() => Err(
                "preparation::collapse-reason-missing: give the reason the Design collapses"
                    .to_owned(),
            ),
            Choice::Collapse => Ok(DecisionOutcome::Collapsed {
                reason: comment.to_owned(),
            }),
            Choice::Conclude => Ok(DecisionOutcome::Concluded {}),
            Choice::Resolve => {
                if self
                    .verdicts
                    .keys()
                    .any(|output| !offered.iter().any(|revision| revision.task == *output))
                {
                    return Err(
                        "preparation::decision-foreign-output: decide only the outputs this pass offers"
                            .to_owned(),
                    );
                }
                let undecided = offered
                    .iter()
                    .filter(|revision| !self.verdicts.contains_key(&revision.task))
                    .map(|revision| revision.task.to_string())
                    .collect::<Vec<_>>();
                if !undecided.is_empty() {
                    return Err(format!(
                        "preparation::decision-undecided-output: accept or reject every offered output before previewing; undecided: {}",
                        undecided.join(", ")
                    ));
                }
                Ok(DecisionOutcome::Resolved {
                    outputs: offered
                        .iter()
                        .map(|revision| OutputDecision {
                            output: revision.task.clone(),
                            verdict: self.verdicts[&revision.task],
                            comment: trimmed(self.output_comment(&revision.task)),
                        })
                        .collect(),
                    comment: trimmed(&self.comment),
                })
            }
        }
    }

    /// A return carries item feedback without recording any item verdict.
    fn return_reason(&self, offered: &[OutputRevision], feedback: &str) -> String {
        use std::fmt::Write as _;

        let mut reason = feedback.to_owned();
        for (index, revision) in offered.iter().enumerate() {
            let comment = self.output_comment(&revision.task).trim();
            if !comment.is_empty() {
                write!(
                    reason,
                    "\n\n## Output {}: {}\n\n{comment}",
                    index + 1,
                    revision.task
                )
                .expect("writing to a String cannot fail");
            }
        }
        reason
    }
}

/// The Design's offered work on `tasks`: every pending, acceptance-bearing
/// Draft it controls.
pub fn offered_outputs<'a>(
    design: &'a TaskStamp,
    tasks: &'a [BoardTask],
) -> impl Iterator<Item = &'a BoardTask> {
    tasks.iter().filter(move |task| {
        let statement = &task.statement.body;
        task.workflow.body.status.is_pending()
            && task.workflow.body.is_draft()
            && !statement.term.is_empty()
            && statement
                .preparation
                .as_ref()
                .is_some_and(|preparation| preparation.controller == *design)
    })
}

fn revision(task: &BoardTask) -> OutputRevision {
    OutputRevision {
        task: task.statement.body.born.clone(),
        statement: task.statement.position,
        workflow: task.workflow.position,
        dependencies: task.statement.body.dependencies.clone(),
    }
}

fn trimmed(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Every head the decision read: the Design's transition heads and room,
/// each offered output's revision, and each accepted output's transition
/// heads, so an edit since the read refuses it as stale.
fn decision_witnesses(
    design: &BoardTask,
    decision: &DecisionRef,
    basis: &DecisionBasis,
    outcome: &DecisionOutcome,
    tasks: &[BoardTask],
) -> Result<Vec<Witness>, String> {
    let mut expected = transition_witnesses(design);
    expected.push(Witness::Proposal {
        proposal: ProposalRef::Decision {
            decision: decision.clone(),
        },
        head: None,
    });
    expected.push(Witness::Room {
        room: design.statement.body.born.clone(),
        head: basis.room,
    });
    for revision in &basis.outputs {
        expected.push(Witness::Task {
            task: revision.task.clone(),
            head: revision.statement,
        });
        expected.push(Witness::Workflow {
            task: revision.task.clone(),
            head: revision.workflow,
            phase: wire::Phase::Draft,
        });
    }
    for output in outcome.accepted_outputs() {
        let task = find_task(tasks, output).ok_or_else(|| {
            format!("preparation::output-unavailable: accepted output {output} is not on the board")
        })?;
        expected.extend(transition_witnesses(task));
    }
    Ok(expected)
}

fn find_task<'a>(tasks: &'a [BoardTask], stamp: &TaskStamp) -> Option<&'a BoardTask> {
    tasks.iter().find(|task| task.statement.body.born == *stamp)
}

fn statement_witness(task: &BoardTask) -> Witness {
    Witness::Task {
        task: task.statement.body.born.clone(),
        head: task.statement.position,
    }
}

fn transition_witnesses(task: &BoardTask) -> Vec<Witness> {
    let stamp = &task.statement.body.born;
    vec![
        statement_witness(task),
        Witness::Workflow {
            task: stamp.clone(),
            head: task.workflow.position,
            phase: task.workflow.body.phase,
        },
        BoardClaim::plane_witness(stamp, &task.claims),
    ]
}

#[cfg(test)]
mod tests;
