//! Browser-owned steering submissions that survive socket replacement.
//!
//! A submission stops being a draft before the resident has necessarily seen
//! it. This queue owns that interval. Socket code may advance a record, but a
//! socket attempt never owns the record itself, so dropping a reader or writer
//! cannot drop operator prose with it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use wire::body::SteeringRecipient;
use wire::intent::PendingDirective;
use wire::{CanonicalUuid, Disposition, Media};

use crate::composer_drafts::{ComposerDraft, DraftImage, QuoteDraft};

const SUBMISSION_LIMIT: usize = 64;
const LANE_SUBMISSION_LIMIT: usize = 16;
const MEDIA_DATA_URL_BYTES: usize = wire::media::MAX_MEDIA_BYTES.div_ceil(3) * 4 + 64;
const MEDIA_RETAINED_BYTES: usize = MEDIA_DATA_URL_BYTES + 512;
const MESSAGE_BYTES: usize = wire::media::MAX_MEDIA_BYTES;
const PAYLOAD_METADATA_BYTES: usize =
    std::mem::size_of::<SubmissionPayload>() + 2 * std::mem::size_of::<usize>();
const MESSAGE_PAYLOAD_BYTES: usize =
    wire::media::MAX_MEDIA_ITEMS * MEDIA_RETAINED_BYTES + MESSAGE_BYTES + PAYLOAD_METADATA_BYTES;
const SUBMISSION_METADATA_BYTES: usize = std::mem::size_of::<Submission>();
const RETAINED_BYTES_LIMIT: usize = MESSAGE_PAYLOAD_BYTES * 2 + SUBMISSION_METADATA_BYTES;

/// One authored payload shared by every target-specific fanout record.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SubmissionPayload {
    id: CanonicalUuid,
    authored_millis: i64,
    pub(crate) message: String,
    pub(crate) attachments: Vec<Media>,
    /// Intended target-specific siblings of this broadcast.
    recipients: Vec<SteeringRecipient>,
}

impl SubmissionPayload {
    fn retained_bytes(&self) -> usize {
        PAYLOAD_METADATA_BYTES
            .saturating_add(self.message.capacity())
            .saturating_add(attachments_bytes(&self.attachments))
            .saturating_add(self.recipients.capacity() * std::mem::size_of::<SteeringRecipient>())
    }
}

/// The composer a submission's content came from, which its durable
/// acceptance clears and its refusal unlocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ComposerSource {
    /// One lane's composer in its team.
    Lane(CanonicalUuid),
    /// The header composer that asks an agent or creates work.
    Creator,
}

/// One target-specific document detached from its composer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Submission {
    pub(crate) id: CanonicalUuid,
    pub(crate) fanout: CanonicalUuid,
    pub(crate) lane: CanonicalUuid,
    payload: Rc<SubmissionPayload>,
    /// The composer content this record carried. It stays in its composer,
    /// locked, and durable acceptance clears exactly it from there.
    pub(crate) snapshot: Rc<ComposerDraft>,
    source: ComposerSource,
    /// Whether this record carries its source composer's content; a
    /// broadcast's other records carry none.
    carries: bool,
    stage: Stage,
}

impl Submission {
    pub(crate) fn new(
        id: CanonicalUuid,
        fanout: CanonicalUuid,
        lane: CanonicalUuid,
        residency: (CanonicalUuid, CanonicalUuid, i64),
        message: String,
        attachments: Vec<Media>,
        snapshot: ComposerDraft,
    ) -> Self {
        let (occupant, epoch, authored_millis) = residency;
        Self::shared(
            id,
            fanout,
            lane,
            Self::payload(id, authored_millis, message, attachments, Vec::new()),
            Some(snapshot),
            ComposerSource::Lane(lane),
            Stage::Queued {
                occupant: Some(occupant),
                epoch,
            },
        )
    }

    /// Detach a document for a bound seat that has no current occupant.
    ///
    /// The vacant residency is a sufficient steering witness. Publishing the
    /// directive first lets the resident's pending-inbox lifecycle wake the
    /// lane with that directive already visible.
    pub(crate) fn vacant(
        id: CanonicalUuid,
        fanout: CanonicalUuid,
        lane: CanonicalUuid,
        residency: (CanonicalUuid, i64),
        message: String,
        attachments: Vec<Media>,
        snapshot: ComposerDraft,
    ) -> Self {
        let (epoch, authored_millis) = residency;
        Self::shared(
            id,
            fanout,
            lane,
            Self::payload(id, authored_millis, message, attachments, Vec::new()),
            Some(snapshot),
            ComposerSource::Lane(lane),
            Stage::Queued {
                occupant: None,
                epoch,
            },
        )
    }

