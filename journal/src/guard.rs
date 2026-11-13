use std::cell::RefCell;
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::Connection;
use serde::Serialize;

use crate::Journal;

mod claim_index;
mod commit;
mod generation;
mod indexed_read;
mod read_metrics;
mod receipt;
mod steering_content;
mod steering_index;
mod topics;

use commit::visible_cursor;
pub(crate) use commit::{SteeringIdentity, steering_fact_identity, steering_head_agrees};
#[cfg(test)]
pub(crate) use indexed_read::verify_task_page_plan;
pub use indexed_read::{
    CurrentHeadWitness, LaneCheckpointHead, TaskHeadPage, TasteEvidenceFact, TasteEvidencePage,
    TasteEvidenceSummary, ThreadError, TrustHeadPage,
};
#[cfg(test)]
pub(crate) use read_metrics::{
    IndexedReadMetrics, indexed_read_metrics, reset_indexed_read_metrics,
};
pub use receipt::CommandReplay;
pub(crate) use receipt::decode_steer_send_receipt;
pub(crate) use steering_content::supply_steering_content;
pub(crate) use steering_index::{canonical_identity, disposition_name};

/// A fact added earlier in the same guarded transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingFact {
    write: u64,
    index: usize,
}

static NEXT_WRITE: AtomicU64 = AtomicU64::new(1);

pub(crate) type ResidencyCache = RefCell<Option<JournalHead<wire::ResidencyHead>>>;

/// Causal predecessor assigned to a proposed fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactCause {
    /// Start a causal chain.
    Root,
    /// Follow an already durable fact.
    Existing(wire::Seq),
    /// Follow an earlier fact in this transaction.
    Pending(PendingFact),
}

/// One guard evaluated before any proposed effect is written.
pub struct JournalGuard {
    check: Box<GuardCheck>,
}

type GuardCheck = dyn for<'a> FnOnce(&JournalRead<'a>) -> Result<(), GuardError> + Send + 'static;

impl JournalGuard {
    /// Capture a precondition closure for evaluation inside the write
    /// transaction.
    pub fn new<F>(check: F) -> Self
    where
        F: for<'a> FnOnce(&JournalRead<'a>) -> Result<(), GuardError> + Send + 'static,
    {
        Self {
            check: Box::new(check),
        }
    }
}

impl fmt::Debug for JournalGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JournalGuard(..)")
    }
}

/// Read capability scoped to the transaction evaluating a guard.
#[derive(Clone, Copy, Debug)]
pub struct JournalRead<'a> {
    connection: &'a Connection,
    cursor: Option<wire::Seq>,
    residency_cache: Option<&'a ResidencyCache>,
}

/// Greatest number of live steering heads one indexed page can return.
pub const MAX_STEERING_PAGE_ROWS: usize = 64;

/// One bounded page of live steering heads in creation order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteeringPage {
    rows: Vec<JournalHead<wire::SteeringHead>>,
    overflow: Option<wire::Overflow>,
    horizon: Option<wire::Seq>,
}

impl SteeringPage {
    /// Heads carried by this page in creation order.
    #[must_use]
    pub fn rows(&self) -> &[JournalHead<wire::SteeringHead>] {
        &self.rows
    }

    /// Exact omitted count and cursor for the next page, when present.
    #[must_use]
    pub const fn overflow(&self) -> Option<wire::Overflow> {
        self.overflow
    }

    /// Journal authority captured by this page.
    #[must_use]
    pub const fn horizon(&self) -> Option<wire::Seq> {
        self.horizon
    }

    /// Consume the page and return its carried heads.
    #[must_use]
    pub fn into_rows(self) -> Vec<JournalHead<wire::SteeringHead>> {
        self.rows
    }
}

/// One recently retired direction shown as bounded session context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetiredDirection {
    /// Human-sized key retained for the directive's lifetime.
    pub key: String,
    /// Terminal disposition; never [`wire::Disposition::Live`].
    pub disposition: wire::Disposition,
    /// Instant the creating fact was recorded.
    pub published: wire::Instant,
    /// Original direction text.
    pub text: String,
}

/// Indexed starting boundary for one bounded fact page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactPageStart {
    /// Begin strictly after this cursor, or at the first fact when absent.
    After(Option<wire::Seq>),
    /// Begin at this exact continuation cursor.
    At(wire::Seq),
}

/// Repository-owned ceiling for one retained-fact projection read.
pub const MAX_RETAINED_FACT_PAGE_ROWS: usize = 256;

/// Repository-owned aggregate byte target for one retained-fact read.
///
/// A page always admits its first indivisible envelope so its cursor can
/// advance, then stops before another envelope would cross this target.
pub const MAX_RETAINED_FACT_PAGE_BYTES: usize = 1024 * 1024;

