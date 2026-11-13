//! Which current reads a batch of admitted facts invalidated.

use std::collections::BTreeSet;

use super::AppRuntime;
use crate::LaneView;
use crate::socket::Admitted;
use wire::intent::LaneLiveness;
use wire::{CanonicalUuid, FactKind, StreamFrame};

/// Which cached reads a batch of admitted facts has invalidated.
///
/// The stream carries facts, not snapshots: it advances the journal cursor and
/// leaves every read the surface is drawn from exactly as stale as it was.
/// That is not survivable for the watchband, which claims to show what a lane
/// is doing right now. Uncorrected, the shell reports itself online with a
/// current cursor while a tile shows a directive two events old — worse than
/// showing nothing, because the staleness is invisible.
///
/// This lives beside [`AppRuntime::apply_board`], [`AppRuntime::apply_lanes`],
/// [`AppRuntime::apply_inbox`] — one field per read it names — rather than
/// in the socket loop that acts on it, so it can be tested off `wasm32`.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Stale {
    board: bool,
    lanes: bool,
    inbox: bool,
    consent: bool,
    settings: bool,
    principals: bool,
    task_details: BTreeSet<wire::TaskStamp>,
    task_relations: Vec<super::TaskRelationInvalidation>,
    /// Keys of steering moves whose room, if any, only the delivery
    /// projection still knows.
    steering_keys: Vec<String>,
    /// Inviter lanes whose occupant authority moved. Their currently shown
    /// invitation tasks are resolved from admitted detail projections before
    /// those projections are invalidated.
    invitation_inviters: BTreeSet<CanonicalUuid>,
}

impl Stale {
    /// Repository policy moved independently of the journal's task facts.
    pub fn invalidate_configuration(&mut self) {
        self.board = true;
    }

    /// Widen this set by whatever one admitted fact invalidated.
    ///
    /// The fact namespace selects broad snapshot reads here, except for the
    /// facts outside the task namespace that still move task heads.
    pub fn widen(&mut self, kind: FactKind) {
        // Pass-cycle evidence and execution authorization change Design
        // standing and allocator eligibility carried by the board.
        if wire::rooms::DesignCycles::concerns(kind) {
            self.board = true;
        }
        // Membership changes can change ownership and exclusive reservations
        // even when no task head moved.
        if matches!(kind, FactKind::LaneSeated | FactKind::LaneRemoved) {
            self.board = true;
        }
        match kind.as_str().split_once("::") {
            Some(("steer", _)) => {
                self.inbox = true;
            }
            // The inbox is keyed on the lane roster, not only on the steer
            // ledger: the resident builds one row per seated lane and carries
            // that lane's occupant on it, so a lane that arrives, departs or
            // changes occupant moves the inbox even though no directive was
            // sent. Widening only `lanes` here left a newly seated lane with
            // no inbox row in the browser, and the console reads its residency
            // witness out of that row -- so the first direction sent to a lane
            // seated from the app was dropped before it reached the wire.
            Some(("lane", _)) => {
                self.lanes = true;
                self.inbox = true;
            }
            Some(("team", _)) => {
                self.board = true;
                self.lanes = true;
            }
            Some(("consent", _)) => self.consent = true,
            // The settings read carries every principal's display identity,
            // and the principal listing their roles and capabilities.
            Some(("principal", _)) => {
                self.settings = true;
                self.principals = true;
            }
            Some(("task", _)) => self.board = true,
            // A claim binds a task to a lane, so both sides of that join move.
            Some(("claim", _)) => {
                self.board = true;
                self.lanes = true;
            }
            _ => {}
        }
    }

