//! Private task-claim notices for one exact occupant: a stale claim
//! generation, a resident-minted assignment and its withdrawal once the claim
//! ends, shared-round updates, a lapse, a taste hold a lane's own statement
//! write left standing, and a current claim's completion contract moving with
//! its linked work. Each is a keyless background aside under a per-task
//! purpose, so a later notice for the same task replaces an earlier one still
//! queued at omp's next step boundary.

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use wire::CanonicalUuid;

use super::super::super::RuntimePaths;
use super::{Driver, Kick, send};

/// One private task-authority reminder, scoped to the claim's exact occupant.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resident) struct TaskGenerationNotice {
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) task: String,
    pub(in crate::resident) claim: u64,
    pub(in crate::resident) generation: u64,
    pub(in crate::resident) change: String,
    pub(in crate::resident) editor: String,
    pub(in crate::resident) position: u64,
    /// The statement's taste hold as it stands now, when the holder must hear it.
    #[serde(default)]
    pub(in crate::resident) taste: Option<TasteState>,
    pub(in crate::resident) completion_contract: String,
    #[serde(default)]
    pub(in crate::resident) design_research: Option<String>,
}

/// Where a claimant's statement stands against the taste policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::resident) enum TasteState {
    /// Held: each place the current policy matched, findings withheld, and the
    /// rewordings the hold has already survived.
    Held { matches: Vec<String>, rechecks: u32 },
    /// A statement edit left the statement clear of taste review.
    Clear,
}

impl TasteState {
    fn clause(&self, task: &str) -> String {
        match self {
            Self::Held { matches, rechecks } => format!(
                " Its statement stands held for taste review at {} after {rechecks} \
                 recheck(s); no phase completes until a statement field (--title, \
                 --pitch, --description, or --term) reworded through \
                 `spice task modify {task} --title \"...\"` passes.",
                matches.join("; "),
            ),
            Self::Clear => " Its statement now stands clear of taste review.".to_owned(),
        }
    }
}

/// A taste hold one lane's statement write left standing, for that lane's
/// current occupant whether or not the lane holds the task.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resident) struct TasteHoldNotice {
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) task: String,
    /// Journal position of the statement write that left the hold.
    pub(in crate::resident) position: u64,
    /// Each place the policy matched, findings withheld.
    pub(in crate::resident) matches: Vec<String>,
    pub(in crate::resident) completion_contract: String,
}

impl TasteHoldNotice {
    fn text(&self) -> String {
        format!(
            "<system-notice>\nThis lane's statement write at position {} left task {} held \
             for taste review at {}. No phase of it completes while the hold stands, \
             whichever lane holds it. Reword a statement field (--title, --pitch, \
             --description, or --term) with `spice task modify {} --title \"...\"` \
             so the complete current taste policy rechecks it. This is a private taste \
             notice; no acknowledgment key or visible ACK/NAK is required. {}\n</system-notice>",
            self.position,
            self.task,
            self.matches.join("; "),
            self.task,
            self.completion_contract,
        )
    }
}

/// One private proactive-assignment notice, scoped to the claim's exact occupant.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resident) struct TaskAssignmentNotice {
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) task: String,
    pub(in crate::resident) claim: u64,
    pub(in crate::resident) phase: String,
    pub(in crate::resident) completion_contract: String,
    #[serde(default)]
    pub(in crate::resident) design_research: Option<String>,
}

#[derive(Clone, Default)]
pub(super) struct TaskDisclosureState {
    assignment: Option<TaskAssignmentNotice>,
    contract: Option<TaskContractNotice>,
    events: BTreeSet<(String, u64, u64)>,
}

impl TaskDisclosureState {
    fn retain_claim(&mut self, task: &str, claim: u64) {
        self.events
            .retain(|(event_task, event_claim, _)| event_task == task && *event_claim == claim);
    }

    fn disclosed(&self, task: &str, claim: u64) -> bool {
        self.assignment
            .as_ref()
            .is_some_and(|notice| notice.task == task && notice.claim == claim)
            || self
                .contract
                .as_ref()
                .is_some_and(|notice| notice.task == task && notice.claim == claim)
    }
}

/// One durable shared-round event delivered to an already-active partner.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::resident) enum TaskRoundUpdateKind {
    Joined,
    Arrived,
}

/// One participant join or arrival, scoped to the receiving claim.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resident) struct TaskRoundUpdateNotice {
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) task: String,
    pub(in crate::resident) holder_claim: u64,
    pub(in crate::resident) event: u64,
    pub(in crate::resident) phase: String,
    pub(in crate::resident) participant: String,
    pub(in crate::resident) action: TaskRoundUpdateKind,
}