/// Repository-owned hard ceiling for one durable fact envelope.
///
/// This independently bounds the exceptional first row a retained-fact page
/// may decode when that row is larger than its aggregate byte target.
pub const MAX_DURABLE_FACT_ENVELOPE_BYTES: usize = 4 * 1024 * 1024;

const _: () = assert!(wire::media::MAX_MEDIA_DATA_URL_BYTES < MAX_DURABLE_FACT_ENVELOPE_BYTES);

/// Positive, repository-bounded work admitted for one retained-fact page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FactReadBudget {
    rows: usize,
    decoded_bytes: usize,
}

impl FactReadBudget {
    /// The standard maximum retained-fact work admitted by this repository.
    pub const RETAINED: Self = Self {
        rows: MAX_RETAINED_FACT_PAGE_ROWS,
        decoded_bytes: MAX_RETAINED_FACT_PAGE_BYTES,
    };

    /// Admit a smaller positive budget beneath the repository ceilings.
    pub fn new(rows: usize, decoded_bytes: usize) -> Result<Self, GuardError> {
        if rows == 0 || rows > MAX_RETAINED_FACT_PAGE_ROWS {
            return Err(GuardError::Invalid(format!(
                "fact read row budget must be within 1..={MAX_RETAINED_FACT_PAGE_ROWS}, observed {rows}"
            )));
        }
        if decoded_bytes == 0 || decoded_bytes > MAX_RETAINED_FACT_PAGE_BYTES {
            return Err(GuardError::Invalid(format!(
                "fact read decoded-byte budget must be within 1..={MAX_RETAINED_FACT_PAGE_BYTES}, observed {decoded_bytes}"
            )));
        }
        Ok(Self {
            rows,
            decoded_bytes,
        })
    }

    /// Greatest number of returned envelopes this call may decode.
    #[must_use]
    pub const fn rows(self) -> usize {
        self.rows
    }

    /// Aggregate envelope byte target after the first indivisible row.
    #[must_use]
    pub const fn decoded_bytes(self) -> usize {
        self.decoded_bytes
    }
}

/// One bounded retained-kind page and its explicit resume cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedFactPage {
    /// Matching rows within the targets, or one larger indivisible first row.
    pub rows: Vec<wire::Envelope<wire::AnyFact>>,
    /// Last returned cursor when another matching row remains.
    pub continuation: Option<wire::Seq>,
    /// Immutable journal horizon through which this page was selected.
    pub horizon: Option<wire::Seq>,
}

/// One recorded task generation advance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskGeneration {
    /// Generation the advancing transaction left the task at.
    pub generation: u64,
    /// That transaction's last fact mutating the task.
    pub position: wire::Seq,
}

impl<'a> JournalRead<'a> {
    fn scoped(
        connection: &'a Connection,
        residency_cache: &'a ResidencyCache,
    ) -> Result<Self, GuardError> {
        let cursor = visible_cursor(connection)?;
        Ok(Self {
            connection,
            cursor,
            residency_cache: Some(residency_cache),
        })
    }

    /// Read the canonical stream inside this guard's immutable transaction.
    pub fn fact_snapshot(&self) -> Result<crate::FactSnapshot, crate::SnapshotError> {
        crate::read::read_snapshot(self.connection)
    }

    /// Verify every authority plane inside this exact immutable snapshot.
    pub fn verify(&self) -> Result<crate::Verification, crate::VerifyError> {
        crate::replay::verify_connection(self.connection)
    }

    /// Verify every authority plane and keep the canonical stream it decoded.
    pub fn verified_fact_snapshot(
        &self,
    ) -> Result<(crate::Verification, crate::FactSnapshot), crate::VerifyError> {
        crate::replay::verify_connection_after_facts(self.connection, || {})
    }

    pub(crate) const fn connection(&self) -> &'a Connection {
        self.connection
    }
}

/// One authoritative head body and the fact position that established it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalHead<T> {
    position: wire::Seq,
    body: T,
}

/// One task's complete current storage-head bundle.
///
/// `journal` proves that the indexed rows are canonical, consistently keyed,
/// and observed at one cursor. Policy validation of the bodies belongs to
/// `board`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskHeads {
    statement: JournalHead<wire::TaskHead>,
    workflow: JournalHead<wire::WorkflowHead>,
    completion: JournalHead<wire::CompletionHead>,
    claims: Vec<ClaimHeads>,
}

/// One claim of a task and its lease, keyed by the fact that won the claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimHeads {
    claim: JournalHead<wire::ClaimHead>,
    lease: Option<JournalHead<wire::LeaseHead>>,
}

impl ClaimHeads {
    /// The claim head and the position that established it.
    #[must_use]
    pub const fn claim(&self) -> &JournalHead<wire::ClaimHead> {
        &self.claim
    }