    pub(crate) fn payload(
        id: CanonicalUuid,
        authored_millis: i64,
        message: String,
        attachments: Vec<Media>,
        recipients: Vec<SteeringRecipient>,
    ) -> Rc<SubmissionPayload> {
        Rc::new(SubmissionPayload {
            id,
            authored_millis,
            message,
            attachments,
            recipients,
        })
    }

    /// One target of a broadcast from `source`. At most one record of the
    /// fanout carries the source composer's `snapshot`; it alone holds that
    /// composer and settles it. A `None` occupant steers a vacant seat.
    pub(crate) fn broadcast(
        id: CanonicalUuid,
        fanout: CanonicalUuid,
        lane: CanonicalUuid,
        residency: (Option<CanonicalUuid>, CanonicalUuid),
        payload: Rc<SubmissionPayload>,
        snapshot: Option<ComposerDraft>,
        source: ComposerSource,
    ) -> Self {
        let (occupant, epoch) = residency;
        Self::shared(
            id,
            fanout,
            lane,
            payload,
            snapshot,
            source,
            Stage::Queued { occupant, epoch },
        )
    }

    fn shared(
        id: CanonicalUuid,
        fanout: CanonicalUuid,
        lane: CanonicalUuid,
        payload: Rc<SubmissionPayload>,
        snapshot: Option<ComposerDraft>,
        source: ComposerSource,
        stage: Stage,
    ) -> Self {
        Self {
            id,
            fanout,
            lane,
            payload,
            carries: snapshot.is_some(),
            snapshot: Rc::new(snapshot.unwrap_or_default()),
            source,
            stage,
        }
    }

    /// This record carries its composer's sent content and keeps that
    /// composer locked until the resident durably accepts it.
    fn holding(&self) -> bool {
        self.carries && !matches!(&self.stage, Stage::Accepted { .. })
    }

    pub(crate) fn message(&self) -> &str {
        &self.payload.message
    }

    pub(crate) fn attachments(&self) -> &[Media] {
        &self.payload.attachments
    }

    #[cfg_attr(
        all(test, not(target_arch = "wasm32")),
        expect(
            dead_code,
            reason = "only the wasm32 browser connection sends declared recipients"
        )
    )]
    pub(crate) fn recipients(&self) -> &[SteeringRecipient] {
        &self.payload.recipients
    }

    fn retained_bytes(&self) -> usize {
        SUBMISSION_METADATA_BYTES.saturating_add(draft_bytes(&self.snapshot))
    }

    pub(crate) fn residency(&self) -> Option<(Option<CanonicalUuid>, CanonicalUuid)> {
        match &self.stage {
            Stage::Queued { occupant, epoch } => Some((*occupant, *epoch)),
            Stage::AwaitingResidency | Stage::Sent { .. } | Stage::Accepted { .. } => None,
        }
    }
}

fn attachments_bytes(attachments: &Vec<Media>) -> usize {
    attachments.iter().fold(
        attachments
            .capacity()
            .saturating_mul(std::mem::size_of::<Media>()),
        |bytes, attachment| {
            bytes
                .saturating_add(attachment.name.capacity())
                .saturating_add(attachment.content_type.capacity())
                .saturating_add(attachment.data_url.capacity())
        },
    )
}

fn draft_images_bytes(images: &Vec<DraftImage>) -> usize {
    images.iter().fold(
        images
            .capacity()
            .saturating_mul(std::mem::size_of::<DraftImage>()),
        |bytes, image| {
            let media_bytes = |media: &Media| {
                media
                    .name
                    .capacity()
                    .saturating_add(media.content_type.capacity())
                    .saturating_add(media.data_url.capacity())
                    .saturating_add(std::mem::size_of::<Media>())
            };
            bytes
                .saturating_add(media_bytes(&image.media))
                .saturating_add(if Rc::ptr_eq(&image.media, &image.original) {
                    0
                } else {
                    media_bytes(&image.original)
                })
                .saturating_add(
                    image
                        .shapes
                        .capacity()
                        .saturating_mul(std::mem::size_of::<crate::image_annotation::Shape>()),
                )
                .saturating_add(image.shapes.iter().map(|shape| shape.text.len()).sum())
        },
    )
}

fn draft_bytes(draft: &ComposerDraft) -> usize {
    draft.quotes.iter().fold(
        draft
            .body
            .capacity()
            .saturating_add(draft.continuation.capacity())
            .saturating_add(draft_images_bytes(&draft.attachments))
            .saturating_add(
                draft
                    .quotes
                    .capacity()
                    .saturating_mul(std::mem::size_of::<QuoteDraft>()),
            ),
        |bytes, quote| {
            bytes
                .saturating_add(quote.identity.capacity())
                .saturating_add(quote.preview.capacity())
                .saturating_add(quote.body.capacity())
                .saturating_add(draft_images_bytes(&quote.attachments))
        },
    )
}

