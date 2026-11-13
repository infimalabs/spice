//! Admitted app subscriptions and their bounded live deliveries.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bus::{AppHeads, JournalCommit};
use wire::{
    Packed, Refusal, Seq, StreamCursor, StreamFact, StreamFrame, StreamRequest, ViewSubject,
    WireVersion,
};

use super::policy::{
    app_visible_fact, pack_unique, present_facts, protocol_fault, refusal, required_horizon,
    trust_rows,
};
use super::{Failure, Source};

/// One bounded live-delivery transaction awaiting successful socket sends.
#[derive(Debug)]
pub struct LiveDelivery {
    frames: Vec<StreamFrame>,
    fact_cursor: Option<Seq>,
    complete: bool,
    trust_continuation: Option<Seq>,
}

impl LiveDelivery {
    /// Frames materialized by this one delivery quantum.
    #[must_use]
    pub fn frames(&self) -> &[StreamFrame] {
        &self.frames
    }
}

/// One admitted subscription and its exact continuation/live cursor state.
#[derive(Debug)]
pub struct Subscription {
    pub(super) id: wire::CanonicalUuid,
    pub(super) epoch: wire::CanonicalUuid,
    pub(super) subjects: BTreeSet<ViewSubject>,
    pub(super) cursor: Option<Seq>,
    pub(super) pending: BTreeMap<ViewSubject, Seq>,
    pub(super) barrier: Option<AppHeads>,
    pub(super) catch_up: Option<AppHeads>,
    pub(super) rows: usize,
    pub(super) message_generation: Option<u64>,
    pub(super) quota_generation: Option<u64>,
}

impl Subscription {
    /// Correlation identity of this subscription's live updates.
    #[must_use]
    pub const fn id(&self) -> wire::CanonicalUuid {
        self.id
    }

    /// Last fact cursor this subscription can resume strictly after.
    #[must_use]
    pub const fn cursor(&self) -> Option<Seq> {
        self.cursor
    }