    /// The claim's lease head, absent only where none was minted.
    #[must_use]
    pub const fn lease(&self) -> Option<&JournalHead<wire::LeaseHead>> {
        self.lease.as_ref()
    }

    /// Consume the storage view and return its typed bodies.
    #[must_use]
    pub fn into_bodies(self) -> (wire::ClaimHead, Option<wire::LeaseHead>) {
        (
            self.claim.into_body(),
            self.lease.map(JournalHead::into_body),
        )
    }
}

impl TaskHeads {
    /// Current task statement head and the position that established it.
    #[must_use]
    pub const fn statement(&self) -> &JournalHead<wire::TaskHead> {
        &self.statement
    }

    /// Current workflow head and the position that established it.
    #[must_use]
    pub const fn workflow(&self) -> &JournalHead<wire::WorkflowHead> {
        &self.workflow
    }

    /// Current completion head and the position that established it.
    #[must_use]
    pub const fn completion(&self) -> &JournalHead<wire::CompletionHead> {
        &self.completion
    }

    /// Every retained claim, in winning order.
    #[must_use]
    pub fn claims(&self) -> &[ClaimHeads] {
        &self.claims
    }

    /// The retained claim won at `claimed`.
    #[must_use]
    pub fn claim(&self, claimed: wire::Seq) -> Option<&ClaimHeads> {
        self.claims
            .iter()
            .find(|claim| claim.claim.body.claimed == claimed)
    }

    /// The most recently won retained claim, whatever its disposition.
    #[must_use]
    pub fn latest_claim(&self) -> Option<&ClaimHeads> {
        self.claims.last()
    }

    /// The active claim `actor` holds on this task.
    #[must_use]
    pub fn active_claim_of(&self, actor: &wire::Actor) -> Option<&ClaimHeads> {
        self.claims
            .iter()
            .find(|claim| claim.claim.body.is_active() && claim.claim.body.actor == *actor)
    }

    /// Whether any retained claim is active.
    #[must_use]
    pub fn actively_claimed(&self) -> bool {
        self.claims.iter().any(|claim| claim.claim.body.is_active())
    }

    /// Every retained claim's head and lease bodies, in winning order.
    #[must_use]
    pub fn claim_bodies(&self) -> Vec<(wire::ClaimHead, Option<wire::LeaseHead>)> {
        self.claims
            .iter()
            .map(|held| {
                (
                    held.claim.body.clone(),
                    held.lease.as_ref().map(|lease| lease.body.clone()),
                )
            })
            .collect()
    }

    /// The claim-plane head: the newest head across retained claims, absent
    /// before the first win.
    #[must_use]
    pub fn claim_plane(&self) -> Option<wire::Seq> {
        self.claims.iter().map(|held| held.claim.position).max()
    }

    /// Consume the storage view and return its typed bodies.
    #[must_use]
    pub fn into_bodies(self) -> TaskBodies {
        TaskBodies {
            statement: self.statement.into_body(),
            workflow: self.workflow.into_body(),
            completion: self.completion.into_body(),
            claims: self
                .claims
                .into_iter()
                .map(ClaimHeads::into_bodies)
                .collect(),
        }
    }
}

/// One task's typed current heads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskBodies {
    /// Statement head.
    pub statement: wire::TaskHead,
    /// Workflow head.
    pub workflow: wire::WorkflowHead,
    /// Completion head.
    pub completion: wire::CompletionHead,
    /// Every retained claim with its lease, in winning order.
    pub claims: Vec<(wire::ClaimHead, Option<wire::LeaseHead>)>,
}

impl<T> JournalHead<T> {
    /// Pair one replayed body with the fact position that established it.
    #[must_use]
    pub const fn new(position: wire::Seq, body: T) -> Self {
        Self { position, body }
    }

    /// Fact position stored beside this head.
    #[must_use]
    pub const fn position(&self) -> wire::Seq {
        self.position
    }

    /// Typed current head body.
    #[must_use]
    pub const fn body(&self) -> &T {
        &self.body
    }

    /// Consume the read capability and keep its typed body.
    #[must_use]
    pub fn into_body(self) -> T {
        self.body
    }
}

/// Stable reason for a write that current room authority rejects as unsound.
/// The request itself is invalid against durable authority, so retrying it
/// unchanged cannot succeed; the detail names the offending subject.
pub const ROOM_AUTHORITY_REFUSED: &str = "room::authority-refused";

/// A named, expected precondition failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuardRefusal {
    reason: String,
    context: Option<Box<GuardRefusalContext>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct GuardRefusalContext {
    witness: Option<String>,
    expected: Option<String>,
    observed: Option<String>,
    detail: Option<String>,
    recovery: Option<wire::SpiceCommand>,
}