fn admit_payload(payload: &SubmissionPayload) -> Result<(), String> {
    if payload.attachments.len() > wire::media::MAX_MEDIA_ITEMS {
        return Err(format!(
            "Submissions admit at most {} attachments.",
            wire::media::MAX_MEDIA_ITEMS
        ));
    }
    if payload.message.len() > MESSAGE_BYTES
        || payload
            .attachments
            .iter()
            .any(|attachment| attachment.data_url.len() > MEDIA_DATA_URL_BYTES)
        || payload.retained_bytes() > MESSAGE_PAYLOAD_BYTES
    {
        return Err(format!(
            "Submission payload exceeds the {MESSAGE_PAYLOAD_BYTES}-byte limit."
        ));
    }
    Ok(())
}

/// How far one submission has crossed the browser-to-resident boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Stage {
    /// A direct vacant-seat send committed; a fresh lane read must rebase its tail.
    AwaitingResidency,
    Queued {
        occupant: Option<CanonicalUuid>,
        epoch: CanonicalUuid,
    },
    Sent {
        occupant: Option<CanonicalUuid>,
        epoch: CanonicalUuid,
    },
    Accepted {
        directive: CanonicalUuid,
        key: String,
        disposition: Disposition,
    },
}

impl Stage {
    fn acceptance(&self) -> Option<(CanonicalUuid, &str)> {
        match self {
            Self::Accepted { directive, key, .. } => Some((*directive, key)),
            _ => None,
        }
    }
}

/// Renderable fold of all submissions not yet covered by a fresh inbox.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Prediction {
    directive: CanonicalUuid,
    key: Option<String>,
}

/// A send from one composer to several chosen agents, which settles that
/// composer as one: it stays held until every branch has an outcome, and only
/// a send every branch admitted clears it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Settlement {
    source: ComposerSource,
    snapshot: Rc<ComposerDraft>,
    /// Each branch still awaiting its outcome, by identity, with its lane.
    pending: BTreeMap<CanonicalUuid, CanonicalUuid>,
    admitted: BTreeSet<CanonicalUuid>,
    refused: BTreeMap<CanonicalUuid, String>,
}

/// How one branch of a settled send ended.
enum Branch<'a> {
    Admitted,
    Refused(&'a str),
    /// Rolled back before it reached the resident: no outcome to report.
    Withdrawn,
}

/// How every branch of a composer's last several-agent send ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FanoutOutcome {
    /// The send this settles; each send from a composer has its own.
    pub(crate) fanout: CanonicalUuid,
    /// Lanes whose branch the resident durably admitted.
    pub(crate) admitted: BTreeSet<CanonicalUuid>,
    /// Lanes whose branch was not sent, each with why.
    pub(crate) refused: BTreeMap<CanonicalUuid, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SubmissionStatus {
    rows: BTreeMap<CanonicalUuid, Vec<Prediction>>,
    keyless: usize,
    held: BTreeSet<ComposerSource>,
    settled: BTreeMap<ComposerSource, FanoutOutcome>,
}

impl SubmissionStatus {
    /// Submissions the resident has not yet accepted, queued or in flight.
    pub(crate) fn awaiting(&self) -> usize {
        self.keyless
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.values().map(Vec::len).sum()
    }

    /// Whether this composer is locked behind a send that awaits durable
    /// acceptance.
    pub(crate) fn holds(&self, source: ComposerSource) -> bool {
        self.held.contains(&source)
    }

    /// How this composer's last several-agent send settled.
    pub(crate) fn settled(&self, source: ComposerSource) -> Option<&FanoutOutcome> {
        self.settled.get(&source)
    }

    /// Backend truth plus predictions whose stable identity it does not carry.
    pub(crate) fn pending(&self, lane: CanonicalUuid, backend: &[PendingDirective]) -> usize {
        backend.len() + self.uncovered(lane, backend)
    }

    /// Backend direction that can wake the lane plus uncovered predictions,
    /// which are the operator's own sends and so always wake it.
    pub(crate) fn waking(&self, lane: CanonicalUuid, backend: &[PendingDirective]) -> usize {
        backend.iter().filter(|directive| directive.wakes).count() + self.uncovered(lane, backend)
    }

