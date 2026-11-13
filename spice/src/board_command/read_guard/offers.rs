//! Recruitment exclusivity derived from live steering and exact asking authority.

use std::fmt::Write as _;

use super::*;

#[derive(Clone)]
pub(in crate::board_command) struct AskingOffer {
    task: wire::TaskStamp,
    phase: wire::Phase,
    generation: u64,
    inviter: wire::CanonicalUuid,
    address: Option<wire::OccupantAddress>,
    claim: Seq,
}

impl AskingOffer {
    pub(in crate::board_command) fn new(
        snapshot: &ValidatedSnapshot,
        selected: &Entry,
        inviter: wire::CanonicalUuid,
        claim: Seq,
    ) -> Self {
        Self {
            task: selected.born().clone(),
            phase: selected.workflow().phase,
            generation: selected.workflow().generation,
            inviter,
            address: snapshot
                .answer
                .addresses
                .get(&inviter)
                .and_then(wire::SeatAddress::address),
            claim,
        }
    }

    pub(in crate::board_command) fn admit(
        &self,
        bus: &Bus,
        accepting: Option<wire::CanonicalUuid>,
        at: Instant,
    ) -> Result<(), Outcome<CommandAnswer>> {
        let authority = self.clone();
        match bus.read(move |read| authority.check(read, accepting, at)) {
            Ok(()) => Ok(()),
            Err(bus::BusReadError::Head(GuardError::Refused(detail))) => Err(refusal(
                "task::invite-outstanding",
                None,
                detail.to_string(),
            )),
            Err(error) => Err(store_fault(
                "task::invite-heads-unreadable",
                error.to_string(),
            )),
        }
    }

    pub(in crate::board_command) fn guard(
        &self,
        write: &mut JournalWrite,
        accepting: Option<wire::CanonicalUuid>,
        at: Instant,
    ) {
        let authority = self.clone();
        write.guard(JournalGuard::new(move |read| {
            authority.check(read, accepting, at)
        }));
    }

    fn check(
        &self,
        read: &JournalRead<'_>,
        accepting: Option<wire::CanonicalUuid>,
        at: Instant,
    ) -> Result<(), GuardError> {
        let heads = read.live_claim_invitations(&self.task, self.claim)?;
        let mut current = heads.iter().filter(|head| {
            let head = head.body();
            head.sender == wire::Actor::Lane(self.inviter)
                && head
                    .control
                    .as_deref()
                    .and_then(|text| text.parse::<wire::TaskInviteControl>().ok())
                    .is_some_and(|control| {
                        control.claim() == self.claim
                            && invitation_current(
                                head,
                                &control,
                                (&self.phase, self.generation),
                                self.address,
                                Some(self.claim),
                                at,
                            )
                    })
        });
        let first = current.next();
        let second = current.next();
        if first.is_none() && accepting.is_none()
            || second.is_none()
                && first.is_some_and(|head| Some(head.body().directive) == accepting)
        {
            return Ok(());
        }
        let mut detail = format!(
            "asking claim {} requires a single live offer; current offers:",
            self.claim
        );
        for head in first.into_iter().chain(second).chain(current) {
            let head = head.body();
            write!(
                &mut detail,
                " {} to lane {} until {}; cancel with `spice steer cancel {}`;",
                head.key, head.recipient, head.expires, head.directive
            )
            .expect("writing to a string");
        }
        Err(GuardError::Refused(
            journal::GuardRefusal::new("task::invite-outstanding").with_detail(detail),
        ))
    }
}
