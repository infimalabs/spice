//! Durable taste recognition over the sole admitted lane-capture path.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread;

use bus::{Bus, JournalCommit};
use journal::{FactCause, GuardError, JournalGuard, JournalWrite};
use spice::ambient::Ambient;
use wire::body::TranscriptLocus;
use wire::body::{TasteDecision, TasteJudgeAttempt, TasteJudgeReply, TasteJudged};
use wire::{Actor, CanonicalUuid, Digest, Instant, Outcome, Producer, ResidencyHead, Seq};

use crate::message_shape::classify;

use super::{
    CommandResult, RuntimePaths, commit, current_instant, current_residency, environment_fault,
    new_uuid, steering, store_fault, typed_outcome,
};

const COMMIT_RETRIES: usize = 4;
const ADJUDICATION_CAPACITY: usize = 16;
/// The fault reason a full judge request queue raises.
pub(super) const JUDGE_SATURATED: &str = "taste::judge-worker-saturated";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Candidate {
    taste: String,
    content: Digest,
    bag: Digest,
    trigger: String,
    vendor: String,
    source: String,
    locus: TranscriptLocus,
    text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Judgment {
    pub(super) confirmed: bool,
    pub(super) attempts: Vec<TasteJudgeAttempt>,
}

#[derive(Clone, Debug)]
struct JudgedCandidate {
    candidate: Candidate,
    judgment: Judgment,
}

#[derive(Clone, Debug)]
struct PreparedJudgment {
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    anchor: Seq,
    bag: Digest,
    candidates: Vec<Candidate>,
}

#[derive(Clone, Debug)]
pub(super) struct CompletedJudgment {
    prepared: PreparedJudgment,
    decisions: Vec<JudgedCandidate>,
}

#[derive(Clone, Debug)]
struct Opening {
    candidate: Candidate,
    directive: CanonicalUuid,
    key: steer::Key,
}

#[derive(Debug, Default)]
struct TasteDirectiveProvenance {
    keys: BTreeSet<steer::Key>,
}

impl steer::Standing for TasteDirectiveProvenance {
    fn disposition(&self, key: &steer::Key) -> Option<wire::Disposition> {
        self.keys.contains(key).then_some(wire::Disposition::Live)
    }
}

impl TasteDirectiveProvenance {
    fn from_facts(facts: &bus::TaskFacts) -> CommandResult<Self> {
        let mut provenance = Self::default();
        for envelope in facts.iter() {
            let key = if let Some(body) = envelope.fact.read::<wire::body::TasteReminded>() {
                Some(body.key)
            } else if let Some(body) = envelope.fact.read::<TasteJudged>() {
                match body.decision {
                    TasteDecision::Confirmed { key, .. } => Some(key),
                    TasteDecision::Cleared => None,
                }
            } else {
                None
            };
            let Some(key) = key else {
                continue;
            };
            provenance
                .keys
                .insert(key.parse::<steer::Key>().map_err(|error| {
                    store_fault("taste::directive-key-invalid", &error.to_string())
                })?);
        }
        Ok(provenance)
    }
}

pub(super) trait Adjudicator {
    fn adjudicate(&mut self, candidate: &Candidate) -> Result<Judgment, crate::judge::Error>;
}

#[derive(Debug)]
pub(super) struct ProcessAdjudicator {
    process: crate::judge::Process,
}

impl Adjudicator for ProcessAdjudicator {
    fn adjudicate(&mut self, candidate: &Candidate) -> Result<Judgment, crate::judge::Error> {
        let decision = self.process.adjudicate(&judge_prompts(candidate))?;
        Ok(Judgment {
            confirmed: decision.confirmed(),
            attempts: decision
                .attempts()
                .iter()
                .map(|attempt| TasteJudgeAttempt {
                    framing: attempt.framing(),
                    reply: attempt.reply().to_owned(),
                    verdict: match attempt.answer() {
                        Some(true) => TasteJudgeReply::Yes,
                        Some(false) => TasteJudgeReply::No,
                        None => TasteJudgeReply::Ambiguous,
                    },
                })
                .collect(),
        })
    }
}

impl ProcessAdjudicator {
    fn new(cancellation: sill::CancellationToken, ambient: Ambient) -> Self {
        Self {
            process: crate::judge::Process::new(cancellation, ambient),
        }
    }
}

#[cfg(test)]
pub(super) struct FunctionAdjudicator<F>(pub(super) F);

#[cfg(test)]
impl<F> Adjudicator for FunctionAdjudicator<F>
where
    F: FnMut(&Candidate) -> Result<Judgment, crate::judge::Error>,
{
    fn adjudicate(&mut self, candidate: &Candidate) -> Result<Judgment, crate::judge::Error> {
        (self.0)(candidate)
    }
}

/// Bounded authority for submitting prepared questions to the one judge worker.
pub(super) struct AdjudicationSubmitter {
    requests: Option<SyncSender<PreparedJudgment>>,
    cancellation: sill::CancellationToken,
    worker: Option<thread::JoinHandle<()>>,
    pending: BTreeSet<(Digest, Digest, u64)>,
}

/// The bounded completion side stays distinct so the resident select loop can
/// await it while another branch mutably submits a newly admitted record.
pub(super) struct AdjudicationCompletions {
    receiver: tokio::sync::mpsc::Receiver<CompletedJudgment>,
}

pub(super) struct AdjudicationRuntime {
    submitter: AdjudicationSubmitter,
    completions: AdjudicationCompletions,
}

pub(super) fn start_adjudication(ambient: &Ambient) -> CommandResult<AdjudicationRuntime> {
    let ambient = ambient.clone();
    start_adjudication_with(move |cancellation| ProcessAdjudicator::new(cancellation, ambient))
}

fn start_adjudication_with<A>(
    make_adjudicator: impl FnOnce(sill::CancellationToken) -> A + Send + 'static,
) -> CommandResult<AdjudicationRuntime>
where
    A: Adjudicator + Send + 'static,
{
    let cancellation = sill::CancellationToken::new();
    let worker_cancellation = cancellation.clone();
    let worker_stop = cancellation.clone();
    let (request_sender, request_receiver) =
        mpsc::sync_channel::<PreparedJudgment>(ADJUDICATION_CAPACITY);
    let (completion_sender, completion_receiver) =
        tokio::sync::mpsc::channel(ADJUDICATION_CAPACITY);
    let worker = thread::Builder::new()
        .name("spice-taste-judge".to_owned())
        .spawn(move || {
            let mut adjudicator = make_adjudicator(worker_cancellation);
            while let Ok(prepared) = request_receiver.recv() {
                if worker_stop.is_cancelled() {
                    break;
                }
                let mut cache = BTreeMap::new();
                let decisions = adjudicate_candidates(
                    prepared.candidates.clone(),
                    prepared.anchor,
                    &mut cache,
                    &mut adjudicator,
                );
                if completion_sender
                    .blocking_send(CompletedJudgment {
                        prepared,
                        decisions,
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .map_err(|error| {
            environment_fault("taste::judge-worker-unavailable", &error.to_string())
        })?;
    Ok(AdjudicationRuntime {
        submitter: AdjudicationSubmitter {
            requests: Some(request_sender),
            cancellation,
            worker: Some(worker),
            pending: BTreeSet::new(),
        },
        completions: AdjudicationCompletions {
            receiver: completion_receiver,
        },
    })
}

impl AdjudicationRuntime {
    pub(super) fn parts(&mut self) -> (&mut AdjudicationSubmitter, &mut AdjudicationCompletions) {
        (&mut self.submitter, &mut self.completions)
    }
}

impl Drop for AdjudicationRuntime {
    fn drop(&mut self) {
        // Unblock a worker waiting to publish before cancellation and join.
        self.completions.receiver.close();
        self.submitter.shutdown();
    }
}

impl AdjudicationSubmitter {
    pub(super) fn recognize(
        &mut self,
        bus: &Bus,
        epoch: CanonicalUuid,
        bag: &taste::Bag,
        record: &lane::capture::CaptureRecord,
    ) -> CommandResult<()> {
        let Some(mut prepared) = prepare_judgment(bus, epoch, bag, record)? else {
            return Ok(());
        };
        prepared.candidates.retain(|candidate| {
            self.pending
                .insert(judgment_key(candidate, prepared.anchor))
        });
        if prepared.candidates.is_empty() {
            return Ok(());
        }
        let sender = self.requests.as_ref().ok_or_else(|| {
            environment_fault(
                "taste::judge-worker-closed",
                "the judge request authority closed while the resident was live",
            )
        })?;
        match sender.try_send(prepared) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(prepared)) => {
                self.forget(&prepared);
                Err(environment_fault(
                    JUDGE_SATURATED,
                    "the bounded judge request authority reached capacity",
                ))
            }
            Err(TrySendError::Disconnected(prepared)) => {
                self.forget(&prepared);
                Err(environment_fault(
                    "taste::judge-worker-closed",
                    "the judge request authority closed while the resident was live",
                ))
            }
        }
    }

    pub(super) fn finished(&mut self, completed: &CompletedJudgment) {
        self.forget(&completed.prepared);
    }

    fn forget(&mut self, prepared: &PreparedJudgment) {
        for candidate in &prepared.candidates {
            self.pending
                .remove(&judgment_key(candidate, prepared.anchor));
        }
    }
}

impl Drop for AdjudicationSubmitter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl AdjudicationSubmitter {
    fn shutdown(&mut self) {
        self.cancellation.cancel();
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl AdjudicationCompletions {
    pub(super) async fn recv(&mut self) -> CommandResult<CompletedJudgment> {
        self.receiver.recv().await.ok_or_else(|| {
            environment_fault(
                "taste::judge-completions-closed",
                "the judge completion authority closed while the resident was live",
            )
        })
    }
}

impl Drop for AdjudicationCompletions {
    fn drop(&mut self) {
        // Closing this side releases a worker blocked on a full completion
        // queue before the submitter cancels and joins it.
        self.receiver.close();
    }
}

/// Recognize and durably raise every unsuppressed reminder in one admitted
/// capture record.
///
/// The caller has already proved the record belongs to the current supervised
/// occupant and committed any compaction boundary it carries. An empty bag or
/// a record without eligible prose performs no journal read at all.
pub(super) fn recognize(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    bag: &taste::Bag,
    steering_config: &steer::Config,
    judge: Option<&mut dyn Adjudicator>,
    record: &lane::capture::CaptureRecord,
) -> CommandResult<()> {
    recognize_with(
        bus,
        epoch,
        bag,
        steering_config,
        judge,
        record,
        |bus, openings, proposal| {
            let committed = commit(bus, proposal)?;
            // The shared fold authority turns an inbox failure into durable
            // parked progress. Only failure to record either publication or
            // parking remains an error here.
            if !openings.is_empty() {
                steering::notify_inbox(paths, record.lane);
            }
            Ok(committed)
        },
    )
}

fn has_authored_prose(record: &lane::capture::CaptureRecord) -> bool {
    let provenance = TasteDirectiveProvenance::default();
    record.decoded.events().iter().any(|event| {
        let listen::Event::Prose(prose) = event else {
            return false;
        };
        authored_prose(prose, &provenance).is_some()
    })
}

fn taste_directive_provenance(
    bus: &Bus,
    record: &lane::capture::CaptureRecord,
) -> CommandResult<TasteDirectiveProvenance> {
    let has_acknowledgement = record.decoded.events().iter().any(|event| {
        matches!(event, listen::Event::Prose(prose) if steer::has_acknowledgement(&prose.text))
    });
    if !has_acknowledgement {
        return Ok(TasteDirectiveProvenance::default());
    }
    let snapshot = bus
        .task_snapshot()
        .map_err(|error| store_fault("taste::journal-unreadable", &error.to_string()))?;
    TasteDirectiveProvenance::from_facts(snapshot.facts())
}

fn applicability_context(
    residency: &ResidencyHead,
    record: &lane::capture::CaptureRecord,
) -> CommandResult<gate::PolicyContext> {
    let row = residency
        .lanes
        .iter()
        .find(|row| row.lane == record.lane && row.occupant == Some(record.occupant))
        .ok_or_else(|| {
            store_fault(
                "taste::occupant-moved",
                "the reminder capture no longer belongs to the current occupant",
            )
        })?;
    let mut context = gate::PolicyContext::new().with_vendor(row.vendor.as_str());
    if let Some(model) = row
        .session
        .as_ref()
        .map(|session| session.launch.model.as_str())
    {
        context = context.with_model(model);
    }
    Ok(context)
}

/// [`recognize`] with `commit_write` committing each proposal and announcing
/// its openings.
fn recognize_with<C>(
    bus: &Bus,
    epoch: CanonicalUuid,
    bag: &taste::Bag,
    steering_config: &steer::Config,
    mut judge: Option<&mut dyn Adjudicator>,
    record: &lane::capture::CaptureRecord,
    mut commit_write: C,
) -> CommandResult<()>
where
    C: FnMut(&Bus, &[Opening], JournalWrite) -> CommandResult<JournalCommit>,
{
    if !bag.tastes().iter().any(taste::Taste::enabled) || !has_authored_prose(record) {
        return Ok(());
    }
    let observed = current_residency(bus, epoch)?;
    let context = applicability_context(observed.body(), record)?;
    let provenance = taste_directive_provenance(bus, record)?;
    let candidates = candidates_with_provenance(bag, &context, record, &provenance)?;
    if candidates.is_empty() {
        return Ok(());
    }
    let mut decision_cache = BTreeMap::<(Digest, Digest, u64), Option<Judgment>>::new();

    for attempt in 0..COMMIT_RETRIES {
        let observed = current_residency(bus, epoch)?;
        require_current_occupant(observed.body(), record.lane, record.occupant)?;
        let snapshot = bus
            .task_snapshot()
            .map_err(|error| store_fault("taste::journal-unreadable", &error.to_string()))?;
        let context = Context::current(snapshot.facts(), record.lane, record.occupant)
            .map_err(|detail| store_fault("taste::epoch-missing", &detail))?;
        let anchor = context.anchor;
        let origin = context_anchor_origin(snapshot.facts(), anchor)
            .map_err(|detail| store_fault("taste::epoch-missing", &detail))?;
        let unsuppressed = context.admitted(&candidates);
        if unsuppressed.is_empty() {
            return Ok(());
        }

        let basis = Basis::read(
            bus,
            steering_config,
            epoch,
            origin,
            record.lane,
            record.occupant,
            anchor,
        )?;
        let proposal = if let Some(adjudicator) = judge.as_deref_mut() {
            judged_recognition_proposal(&basis, unsuppressed, &mut decision_cache, adjudicator)?
        } else {
            Some(reminder_proposal(
                &basis,
                one_candidate_per_warning(unsuppressed),
            )?)
        };
        let Some((openings, proposal)) = proposal else {
            return Ok(());
        };

        match commit_write(bus, &openings, proposal) {
            Ok(_) => return Ok(()),
            Err(Outcome::Refusal(_)) if attempt + 1 < COMMIT_RETRIES => {
                // A command or duplicate capture may have won between the
                // optimistic read and the guarded commit. Rebuild from the
                // new authority: a duplicate disappears, while a key collision
                // receives a fresh key.
            }
            Err(outcome) => return Err(outcome),
        }
    }
    unreachable!("the final commit attempt returns instead of retrying")
}

fn one_candidate_per_warning(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut warnings = BTreeSet::new();
    candidates
        .into_iter()
        .filter(|candidate| warnings.insert(candidate.taste.clone()))
        .collect()
}

/// The authority one taste proposal binds: the capture's occupant and
/// compaction anchor under `epoch`, the anchor's origin, and the steering key
/// clock and instants every opening it mints carries.
struct Basis {
    epoch: CanonicalUuid,
    origin: wire::Origin,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    anchor: Seq,
    minted_micros: Option<i64>,
    at: Instant,
    expires: Instant,
}

impl Basis {
    /// Read the steering key clock and stamp one proposal attempt now.
    fn read(
        bus: &Bus,
        steering_config: &steer::Config,
        epoch: CanonicalUuid,
        origin: wire::Origin,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        anchor: Seq,
    ) -> CommandResult<Self> {
        let minted_micros = bus
            .read(|read| read.steering_minted_micros())
            .map_err(|error| store_fault("taste::steering-index-unreadable", &error.to_string()))?;
        let at = current_instant()?;
        let expires =
            steering::expiry(at, steering_config.expiry().get()).map_err(typed_outcome)?;
        Ok(Self {
            epoch,
            origin,
            lane,
            occupant,
            anchor,
            minted_micros,
            at,
            expires,
        })
    }
}

fn judged_recognition_proposal(
    basis: &Basis,
    unsuppressed: Vec<Candidate>,
    decision_cache: &mut BTreeMap<(Digest, Digest, u64), Option<Judgment>>,
    adjudicator: &mut dyn Adjudicator,
) -> CommandResult<Option<(Vec<Opening>, JournalWrite)>> {
    let decisions = adjudicate_candidates(unsuppressed, basis.anchor, decision_cache, adjudicator);
    if decisions.is_empty() {
        return Ok(None);
    }
    judged_proposal(basis, decisions).map(Some)
}

fn judged_proposal(
    basis: &Basis,
    decisions: Vec<JudgedCandidate>,
) -> CommandResult<(Vec<Opening>, JournalWrite)> {
    let &Basis {
        epoch,
        ref origin,
        lane,
        occupant,
        anchor,
        minted_micros,
        at,
        expires,
    } = basis;
    let openings = openings(
        decisions
            .iter()
            .filter(|decision| decision.judgment.confirmed)
            .map(|decision| decision.candidate.clone())
            .collect(),
        minted_micros,
    )?;
    let mut proposal = JournalWrite::new(Actor::Spice, origin.clone(), at, Producer::Judge);

    proposal.guard(judgment_guard(
        epoch,
        lane,
        occupant,
        anchor,
        decisions.clone(),
        openings.clone(),
    ));
    for decided in &decisions {
        let decision = taste_decision(decided, &openings, expires);
        let judged = TasteJudged {
            lane,
            occupant,
            compaction: anchor.get(),
            taste: decided.candidate.taste.clone(),
            content: decided.candidate.content.clone(),
            bag: decided.candidate.bag.clone(),
            trigger: decided.candidate.trigger.clone(),
            vendor: decided.candidate.vendor.clone(),
            source: decided.candidate.source.clone(),
            locus: decided.candidate.locus,
            attempts: decided.judgment.attempts.clone(),
            decision,
        };
        let pending = proposal
            .fact(judged.clone(), FactCause::Existing(anchor))
            .map_err(|error| store_fault("taste::judgment-invalid", &error.to_string()))?;
        if decided.judgment.confirmed {
            let head_origin = origin.clone();
            proposal.steering_head(pending, move |seq| {
                steer::Ledger::judged_head(&judged, head_origin, at, seq)
                    .expect("the admitted judgment builds its authority head")
            });
        }
    }
    Ok((openings, proposal))
}

fn taste_decision(
    decided: &JudgedCandidate,
    openings: &[Opening],
    expires: Instant,
) -> TasteDecision {
    if decided.judgment.confirmed {
        let opening = openings
            .iter()
            .find(|opening| opening.candidate.content == decided.candidate.content)
            .expect("every confirmed decision owns one opening");
        TasteDecision::Confirmed {
            directive: opening.directive,
            key: opening.key.to_string(),
            text: opening.candidate.text.clone(),
            expires,
        }
    } else {
        TasteDecision::Cleared
    }
}

fn reminder_proposal(
    basis: &Basis,
    candidates: Vec<Candidate>,
) -> CommandResult<(Vec<Opening>, JournalWrite)> {
    let &Basis {
        epoch,
        ref origin,
        lane,
        occupant,
        anchor,
        minted_micros,
        at,
        expires,
    } = basis;
    let openings = openings(candidates, minted_micros)?;
    let mut proposal = JournalWrite::new(
        Actor::Spice,
        origin.clone(),
        at,
        Producer::ResidentRecognition,
    );
    proposal.guard(reminder_guard(
        epoch,
        lane,
        occupant,
        anchor,
        openings.clone(),
    ));
    for opening in &openings {
        let reminded = wire::body::TasteReminded {
            directive: opening.directive,
            key: opening.key.to_string(),
            lane,
            taste: opening.candidate.taste.clone(),
            content: opening.candidate.content.clone(),
            occupant,
            compaction: anchor.get(),
            locus: opening.candidate.locus,
            text: opening.candidate.text.clone(),
            expires,
        };
        let pending = proposal
            .fact(reminded.clone(), FactCause::Existing(anchor))
            .map_err(|error| store_fault("taste::reminder-invalid", &error.to_string()))?;
        let head_origin = origin.clone();
        proposal.steering_head(pending, move |seq| {
            steer::Ledger::reminded_head(&reminded, head_origin, at, seq)
                .expect("the admitted reminder builds its authority head")
        });
    }
    Ok((openings, proposal))
}

fn prepare_judgment(
    bus: &Bus,
    epoch: CanonicalUuid,
    bag: &taste::Bag,
    record: &lane::capture::CaptureRecord,
) -> CommandResult<Option<PreparedJudgment>> {
    if !bag.tastes().iter().any(taste::Taste::enabled) || !has_authored_prose(record) {
        return Ok(None);
    }
    let observed = current_residency(bus, epoch)?;
    let context = applicability_context(observed.body(), record)?;
    let provenance = taste_directive_provenance(bus, record)?;
    let candidates = candidates_with_provenance(bag, &context, record, &provenance)?;
    if candidates.is_empty() {
        return Ok(None);
    }
    require_current_occupant(observed.body(), record.lane, record.occupant)?;
    let snapshot = bus
        .task_snapshot()
        .map_err(|error| store_fault("taste::journal-unreadable", &error.to_string()))?;
    let context = Context::current(snapshot.facts(), record.lane, record.occupant)
        .map_err(|detail| store_fault("taste::epoch-missing", &detail))?;
    let candidates = context.admitted(&candidates);
    if candidates.is_empty() {
        return Ok(None);
    }
    Ok(Some(PreparedJudgment {
        lane: record.lane,
        occupant: record.occupant,
        anchor: context.anchor,
        bag: taste::bag_identity(bag),
        candidates,
    }))
}

/// Re-admit a completed external decision under the resident's current
/// authority before appending any durable fact or publishing an opening.
pub(super) fn complete_judgment(
    bus: &Bus,
    paths: &RuntimePaths,
    epoch: CanonicalUuid,
    bag: &taste::Bag,
    steering_config: &steer::Config,
    completed: CompletedJudgment,
) -> CommandResult<()> {
    if taste::bag_identity(bag) != completed.prepared.bag {
        return Ok(());
    }
    for attempt in 0..COMMIT_RETRIES {
        let observed = current_residency(bus, epoch)?;
        if !is_current_occupant(
            observed.body(),
            completed.prepared.lane,
            completed.prepared.occupant,
        ) {
            return Ok(());
        }
        let snapshot = bus
            .task_snapshot()
            .map_err(|error| store_fault("taste::journal-unreadable", &error.to_string()))?;
        let context = Context::current(
            snapshot.facts(),
            completed.prepared.lane,
            completed.prepared.occupant,
        )
        .map_err(|detail| store_fault("taste::epoch-missing", &detail))?;
        if context.anchor != completed.prepared.anchor {
            return Ok(());
        }
        let origin = context_anchor_origin(snapshot.facts(), context.anchor)
            .map_err(|detail| store_fault("taste::epoch-missing", &detail))?;
        let decisions = completed
            .decisions
            .iter()
            .filter(|decided| {
                decided.candidate.bag == completed.prepared.bag
                    && context.admits(&decided.candidate)
            })
            .cloned()
            .collect::<Vec<_>>();
        if decisions.is_empty() {
            return Ok(());
        }
        let basis = Basis::read(
            bus,
            steering_config,
            epoch,
            origin,
            completed.prepared.lane,
            completed.prepared.occupant,
            completed.prepared.anchor,
        )?;
        let (openings, proposal) = judged_proposal(&basis, decisions)?;
        match commit(bus, proposal) {
            Ok(_) => {
                if !openings.is_empty() {
                    steering::notify_inbox(paths, completed.prepared.lane);
                }
                return Ok(());
            }
            Err(Outcome::Refusal(_)) if attempt + 1 < COMMIT_RETRIES => {}
            Err(outcome) => return Err(outcome),
        }
    }
    unreachable!("the final commit attempt returns instead of retrying")
}

fn judgment_key(candidate: &Candidate, anchor: Seq) -> (Digest, Digest, u64) {
    (
        candidate.content.clone(),
        candidate.bag.clone(),
        anchor.get(),
    )
}

#[cfg(test)]
fn candidates(
    bag: &taste::Bag,
    context: &gate::PolicyContext,
    record: &lane::capture::CaptureRecord,
) -> CommandResult<Vec<Candidate>> {
    candidates_with_provenance(bag, context, record, &TasteDirectiveProvenance::default())
}

fn candidates_with_provenance(
    bag: &taste::Bag,
    context: &gate::PolicyContext,
    record: &lane::capture::CaptureRecord,
    provenance: &TasteDirectiveProvenance,
) -> CommandResult<Vec<Candidate>> {
    if bag.tastes().is_empty() {
        return Ok(Vec::new());
    }
    let bag_identity = taste::bag_identity(bag);
    let mut candidates = Vec::new();
    for (event, decoded) in record.decoded.events().iter().enumerate() {
        let listen::Event::Prose(prose) = decoded else {
            continue;
        };
        let Some(authored) = authored_prose(prose, provenance) else {
            continue;
        };
        let authored = listen::Event::Prose(listen::Prose {
            role: prose.role,
            text: authored,
            phase: prose.phase.clone(),
            // Taste derivation rewrites text, not the durable media record.
            media: prose.media.clone(),
            external_media: prose.external_media.clone(),
            locus: prose.locus,
        });
        let event = checked_locus(event)?;
        let hits = bag
            .hits(&authored, context)
            .map_err(|error| store_fault("taste::applicability-refused", &error.to_string()))?;
        for hit in hits {
            let content = taste::reminder_identity(hit.taste(), hit.context());
            if candidates
                .iter()
                .any(|candidate: &Candidate| candidate.content == content)
            {
                continue;
            }
            candidates.push(Candidate {
                taste: hit.taste().to_string(),
                content,
                bag: bag_identity.clone(),
                trigger: hit.trigger().to_owned(),
                vendor: hit.message().to_owned(),
                source: hit.judge_source(),
                locus: TranscriptLocus {
                    source: None,
                    member: hit.locus().member,
                    offset: hit.locus().offset,
                    length: hit.locus().length,
                    event,
                    line: checked_locus(hit.line())?,
                    column: checked_locus(hit.column())?,
                },
                text: hit.message().to_owned(),
            });
        }
    }
    Ok(candidates)
}

fn authored_prose(prose: &listen::Prose, provenance: &TasteDirectiveProvenance) -> Option<String> {
    let authored = classify(&prose.text).authored?;
    let mut eligible = String::with_capacity(authored.len());
    for terminated in authored.split_inclusive('\n') {
        let line = terminated.strip_suffix('\n').unwrap_or(terminated);
        let quoted = line.trim_start().starts_with('>');
        if !quoted {
            let response = steer::ack::scan::scan_acknowledgements(line, provenance)
                .into_iter()
                .find(|acknowledgement| {
                    acknowledgement.recognitions.iter().any(|recognition| {
                        matches!(
                            recognition.outcome,
                            steer::ack::scan::Outcome::Retired { .. }
                        )
                    })
                })
                .map_or(line.len(), |acknowledgement| acknowledgement.start);
            eligible.push_str(&line[..response]);
        }
        if terminated.ends_with('\n') {
            eligible.push('\n');
        }
    }
    let eligible = eligible.trim();
    (!eligible.is_empty()).then(|| eligible.to_owned())
}

fn adjudicate_candidates(
    candidates: Vec<Candidate>,
    anchor: Seq,
    cache: &mut BTreeMap<(Digest, Digest, u64), Option<Judgment>>,
    judge: &mut dyn Adjudicator,
) -> Vec<JudgedCandidate> {
    let mut confirmed = BTreeSet::new();
    candidates
        .into_iter()
        .filter_map(|candidate| {
            if confirmed.contains(&candidate.taste) {
                return None;
            }
            let key = (
                candidate.content.clone(),
                candidate.bag.clone(),
                anchor.get(),
            );
            let judgment = if let Some(cached) = cache.get(&key) {
                cached.clone()
            } else {
                let decided = judge.adjudicate(&candidate).ok();
                cache.insert(key, decided.clone());
                decided
            }?;
            if judgment.confirmed {
                confirmed.insert(candidate.taste.clone());
            }
            Some(JudgedCandidate {
                candidate,
                judgment,
            })
        })
        .collect()
}

fn judge_prompts(candidate: &Candidate) -> Vec<crate::judge::Prompt> {
    let subject = format!(
        "taste={}; trigger={}; vendor={}; authored_source={}",
        serde_json::to_string(&candidate.taste).expect("taste text encodes"),
        serde_json::to_string(&candidate.trigger).expect("trigger text encodes"),
        serde_json::to_string(&candidate.vendor).expect("vendor text encodes"),
        serde_json::to_string(&candidate.source).expect("source text encodes"),
    );
    let mut prompts = vec![
        crate::judge::Prompt::new(
            0,
            format!(
                "PROPOSITION: the authored source genuinely exhibits the configured taste behavior, rather than merely mentioning, quoting, denying, or discussing it.\n{subject}\nANSWER ONLY YES iff the proposition is true; otherwise ANSWER ONLY NO.\n"
            ),
        ),
        crate::judge::Prompt::new(
            1,
            format!(
                "Decide whether this configured taste actually applies to the authored source. A mere reference, quotation, negation, or meta-discussion does not apply.\n{subject}\nReturn exactly YES when it applies and exactly NO when it does not.\n"
            ),
        ),
        crate::judge::Prompt::new(
            2,
            format!(
                "Classify the same proposition: the source demonstrates the behavior selected by \
                 the taste trigger and warrants its vendor. Mentions without demonstrated behavior \
                 are false.\n{subject}\nOutput one token only: YES for true, NO for false.\n"
            ),
        ),
    ];
    let seed = candidate.content.as_str().as_bytes();
    let count = prompts.len();
    prompts.rotate_left(usize::from(seed[0]) % count);
    if seed[1] % 2 == 1 {
        prompts.reverse();
    }
    prompts
}

fn checked_locus(value: usize) -> CommandResult<u32> {
    u32::try_from(value)
        .map_err(|error| store_fault("taste::transcript-locus-invalid", &error.to_string()))
}

fn require_current_occupant(
    residency: &ResidencyHead,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> CommandResult<()> {
    if is_current_occupant(residency, lane, occupant) {
        Ok(())
    } else {
        Err(store_fault(
            "taste::occupant-moved",
            "the reminder capture no longer belongs to the current occupant",
        ))
    }
}

fn is_current_occupant(
    residency: &ResidencyHead,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> bool {
    residency
        .lanes
        .iter()
        .any(|row| row.lane == lane && row.occupant == Some(occupant))
}

fn openings(candidates: Vec<Candidate>, previous: Option<i64>) -> CommandResult<Vec<Opening>> {
    let now = jiff::Timestamp::now().as_microsecond();
    let mut micros = previous.map_or(Ok(now), |previous| {
        previous
            .checked_add(1)
            .map(|next| next.max(now))
            .ok_or_else(|| {
                environment_fault(
                    "taste::steering-key-invalid",
                    "epoch microsecond overflowed while advancing the indexed key clock",
                )
            })
    })?;
    let mut keys = BTreeSet::new();
    let mut opened = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let directive = new_uuid();
        let key = steer::Key::for_micros(micros).map_err(|error| {
            environment_fault("taste::steering-key-invalid", &error.to_string())
        })?;
        let inserted = keys.insert(key.clone());
        debug_assert!(inserted, "monotone moments mint distinct keys");
        micros = micros.checked_add(1).ok_or_else(|| {
            environment_fault(
                "taste::steering-key-invalid",
                "epoch microsecond overflowed while allocating one reminder batch",
            )
        })?;
        opened.push(Opening {
            candidate,
            directive,
            key,
        });
    }
    Ok(opened)
}

fn reminder_guard(
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    anchor: Seq,
    openings: Vec<Opening>,
) -> JournalGuard {
    JournalGuard::new(move |read| {
        let residency = read
            .residency_head()?
            .ok_or_else(|| GuardError::refused("taste::residency-missing"))?;
        if residency.body().epoch != epoch
            || !residency
                .body()
                .lanes
                .iter()
                .any(|row| row.lane == lane && row.occupant == Some(occupant))
        {
            return Err(GuardError::refused("taste::occupant-moved"));
        }

        let context = guarded_context(read, anchor, lane, occupant)?;
        for opening in &openings {
            if !context.admits(&opening.candidate) {
                return Err(GuardError::refused("taste::reminder-recorded"));
            }
            if read.steering_head(opening.directive)?.is_some()
                || read.steering_key(opening.key.as_str())?.is_some()
            {
                return Err(GuardError::refused("taste::steering-identity-taken"));
            }
        }
        Ok(())
    })
}

fn judgment_guard(
    epoch: CanonicalUuid,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    anchor: Seq,
    decisions: Vec<JudgedCandidate>,
    openings: Vec<Opening>,
) -> JournalGuard {
    JournalGuard::new(move |read| {
        let residency = read
            .residency_head()?
            .ok_or_else(|| GuardError::refused("taste::residency-missing"))?;
        if residency.body().epoch != epoch
            || !residency
                .body()
                .lanes
                .iter()
                .any(|row| row.lane == lane && row.occupant == Some(occupant))
        {
            return Err(GuardError::refused("taste::occupant-moved"));
        }
        let context = guarded_context(read, anchor, lane, occupant)?;
        for decided in &decisions {
            if !context.admits(&decided.candidate) {
                return Err(GuardError::refused("taste::judgment-recorded"));
            }
        }
        for opening in &openings {
            if read.steering_head(opening.directive)?.is_some()
                || read.steering_key(opening.key.as_str())?.is_some()
            {
                return Err(GuardError::refused("taste::steering-identity-taken"));
            }
        }
        Ok(())
    })
}

/// The kinds that open an occupant's context or answer under it.
const CONTEXT_KINDS: [wire::FactKind; 5] = [
    wire::FactKind::LaneStarted,
    wire::FactKind::LaneRenewed,
    wire::FactKind::LaneCompacted,
    wire::FactKind::TasteJudged,
    wire::FactKind::TasteReminded,
];

/// One occupant's context: the fact that opened it and what has been judged
/// or warned about under it since.
///
/// Every answer under an anchor is appended after it, so the whole context
/// is one forward walk from the anchor. No candidate costs a scan of the
/// history before it, however long the lane has run.
struct Context {
    anchor: Seq,
    /// Judged content digests, each with the bags it was judged under.
    judged: BTreeMap<Digest, BTreeSet<Digest>>,
    /// Tastes already reminded about or confirmed.
    warned: BTreeSet<String>,
}

impl Context {
    /// The occupant's current context in the shared task history.
    fn current(
        facts: &bus::TaskFacts,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
    ) -> Result<Self, String> {
        let anchor = context_anchor(facts, lane, occupant)?;
        Self::since(anchor, lane, occupant, facts.after(Some(anchor)))
    }

    /// The context `anchor` opened, folded from the facts after it.
    ///
    /// # Errors
    ///
    /// Names the later fact that opened a newer context for the occupant.
    fn since<'a>(
        anchor: Seq,
        lane: CanonicalUuid,
        occupant: CanonicalUuid,
        facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>,
    ) -> Result<Self, String> {
        let mut context = Self {
            anchor,
            judged: BTreeMap::new(),
            warned: BTreeSet::new(),
        };
        for envelope in facts {
            if let Some(body) = envelope.fact.read::<TasteJudged>() {
                if body.compaction != anchor.get() {
                    continue;
                }
                if matches!(body.decision, TasteDecision::Confirmed { .. }) {
                    context.warned.insert(body.taste);
                }
                context
                    .judged
                    .entry(body.content)
                    .or_default()
                    .insert(body.bag);
            } else if let Some(body) = envelope.fact.read::<wire::body::TasteReminded>() {
                if body.compaction == anchor.get() {
                    context.warned.insert(body.taste);
                }
            } else if bus::context_opening(&envelope.fact) == Some((lane, occupant)) {
                return Err(format!(
                    "lane {lane} occupant {occupant} opened a newer context at {}",
                    envelope.seq
                ));
            }
        }
        Ok(context)
    }

    /// Whether nothing under this context has answered the candidate yet.
    fn admits(&self, candidate: &Candidate) -> bool {
        !self.warned.contains(&candidate.taste)
            && !self
                .judged
                .get(&candidate.content)
                .is_some_and(|bags| bags.contains(&candidate.bag))
    }

    /// The candidates nothing under this context has answered yet.
    fn admitted(&self, candidates: &[Candidate]) -> Vec<Candidate> {
        candidates
            .iter()
            .filter(|candidate| self.admits(candidate))
            .cloned()
            .collect()
    }
}

/// The context `anchor` opened, read inside one guarded transaction.
///
/// The guard reads only the context kinds after the anchor rather than the
/// whole journal, and refuses when the occupant has since opened a newer
/// context, exactly as it would have found a different current anchor.
fn guarded_context(
    read: &journal::JournalRead<'_>,
    anchor: Seq,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> Result<Context, GuardError> {
    let page = read.selected_fact_page(
        &CONTEXT_KINDS,
        Some(anchor),
        None,
        journal::FactReadBudget::RETAINED,
    )?;
    if let Some(continuation) = page.continuation {
        return Err(GuardError::refused(format!(
            "taste context guard requires continuation after {continuation}"
        )));
    }
    Context::since(anchor, lane, occupant, &page.rows)
        .map_err(|_| GuardError::refused("taste::epoch-moved"))
}

/// The newest fact opening a context for the occupant.
pub(super) fn context_anchor(
    facts: &bus::TaskFacts,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
) -> Result<Seq, String> {
    facts.context_anchor(lane, occupant).ok_or_else(|| {
        format!("lane {lane} occupant {occupant} has no start, renewal, or compaction fact")
    })
}

/// Origin of the fact opening one context epoch.
///
/// A replacement resident may continue an occupant whose context began under
/// its predecessor. New taste facts extend that same causal chain until a lane
/// renewal or compaction opens a new context, so their write origin belongs to
/// the anchor rather than necessarily to the current resident epoch.
fn context_anchor_origin(facts: &bus::TaskFacts, anchor: Seq) -> Result<wire::Origin, String> {
    facts
        .at(anchor)
        .map(|envelope| envelope.origin.clone())
        .ok_or_else(|| format!("context anchor {anchor} is absent from its fact snapshot"))
}

#[cfg(test)]
mod tests;