    fn uncovered(&self, lane: CanonicalUuid, backend: &[PendingDirective]) -> usize {
        let durable = backend
            .iter()
            .map(|directive| (directive.directive, directive.key.as_str()))
            .collect::<BTreeMap<_, _>>();
        self.rows
            .get(&lane)
            .into_iter()
            .flatten()
            .filter(|prediction| match prediction.key.as_deref() {
                Some(key) => durable
                    .get(&prediction.directive)
                    .is_none_or(|backend_key| *backend_key != key),
                None => !durable.contains_key(&prediction.directive),
            })
            .count()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RetainedPayload {
    payload: Rc<SubmissionPayload>,
    records: usize,
}

/// Bounded per-lane submission queues shared across every connection attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SubmissionQueue {
    rows: BTreeMap<CanonicalUuid, VecDeque<Submission>>,
    identities: BTreeMap<CanonicalUuid, CanonicalUuid>,
    backend: BTreeMap<CanonicalUuid, String>,
    activity_read_generation: u64,
    activity_read_required: Option<u64>,
    payloads: BTreeMap<CanonicalUuid, RetainedPayload>,
    retained_bytes: usize,
    /// Several-agent sends still awaiting a branch outcome, by fanout.
    settlements: BTreeMap<CanonicalUuid, Settlement>,
    /// Each composer's last settled several-agent send.
    settled: BTreeMap<ComposerSource, FanoutOutcome>,
}

impl SubmissionQueue {
    pub(crate) fn enqueue(&mut self, submission: Submission) -> Result<(), String> {
        self.enqueue_all(vec![submission])
    }

    /// Admit a send from one composer to several distinct agents whose
    /// branches settle it as one. No branch carries the composer's content:
    /// the send holds `snapshot` until every branch has an outcome.
    pub(crate) fn enqueue_settled(
        &mut self,
        snapshot: ComposerDraft,
        branches: Vec<Submission>,
    ) -> Result<(), String> {
        let Some(first) = branches.first() else {
            return Err("a several-agent send names no agent".to_owned());
        };
        let (fanout, source) = (first.fanout, first.source);
        let lanes = branches
            .iter()
            .map(|branch| branch.lane)
            .collect::<BTreeSet<_>>();
        if lanes.len() != branches.len()
            || self.settlements.contains_key(&fanout)
            || branches
                .iter()
                .any(|branch| branch.fanout != fanout || branch.source != source || branch.carries)
        {
            return Err(format!(
                "several-agent send {fanout} must name each agent once under one fanout and composer"
            ));
        }
        let pending = branches
            .iter()
            .map(|branch| (branch.id, branch.lane))
            .collect();
        let reserved = draft_bytes(&snapshot);
        self.enqueue_batch(branches, reserved)?;
        self.retained_bytes = self.retained_bytes.saturating_add(reserved);
        self.settlements.insert(
            fanout,
            Settlement {
                source,
                snapshot: Rc::new(snapshot),
                pending,
                admitted: BTreeSet::new(),
                refused: BTreeMap::new(),
            },
        );
        Ok(())
    }

    /// Admit a complete fanout batch before publishing any of its destinations.
    pub(crate) fn enqueue_all(&mut self, submissions: Vec<Submission>) -> Result<(), String> {
        self.enqueue_batch(submissions, 0)
    }