    fn widen_fact(&mut self, fact: &wire::StreamFact) {
        self.widen(fact.kind());
        if let Some(task) = fact.task() {
            self.task_details.insert(task);
        }
        if let Some(("steer", _)) = fact.kind().as_str().split_once("::") {
            match fact.read::<wire::body::SteerSent>() {
                Some(sent) => self.task_details.extend(sent.room),
                None => self.steering_keys.extend(
                    fact.body()
                        .get("key")
                        .and_then(|key| key.as_str())
                        .map(str::to_owned),
                ),
            }
        }
        if let Some(("lane", _)) = fact.kind().as_str().split_once("::")
            && let Some(lane) = fact
                .body()
                .get("lane")
                .and_then(|lane| lane.as_str())
                .and_then(|lane| lane.parse().ok())
        {
            self.invitation_inviters.insert(lane);
        }
        if let Some(authorized) = fact.read::<wire::rooms::ExecutionAuthorized>() {
            self.task_details
                .insert(authorized.authorization.decision.controller);
            self.task_details
                .extend(authorized.authorization.selected_outputs);
        }
        if let Some(change) = super::TaskRelationInvalidation::from_fact(fact) {
            self.task_relations.push(change);
        }
    }

    /// Mark the task rooms the recorded steering moves speak in, read from
    /// `conversation` before those moves are applied to it, since a retired
    /// delivery no record names leaves the projection as it retires.
    pub fn resolve_steering_rooms(&mut self, conversation: &super::rooms::ConversationProjection) {
        for key in self.steering_keys.drain(..) {
            if let Some(room) = conversation
                .keyed_delivery(&key)
                .and_then(|delivery| delivery.value.room.clone())
            {
                self.task_details.insert(room);
            }
        }
    }

    /// Resolve the invitation surfaces this batch moves against what the
    /// runtime currently shows.
    ///
    /// A still-shown invitation row issued by a lane whose occupant authority
    /// moved re-reads its task detail; recipient-lane moves are intentionally
    /// absent, since an invitation binds a lane lineage and survives its
    /// reseating. A pending invitation into a task this batch touched (its
    /// claim released, its phase or generation moved) re-reads the inbox, so
    /// its ACK and NAK leave with the inviter's authority instead of waiting
    /// for unrelated steering.
    pub fn resolve_invitations(&mut self, runtime: &AppRuntime) {
        self.task_details
            .extend(runtime.invitation_tasks_from(&self.invitation_inviters));
        self.invitation_inviters.clear();
        self.inbox = self.inbox
            || !self.task_details.is_empty()
                && runtime.inbox().is_some_and(|inbox| {
                    inbox.lanes.iter().any(|lane| {
                        lane.pending.iter().any(|directive| {
                            directive
                                .control
                                .as_deref()
                                .and_then(|control| control.parse::<wire::TaskInviteControl>().ok())
                                .is_some_and(|control| self.task_details.contains(control.task()))
                        })
                    })
                });
    }

    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    pub(crate) fn invalidate_task_details(&self, runtime: &mut AppRuntime, changed: bool) {
        if changed {
            for task in &self.task_details {
                runtime.invalidate_task_detail(task);
            }
        }
    }

    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    pub(crate) fn resolve_task_relations(&mut self, runtime: &mut AppRuntime, changed: bool) {
        if changed {
            runtime.invalidate_task_relations(&self.task_relations);
        }
        self.task_relations.clear();
    }

    /// Affected open task details, in durable identity order.
    pub fn task_details(&self) -> impl Iterator<Item = &wire::TaskStamp> {
        self.task_details.iter()
    }

    /// Re-read a quiet occupant when a live message may have resumed activity.
    ///
    /// The resident still decides liveness; replayed messages cannot make a
    /// process working locally. Rows already heard from need no redundant read.
    pub fn invalidate_quiet_lane(&mut self, view: Option<&LaneView>, lane: CanonicalUuid) {
        self.lanes |= view.is_some_and(|view| {
            view.lanes.iter().any(|team| {
                team.member(lane)
                    .is_some_and(|member| member.liveness == LaneLiveness::Running)
            })
        });
    }

    /// Re-read only the current projections one live message generation moved.
    ///
    /// A final answer is authoritative turn-ending evidence, but it does not
    /// locally decide the lane's resulting residency or supervision state.
    /// Settled acknowledgements likewise prove the inbox moved without
    /// carrying its complete current contents.
    pub fn invalidate_messages(
        &mut self,
        view: Option<&LaneView>,
        messages: &[wire::AgentMessage],
    ) {
        for message in messages {
            self.invalidate_quiet_lane(view, message.lane);
            self.lanes |= message.final_answer;
            self.inbox |= matches!(
                message.acknowledgement,
                Some(
                    wire::MessageAcknowledgement::Acknowledged
                        | wire::MessageAcknowledgement::Declined
                )
            );
        }
    }

