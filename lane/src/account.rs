//! One accounted transcript batch and the checkpoint it advances.
//!
//! Accounting is a single guarded journal transaction. The stored source head
//! decides whether a batch may be accounted at all, and the batch fact and the
//! advanced head then cross together or not at all. There is deliberately no
//! second path that appends the fact and moves the head separately: a fact
//! whose head did not move is re-accounted on the next pass, and a head that
//! moved without its fact silently skips the spend it passed over.

use std::error::Error;
use std::fmt;

use journal::crossing::{
    Actor, CanonicalUuid, Instant, Origin, Producer, TranscriptSourceHead, TranscriptStreamKey,
    body,
};
use journal::{
    Applied, BuildError, CommitError, FactCause, GuardError, Journal, JournalGuard, JournalHead,
    JournalWrite,
};

/// Position every source generation is accounted from before its first batch.
///
/// Tape members are numbered from one, so the first byte a generation can have
/// written lives at member one offset zero, not at a zeroth member that never
/// exists.
const ORIGIN: body::TranscriptPosition = body::TranscriptPosition {
    member: 1,
    offset: 0,
};

/// Prefix marking a refusal this guard raised rather than a journal fault.
///
/// `apply` refuses for its own reasons too, and those are faults in the caller
/// rather than a statement about the batch, so the two are kept apart by a
/// spelling the guard owns instead of by matching on prose.
const REFUSED: &str = "transcript::not-continuous";

/// One batch offered for accounting, with the authorship the fact records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Accounting {
    /// Actor credited with reading the transcript.
    pub actor: Actor,
    /// Origin the ingestion fact is threaded under.
    pub origin: Origin,
    /// Instant the batch was accounted at.
    pub at: Instant,
    /// The batch itself, naming its own stream, generation, and extent.
    pub batch: body::JournalIngested,
    /// The newest clean boundary the batch closes, written after it in the
    /// same transaction so a checkpoint never exists without the accounting
    /// that covers it.
    pub checkpoint: Option<body::LaneCheckpoint>,
}

/// Why one batch was not accounted.
#[derive(Debug)]
pub enum AccountError {
    /// The batch does not continue the stored head, so nothing was written.
    Refused(String),
    /// The batch could not be prepared as journal work.
    Build(BuildError),
    /// The transaction failed for a reason that is not the batch's claim.
    Commit(CommitError),
}

impl fmt::Display for AccountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(detail) => write!(formatter, "batch is not continuous: {detail}"),
            Self::Build(error) => write!(formatter, "batch could not be prepared: {error}"),
            Self::Commit(error) => write!(formatter, "batch could not be committed: {error}"),
        }
    }
}

impl Error for AccountError {}

/// Account one batch and advance its source head in the same transaction.
///
/// # Errors
///
/// [`AccountError::Refused`] when the batch does not continue the stored head;
/// nothing is written and the prior head stands. The other variants report a
/// journal that could not carry the work.
pub fn account(journal: &mut Journal, accounting: Accounting) -> Result<Applied, AccountError> {
    let head = advanced_head(&accounting.batch);
    let mut write = JournalWrite::new(
        accounting.actor,
        accounting.origin,
        accounting.at,
        Producer::ResidentRecognition,
    );
    write.guard(continuation_guard(Continuation::of(
        &accounting.batch,
        head.key(),
    )));
    let pending = write
        .fact(accounting.batch, FactCause::Root)
        .map_err(AccountError::Build)?;
    if let Some(checkpoint) = accounting.checkpoint {
        write
            .fact(checkpoint, FactCause::Pending(pending))
            .map_err(AccountError::Build)?;
    }
    write.transcript_source_head(pending, move |_seq| head);
    journal.apply(write).map_err(map_commit_error)
}

/// The head this batch leaves behind, recomposed from the batch alone.
///
/// The head is derived rather than accepted so a caller cannot advance a
/// checkpoint to a position no batch accounted, which is the one way the fact
/// and the head could disagree while both are written.
fn advanced_head(batch: &body::JournalIngested) -> TranscriptSourceHead {
    TranscriptSourceHead {
        lane: batch.lane,
        vendor: batch.vendor.clone(),
        stream: batch.stream.clone(),
        source: batch.source,
        next: batch.to,
    }
}

/// Exactly what the stored head is judged against.
struct Continuation {
    key: TranscriptStreamKey,
    source: CanonicalUuid,
    supersedes: Option<CanonicalUuid>,
    from: body::TranscriptPosition,
    to: body::TranscriptPosition,
}