    /// Admit `submissions` and `reserved` further retained bytes, or nothing.
    fn enqueue_batch(
        &mut self,
        submissions: Vec<Submission>,
        reserved: usize,
    ) -> Result<(), String> {
        if submissions.len() > SUBMISSION_LIMIT.saturating_sub(self.identities.len()) {
            return Err(format!(
                "{} submissions are already retained; wait for delivery before retrying.",
                self.identities.len()
            ));
        }
        let mut identities = BTreeSet::new();
        let mut lane_counts = BTreeMap::<CanonicalUuid, usize>::new();
        let mut payloads = BTreeMap::<CanonicalUuid, Rc<SubmissionPayload>>::new();
        let mut additional_bytes = reserved;
        for submission in &submissions {
            if self.identities.contains_key(&submission.id) || !identities.insert(submission.id) {
                return Err(format!(
                    "composer submission identity {} is already queued",
                    submission.id
                ));
            }
            let lane_count = self.rows.get(&submission.lane).map_or(0, VecDeque::len)
                + *lane_counts.entry(submission.lane).or_default();
            if lane_count >= LANE_SUBMISSION_LIMIT {
                return Err(format!(
                    "{LANE_SUBMISSION_LIMIT} submissions are already retained for lane {}; wait for delivery before retrying.",
                    submission.lane
                ));
            }
            *lane_counts
                .get_mut(&submission.lane)
                .expect("the batch lane count was initialized") += 1;
            admit_payload(&submission.payload)?;
            additional_bytes = additional_bytes.saturating_add(submission.retained_bytes());
            let retained = self
                .payloads
                .get(&submission.payload.id)
                .map(|retained| &retained.payload)
                .or_else(|| payloads.get(&submission.payload.id));
            if retained.is_some_and(|payload| !Rc::ptr_eq(payload, &submission.payload)) {
                return Err(format!(
                    "composer payload identity {} was reused",
                    submission.payload.id
                ));
            }
            if retained.is_none() {
                additional_bytes =
                    additional_bytes.saturating_add(submission.payload.retained_bytes());
                payloads.insert(submission.payload.id, Rc::clone(&submission.payload));
            }
        }
        if additional_bytes > RETAINED_BYTES_LIMIT.saturating_sub(self.retained_bytes) {
            return Err(format!(
                "Submission retention exceeds the {RETAINED_BYTES_LIMIT}-byte limit; wait for delivery before retrying."
            ));
        }
        for mut submission in submissions {
            if matches!(&submission.stage, Stage::Queued { occupant: None, .. })
                && self.rows.get(&submission.lane).is_some_and(|rows| {
                    rows.iter().any(|row| row.stage == Stage::AwaitingResidency)
                })
            {
                submission.stage = Stage::AwaitingResidency;
            }
            self.retained_bytes = self
                .retained_bytes
                .saturating_add(submission.retained_bytes());
            self.identities.insert(submission.id, submission.lane);
            match self.payloads.entry(submission.payload.id) {
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().records += 1;
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let bytes = submission.payload.retained_bytes();
                    self.retained_bytes = self.retained_bytes.saturating_add(bytes);
                    entry.insert(RetainedPayload {
                        payload: Rc::clone(&submission.payload),
                        records: 1,
                    });
                }
            }
            self.rows
                .entry(submission.lane)
                .or_default()
                .push_back(submission);
        }
        Ok(())
    }

    pub(crate) fn source(&self, id: CanonicalUuid) -> Result<ComposerSource, String> {
        self.submission(id).map(|submission| submission.source)
    }

    /// Whether this composer is locked behind a send that the resident has
    /// not yet durably accepted, so a second send from it is a duplicate.
    pub(crate) fn holds(&self, source: ComposerSource) -> bool {
        self.rows
            .values()
            .flatten()
            .any(|submission| submission.source == source && submission.holding())
            || self
                .settlements
                .values()
                .any(|settlement| settlement.source == source)
    }

    /// The composer this send's durable acceptance clears and the content it
    /// clears: a carrying send's own, or a several-agent send's once this is
    /// its last outstanding branch and every other branch was admitted.
    pub(crate) fn held_content(
        &self,
        id: CanonicalUuid,
    ) -> Option<(ComposerSource, Rc<ComposerDraft>)> {
        let submission = self.submission(id).ok()?;
        if submission.holding() {
            return Some((submission.source, Rc::clone(&submission.snapshot)));
        }
        self.settlements
            .get(&submission.fanout)
            .filter(|settlement| {
                settlement.refused.is_empty()
                    && settlement.pending.len() == 1
                    && settlement.pending.contains_key(&id)
            })
            .map(|settlement| (settlement.source, Rc::clone(&settlement.snapshot)))
    }

    /// Remove an enqueue whose command could not reach the connection vendor.
    pub(crate) fn rollback(&mut self, id: CanonicalUuid) -> Option<Submission> {
        let lane = self.lane_for(id)?;
        let rows = self.rows.get_mut(&lane)?;
        let index = rows.iter().position(|submission| {
            submission.id == id
                && matches!(
                    &submission.stage,
                    Stage::Queued { .. } | Stage::AwaitingResidency
                )
        })?;
        let removed = rows.remove(index);
        if rows.is_empty() {
            self.rows.remove(&lane);
        }
        if let Some(submission) = &removed {
            self.release(submission);
            self.settle(submission, Branch::Withdrawn);
        }
        removed
    }

    /// At most one ready submission from each concrete lane.
    pub(crate) fn ready(&self) -> Vec<Submission> {
        self.rows
            .values()
            .filter(|rows| {
                !rows
                    .iter()
                    .any(|submission| matches!(&submission.stage, Stage::Sent { .. }))
            })
            .filter_map(|rows| {
                rows.iter()
                    .find(|submission| !matches!(&submission.stage, Stage::Accepted { .. }))
                    .filter(|submission| matches!(&submission.stage, Stage::Queued { .. }))
                    .cloned()
            })
            .collect()
    }