    /// Whether the board read went stale.
    #[must_use]
    pub const fn board(&self) -> bool {
        self.board
    }

    /// Whether the lane read went stale.
    #[must_use]
    pub const fn lanes(&self) -> bool {
        self.lanes
    }

    /// Whether the steering inbox read went stale.
    #[must_use]
    pub const fn inbox(&self) -> bool {
        self.inbox
    }

    /// Whether the operator consent queue read went stale.
    #[must_use]
    pub const fn consent(&self) -> bool {
        self.consent
    }

    /// Whether the settings read, and the principal identities it carries,
    /// went stale.
    #[must_use]
    pub const fn settings(&self) -> bool {
        self.settings
    }

    /// Whether the principal listing went stale.
    #[must_use]
    pub const fn principals(&self) -> bool {
        self.principals
    }

    /// Whether anything needs re-reading at all.
    #[must_use]
    pub fn any(&self) -> bool {
        self.board
            || self.lanes
            || self.inbox
            || self.consent
            || self.settings
            || self.principals
            || !self.task_details.is_empty()
            || !self.invitation_inviters.is_empty()
            || !self.task_relations.is_empty()
    }
}
/// Projects current-read invalidation from live facts or complete backlog tails, never history pages.
#[must_use]
pub fn stale_reads(admitted: &Admitted<StreamFrame>) -> Stale {
    let mut stale = Stale::default();
    match admitted.get() {
        StreamFrame::Fact { fact, .. } => stale.widen_fact(fact),
        StreamFrame::Facts { packed, .. } if packed.overflow.is_none() => {
            packed.rows.iter().for_each(|row| stale.widen_fact(row));
        }
        _ => {}
    }
    stale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_messages_invalidate_only_the_authority_they_move() {
        let ordinary = message();
        let mut stale = Stale::default();
        stale.invalidate_messages(None, std::slice::from_ref(&ordinary));
        assert!(!stale.lanes());
        assert!(!stale.inbox());

        let mut final_answer = ordinary.clone();
        final_answer.final_answer = true;
        final_answer.acknowledgement = Some(wire::MessageAcknowledgement::Pending);
        let mut stale = Stale::default();
        stale.invalidate_messages(None, &[final_answer]);
        assert!(stale.lanes());
        assert!(!stale.inbox());

        for acknowledgement in [
            wire::MessageAcknowledgement::Acknowledged,
            wire::MessageAcknowledgement::Declined,
        ] {
            let mut settled = ordinary.clone();
            settled.acknowledgement = Some(acknowledgement);
            let mut stale = Stale::default();
            stale.invalidate_messages(None, &[settled]);
            assert!(!stale.lanes());
            assert!(stale.inbox());
        }
    }

    fn message() -> wire::AgentMessage {
        wire::AgentMessage {
            lane: uuid(1),
            author: uuid(2),
            locus: wire::MessageLocus {
                member: 1,
                offset: 1,
                length: 1,
                event: 0,
            },
            prose_origin: None,
            authored: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
            authored_synthetic: false,
            observed: None,
            activity: wire::AgentActivityKind::Prose,
            compaction_method: None,
            final_answer: false,
            acknowledgement: None,
            text: "answer".to_owned(),
            context_bytes: 0,
            provider_error: None,
            quote_target: None,
            directive_contexts: Vec::new(),
            sends: Vec::new(),
            quotes: Vec::new(),
            automatic_speech: None,
            media: Vec::new(),
            media_quotes: Vec::new(),
            markdown_images: Vec::new(),
        }
    }

    fn uuid(value: u8) -> CanonicalUuid {
        format!("00000000-0000-4000-8000-{value:012}")
            .parse()
            .expect("fixture UUID")
    }
}