impl GuardRefusal {
    /// Name the predicate that prevented this transaction.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            context: None,
        }
    }

    /// Attach the failed predicate evidence retained by the command surface.
    #[must_use]
    pub fn with_evidence(
        mut self,
        witness: impl Into<String>,
        expected: Option<String>,
        observed: Option<String>,
    ) -> Self {
        let context = self.context();
        context.witness = Some(witness.into());
        context.expected = expected;
        context.observed = observed;
        self
    }

    /// Attach human context that is not itself the runnable recovery command.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.context().detail = Some(detail.into());
        self
    }

    /// Attach the exact Spice command that can recover from this refusal.
    #[must_use]
    pub fn with_recovery(mut self, recovery: wire::SpiceCommand) -> Self {
        self.context().recovery = Some(recovery);
        self
    }

    /// Stable refusal text supplied by the guard.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Consume this refusal into the wire diagnostic and optional recovery it retained.
    #[must_use]
    pub fn into_diagnostic(self) -> (wire::RefusalDiagnostic, Option<wire::SpiceCommand>) {
        let context = self.context.map(|context| *context).unwrap_or_default();
        let diagnostic = wire::RefusalDiagnostic::new(
            self.reason,
            context.witness.or_else(|| Some("journal-guard".to_owned())),
            context.expected,
            context.observed,
        );
        let diagnostic = match context.detail {
            Some(detail) => diagnostic.with_detail(detail),
            None => diagnostic,
        };
        (diagnostic, context.recovery)
    }

    fn context(&mut self) -> &mut GuardRefusalContext {
        self.context
            .get_or_insert_with(|| Box::new(GuardRefusalContext::default()))
    }
}

impl fmt::Display for GuardRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.reason)?;
        match self
            .context
            .as_ref()
            .and_then(|context| context.detail.as_ref())
        {
            Some(detail) => write!(formatter, "; {detail}"),
            None => Ok(()),
        }
    }
}

impl Error for GuardRefusal {}

/// A guard either refused the transition or could not read authority safely.
#[derive(Debug)]
pub enum GuardError {
    /// The observed authority did not satisfy the command predicate.
    Refused(GuardRefusal),
    /// SQLite could not evaluate the predicate.
    Store(rusqlite::Error),
    /// A stored value was outside the current journal contract.
    Invalid(String),
}

impl GuardError {
    /// Construct a named zero-write refusal.
    #[must_use]
    pub fn refused(reason: impl Into<String>) -> Self {
        Self::Refused(GuardRefusal::new(reason))
    }
}

impl fmt::Display for GuardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => refusal.fmt(formatter),
            Self::Store(error) => write!(formatter, "guard read failed: {error}"),
            Self::Invalid(detail) => write!(formatter, "guard read invalid authority: {detail}"),
        }
    }
}

impl Error for GuardError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Refused(refusal) => Some(refusal),
            Self::Store(error) => Some(error),
            Self::Invalid(_) => None,
        }
    }
}

/// A durable read failed in the journal or in the typed answer builder.
#[derive(Debug)]
pub enum ReadError<E> {
    Journal(GuardError),
    Answer(E),
}

impl<E: fmt::Display> fmt::Display for ReadError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Journal(error) => write!(formatter, "journal read failed: {error}"),
            Self::Answer(error) => write!(formatter, "journal answer refused: {error}"),
        }
    }
}

impl<E> Error for ReadError<E>
where
    E: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Journal(error) => Some(error),
            Self::Answer(error) => Some(error),
        }
    }
}

/// A complete guarded write proposed to the resident bus.
#[derive(Debug)]
pub struct JournalWrite {
    id: u64,
    actor: wire::Actor,
    origin: wire::Origin,
    at: wire::Instant,
    producer: wire::Producer,
    guards: Vec<JournalGuard>,
    facts: Vec<ProposedFact>,
    heads: Vec<HeadUpdate>,
    receipt: Option<PendingReceipt>,
}

impl JournalWrite {
    /// Begin a write attributed to one actor, origin, instant, and closed
    /// producer class.
    #[must_use]
    pub fn new(
        actor: wire::Actor,
        origin: wire::Origin,
        at: wire::Instant,
        producer: wire::Producer,
    ) -> Self {
        Self {
            id: NEXT_WRITE.fetch_add(1, Ordering::Relaxed),
            actor,
            origin,
            at,
            producer,
            guards: Vec::new(),
            facts: Vec::new(),
            heads: Vec::new(),
            receipt: None,
        }
    }

    /// Evaluate this precondition inside the eventual write transaction.
    pub fn guard(&mut self, guard: JournalGuard) {
        self.guards.push(guard);
    }