impl Continuation {
    fn of(batch: &body::JournalIngested, key: TranscriptStreamKey) -> Self {
        Self {
            key,
            source: batch.source,
            supersedes: batch.supersedes,
            from: batch.from,
            to: batch.to,
        }
    }
}

fn continuation_guard(proposed: Continuation) -> JournalGuard {
    JournalGuard::new(move |read| {
        let stored = read.transcript_source_head(&proposed.key)?;
        let (from, supersedes) = match stored.as_ref().map(JournalHead::body) {
            // An unaccounted stream holds no generation to retire, so a batch
            // naming one describes a head this authority never carried.
            None => (ORIGIN, None),
            // The same generation resumes at exactly the stored checkpoint.
            // Any other position re-accounts bytes or skips over them.
            Some(head) if head.source == proposed.source => (head.next, None),
            // A different generation restarts the stream and has to say which
            // one it retires. Inferring that from a source that merely differs
            // would turn a batch written against a stale head, or one that
            // named the wrong generation, into a silent abandonment of the
            // accounted position.
            Some(head) => (ORIGIN, Some(head.source)),
        };
        if proposed.from != from {
            return Err(GuardError::refused(format!(
                "{REFUSED}: batch continues from {} but the head stands at {}",
                locate(proposed.from),
                locate(from),
            )));
        }
        if proposed.supersedes != supersedes {
            return Err(GuardError::refused(format!(
                "{REFUSED}: batch supersedes {} but the head holds {}",
                generation(proposed.supersedes),
                generation(supersedes),
            )));
        }
        // A batch that ends before it started would move the checkpoint
        // backwards, and every byte between would be accounted a second time by
        // the batch that legitimately continues from there.
        if (proposed.to.member, proposed.to.offset) < (proposed.from.member, proposed.from.offset) {
            return Err(GuardError::refused(format!(
                "{REFUSED}: batch ends at {} before it began at {}",
                locate(proposed.to),
                locate(proposed.from),
            )));
        }
        Ok(())
    })
}

/// Keep the guard's own refusal apart from a refusal `apply` raised itself.
fn map_commit_error(error: CommitError) -> AccountError {
    match error {
        CommitError::Refused(refusal) if refusal.reason().starts_with(REFUSED) => {
            AccountError::Refused(
                refusal
                    .reason()
                    .strip_prefix(REFUSED)
                    .expect("prefix matched")
                    .trim_start_matches(": ")
                    .to_owned(),
            )
        }
        error => AccountError::Commit(error),
    }
}

fn locate(position: body::TranscriptPosition) -> String {
    format!("member {} offset {}", position.member, position.offset)
}

fn generation(source: Option<CanonicalUuid>) -> String {
    source.map_or_else(|| "no generation".to_owned(), |source| source.to_string())
}

#[cfg(test)]
mod tests {
    use journal::crossing::{
        Actor, CanonicalUuid, FactKind, Origin, Producer, TranscriptStreamKey, body,
    };
    use journal::{FactCause, Journal, JournalWrite};

    use super::{AccountError, Accounting, account};

    const LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116305";
    const FIRST: &str = "018f82d4-6f59-7d06-92e3-bce913116306";
    const SECOND: &str = "018f82d4-6f59-7d06-92e3-bce913116307";
    const THIRD: &str = "018f82d4-6f59-7d06-92e3-bce913116308";
    /// Bytes the fixture's first batch accounts, starting at the origin.
    const OPENING: u64 = 4096;
    const INITIAL_SEQUENCE: u64 = journal::crossing::Seq::FIRST.get();

    /// A batch accounted after another one, and the head it left behind.
    ///
    /// The pair is what the durable stream promises: bytes are accounted once
    /// and the checkpoint stands where the last accounted byte ended, so the
    /// next batch has exactly one position it may continue from.
    #[test]
    fn a_batch_and_the_checkpoint_it_advances_cross_together() {
        let journal = &mut authority();
        account(journal, accounting(batch(FIRST, 0, OPENING))).expect("the opening batch accounts");
        let second = batch(FIRST, OPENING, 512);
        account(journal, accounting(second.clone())).expect("the continuation accounts");

        assert_eq!(checkpoint(journal), Some(second.to));
        assert_eq!(accounted(journal), vec![(0, OPENING), (OPENING, 512)]);
    }

