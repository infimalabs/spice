//! Why each lane is in a room, derived from admitted authority alone.

use std::collections::BTreeMap;

use wire::rooms as authority;
use wire::{CanonicalUuid, Seq, TaskStamp};

use super::{
    BoardView, ParticipantReason, RoomEventKind, RoomParticipant, RoomProjection, associated,
};

impl RoomProjection {
    /// Current logical-lane participants and the exact reasons for their
    /// presence. Publication history and native duties stay; authored room
    /// messages and typed contributions are participation, never attention,
    /// and do not end a leave. A Design request stands until the lane's next
    /// room publication, a leave included, answers or declines it.
    pub fn participants(
        &self,
        room: &TaskStamp,
        board: Option<&BoardView>,
    ) -> Vec<RoomParticipant> {
        let mut reasons = BTreeMap::<CanonicalUuid, Vec<ParticipantReason>>::new();
        let mut add = |lane: CanonicalUuid, reason| reasons.entry(lane).or_default().push(reason);
        if let Some(state) = self.rooms.get(room) {
            let mut leaves = BTreeMap::new();
            let mut published = BTreeMap::<CanonicalUuid, Seq>::new();
            for (publication, event) in &state.events {
                let lane = match &event.kind {
                    RoomEventKind::Messaged { source, .. } => {
                        let lane = source.head.lane;
                        add(lane, ParticipantReason::Publication(publication.clone()));
                        lane
                    }
                    RoomEventKind::Left { lane, clause } => {
                        leaves.insert(*lane, (publication, clause));
                        *lane
                    }
                    RoomEventKind::Contributed { lane, .. } => {
                        add(*lane, ParticipantReason::Publication(publication.clone()));
                        *lane
                    }
                    _ => continue,
                };
                let newest = published.entry(lane).or_insert(event.position);
                *newest = (*newest).max(event.position);
            }
            for (lane, ask) in &state.asked {
                if published
                    .get(lane)
                    .is_none_or(|answered| ask.position > *answered)
                {
                    add(
                        *lane,
                        ParticipantReason::Asked {
                            evidence: ask.evidence.clone(),
                            questions: ask.questions.clone(),
                            admission: ask.admission.clone(),
                        },
                    );
                }
            }
            for (lane, (publication, clause)) in leaves {
                add(
                    lane,
                    ParticipantReason::Left {
                        publication: publication.clone(),
                        clause: clause.clone(),
                    },
                );
            }
        }
        self.native_reasons(room, board, &mut add);
        reasons
            .into_iter()
            .map(|(lane, mut reasons)| {
                reasons.sort();
                reasons.dedup();
                RoomParticipant { lane, reasons }
            })
            .collect()
    }

    /// Whether any lane currently has a reason to receive bounded catch-up.
    ///
    /// Participation and a leave never listen; Design requests and native
    /// duties do, matching the resident's attention.
    #[must_use]
    pub fn has_current_listener(&self, room: &TaskStamp, board: Option<&BoardView>) -> bool {
        self.participants(room, board)
            .iter()
            .flat_map(|participant| &participant.reasons)
            .any(|reason| match reason {
                ParticipantReason::Publication(_) | ParticipantReason::Left { .. } => false,
                ParticipantReason::Asked { .. }
                | ParticipantReason::Work(_)
                | ParticipantReason::Stop(_) => true,
            })
    }

    /// Whether an operator-authored post is still unproved as heard.
    ///
    /// Current listening authority suppresses the mark immediately. A later
    /// lane-authored room event suppresses it permanently, including after
    /// that lane leaves and no current listening reason remains. Callers
    /// rendering several cards compute `has_current_listener` once.
    #[must_use]
    pub fn unheard_post(
        &self,
        room: &TaskStamp,
        publication: &authority::RoomAuthorityStamp,
        has_current_listener: bool,
    ) -> bool {
        let Some(state) = self.rooms.get(room) else {
            return false;
        };
        let Some(post) = state.events.get(publication) else {
            return false;
        };
        !has_current_listener
            && matches!(
                &post.kind,
                RoomEventKind::Published {
                    author: wire::Actor::Principal(_),
                    content: authority::RoomPublicationContent::Authored { .. },
                }
            )
            && state
                .latest_lane_event
                .is_none_or(|position| position <= post.position)
    }

    /// Reasons a lane listens regardless of any leave: a claim on associated
    /// work and the stop it holds.
    fn native_reasons(
        &self,
        room: &TaskStamp,
        board: Option<&BoardView>,
        add: &mut impl FnMut(CanonicalUuid, ParticipantReason),
    ) {
        for task in board.into_iter().flat_map(|board| &board.tasks) {
            let statement = &task.statement.body;
            if associated(statement, room) {
                for claim in task.active_claims() {
                    add(
                        claim.claim.body.lane,
                        ParticipantReason::Work(statement.born.clone()),
                    );
                }
            }
        }
        if let Some((stop, lane)) = self.stop_holder(room, board) {
            add(lane, ParticipantReason::Stop(stop.value.key.clone()));
        }
    }
}