    /// Add one registered fact body and return its transaction-local identity.
    ///
    /// # Errors
    ///
    /// Refuses a fact whose kind does not admit this write's producer, or a
    /// body that cannot cross the current wire encoder.
    pub fn fact<F>(&mut self, body: F, cause: FactCause) -> Result<PendingFact, BuildError>
    where
        F: wire::Body,
    {
        if let FactCause::Pending(pending) = cause
            && pending.write != self.id
        {
            return Err(BuildError::ForeignPendingFact);
        }
        let fact = wire::Fact::new(body);
        if !fact.admits_producer(self.producer, &self.actor) {
            return Err(BuildError::WrongProducer {
                kind: fact.kind(),
                expected: self.producer,
                actual: fact.producer(),
            });
        }
        let encoded = wire::encode(&fact).map_err(BuildError::Wire)?;
        let fact = wire::decode::<wire::AnyFact>(&encoded).map_err(BuildError::Wire)?;
        let pending = PendingFact {
            write: self.id,
            index: self.facts.len(),
        };
        self.facts.push(ProposedFact { fact, cause });
        Ok(pending)
    }
    /// Commit instant shared by every fact and head in this proposal.
    #[must_use]
    pub const fn at(&self) -> wire::Instant {
        self.at
    }

    /// Root attribution shared by every fact in this proposal.
    #[must_use]
    pub const fn origin(&self) -> &wire::Origin {
        &self.origin
    }

    /// Bound the largest proposed envelope before entering the commit door.
    ///
    /// Fact bytes and attribution are borrowed or fixed. Unknown assigned
    /// positions reserve the widest wire cursor, so a concurrent append cannot
    /// make an admitted envelope outgrow this bound. No encoded copy is retained.
    ///
    /// # Errors
    ///
    /// Returns an encoder error when a proposed envelope cannot be represented.
    pub fn envelope_byte_bound(&self) -> Result<usize, serde_json::Error> {
        let widest = wire::Seq::new(u64::MAX).expect("positive wire cursor");
        self.facts.iter().try_fold(0, |maximum, proposed| {
            let cause = match proposed.cause {
                FactCause::Root => None,
                FactCause::Existing(seq) => Some(seq),
                FactCause::Pending(_) => Some(widest),
            };
            let envelope = wire::Envelope {
                v: wire::WireVersion::CURRENT,
                seq: widest,
                at: self.at,
                actor: self.actor.clone(),
                origin: self.origin.clone(),
                cause,
                fact: &proposed.fact,
            };
            wire::encoded_len(&envelope).map(|bytes| maximum.max(bytes))
        })
    }

    /// Replace the current task statement head at the assigned fact position.
    pub fn task_head<F>(&mut self, fact: PendingFact, build: F)
    where
        F: FnOnce(wire::Seq) -> wire::TaskHead + Send + 'static,
    {
        self.heads.push(HeadUpdate::new(
            fact,
            HeadUpdateKind::Ordinary,
            move |seq| {
                let head = build(seq);
                let key = head.born.as_str().to_owned();
                if head.revision != seq {
                    return Err("task head revision does not name its assigned fact".to_owned());
                }
                HeadRow::text("task_heads", "stamp", key, seq, &head)
            },
        ));
    }

    /// Replace the current workflow head at the assigned fact position.
    pub fn workflow_head<F>(&mut self, task: wire::TaskStamp, fact: PendingFact, build: F)
    where
        F: FnOnce(wire::Seq) -> wire::WorkflowHead + Send + 'static,
    {
        self.heads.push(HeadUpdate::new(
            fact,
            HeadUpdateKind::Ordinary,
            move |seq| {
                HeadRow::text(
                    "workflow_heads",
                    "stamp",
                    task.as_str().to_owned(),
                    seq,
                    &build(seq),
                )
            },
        ));
    }

    /// Replace one claim's head at the assigned fact position.
    ///
    /// The row is keyed by the head's `claimed` identity; a head whose claim
    /// this fact wins names the assigned position as both.
    pub fn claim_head<F>(&mut self, task: wire::TaskStamp, fact: PendingFact, build: F)
    where
        F: FnOnce(wire::Seq) -> wire::ClaimHead + Send + 'static,
    {
        self.heads.push(HeadUpdate::new(
            fact,
            HeadUpdateKind::Ordinary,
            move |seq| {
                let head = build(seq);
                if head.fact != seq {
                    return Err("claim head fact does not name its assigned fact".to_owned());
                }
                if head.claimed > seq {
                    return Err("claim head cannot be won after its own fact".to_owned());
                }
                HeadRow::claim(task.as_str().to_owned(), seq, &head)
            },
        ));
    }