    /// A batch that does not continue the stored head writes nothing at all.
    ///
    /// This is the guard's whole load: delete the exact-from comparison and a
    /// batch that skips 512 bytes still commits, the head still advances past
    /// them, and the skipped spend is unrecoverable because the position that
    /// would have accounted it is now behind the checkpoint.
    #[test]
    fn a_batch_that_skips_the_stored_checkpoint_is_refused_with_no_write() {
        let journal = &mut authority();
        let opening = batch(FIRST, 0, OPENING);
        account(journal, accounting(opening.clone())).expect("the opening batch accounts");

        refusal(journal, batch(FIRST, OPENING + 512, 512));
        assert_eq!(checkpoint(journal), Some(opening.to));
        assert_eq!(accounted(journal), vec![(0, OPENING)]);
    }

    /// A batch ending before it began is refused, however continuous its start.
    ///
    /// Its start passes the exact-from comparison, so only the second guard
    /// stands between it and a checkpoint that moves backwards over bytes an
    /// earlier batch already accounted.
    #[test]
    fn a_batch_that_ends_before_it_began_is_refused() {
        let journal = &mut authority();
        let opening = batch(FIRST, 0, OPENING);
        account(journal, accounting(opening.clone())).expect("the opening batch accounts");

        let mut backwards = batch(FIRST, OPENING, 0);
        backwards.to = body::TranscriptPosition {
            member: 1,
            offset: 1024,
        };
        refusal(journal, backwards);
        assert_eq!(checkpoint(journal), Some(opening.to));
    }

    /// A new generation restarts at the origin and names the one it retires.
    ///
    /// A source that merely differs is not evidence of a new generation, so the
    /// batch that omits the claim is refused; the one that makes it is seated,
    /// and the retired generation's batch stays readable at the position it was
    /// always accounted at.
    #[test]
    fn a_new_generation_must_name_the_generation_it_retires() {
        let journal = &mut authority();
        let opening = batch(FIRST, 0, OPENING);
        account(journal, accounting(opening.clone())).expect("the opening batch accounts");

        refusal(journal, batch(SECOND, 0, 256));

        let mut successor = batch(SECOND, 0, 256);
        successor.supersedes = Some(uuid(FIRST));
        account(journal, accounting(successor.clone())).expect("a named supersession accounts");

        assert_eq!(checkpoint(journal), Some(successor.to));
        assert_eq!(
            batches(journal),
            vec![
                (INITIAL_SEQUENCE, opening),
                (INITIAL_SEQUENCE + 1, successor),
            ],
            "seating a new generation disturbed the batches the old one wrote",
        );
    }

    /// A supersession has to name the generation the head actually holds.
    ///
    /// Making the claim is not the same as making it truthfully. A batch naming
    /// some third generation describes a head this authority never carried, and
    /// seating it would abandon the position the stored generation really is
    /// accounted to: every byte FIRST already spent would be re-accounted from
    /// the origin under SECOND. The sibling above only ever offers the claim
    /// absent or exactly right, so it cannot tell identity from presence.
    #[test]
    fn a_supersession_naming_a_generation_the_head_never_held_is_refused() {
        let journal = &mut authority();
        let opening = batch(FIRST, 0, OPENING);
        account(journal, accounting(opening.clone())).expect("the opening batch accounts");

        let mut successor = batch(SECOND, 0, 256);
        successor.supersedes = Some(uuid(THIRD));
        let detail = refusal(journal, successor);
        assert!(
            detail.contains(THIRD) && detail.contains(FIRST),
            "the refusal named neither the claimed generation nor the stored one: {detail}",
        );

        assert_eq!(checkpoint(journal), Some(opening.to));
        assert_eq!(
            batches(journal),
            vec![(INITIAL_SEQUENCE, opening)],
            "a refused supersession still appended its batch",
        );
    }