    pub(crate) fn sent(&mut self, id: CanonicalUuid) -> Result<(), String> {
        let submission = self.submission_mut(id)?;
        let Stage::Queued { occupant, epoch } = submission.stage.clone() else {
            return Err(format!(
                "composer submission {id} was not queued when its bytes were sent"
            ));
        };
        submission.stage = Stage::Sent { occupant, epoch };
        Ok(())
    }

    /// Make every result-less write eligible for replay with the same identity.
    pub(crate) fn reconnect(&mut self) {
        for submission in self.rows.values_mut().flatten() {
            submission.stage = match submission.stage.clone() {
                Stage::Sent { occupant, epoch } => Stage::Queued { occupant, epoch },
                other => other,
            };
        }
    }

    /// Submissions whose vacant witness must be refreshed after an earlier send.
    pub(crate) fn awaiting_residency(&self) -> Vec<CanonicalUuid> {
        self.rows
            .values()
            .flatten()
            .filter(|submission| submission.stage == Stage::AwaitingResidency)
            .map(|submission| submission.id)
            .collect()
    }

    /// Lanes still awaiting residency among the identities one read nominated.
    #[cfg_attr(
        all(test, not(target_arch = "wasm32")),
        expect(
            dead_code,
            reason = "only the wasm32 browser connection's lane read checks awaiting lanes"
        )
    )]
    pub(crate) fn awaiting_lanes(&self, ids: &[CanonicalUuid]) -> BTreeSet<CanonicalUuid> {
        ids.iter()
            .filter_map(|id| {
                let submission = self.submission(*id).ok()?;
                (submission.stage == Stage::AwaitingResidency).then_some(submission.lane)
            })
            .collect()
    }

    /// Rebase only submissions nominated when this lane read started.
    pub(crate) fn rebase_awaiting_lane(
        &mut self,
        ids: &[CanonicalUuid],
        lane: CanonicalUuid,
        occupant: Option<CanonicalUuid>,
        epoch: CanonicalUuid,
    ) {
        for id in ids {
            if self.lane_for(*id) != Some(lane) {
                continue;
            }
            let submission = self
                .submission_mut(*id)
                .expect("a nominated submission retains its lane queue");
            if submission.stage == Stage::AwaitingResidency {
                submission.stage = Stage::Queued { occupant, epoch };
            }
        }
    }

    pub(crate) fn accept(
        &mut self,
        id: CanonicalUuid,
        directive: CanonicalUuid,
        key: String,
        disposition: Disposition,
    ) -> Result<Option<Rc<SubmissionPayload>>, String> {
        if directive != id {
            return Err(format!(
                "composer submission {id} received mismatched directive {directive}"
            ));
        }
        let vacant = match &self.submission(id)?.stage {
            Stage::Sent { occupant, .. } => occupant.is_none(),
            _ => {
                return Err(format!(
                    "composer submission {id} received a result before it was sent"
                ));
            }
        };
        let lane = self
            .lane_for(id)
            .expect("an admitted submission retains its lane identity");
        let observed = self.backend.get(&directive).cloned();
        if let Some(backend_key) = &observed
            && backend_key != &key
        {
            return Err(format!(
                "composer submission {id} received key {key} after backend key {backend_key}"
            ));
        }
        let accepted = if disposition == Disposition::Live {
            Some(Rc::clone(&self.submission(id)?.payload))
        } else {
            None
        };
        let fanout = self.submission(id)?.fanout;
        self.settle_branch(fanout, id, lane, Branch::Admitted);
        if disposition != Disposition::Live || observed.is_some() {
            self.remove(id);
        } else {
            self.submission_mut(id)?.stage = Stage::Accepted {
                directive,
                key,
                disposition,
            };
        }
        if vacant && let Some(rows) = self.rows.get_mut(&lane) {
            for submission in rows {
                if matches!(&submission.stage, Stage::Queued { occupant: None, .. }) {
                    submission.stage = Stage::AwaitingResidency;
                }
            }
        }
        Ok(accepted)
    }

    /// Drop the refused write and every later write that never became durable,
    /// each refused for `reason`. A dropped send's content never left its
    /// composer; dropping it unlocks. Answers whether the refused write is a
    /// branch of a several-agent send, which reports it once it settles.
    pub(crate) fn refuse(&mut self, id: CanonicalUuid, reason: &str) -> Result<bool, String> {
        self.refuse_when(id, reason, |stage| matches!(stage, Stage::Sent { .. }))
    }

    /// Drop the first nominated tail whose fresh lane read found no target.
    pub(crate) fn refuse_awaiting_lane(
        &mut self,
        lane: CanonicalUuid,
        ids: &[CanonicalUuid],
        reason: &str,
    ) -> Result<bool, String> {
        let id = ids
            .iter()
            .copied()
            .find(|id| {
                self.lane_for(*id) == Some(lane)
                    && self
                        .submission(*id)
                        .is_ok_and(|submission| submission.stage == Stage::AwaitingResidency)
            })
            .ok_or_else(|| format!("lane {lane} has no submission awaiting residency"))?;
        self.refuse_when(id, reason, |stage| *stage == Stage::AwaitingResidency)
    }

    fn refuse_when(
        &mut self,
        id: CanonicalUuid,
        reason: &str,
        expected: impl FnOnce(&Stage) -> bool,
    ) -> Result<bool, String> {
        let lane = self
            .lane_for(id)
            .ok_or_else(|| format!("composer result named unknown submission {id}"))?;
        let (removed, empty) = {
            let rows = self
                .rows
                .get_mut(&lane)
                .expect("an indexed submission retains its lane queue");
            let index = rows
                .iter()
                .position(|submission| submission.id == id)
                .expect("an indexed submission remains in its lane queue");
            if !expected(&rows[index].stage) {
                return Err(format!(
                    "composer submission {id} was not awaiting a result"
                ));
            }
            let removed = rows.drain(index..).collect::<Vec<_>>();
            (removed, rows.is_empty())
        };
        if empty {
            self.rows.remove(&lane);
        }
        let settled = self.settlements.contains_key(&removed[0].fanout);
        for submission in &removed {
            self.release(submission);
            self.settle(submission, Branch::Refused(reason));
        }
        Ok(settled)
    }

    fn settle(&mut self, submission: &Submission, outcome: Branch<'_>) {
        self.settle_branch(submission.fanout, submission.id, submission.lane, outcome);
    }

    /// Record one branch's outcome; its send's last outcome settles the send.
    fn settle_branch(
        &mut self,
        fanout: CanonicalUuid,
        id: CanonicalUuid,
        lane: CanonicalUuid,
        outcome: Branch<'_>,
    ) {
        let Some(settlement) = self.settlements.get_mut(&fanout) else {
            return;
        };
        if settlement.pending.remove(&id).is_none() {
            return;
        }
        match outcome {
            Branch::Admitted => {
                settlement.admitted.insert(lane);
            }
            Branch::Refused(reason) => {
                settlement.refused.insert(lane, reason.to_owned());
            }
            Branch::Withdrawn => {}
        }
        if !settlement.pending.is_empty() {
            return;
        }
        let settlement = self
            .settlements
            .remove(&fanout)
            .expect("the settling send is retained");
        self.retained_bytes = self
            .retained_bytes
            .saturating_sub(draft_bytes(&settlement.snapshot));
        if settlement.admitted.is_empty() && settlement.refused.is_empty() {
            return;
        }
        self.settled.insert(
            settlement.source,
            FanoutOutcome {
                fanout,
                admitted: settlement.admitted,
                refused: settlement.refused,
            },
        );
    }

    pub(crate) fn accepted(&self) -> BTreeMap<CanonicalUuid, String> {
        self.rows
            .values()
            .flatten()
            .filter_map(|submission| {
                submission
                    .stage
                    .acceptance()
                    .map(|(directive, key)| (directive, key.to_owned()))
            })
            .collect()
    }

    /// Causal generation that still requires a lane read to start.
    pub(crate) const fn activity_read_requirement(&self) -> Option<u64> {
        self.activity_read_required
    }

    /// Observe that one lane read carrying this causal generation started.
    pub(crate) fn activity_read_started(&mut self, generation: Option<u64>) {
        if generation.is_some() && generation == self.activity_read_required {
            self.activity_read_required = None;
        }
    }

    /// Retire accepted predictions after one causally fresh inbox observation.
    ///
    /// A consumed directive may already be running, so this records a lane-read
    /// generation that an older concurrent read cannot satisfy.
    pub(crate) fn observe_backend<'a>(
        &mut self,
        pending: impl Iterator<Item = &'a PendingDirective>,
        covered: &BTreeMap<CanonicalUuid, String>,
    ) -> Result<(), String> {
        let backend = pending
            .filter(|directive| self.identities.contains_key(&directive.directive))
            .map(|directive| (directive.directive, directive.key.clone()))
            .collect::<BTreeMap<_, _>>();
        for submission in self.rows.values().flatten() {
            let Some((directive, key)) = submission.stage.acceptance() else {
                continue;
            };
            for (source, observed) in [
                ("backend", backend.get(&directive)),
                ("captured", covered.get(&directive)),
            ] {
                if let Some(observed) = observed
                    && observed != key
                {
                    return Err(format!(
                        concat!("composer submission {} has {} key {} ", "instead of {}"),
                        directive, source, observed, key,
                    ));
                }
            }
        }
        let consumed = self.rows.values().flatten().any(|submission| {
            submission
                .stage
                .acceptance()
                .is_some_and(|(directive, key)| {
                    !backend.contains_key(&directive)
                        && covered
                            .get(&directive)
                            .is_some_and(|observed| observed == key)
                })
        });
        let retired = self
            .rows
            .values()
            .flatten()
            .filter_map(|submission| {
                let (directive, key) = submission.stage.acceptance()?;
                [backend.get(&directive), covered.get(&directive)]
                    .into_iter()
                    .flatten()
                    .any(|observed| observed == key)
                    .then_some(submission.id)
            })
            .collect::<Vec<_>>();
        self.backend = backend;
        for id in retired {
            self.remove(id);
        }
        if consumed {
            self.activity_read_generation = self
                .activity_read_generation
                .checked_add(1)
                .expect("composer activity-read generation is exhausted");
            self.activity_read_required = Some(self.activity_read_generation);
        }
        Ok(())
    }

    pub(crate) fn status(&self) -> SubmissionStatus {
        SubmissionStatus {
            rows: self
                .rows
                .iter()
                .map(|(lane, rows)| {
                    (
                        *lane,
                        rows.iter()
                            .map(|submission| Prediction {
                                directive: submission.id,
                                key: submission.stage.acceptance().map(|(_, key)| key.to_owned()),
                            })
                            .collect(),
                    )
                })
                .collect(),
            keyless: self
                .rows
                .values()
                .flatten()
                .filter(|submission| !matches!(&submission.stage, Stage::Accepted { .. }))
                .count(),
            held: self
                .rows
                .values()
                .flatten()
                .filter(|submission| submission.holding())
                .map(|submission| submission.source)
                .chain(
                    self.settlements
                        .values()
                        .map(|settlement| settlement.source),
                )
                .collect(),
            settled: self.settled.clone(),
        }
    }

    fn lane_for(&self, id: CanonicalUuid) -> Option<CanonicalUuid> {
        self.identities.get(&id).copied()
    }

    fn submission(&self, id: CanonicalUuid) -> Result<&Submission, String> {
        let lane = self
            .lane_for(id)
            .ok_or_else(|| format!("composer result named unknown submission {id}"))?;
        self.rows
            .get(&lane)
            .expect("an indexed submission retains its lane queue")
            .iter()
            .find(|submission| submission.id == id)
            .ok_or_else(|| format!("composer result named unknown submission {id}"))
    }

    fn submission_mut(&mut self, id: CanonicalUuid) -> Result<&mut Submission, String> {
        let lane = self
            .identities
            .get(&id)
            .copied()
            .ok_or_else(|| format!("composer result named unknown submission {id}"))?;
        self.rows
            .get_mut(&lane)
            .expect("an indexed submission retains its lane queue")
            .iter_mut()
            .find(|submission| submission.id == id)
            .ok_or_else(|| format!("composer result named unknown submission {id}"))
    }

    fn remove(&mut self, id: CanonicalUuid) {
        let _ = self.remove_submission(id);
    }

    fn remove_submission(&mut self, id: CanonicalUuid) -> Option<Submission> {
        let lane = self.lane_for(id)?;
        let (removed, empty) = {
            let rows = self
                .rows
                .get_mut(&lane)
                .expect("an indexed submission retains its lane queue");
            let index = rows.iter().position(|submission| submission.id == id)?;
            let removed = rows.remove(index);
            (removed, rows.is_empty())
        };
        if empty {
            self.rows.remove(&lane);
        }
        if let Some(submission) = &removed {
            self.release(submission);
        }
        removed
    }

    fn release(&mut self, submission: &Submission) {
        assert_eq!(
            self.identities.remove(&submission.id),
            Some(submission.lane),
            "released submission identity remains indexed to its lane"
        );
        self.backend.remove(&submission.id);
        self.retained_bytes = self
            .retained_bytes
            .saturating_sub(submission.retained_bytes());
        let remove_payload = {
            let payload = self
                .payloads
                .get_mut(&submission.payload.id)
                .expect("released submission retains its payload accounting");
            payload.records -= 1;
            payload.records == 0
        };
        if remove_payload {
            let payload = self
                .payloads
                .remove(&submission.payload.id)
                .expect("empty payload accounting remains present");
            self.retained_bytes = self
                .retained_bytes
                .saturating_sub(payload.payload.retained_bytes());
        }
    }
}

#[cfg(test)]
mod tests;