    /// Replace one claim's lease head at the assigned fact position, keyed by
    /// the lease's `claimed` identity.
    pub fn lease_head<F>(&mut self, task: wire::TaskStamp, fact: PendingFact, build: F)
    where
        F: FnOnce(wire::Seq) -> wire::LeaseHead + Send + 'static,
    {
        self.heads.push(HeadUpdate::new(
            fact,
            HeadUpdateKind::Ordinary,
            move |seq| {
                let head = build(seq);
                if head.claimed > seq {
                    return Err("lease cannot name a claim won after its own fact".to_owned());
                }
                HeadRow::lease(task.as_str().to_owned(), seq, &head)
            },
        ));
    }

    /// Replace a task's completion head at the assigned fact position.
    pub fn completion_head<F>(&mut self, task: wire::TaskStamp, fact: PendingFact, build: F)
    where
        F: FnOnce(wire::Seq) -> wire::CompletionHead + Send + 'static,
    {
        self.heads.push(HeadUpdate::new(
            fact,
            HeadUpdateKind::Ordinary,
            move |seq| {
                HeadRow::text(
                    COMPLETION_HEADS,
                    "stamp",
                    task.as_str().to_owned(),
                    seq,
                    &build(seq),
                )
            },
        ));
    }

    /// Replace one executable trust head at the assigned fact position.
    pub fn trust_head<F>(&mut self, fact: PendingFact, build: F)
    where
        F: FnOnce(wire::Seq) -> wire::TrustHead + Send + 'static,
    {
        self.heads.push(HeadUpdate::new(
            fact,
            HeadUpdateKind::Ordinary,
            move |seq| {
                let head = build(seq);
                HeadRow::text(
                    "trust_heads",
                    "digest",
                    head.digest.as_str().to_owned(),
                    seq,
                    &head,
                )
            },
        ));
    }

    /// Replace one principal's head at the assigned fact position.
    pub fn principal_head<F>(&mut self, fact: PendingFact, build: F)
    where
        F: FnOnce(wire::Seq) -> wire::principal::PrincipalHead + Send + 'static,
    {
        self.heads.push(HeadUpdate::new(
            fact,
            HeadUpdateKind::Ordinary,
            move |seq| {
                let head = build(seq);
                if head.born > seq {
                    return Err("principal head is born after its assigned fact".to_owned());
                }
                let key = head.principal.to_string();
                HeadRow::text("principal_heads", "principal", key, seq, &head)
            },
        ));
    }

    /// Replace one directive's current steering head at the assigned fact.
    pub fn steering_head<F>(&mut self, fact: PendingFact, build: F)
    where
        F: FnOnce(wire::Seq) -> wire::SteeringHead + Send + 'static,
    {
        self.heads.push(HeadUpdate::new(
            fact,
            HeadUpdateKind::Steering,
            move |seq| HeadRow::steering(seq, build(seq)),
        ));
    }

    /// Index one exact missed-recognition body at its evidence fact.
    pub fn steering_miss(&mut self, fact: PendingFact, miss: wire::body::SteerMissed) {
        self.heads.push(HeadUpdate::new(
            fact,
            HeadUpdateKind::SteeringMiss,
            move |seq| {
                let identity = canonical_identity(&miss).map_err(|error| error.to_string())?;
                HeadRow::text("steering_misses", "identity", identity, seq, &miss)
            },
        ));
    }

    /// Replace one transcript source head at the assigned fact position.
    pub fn transcript_source_head<F>(&mut self, fact: PendingFact, build: F)
    where
        F: FnOnce(wire::Seq) -> wire::TranscriptSourceHead + Send + 'static,
    {
        self.heads.push(HeadUpdate::new(
            fact,
            HeadUpdateKind::Ordinary,
            move |seq| {
                let head = build(seq);
                let key = head.key().into_string();
                HeadRow::transcript(key, seq, &head)
            },
        ));
    }

    /// Replace the singleton resident head at the assigned fact position.
    pub fn residency_head<F>(&mut self, fact: PendingFact, build: F)
    where
        F: FnOnce(wire::Seq) -> wire::ResidencyHead + Send + 'static,
    {
        self.heads.push(HeadUpdate::new(
            fact,
            HeadUpdateKind::Ordinary,
            move |seq| HeadRow::singleton("residency_head", seq, &build(seq)),
        ));
    }