    /// True once every initial packed page has been continued to its barrier.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        self.barrier.is_none()
    }

    /// Continue the exact subject cursor reported by the preceding page.
    pub fn continue_page(
        &mut self,
        source: &Source,
        request: &StreamRequest,
    ) -> Result<StreamFrame, Failure> {
        request.admit(self.epoch).map_err(Failure::admission)?;
        let StreamRequest::Continue {
            id,
            subject,
            cursor,
            ..
        } = request
        else {
            return Err(Failure::Refusal(refusal(
                "stream::continuation-required",
                "the request is not a packed continuation",
            )));
        };
        let expected = self.pending.get(subject).copied().ok_or_else(|| {
            Failure::Refusal(refusal(
                "stream::continuation-unexpected",
                "this subject has no pending packed continuation",
            ))
        })?;
        if cursor.seq != expected {
            return Err(Failure::Refusal(Refusal::bare(
                wire::RefusalDiagnostic::new(
                    "stream::continuation-moved",
                    Some("cursor".to_owned()),
                    Some(expected.to_string()),
                    Some(cursor.seq.to_string()),
                ),
            )));
        }
        let frame = self.page(source, *subject, Some(cursor.seq), *id)?;
        let horizon = required_horizon(
            self.barrier
                .as_ref()
                .expect("pending continuations retain their barrier"),
        )?;
        self.settle_barrier(horizon);
        Ok(frame)
    }

    /// Whether a proved live barrier still has facts to deliver.
    #[must_use]
    pub const fn has_continuation(&self) -> bool {
        self.catch_up.is_some()
    }

    /// Materialize at most one live fact quantum after a durable Bus notification.
    ///
    /// The returned delivery has not yet advanced the resumable cursor. The
    /// socket owner must call [`Self::admit`] only after every frame was sent.
    pub fn advance(
        &mut self,
        source: &Source,
        commit: JournalCommit,
    ) -> Result<Option<LiveDelivery>, Failure> {
        if !self.is_live() {
            return Ok(None);
        }
        let target = self.catch_up.as_ref().map(required_horizon).transpose()?;
        if target.is_none_or(|target| commit.last() > target.get()) {
            if self
                .cursor
                .is_some_and(|cursor| commit.last() <= cursor.get())
            {
                return Ok(None);
            }
            let heads = source.heads()?;
            let horizon = required_horizon(&heads)?;
            if self.cursor.is_some_and(|cursor| horizon <= cursor) {
                return Ok(None);
            }
            self.catch_up = Some(heads);
        }
        self.next_delivery(source).map(Some)
    }

    /// Materialize the next quantum from the retained live barrier.
    pub fn continue_live(&self, source: &Source) -> Result<Option<LiveDelivery>, Failure> {
        self.catch_up
            .as_ref()
            .map(|_| self.next_delivery(source))
            .transpose()
    }

    /// Admit one delivery after every frame in it reached the socket.
    pub fn admit(&mut self, delivery: LiveDelivery) {
        if let Some(cursor) = delivery.fact_cursor {
            self.cursor = Some(cursor);
        }
        if !delivery.complete {
            return;
        }
        let heads = self
            .catch_up
            .take()
            .expect("a complete live delivery retains its proved barrier");
        let horizon = required_horizon(&heads).expect("a retained barrier has a horizon");
        if let Some(continuation) = delivery.trust_continuation {
            self.pending.insert(ViewSubject::Trust, continuation);
            self.barrier = Some(heads);
        } else {
            self.pending.remove(&ViewSubject::Trust);
            self.barrier = None;
        }
        self.cursor = Some(horizon);
    }

    fn next_delivery(&self, source: &Source) -> Result<LiveDelivery, Failure> {
        let heads = self
            .catch_up
            .as_ref()
            .expect("live continuation retains its proved barrier");
        let horizon = required_horizon(heads)?;
        let mut frames = Vec::new();
        let mut fact_cursor = None;
        let mut complete = true;
        if self.subjects.contains(&ViewSubject::Facts) {
            let packed = source.facts(journal::FactPageStart::After(self.cursor), horizon)?;
            fact_cursor = packed.rows.last().map(|fact| fact.seq);
            complete = packed.overflow.is_none();
            frames.extend(
                packed
                    .rows
                    .into_iter()
                    .filter(app_visible_fact)
                    .map(StreamFact::from)
                    .map(|fact| StreamFrame::Fact {
                        v: WireVersion::CURRENT,
                        id: self.id,
                        epoch: self.epoch,
                        fact,
                    }),
            );
        }
        let trust_continuation = if complete && self.subjects.contains(&ViewSubject::Trust) {
            let trust = trust_rows(heads);
            let packed = pack_unique(trust, self.rows, horizon, |row| row.position)?;
            let continuation = packed.overflow.map(|overflow| overflow.continuation);
            frames.push(StreamFrame::Trust {
                v: WireVersion::CURRENT,
                id: self.id,
                epoch: self.epoch,
                packed,
            });
            continuation
        } else {
            None
        };
        Ok(LiveDelivery {
            frames,
            fact_cursor,
            complete,
            trust_continuation,
        })
    }

    /// Project one newer live transcript generation for subscriptions that asked for it.
    #[must_use]
    pub fn message(&self, generation: wire::TranscriptGeneration) -> Option<StreamFrame> {
        (self.is_live()
            && self.subjects.contains(&ViewSubject::Messages)
            && self
                .message_generation
                .is_none_or(|current| generation.generation > current))
        .then_some(StreamFrame::MessageGeneration {
            v: WireVersion::CURRENT,
            id: self.id,
            epoch: self.epoch,
            generation,
        })
    }

    /// Advance the transcript watermark after its frame reached the socket.
    pub fn admit_message(&mut self, generation: u64) {
        self.message_generation = Some(
            self.message_generation
                .map_or(generation, |current| current.max(generation)),
        );
    }

    /// Project one newer live quota generation for operator subscriptions.
    #[must_use]
    pub fn quota(&self, generation: Arc<wire::QuotaGeneration>) -> Option<StreamFrame> {
        (self.is_live()
            && self.subjects.contains(&ViewSubject::Quota)
            && self
                .quota_generation
                .is_none_or(|current| generation.generation > current))
        .then(|| StreamFrame::QuotaGeneration {
            v: WireVersion::CURRENT,
            id: self.id,
            epoch: self.epoch,
            generation: (*generation).clone(),
        })
    }

    /// Advance the quota watermark after its frame reached the socket.
    pub fn admit_quota(&mut self, generation: u64) {
        self.quota_generation = Some(
            self.quota_generation
                .map_or(generation, |current| current.max(generation)),
        );
    }

    /// Terminal bounded-backpressure frame, resumable after the admitted cursor.
    pub fn overflow(&self, dropped: u64) -> Result<StreamFrame, Failure> {
        let cursor = self.cursor.ok_or_else(|| {
            Failure::Fault(protocol_fault(
                "stream::cursor-missing",
                "subscription lagged before it admitted a resumable cursor",
            ))
        })?;
        Ok(StreamFrame::Overflow {
            v: WireVersion::CURRENT,
            id: self.id,
            epoch: self.epoch,
            after: StreamCursor {
                epoch: self.epoch,
                seq: cursor,
            },
            dropped,
        })
    }

    /// Terminal generation shutdown frame at the last admitted cursor.
    pub fn closing(&self) -> Option<StreamFrame> {
        self.cursor.map(|cursor| StreamFrame::Closing {
            v: WireVersion::CURRENT,
            id: self.id,
            epoch: self.epoch,
            after: StreamCursor {
                epoch: self.epoch,
                seq: cursor,
            },
        })
    }

    pub(super) fn page(
        &mut self,
        source: &Source,
        subject: ViewSubject,
        continuation: Option<Seq>,
        request_id: wire::CanonicalUuid,
    ) -> Result<StreamFrame, Failure> {
        let heads = self.barrier.as_ref().ok_or_else(|| {
            Failure::Refusal(refusal(
                "stream::continuation-stale",
                "the subscription has already crossed into live delivery",
            ))
        })?;
        let horizon = required_horizon(heads)?;
        let frame = match subject {
            ViewSubject::Facts => {
                let start = continuation.map_or(
                    journal::FactPageStart::After(self.cursor),
                    journal::FactPageStart::At,
                );
                let packed = present_facts(source.facts(start, horizon)?);
                self.record_page(subject, &packed, StreamFact::seq);
                StreamFrame::Facts {
                    v: WireVersion::CURRENT,
                    id: request_id,
                    epoch: self.epoch,
                    packed,
                }
            }
            ViewSubject::Rooms => {
                let start = continuation.map_or(
                    journal::FactPageStart::After(None),
                    journal::FactPageStart::At,
                );
                let packed = present_facts(source.rooms(start, horizon)?);
                self.record_page(subject, &packed, StreamFact::seq);
                StreamFrame::Rooms {
                    v: WireVersion::CURRENT,
                    id: request_id,
                    epoch: self.epoch,
                    packed,
                }
            }
            ViewSubject::Trust => {
                let rows = trust_rows(heads)
                    .into_iter()
                    .filter(|row| continuation.is_none_or(|cursor| row.position >= cursor))
                    .collect::<Vec<_>>();
                let packed = pack_unique(rows, self.rows, horizon, |row| row.position)?;
                self.record_page(subject, &packed, |row| row.position);
                StreamFrame::Trust {
                    v: WireVersion::CURRENT,
                    id: request_id,
                    epoch: self.epoch,
                    packed,
                }
            }
            ViewSubject::Messages | ViewSubject::Quota => {
                return Err(Failure::Refusal(refusal(
                    "stream::ephemeral-continuation-invalid",
                    "ephemeral snapshots never carry a journal continuation",
                )));
            }
        };
        Ok(frame)
    }

    fn record_page<T>(
        &mut self,
        subject: ViewSubject,
        packed: &Packed<T>,
        position: impl Fn(&T) -> Seq,
    ) {
        if let Some(overflow) = packed.overflow {
            self.pending.insert(subject, overflow.continuation);
        } else {
            self.pending.remove(&subject);
        }
        if subject == ViewSubject::Facts
            && let Some(last) = packed.rows.last()
        {
            self.cursor = Some(position(last));
        }
    }

    pub(super) fn settle_barrier(&mut self, horizon: Seq) {
        if self.pending.is_empty() {
            self.cursor = Some(horizon);
            self.barrier = None;
        }
    }
}