impl TaskRoundUpdateNotice {
    fn text(&self) -> String {
        let action = match self.action {
            TaskRoundUpdateKind::Joined => "joined",
            TaskRoundUpdateKind::Arrived => "arrived",
        };
        format!(
            "<system-notice>\nA participant {action} task {} in phase {}: {}. Coordinate in \
             the task room; participant room messages may wake their addressed partner. This \
             is a private round update; no acknowledgment key or visible ACK/NAK is \
             required.\n</system-notice>",
            self.task, self.phase, self.participant,
        )
    }
}

/// One ended claim, scoped to the exact occupant that held it when it ended.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resident) struct TaskAssignmentEnd {
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) task: String,
    pub(in crate::resident) claim: u64,
}

impl TaskAssignmentNotice {
    fn text(&self) -> String {
        format!(
            "<system-notice>\nTask {} was assigned to this lane in phase {}. Continue \
             this task now; run `spice task show {} --full` for its contract before asking \
             the allocator for work. When your share is complete, run \
             `spice task done --validation \"...\"` (review: `spice task review`), then \
             `spice task next`; do not wait for partners. This is a private assignment \
             notice; no acknowledgment key or visible ACK/NAK is required. {} {}\n</system-notice>",
            self.task,
            self.phase,
            self.task,
            self.completion_contract,
            self.design_research.as_deref().unwrap_or(""),
        )
    }
}

/// Current completion and research guidance for an acknowledged claim after
/// its initial assignment or claim answer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resident) struct TaskContractNotice {
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) task: String,
    pub(in crate::resident) claim: u64,
    pub(in crate::resident) completion_contract: String,
    #[serde(default)]
    pub(in crate::resident) design_research: Option<String>,
}

impl TaskContractNotice {
    fn text(&self) -> String {
        format!(
            "<system-notice>\nCurrent work guidance for task {}: {} {} This is a private \
             authority reminder; no acknowledgment key or visible ACK/NAK is required.\n</system-notice>",
            self.task,
            self.completion_contract,
            self.design_research.as_deref().unwrap_or(""),
        )
    }
}

/// One private lapse notice for the lane that held the expired claim, scoped to
/// that lane's current occupant: a restart may have re-seated the lane since
/// the claim was won.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::resident) struct ClaimLapseNotice {
    pub(in crate::resident) occupant: CanonicalUuid,
    pub(in crate::resident) task: String,
    pub(in crate::resident) expiry: wire::Instant,
}