    /// Store one successful command result with exactly the witnesses the
    /// canonical request carried.
    ///
    /// # Errors
    ///
    /// Refuses a second receipt, a command outside its witness law, or a body
    /// that cannot cross the current wire encoder.
    pub fn command_result<B, R>(
        &mut self,
        command: wire::Command<B>,
        epoch: wire::CanonicalUuid,
        result: R,
    ) -> Result<(), BuildError>
    where
        B: Serialize,
        R: Serialize,
    {
        if self.receipt.is_some() {
            return Err(BuildError::DuplicateReceipt);
        }
        command.witnessed().map_err(BuildError::Law)?;
        let binding = command.kind.binding();
        let body = wire::encode(&command.body).map_err(BuildError::Wire)?;
        binding.body.decode(&body).map_err(BuildError::Wire)?;
        let result = wire::encode(&result).map_err(BuildError::Wire)?;
        let result = binding.answer.decode(&result).map_err(BuildError::Wire)?;
        let request = command.canonical_json().map_err(BuildError::Wire)?;
        let command = wire::decode::<wire::Command<serde_json::Value>>(request.as_bytes())
            .map_err(BuildError::Wire)?;
        let (command, request) =
            receipt::compact_receipt_request(command).map_err(BuildError::Other)?;
        if !command.v.is_current() {
            return Err(BuildError::Other(
                "command result must use the current wire version".to_owned(),
            ));
        }
        let result = serde_json::to_value(result).map_err(BuildError::Wire)?;
        self.receipt = Some(PendingReceipt {
            command,
            request,
            epoch,
            result,
        });
        Ok(())
    }
}

#[derive(Debug)]
struct ProposedFact {
    fact: wire::AnyFact,
    cause: FactCause,
}

type HeadBuilder = dyn FnOnce(wire::Seq) -> Result<HeadRow, String> + Send + 'static;

struct HeadUpdate {
    fact: PendingFact,
    kind: HeadUpdateKind,
    build: Box<HeadBuilder>,
}

impl HeadUpdate {
    fn new<F>(fact: PendingFact, kind: HeadUpdateKind, build: F) -> Self
    where
        F: FnOnce(wire::Seq) -> Result<HeadRow, String> + Send + 'static,
    {
        Self {
            fact,
            kind,
            build: Box::new(build),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeadUpdateKind {
    Ordinary,
    Steering,
    SteeringMiss,
}

impl fmt::Debug for HeadUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeadUpdate")
            .field("fact", &self.fact)
            .finish_non_exhaustive()
    }
}

enum HeadKey {
    Text(String),
    Singleton,
    /// One claim's row, keyed by its task and the fact that won it.
    Claim {
        stamp: String,
        claimed: wire::Seq,
    },
}

/// Table holding each task's completion head.
pub(crate) const COMPLETION_HEADS: &str = "completion_heads";

struct HeadRow {
    table: &'static str,
    key_column: &'static str,
    key: HeadKey,
    seq: wire::Seq,
    body: Vec<u8>,
    steering: Option<SteeringHeadColumns>,
    claim: Option<ClaimIndexColumns>,
    transcript_lane: Option<String>,
}

struct ClaimIndexColumns {
    actor: String,
    lane: String,
    disposition: &'static str,
}

struct SteeringHeadColumns {
    key: String,
    recipient: String,
    disposition: &'static str,
    created: wire::Seq,
    minted_micros: i64,
    room: Option<String>,
}

impl HeadRow {
    fn text<T: Serialize>(
        table: &'static str,
        key_column: &'static str,
        key: String,
        seq: wire::Seq,
        body: &T,
    ) -> Result<Self, String> {
        if key.is_empty() {
            return Err(format!("{table} head key must not be empty"));
        }
        Ok(Self {
            table,
            key_column,
            key: HeadKey::Text(key),
            seq,
            body: wire::encode(body).map_err(|error| error.to_string())?,
            steering: None,
            claim: None,
            transcript_lane: None,
        })
    }

    fn singleton<T: Serialize>(
        table: &'static str,
        seq: wire::Seq,
        body: &T,
    ) -> Result<Self, String> {
        Ok(Self {
            table,
            key_column: "singleton",
            key: HeadKey::Singleton,
            seq,
            body: wire::encode(body).map_err(|error| error.to_string())?,
            steering: None,
            claim: None,
            transcript_lane: None,
        })
    }

    fn claim(stamp: String, seq: wire::Seq, head: &wire::ClaimHead) -> Result<Self, String> {
        if stamp.is_empty() {
            return Err("claim_heads head key must not be empty".to_owned());
        }
        Ok(Self {
            table: "claim_heads",
            key_column: "stamp, claimed",
            key: HeadKey::Claim {
                stamp,
                claimed: head.claimed,
            },
            seq,
            body: wire::encode(head).map_err(|error| error.to_string())?,
            steering: None,
            claim: Some(ClaimIndexColumns {
                actor: head.actor.to_string(),
                lane: head.lane.to_string(),
                disposition: claim_disposition_name(head.disposition),
            }),
            transcript_lane: None,
        })
    }

    fn lease(stamp: String, seq: wire::Seq, head: &wire::LeaseHead) -> Result<Self, String> {
        if stamp.is_empty() {
            return Err("lease_heads head key must not be empty".to_owned());
        }
        Ok(Self {
            table: "lease_heads",
            key_column: "stamp, claimed",
            key: HeadKey::Claim {
                stamp,
                claimed: head.claimed,
            },
            seq,
            body: wire::encode(head).map_err(|error| error.to_string())?,
            steering: None,
            claim: None,
            transcript_lane: None,
        })
    }

