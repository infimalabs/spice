//! Disposable outbox indexes recovered from immutable admission and judgment facts.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, mpsc};

use journal::crossing::{CanonicalUuid, Digest, FactKind, Instant, Seq, outbox};
use journal::{FactReadBudget, GuardError, Journal};

use super::{BusError, BusReadError, BusReader, Request};

/// One admission with its original packet judgments at a writer barrier.
#[derive(Clone, Debug)]
pub struct OutboxSubmission {
    /// Journal position admitting the complete authored body.
    pub accepted: Seq,
    /// Journal instant admitting it: the observed time of its timestamp-less
    /// host record, shared by live and replayed presentation.
    pub at: Instant,
    /// Canonical source and exact author of that body.
    pub body: outbox::Accepted,
    /// Decided packets in stable source order; missing slots remain unresolved.
    pub packets: Vec<Option<outbox::PacketReceipt>>,
}

/// Indexed reads needed by admission, projection, inspection and recovery.
#[derive(Clone, Debug)]
pub enum OutboxQuery {
    /// Resolve one stable submission reference.
    Submission(CanonicalUuid),
    /// Find a previous occupant/body admission of an outbox write; the caller
    /// still compares exact bytes. An invitation's message is one admission
    /// per captured record, never found by its body.
    Body {
        /// Authenticated authoring lane.
        lane: CanonicalUuid,
        /// Exact author, not its successors.
        occupant: CanonicalUuid,
        /// Digest of the complete authored body.
        digest: Digest,
    },
    /// Interpret one exact captured tape record only after its admission.
    Source {
        /// Lane owning the tape.
        lane: CanonicalUuid,
        /// Exact tape generation.
        source: CanonicalUuid,
        /// Tape member.
        member: u64,
        /// Record byte offset.
        offset: u64,
    },
    /// Recover a bounded number of unfinished admissions in original order.
    Pending {
        /// Greatest number returned in one recovery step.
        limit: usize,
    },
}

impl BusReader {
    /// Read admitted bodies and original judgments without running any packets.
    pub fn outbox(&self, query: OutboxQuery) -> Result<Vec<Arc<OutboxSubmission>>, BusReadError> {
        let (answer, receiver) = mpsc::sync_channel(1);
        self.requests
            .send(Request::Outbox { query, answer })
            .map_err(|_| BusReadError::Bus(BusError::Closed))?;
        receiver
            .recv()
            .map_err(|_| BusReadError::Bus(BusError::Closed))?
            .map_err(BusReadError::Head)
    }
}

#[derive(Default)]
pub(super) struct OutboxState {
    cursor: Option<Seq>,
    submissions: BTreeMap<CanonicalUuid, Arc<OutboxSubmission>>,
    bodies: BTreeMap<(CanonicalUuid, CanonicalUuid, Digest), CanonicalUuid>,
    sources: BTreeMap<(CanonicalUuid, CanonicalUuid, u64, u64), CanonicalUuid>,
    pending: BTreeSet<(Seq, CanonicalUuid)>,
}

impl OutboxState {
    pub(super) fn select(
        &mut self,
        journal: &Journal,
        query: OutboxQuery,
    ) -> Result<Vec<Arc<OutboxSubmission>>, GuardError> {
        journal.read(|read| {
            while self.cursor != read.cursor()? {
                let page = read.selected_fact_page(
                    &[FactKind::OutboxAccepted, FactKind::OutboxJudged],
                    self.cursor,
                    None,
                    FactReadBudget::RETAINED,
                )?;
                for fact in &page.rows {
                    self.apply(fact)?;
                }
                self.cursor = page.continuation.or(page.horizon);
            }
            Ok(self.selected(query))
        })
    }

    fn selected(&self, query: OutboxQuery) -> Vec<Arc<OutboxSubmission>> {
        let identity = match query {
            OutboxQuery::Submission(identity) => Some(identity),
            OutboxQuery::Body {
                lane,
                occupant,
                digest,
            } => self.bodies.get(&(lane, occupant, digest)).copied(),
            OutboxQuery::Source {
                lane,
                source,
                member,
                offset,
            } => self.sources.get(&(lane, source, member, offset)).copied(),
            OutboxQuery::Pending { limit } => {
                return self
                    .pending
                    .iter()
                    .take(limit)
                    .filter_map(|(_, identity)| self.submissions.get(identity).cloned())
                    .collect();
            }
        };
        identity
            .and_then(|identity| self.submissions.get(&identity).cloned())
            .into_iter()
            .collect()
    }

    fn apply(
        &mut self,
        envelope: &journal::crossing::Envelope<journal::crossing::AnyFact>,
    ) -> Result<(), GuardError> {
        if let Some(body) = envelope.fact.read::<outbox::Accepted>() {
            let count = usize::try_from(body.packets)
                .map_err(|_| invalid("packet count cannot be represented"))?;
            if count > outbox::PACKETS_MAX || self.submissions.contains_key(&body.submission) {
                return Err(invalid("duplicate admission or invalid packet count"));
            }
            let source = &body.hunk;
            let source_key = (
                body.lane,
                source.head.source,
                source.locus.member,
                source.locus.offset,
            );
            // A repeated outbox body is one submission; a repeated invitation
            // message is another invitation's, so only its record is unique.
            let body_key = body
                .invitation
                .is_none()
                .then(|| (body.lane, body.occupant, body.digest.clone()));
            if self.sources.contains_key(&source_key)
                || body_key
                    .as_ref()
                    .is_some_and(|key| self.bodies.contains_key(key))
            {
                return Err(invalid(
                    "one source or occupant/body has multiple admissions",
                ));
            }
            self.sources.insert(source_key, body.submission);
            if let Some(body_key) = body_key {
                self.bodies.insert(body_key, body.submission);
            }
            if count != 0 {
                self.pending.insert((envelope.seq, body.submission));
            }
            self.submissions.insert(
                body.submission,
                Arc::new(OutboxSubmission {
                    accepted: envelope.seq,
                    at: envelope.at,
                    body,
                    packets: vec![None; count],
                }),
            );
        } else if let Some(judgment) = envelope.fact.read::<outbox::Judged>() {
            let submission = self
                .submissions
                .get_mut(&judgment.submission)
                .ok_or_else(|| invalid("judgment precedes its admission"))?;
            let submission = Arc::make_mut(submission);
            let slot = submission
                .packets
                .get_mut(judgment.packet as usize)
                .ok_or_else(|| invalid("judgment names a nonexistent packet"))?;
            if slot.is_some() {
                return Err(invalid("a decided packet was judged twice"));
            }
            *slot = Some(outbox::PacketReceipt {
                packet: judgment.packet,
                position: envelope.seq,
                outcome: judgment.outcome,
            });
            if submission.packets.iter().all(Option::is_some) {
                self.pending
                    .remove(&(submission.accepted, judgment.submission));
            }
        }
        Ok(())
    }
}

fn invalid(detail: &str) -> GuardError {
    GuardError::Invalid(format!("outbox authority: {detail}"))
}