    /// A fault after the fact is inserted leaves neither the fact nor the head.
    ///
    /// The head update is the step made to fail here, because it is the one
    /// that runs after the fact row is already in the table: if the transaction
    /// did not carry both, the failed write would leave a durable batch whose
    /// checkpoint never moved and the retry below would account it twice.
    #[test]
    fn a_fault_after_the_fact_is_inserted_leaves_neither_effect() {
        let journal = &mut authority();
        let opening = batch(FIRST, 0, OPENING);
        account(journal, accounting(opening.clone())).expect("the opening batch accounts");
        let second = batch(FIRST, OPENING, 512);

        let mut faulted = write();
        let stranded = write()
            .fact(second.clone(), FactCause::Root)
            .expect("a fact from another transaction");
        faulted
            .fact(second.clone(), FactCause::Root)
            .expect("the batch crosses the writer");
        faulted.transcript_source_head(stranded, {
            let head = super::advanced_head(&second);
            move |_seq| head
        });
        journal
            .apply(faulted)
            .expect_err("the head names a foreign fact");

        assert_eq!(checkpoint(journal), Some(opening.to));
        assert_eq!(accounted(journal), vec![(0, OPENING)]);

        account(journal, accounting(second.clone())).expect("the retry accounts");
        assert_eq!(checkpoint(journal), Some(second.to));
        assert_eq!(accounted(journal), vec![(0, OPENING), (OPENING, 512)]);
    }

    fn authority() -> Journal {
        Journal::in_memory().expect("in-memory authority")
    }

    fn uuid(text: &str) -> CanonicalUuid {
        text.parse().expect("fixture uuid")
    }

    fn origin() -> Origin {
        format!("lane:{LANE}").parse().expect("lane origin")
    }

    fn write() -> JournalWrite {
        JournalWrite::new(
            Actor::Spice,
            origin(),
            "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
            Producer::ResidentRecognition,
        )
    }

    fn accounting(batch: body::JournalIngested) -> Accounting {
        Accounting {
            actor: Actor::Spice,
            origin: origin(),
            at: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
            batch,
            checkpoint: None,
        }
    }

    /// One batch of `length` bytes read from `source`, starting at `from`.
    fn batch(source: &str, from: u64, length: u64) -> body::JournalIngested {
        body::JournalIngested {
            lane: uuid(LANE),
            vendor: "omp".parse().expect("fixture vendor"),
            stream: "session".to_owned(),
            source: uuid(source),
            supersedes: None,
            from: body::TranscriptPosition {
                member: 1,
                offset: from,
            },
            to: body::TranscriptPosition {
                member: 1,
                offset: from + length,
            },
            usage: vec![body::TranscriptUsage {
                turns: 1,
                input_tokens: 22005,
                cached_input_tokens: 4213,
                cache_write_input_tokens: 0,
                output_tokens: 311,
                reasoning_output_tokens: None,
                spans: vec![body::TranscriptSpan {
                    member: 1,
                    offset: from,
                    length,
                }],
                ordinal: 0,
            }],
            non_usage_records: 3,
            residue_records: 1,
        }
    }

    /// The guard's own refusal detail, requiring that nothing else went wrong.
    fn refusal(journal: &mut Journal, batch: body::JournalIngested) -> String {
        match account(journal, accounting(batch)) {
            Err(AccountError::Refused(detail)) => detail,
            other => panic!("the batch was not refused by the guard: {other:?}"),
        }
    }

    /// Position the stored head stands at, absent until a batch is accounted.
    ///
    /// The key is composed through the same constructor the writer composes
    /// with, from the same three fields, so this reader cannot address a row
    /// the writer never wrote and report an accounted stream as never
    /// accounted.
    fn checkpoint(journal: &Journal) -> Option<body::TranscriptPosition> {
        let vendor = "omp".parse().expect("fixture vendor");
        let key = TranscriptStreamKey::new(&uuid(LANE), &vendor, "session");
        journal
            .read(|read| read.transcript_source_head(&key))
            .expect("read the stored checkpoint")
            .map(|head| head.body().next)
    }

    /// Every accounted batch at the journal position it was accounted at.
    fn batches(journal: &Journal) -> Vec<(u64, body::JournalIngested)> {
        journal
            .fact_snapshot()
            .expect("fact snapshot")
            .facts()
            .iter()
            .filter(|envelope| envelope.fact.kind() == FactKind::JournalIngested)
            .map(|envelope| {
                (
                    envelope.seq.get(),
                    envelope.fact.read().expect("the batch reads back"),
                )
            })
            .collect()
    }

    /// Every accounted byte span, so a byte counted twice is a repeated entry.
    fn accounted(journal: &Journal) -> Vec<(u64, u64)> {
        batches(journal)
            .iter()
            .flat_map(|(_, batch)| batch.usage.clone())
            .flat_map(|usage| usage.spans)
            .map(|span| (span.offset, span.length))
            .collect()
    }
}