    /// Message text and attachments stay in the creating fact; reads supply them.
    fn steering(seq: wire::Seq, mut head: wire::SteeringHead) -> Result<Self, String> {
        if head.key.is_empty() {
            return Err("steering head key must not be empty".to_owned());
        }
        if head.created > seq {
            return Err("steering head cannot predate its creating fact".to_owned());
        }
        Ok(Self {
            table: "steering_heads",
            key_column: "directive",
            key: HeadKey::Text(head.directive.to_string()),
            seq,
            body: {
                head.text = String::new();
                head.attachments = Vec::new();
                wire::encode(&head).map_err(|error| error.to_string())?
            },
            steering: Some(SteeringHeadColumns {
                key: head.key.clone(),
                recipient: head.recipient.to_string(),
                disposition: disposition_name(head.disposition),
                created: head.created,
                minted_micros: head.minted_micros,
                room: head.room.as_ref().map(ToString::to_string),
            }),
            claim: None,
            transcript_lane: None,
        })
    }

    fn transcript(
        key: String,
        seq: wire::Seq,
        head: &wire::TranscriptSourceHead,
    ) -> Result<Self, String> {
        let mut row = Self::text("transcript_source_heads", "stream", key, seq, head)?;
        row.transcript_lane = Some(head.lane.to_string());
        Ok(row)
    }
}

const fn claim_disposition_name(disposition: wire::ClaimDisposition) -> &'static str {
    match disposition {
        wire::ClaimDisposition::Active => "active",
        wire::ClaimDisposition::Freed => "freed",
        wire::ClaimDisposition::Lapsed => "lapsed",
    }
}

#[derive(Debug)]
struct PendingReceipt {
    command: wire::Command<serde_json::Value>,
    request: Vec<u8>,
    epoch: wire::CanonicalUuid,
    result: serde_json::Value,
}

/// A proposal could not be made current-wire shaped before submission.
#[derive(Debug)]
pub enum BuildError {
    /// The fact belongs to another producer class.
    WrongProducer {
        kind: wire::FactKind,
        expected: wire::Producer,
        actual: wire::Producer,
    },
    /// The command's witnesses do not satisfy its closed registry row.
    Law(wire::LawError),
    /// A crossing value did not encode or decode.
    Wire(serde_json::Error),
    /// One command transaction proposed more than one receipt.
    DuplicateReceipt,
    /// A transaction-local fact identity came from another proposal.
    ForeignPendingFact,
    /// Another proposal invariant was not met.
    Other(String),
}

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongProducer {
                kind,
                expected,
                actual,
            } => write!(
                formatter,
                "fact {kind} belongs to {actual}, not proposed producer {expected}"
            ),
            Self::Law(error) => error.fmt(formatter),
            Self::Wire(error) => write!(formatter, "journal proposal is not current wire: {error}"),
            Self::DuplicateReceipt => {
                formatter.write_str("one guarded transaction stores at most one command receipt")
            }
            Self::ForeignPendingFact => {
                formatter.write_str("pending fact belongs to another journal transaction")
            }
            Self::Other(detail) => formatter.write_str(detail),
        }
    }
}

impl Error for BuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Law(error) => Some(error),
            Self::Wire(error) => Some(error),
            _ => None,
        }
    }
}

/// Durable range returned only after SQLite committed the whole proposal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Applied {
    first: wire::Seq,
    last: wire::Seq,
}

impl Applied {
    /// First fact assigned by this transaction.
    #[must_use]
    pub const fn first(&self) -> wire::Seq {
        self.first
    }

    /// Last fact assigned by this transaction.
    #[must_use]
    pub const fn last(&self) -> wire::Seq {
        self.last
    }
}

/// A guarded transaction either refused before effects or faulted and rolled
/// them back.
#[derive(Debug)]
pub enum CommitError {
    /// An expected predicate or proposal shape was not satisfied.
    Refused(GuardRefusal),
    /// Storage or canonical encoding prevented a durable commit.
    Fault(CommitFault),
}

impl fmt::Display for CommitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => write!(formatter, "journal commit refused: {refusal}"),
            Self::Fault(fault) => fault.fmt(formatter),
        }
    }
}

impl Error for CommitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Refused(refusal) => Some(refusal),
            Self::Fault(fault) => Some(fault),
        }
    }
}

/// A store or encoding defect that left the transaction with zero writes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitFault {
    detail: String,
}

impl CommitFault {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }

    /// Actionable storage or encoding detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for CommitFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "journal commit fault: {}", self.detail)
    }
}

impl Error for CommitFault {}