impl<W: Write + Send + 'static> Driver<W> {
    /// Replace a stale-claim reminder without interrupting current work.
    pub(super) fn deliver_task_generation(&self, notice: &TaskGenerationNotice) {
        let previous = {
            let mut delivered = self
                .task_generation
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if delivered.as_ref().is_some_and(|(key, generation)| {
                key.task == notice.task
                    && key.claim == notice.claim
                    && (*generation > notice.generation
                        || (*generation == notice.generation
                            && key.completion_contract == notice.completion_contract
                            && key.design_research == notice.design_research))
            }) {
                return;
            }
            delivered.replace((
                TaskContractNotice {
                    occupant: notice.occupant,
                    task: notice.task.clone(),
                    claim: notice.claim,
                    completion_contract: notice.completion_contract.clone(),
                    design_research: notice.design_research.clone(),
                },
                notice.generation,
            ))
        };
        let taste = notice
            .taste
            .as_ref()
            .map(|taste| taste.clause(&notice.task))
            .unwrap_or_default();
        let message = format!(
            "<system-notice>\nTask {} changed after this claim last acknowledged it: \
             generation {} is {} by {} at position {}.{taste} Run `spice task next` \
             to read and accept the current statement before continuing. Completion and review remain \
             refused until task next succeeds. This is a private authority reminder; \
             no acknowledgment key or visible ACK/NAK is required. {} {}\n</system-notice>",
            notice.task,
            notice.generation,
            notice.change,
            notice.editor,
            notice.position,
            notice.completion_contract,
            notice.design_research.as_deref().unwrap_or(""),
        );
        if self
            .background(&format!("task:{}", notice.task), &message)
            .is_ok()
        {
            self.disclosed_contract(
                notice.occupant,
                &notice.task,
                notice.claim,
                &notice.completion_contract,
                notice.design_research.as_deref(),
            );
        } else {
            let mut delivered = self
                .task_generation
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if delivered.as_ref().is_some_and(|(key, generation)| {
                key.task == notice.task
                    && key.claim == notice.claim
                    && *generation == notice.generation
                    && key.completion_contract == notice.completion_contract
                    && key.design_research == notice.design_research
            }) {
                *delivered = previous;
            }
        }
    }

    /// Record the completion and research guidance last told for one claim.
    fn disclosed_contract(
        &self,
        occupant: CanonicalUuid,
        task: &str,
        claim: u64,
        contract: &str,
        research: Option<&str>,
    ) {
        let mut disclosed = self
            .task_disclosure
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        disclosed.retain_claim(task, claim);
        disclosed.contract = Some(TaskContractNotice {
            occupant,
            task: task.to_owned(),
            claim,
            completion_contract: contract.to_owned(),
            design_research: research.map(str::to_owned),
        });
    }
    /// Tell this occupant once each time its current claim's completion or
    /// research guidance changes from the last disclosure.
    ///
    /// The first observation of a claim is the contract its claim answer, its
    /// acknowledgement, or a generation notice already disclosed, so it is
    /// recorded rather than repeated.
    pub(super) fn deliver_task_contract(&self, notice: &TaskContractNotice) {
        let (previous, first_observation) = {
            let mut disclosed = self
                .task_disclosure
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if disclosed.contract.as_ref() == Some(notice) {
                return;
            }
            let same_claim = disclosed
                .contract
                .as_ref()
                .is_some_and(|key| key.task == notice.task && key.claim == notice.claim);
            let previous = disclosed.clone();
            disclosed.retain_claim(&notice.task, notice.claim);
            disclosed.contract = Some(notice.clone());
            (previous, !same_claim)
        };
        if first_observation {
            return;
        }
        if self
            .background(&format!("contract:{}", notice.task), &notice.text())
            .is_err()
        {
            let mut disclosed = self
                .task_disclosure
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if disclosed.contract.as_ref() == Some(notice) {
                *disclosed = previous;
            }
        }
    }

    /// Deliver one task-claim notice addressed to this exact occupant; a
    /// notice for another occupant of the lane is dropped.
    pub(super) fn deliver_task_notice(&self, occupant: CanonicalUuid, kick: Kick) {
        match kick {
            Kick::TaskGeneration(notice) if notice.occupant == occupant => {
                self.deliver_task_generation(&notice);
            }
            Kick::TaskContract(notice) if notice.occupant == occupant => {
                self.deliver_task_contract(&notice);
            }
            Kick::TaskAssignment(notice) if notice.occupant == occupant => {
                self.deliver_task_assignment(&notice);
            }
            Kick::TaskRoundUpdate(notice) if notice.occupant == occupant => {
                self.deliver_task_round_update(&notice);
            }
            Kick::TaskAssignmentEnded(ended) if ended.occupant == occupant => {
                self.withdraw_task_assignment(&ended);
            }
            Kick::ClaimLapse(notice) if notice.occupant == occupant => {
                self.deliver_claim_lapse(&notice);
            }
            Kick::TasteHold(notice) if notice.occupant == occupant => {
                self.deliver_taste_hold(&notice);
            }
            _ => {}
        }
    }

    /// Tell this exact occupant about one resident-minted assignment.
    pub(super) fn deliver_task_assignment(&self, notice: &TaskAssignmentNotice) {
        let previous =
            {
                let mut disclosed = self
                    .task_disclosure
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if disclosed.assignment.as_ref().is_some_and(|told| {
                    told.occupant == notice.occupant
                        && told.task == notice.task
                        && told.claim == notice.claim
                }) {
                    return;
                }
                if disclosed.assignment.as_ref().is_some_and(|told| {
                    told.occupant == notice.occupant && told.claim > notice.claim
                }) {
                    return;
                }
                let previous = disclosed.clone();
                disclosed.retain_claim(&notice.task, notice.claim);
                disclosed.assignment = Some(notice.clone());
                previous
            };
        if self
            .background(&format!("task:{}", notice.task), &notice.text())
            .is_err()
        {
            let mut disclosed = self
                .task_disclosure
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if disclosed.assignment.as_ref() == Some(notice) {
                *disclosed = previous;
            }
        }
    }

    /// Tell this occupant one durable round event once, and only after its own
    /// claim is disclosed; an earlier event is dropped and reprojected.
    pub(super) fn deliver_task_round_update(&self, notice: &TaskRoundUpdateNotice) {
        let key = (notice.task.clone(), notice.holder_claim, notice.event);
        {
            let mut disclosed = self
                .task_disclosure
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !disclosed.disclosed(&notice.task, notice.holder_claim)
                || !disclosed.events.insert(key.clone())
            {
                return;
            }
        }
        let purpose = format!(
            "round:{}:{}:{}",
            notice.task, notice.holder_claim, notice.event
        );
        if self.background(&purpose, &notice.text()).is_err() {
            self.task_disclosure
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .events
                .remove(&key);
        }
    }

    /// Withdraw this occupant's assignment notice for a claim that has ended,
    /// if omp still queues it. The notice stays recorded as delivered, so a
    /// later observation of the same claim never sends it again; any other
    /// notice queued under the task's purpose is left as it stands.
    pub(super) fn withdraw_task_assignment(&self, ended: &TaskAssignmentEnd) {
        let told = self
            .task_disclosure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .assignment
            .clone();
        let Some(told) = told.filter(|told| told.task == ended.task && told.claim == ended.claim)
        else {
            return;
        };
        let text = told.text();
        let _ = self.update_background(&format!("task:{}", told.task), |queued| {
            if queued == text {
                String::new()
            } else {
                queued.to_owned()
            }
        });
    }

    /// Tell this occupant that a claim its lane held lapsed, once per lapse.
    pub(super) fn deliver_claim_lapse(&self, notice: &ClaimLapseNotice) {
        deliver_once(&self.claim_lapse, notice, || {
            let message = format!(
                "<system-notice>\nThe claim this lane held on task {} lapsed: its lease expired \
                 at {} and the resident released it. This lane no longer holds that task; stop \
                 working on it. Commits made for it and not yet published are loose and cannot \
                 complete it; run `spice task show {}` for its current holder before asking the \
                 allocator for work. This is a private lapse notice; no acknowledgment key or \
                 visible ACK/NAK is required.\n</system-notice>",
                notice.task, notice.expiry, notice.task,
            );
            self.background(&format!("task:{}", notice.task), &message)
        });
    }

    /// Tell this occupant once that its own statement write left a task held.
    ///
    /// The purpose is the task's taste rather than the task itself, so the
    /// notice never displaces a queued assignment for the same task.
    pub(super) fn deliver_taste_hold(&self, notice: &TasteHoldNotice) {
        deliver_once(&self.taste_hold, notice, || {
            self.background(&format!("taste:{}", notice.task), &notice.text())
        });
    }
}

