//! Replay of the claim positions governing each Design pass.
//!
//! A Design pass names one of its controller's active claim-head positions.
//! Winning, carrying, rebinding, and explicit generation acknowledgement set a
//! claim's position; the resident's lease timer never moves it. Retained
//! manual renewals also moved that position when they acknowledged a
//! generation. Replay follows the commit guard's acknowledgement rule
//! (`guard/generation.rs`) from facts alone: a transaction mutating the task
//! leaves every claim trailing except one that transaction won and the
//! committing holder's own.
//!
//! Only a win names a claim's identity. A lane holds at most one active claim
//! and an actor at most one per task, so every later claim fact selects its
//! claim among the task's active claims by what it already names: carrying
//! and rebinding by lane, freeing by holder, lapsing by occupant, and
//! acknowledgement or manual renewal by the committing holder.

use std::collections::{BTreeMap, BTreeSet};

use wire::{Actor, AnyFact, CanonicalUuid, FactKind, Instant, Seq, TaskStamp};

/// One active claim on a task as replay sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActiveClaim {
    /// Holder of the claim.
    pub(crate) actor: Actor,
    /// Lane bound to the claim.
    pub(crate) lane: CanonicalUuid,
    /// Occupant currently holding that lane's possession.
    pub(crate) occupant: CanonicalUuid,
    /// Position of the claim's current head.
    pub(crate) position: Seq,
}

/// Move one task's active claims across one claim fact at `seq`, committed by
/// `actor`.
pub(super) fn advance(claims: &mut Vec<ActiveClaim>, seq: Seq, actor: &Actor, fact: &AnyFact) {
    if let Some(won) = fact.read::<wire::body::ClaimWon>() {
        claims.push(ActiveClaim {
            actor: won.actor,
            lane: won.lane,
            occupant: won.occupant,
            position: seq,
        });
    } else if let Some(carried) = fact.read::<wire::body::ClaimCarried>() {
        if let Some(claim) = claims.iter_mut().find(|claim| claim.lane == carried.lane) {
            claim.occupant = carried.occupant;
            claim.position = seq;
        }
    } else if let Some(rebound) = fact.read::<wire::body::ClaimRebound>() {
        if let Some(claim) = claims.iter_mut().find(|claim| claim.lane == rebound.lane) {
            claim.occupant = rebound.occupant;
            claim.position = seq;
        }
    } else if fact.kind() == FactKind::ClaimAcknowledged {
        if let Some(claim) = claims.iter_mut().find(|claim| claim.actor == *actor) {
            claim.position = seq;
        }
    } else if let Some(freed) = fact.read::<wire::body::ClaimFreed>() {
        claims.retain(|claim| claim.actor != freed.actor);
    } else if let Some(lapsed) = fact.read::<wire::body::ClaimLapsed>() {
        claims.retain(|claim| claim.occupant != lapsed.occupant);
    }
}

/// Whether each active claim acknowledged its task's latest generation.
#[derive(Default)]
pub(super) struct Acknowledgements {
    claims: BTreeMap<String, Vec<Acknowledgement>>,
    /// Writer and commit instant shared by the facts of the transaction being
    /// replayed, beside the `(task, holder)` claims that transaction won.
    transaction: Option<(Actor, Instant)>,
    won: BTreeSet<(String, Actor)>,
}

struct Acknowledgement {
    holder: Actor,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    current: bool,
}

impl Acknowledgements {
    /// Observe one fact in history order, answering the task and holder whose
    /// claim head a retained manual renewal rewrote at this fact.
    pub(super) fn observe(
        &mut self,
        actor: &Actor,
        at: Instant,
        fact: &AnyFact,
    ) -> Option<(TaskStamp, Actor)> {
        let transaction = (actor.clone(), at);
        if self.transaction.as_ref() != Some(&transaction) {
            self.transaction = Some(transaction);
            self.won.clear();
        }
        if let Some(won) = fact.read::<wire::body::ClaimWon>() {
            self.won.insert((won.task.to_string(), won.actor.clone()));
            self.claims
                .entry(won.task.to_string())
                .or_default()
                .push(Acknowledgement {
                    holder: won.actor,
                    lane: won.lane,
                    occupant: won.occupant,
                    current: true,
                });
            return None;
        }
        if let Some(carried) = fact.read::<wire::body::ClaimCarried>() {
            if let Some(claim) = self.held(&carried.task, |claim| claim.lane == carried.lane) {
                claim.occupant = carried.occupant;
            }
            return None;
        }
        if let Some(rebound) = fact.read::<wire::body::ClaimRebound>() {
            if let Some(claim) = self.held(&rebound.task, |claim| claim.lane == rebound.lane) {
                claim.occupant = rebound.occupant;
                claim.current = true;
            }
            return None;
        }
        if let Some(acknowledged) = fact.read::<wire::body::ClaimAcknowledged>() {
            if let Some(claim) = self.held(&acknowledged.task, |claim| claim.holder == *actor) {
                claim.current = true;
            }
            return None;
        }
        if let Some(renewed) = fact.read::<wire::body::ClaimRenewed>() {
            if *actor == Actor::Spice {
                return None;
            }
            let claim = self.held(&renewed.task, |claim| claim.holder == *actor)?;
            if claim.current {
                return None;
            }
            claim.current = true;
            return Some((renewed.task, actor.clone()));
        }
        if let Some(freed) = fact.read::<wire::body::ClaimFreed>() {
            self.release(&freed.task, |claim| claim.holder == freed.actor);
            return None;
        }
        if let Some(lapsed) = fact.read::<wire::body::ClaimLapsed>() {
            self.release(&lapsed.task, |claim| claim.occupant == lapsed.occupant);
            return None;
        }
        for task in fact.mutated_tasks() {
            let Some(claims) = self.claims.get_mut(task.as_str()) else {
                continue;
            };
            for claim in claims {
                if claim.holder != *actor
                    && !self.won.contains(&(task.to_string(), claim.holder.clone()))
                {
                    claim.current = false;
                }
            }
        }
        None
    }

    fn held(
        &mut self,
        task: &TaskStamp,
        selects: impl Fn(&Acknowledgement) -> bool,
    ) -> Option<&mut Acknowledgement> {
        self.claims
            .get_mut(task.as_str())?
            .iter_mut()
            .find(|claim| selects(claim))
    }

    fn release(&mut self, task: &TaskStamp, selects: impl Fn(&Acknowledgement) -> bool) {
        if let Some(claims) = self.claims.get_mut(task.as_str()) {
            claims.retain(|claim| !selects(claim));
            if claims.is_empty() {
                self.claims.remove(task.as_str());
            }
        }
    }
}