/// Run `write` once per distinct notice; a failed write forgets the delivery so
/// the next observation of the same notice retries it.
fn deliver_once<T: Clone + PartialEq, R, E>(
    delivered: &Mutex<Option<T>>,
    notice: &T,
    write: impl FnOnce() -> Result<R, E>,
) {
    let previous = {
        let mut slot = delivered.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref() == Some(notice) {
            return;
        }
        slot.replace(notice.clone())
    };
    if write().is_err() {
        let mut slot = delivered.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref() == Some(notice) {
            *slot = previous;
        }
    }
}

/// Privately notify the exact claimant occupant that its task authority moved.
pub(in crate::resident) fn task_generation(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    notice: TaskGenerationNotice,
) {
    send(paths, lane, &Kick::TaskGeneration(notice));
}

/// Privately tell the exact claimant occupant that its current claim's
/// completion contract stands as projected now.
pub(in crate::resident) fn task_contract(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    notice: TaskContractNotice,
) {
    send(paths, lane, &Kick::TaskContract(notice));
}

/// Privately notify the exact occupant about a resident-minted assignment.
pub(in crate::resident) fn task_assignment(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    notice: TaskAssignmentNotice,
) {
    send(paths, lane, &Kick::TaskAssignment(notice));
}

/// Privately notify one disclosed active holder about a participant's event.
pub(in crate::resident) fn task_round_update(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    notice: TaskRoundUpdateNotice,
) {
    send(paths, lane, &Kick::TaskRoundUpdate(notice));
}

/// Privately tell the exact occupant that a claim it held has ended.
pub(in crate::resident) fn task_assignment_ended(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    ended: TaskAssignmentEnd,
) {
    send(paths, lane, &Kick::TaskAssignmentEnded(ended));
}

/// Privately tell the lane that held a lapsed claim, at its current occupant.
pub(in crate::resident) fn claim_lapse(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    notice: ClaimLapseNotice,
) {
    send(paths, lane, &Kick::ClaimLapse(notice));
}

/// Privately tell the lane whose statement write left a task held, at its
/// current occupant.
pub(in crate::resident) fn taste_hold(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    notice: TasteHoldNotice,
) {
    send(paths, lane, &Kick::TasteHold(notice));
}
