//! The one path from a durable tape into accounted authority.
//!
//! Live capture and startup catch-up differ only in what wakes them, never in
//! how they read. Both hand a lane's tape to [`ingest`], which resumes from the
//! stored head, folds every complete record the scan yielded into one batch,
//! and accounts it. Keeping the read here is what makes the checkpoint mean
//! something: a second reader with its own resume rule would advance the same
//! head from a position this one never observed, and the spend between the two
//! positions would be accounted twice or not at all.
//!
//! Nothing in this module decides identity. A tape whose source generation is
//! missing, unreadable, or malformed is refused rather than guessed at.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use journal::crossing::{
    Actor, CanonicalUuid, Instant, Origin, TranscriptSourceHead, TranscriptStreamKey, body,
};
use journal::{Applied, GuardError, Journal, JournalHead};
use listen::{
    Checkpoint, Dialect, Event, Position, PromptStatus, RecordPayload, ScanError, SourcedEvent,
    TurnEnding, TurnMoment,
};
use tape::{SourceId, Tape};
use wire::Name;

use crate::account::{AccountError, Accounting, account};

/// Position every source generation is accounted from before its first batch.
const ORIGIN: body::TranscriptPosition = body::TranscriptPosition {
    member: 1,
    offset: 0,
};

/// One transcript to read, and the authorship its batch is recorded under.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ingestion {
    /// Lane owning the transcript.
    pub lane: CanonicalUuid,
    /// Vendor that produced it.
    pub vendor: Name,
    /// Logical stream within that lane and vendor.
    pub stream: String,
    /// Directory holding the durable tape.
    pub root: PathBuf,
    /// Decoder contract the records are read under.
    pub dialect: Dialect,
    /// Actor credited with reading the transcript.
    pub actor: Actor,
    /// Origin the ingestion fact is threaded under.
    pub origin: Origin,
    /// Instant the batch is accounted at.
    pub at: Instant,
    /// The occupant whose clean boundaries this pass may checkpoint, absent
    /// while the lane is vacant or its seated occupant has not declared its
    /// session yet. A record before `from` was written by a predecessor and
    /// earns no checkpoint for the occupant named here.
    pub occupancy: Option<Occupancy>,
}

/// The occupant a pass checkpoints for, and where its records begin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Occupancy {
    /// Seated occupant whose session the lane currently resumes.
    pub occupant: CanonicalUuid,
    /// Position of the occupant's own session declaration; every record at or
    /// after it is the occupant's.
    pub from: body::TranscriptPosition,
}

/// What one ingestion pass left behind.
#[derive(Debug)]
pub enum Ingested {
    /// No complete record was available, so no fact was written.
    ///
    /// This is the ordinary answer while a record is still being emitted: the
    /// trailing bytes have no delimiter yet, so they belong to no boundary the
    /// head could name. Writing an empty batch here would append a fact per
    /// poll that accounts nothing and advances the head to where it already
    /// stands.
    Unchanged,
    /// A batch crossed, leaving the head at the position it accounted to.
    Accounted {
        /// Journal outcome of the accounting transaction.
        applied: Applied,
        /// Exact durable batch whose commit instant can timestamp records that
        /// carried no vendor-authored time.
        batch: body::JournalIngested,
        /// Every complete record this pass hands over, in tape order.
        ///
        /// The pass already decoded these to fold the batch. Handing them back
        /// lets the caller present each one without a second read, and without
        /// depending on a per-record notification that may never have arrived.
        /// After another pass overtook a hold, the first records can precede
        /// the batch: that pass accounted them, and none was handed before.
        records: Vec<listen::Record>,
    },
    /// A pass read complete records that are held for a later batch.
    ///
    /// The records are returned exactly as an accounted pass returns them, so
    /// presentation never waits on accounting. Only the durable fact waits:
    /// the held span joins the next batch this stream accounts, and until then
    /// the stored head names the position the last batch reached.
    Deferred {
        /// Span and tallies this pass alone read, for callers that index a
        /// record's position against the pass that observed it.
        batch: body::JournalIngested,
        /// Every complete record this pass hands over, in tape order, which
        /// can begin before the batch exactly as an accounted pass's can.
        records: Vec<listen::Record>,
    },
}

/// Why one ingestion pass accounted nothing.
#[derive(Debug)]
pub enum IngestError {
    /// The stored head could not be read, so no resume point is known.
    Head(GuardError),
    /// The tape's source generation is missing, unreadable, or malformed.
    Identity {
        /// Tape whose identity could not be established.
        path: PathBuf,
        /// Typed tape failure.
        source: tape::Error,
    },
    /// The generation spelling could not cross between tape and authority.
    Spelling {
        /// Exact spelling that did not parse as a canonical generation.
        found: String,
    },
    /// The scan could not continue from its durable contract.
    Scan(ScanError),
    /// The batch was read but could not be accounted.
    Account(AccountError),
}

impl fmt::Display for IngestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Head(error) => write!(formatter, "stored head could not be read: {error}"),
            Self::Identity { path, source } => write!(
                formatter,
                "tape {} has no readable source generation: {source}",
                path.display()
            ),
            Self::Spelling { found } => {
                write!(formatter, "source generation {found:?} is not canonical")
            }
            Self::Scan(error) => write!(formatter, "transcript could not be scanned: {error}"),
            Self::Account(error) => write!(formatter, "transcript could not be accounted: {error}"),
        }
    }
}

impl Error for IngestError {}

/// How much one stream may hold before its held records are accounted.
///
/// A pass that accounts everything it read writes one fact per record, because
/// the resident reads a lane the moment a record lands. That fact is small and
/// the transcript behind it is smaller still: the durable cost of saying where
/// the cursor reached approaches the cost of the bytes it reached over. A
/// bound lets several passes share one batch, and costs only lag: the stored
/// head names the last accounted position, so a resident that stops mid-hold
/// re-reads the held span on its next start rather than losing it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bound {
    bytes: u64,
    records: usize,
}

impl Bound {
    /// Account every pass, holding nothing.
    ///
    /// Startup catch-up, a closing seat, and every caller that needs the head
    /// level with the tape it just read use this.
    pub const IMMEDIATE: Self = Self {
        bytes: 0,
        records: 0,
    };

    /// Live accounting bound for a seated lane.
    ///
    /// Both operands are ceilings on how far the stored head may trail the
    /// records a caller has already been handed, which is also how much a
    /// restart re-reads and how stale a source boundary read against the head
    /// can be.
    pub const LIVE: Self = Self {
        bytes: 64 * 1024,
        records: 64,
    };

    /// One bound of `bytes` durable transcript or `records` complete records.
    #[must_use]
    pub const fn new(bytes: u64, records: usize) -> Self {
        Self { bytes, records }
    }

    /// Whether a held span has grown past what this bound tolerates.
    ///
    /// Usage ends the hold whatever the operands say. A held prose record is
    /// already on the caller's screen, so the only thing its fact adds is a
    /// cursor; a held usage record is spend that no reader can see until it is
    /// accounted. Turn completions are a small fraction of a transcript's
    /// records, so accounting them at once keeps every reported total as
    /// timely as it was while still coalescing the bulk.
    fn reached(self, held: &Held) -> bool {
        !held.batch.usage.is_empty()
            || held.boundary.is_some()
            || held.bytes >= self.bytes
            || held.records >= self.records
    }
}

/// Records read but not yet accounted, one entry per transcript stream.
///
/// The map is the caller's, not the journal's: holding it across passes is
/// what lets a bound span them, and dropping it costs nothing but a re-read
/// from the stored head.
#[derive(Debug, Default)]
pub struct Coalescing {
    held: BTreeMap<TranscriptStreamKey, Held>,
    /// How each stream's last settled assistant message stopped, with the
    /// position the fold that saw it ended at.
    endings: BTreeMap<TranscriptStreamKey, Carried>,
}

impl Coalescing {
    /// A coalescing map holding nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How the stream's last assistant message stopped, when `resume`
    /// continues exactly where the fold that saw it ended.
    fn carried(&self, key: &TranscriptStreamKey, resume: &Resume) -> Option<TurnEnding> {
        self.endings
            .get(key)
            .filter(|carried| carried.source == resume.source && carried.through == resume.from)
            .map(|carried| carried.ending)
    }

    /// Fold one pass that read `records` records into the stream's hold and
    /// remember how it ended, answering the pass's own batch.
    fn hold(
        &mut self,
        key: &TranscriptStreamKey,
        pass: Pass,
        records: usize,
    ) -> body::JournalIngested {
        match pass.ending {
            Some(ending) => {
                self.endings.insert(
                    key.clone(),
                    Carried {
                        source: pass.batch.source,
                        through: pass.batch.to,
                        ending,
                    },
                );
            }
            None => {
                self.endings.remove(key);
            }
        }
        match self.held.get_mut(key) {
            Some(held) => held.absorb(&pass, records),
            None => {
                self.held.insert(
                    key.clone(),
                    Held {
                        bytes: span(&pass.batch),
                        records,
                        batch: pass.batch.clone(),
                        checkpoint: pass.checkpoint,
                        boundary: pass.boundary,
                    },
                );
            }
        }
        pass.batch
    }
}

/// One stream's last assistant ending, valid only for the pass that
/// continues exactly where the fold that saw it stopped.
///
/// A pass that resumes anywhere else did not read every record since, so the
/// ending it would inherit may already be superseded by one it never saw.
#[derive(Clone, Copy, Debug)]
struct Carried {
    source: CanonicalUuid,
    through: body::TranscriptPosition,
    ending: TurnEnding,
}

/// One stream's held span, folded as each pass reads more of it.
#[derive(Debug)]
struct Held {
    batch: body::JournalIngested,
    checkpoint: Checkpoint,
    bytes: u64,
    records: usize,
    /// The newest clean boundary the span closes, accounted with it.
    boundary: Option<body::LaneCheckpoint>,
}

impl Held {
    /// Fold one pass into the held batch, extending the span it will account.
    fn absorb(&mut self, pass: &Pass, records: usize) {
        self.batch.to = pass.batch.to;
        self.batch.usage.extend(pass.batch.usage.iter().cloned());
        self.batch.non_usage_records += pass.batch.non_usage_records;
        self.batch.residue_records += pass.batch.residue_records;
        self.bytes += span(&pass.batch);
        self.records += records;
        self.checkpoint = pass.checkpoint.clone();
        if pass.boundary.is_some() {
            self.boundary.clone_from(&pass.boundary);
        }
    }
}

/// What one pass read: its batch, where it left the stream, the newest clean
/// boundary it closed, and how its last settled assistant message stopped.
struct Pass {
    batch: body::JournalIngested,
    checkpoint: Checkpoint,
    boundary: Option<body::LaneCheckpoint>,
    ending: Option<TurnEnding>,
}

/// Durable bytes one pass read, counted within the member it ended in.
///
/// A pass that crossed a member seal read the sealed member's tail as well,
/// but the head names positions rather than a byte total, and a bound that
/// undercounts a rollover only accounts sooner than it had to.
fn span(pass: &body::JournalIngested) -> u64 {
    if pass.from.member == pass.to.member {
        pass.to.offset.saturating_sub(pass.from.offset)
    } else {
        pass.to.offset
    }
}

/// Read one transcript forward, accounting only once `bound` is reached.
///
/// The stored head alone decides where the read resumes and whether the
/// observed generation supersedes an earlier one. Both decisions are made
/// again by the accounting guard inside the transaction, so a head that moves
/// between this read and that commit refuses the batch rather than accounting
/// it from a position that is no longer current.
///
/// Records are returned by every pass, held or accounted, so a bound never
/// delays presentation. What it delays is the fact and the head that cross
/// with it, which are exactly the two that must move together: a held span is
/// either accounted whole or read again from the head that never moved. A
/// head that another pass moved past a hold's start wins: the hold goes, the
/// pass accounts from that head, and it hands over each record once.
///
/// # Errors
///
/// Returns [`IngestError`] when the head cannot be read, the tape has no
/// readable identity, the scan cannot continue, or the batch is refused. Every
/// one of them leaves the stored head exactly as it was found; a refused batch
/// releases the held span rather than retrying it, so the next pass resumes
/// from that head and reads those records again.
pub fn ingest_bounded(
    journal: &mut Journal,
    ingestion: Ingestion,
    coalescing: &mut Coalescing,
    bound: Bound,
) -> Result<Ingested, IngestError> {
    let key = TranscriptStreamKey::new(&ingestion.lane, &ingestion.vendor, &ingestion.stream);
    let observed = observe(&ingestion.root)?;
    let source = canonical(&observed)?;
    if coalescing
        .held
        .get(&key)
        .is_some_and(|held| held.batch.source != source)
    {
        // The generation the held span was read from is gone. Its spend is
        // accounted against the head it continued before the successor claims
        // that head, and the successor's first bytes wait for the next pass.
        return account_held(journal, &ingestion, coalescing, &key, Vec::new());
    }
    let stored = journal
        .read(|read| read.transcript_source_head(&key))
        .map_err(IngestError::Head)?
        .map(JournalHead::into_body);
    let decided = Resume::decide(stored.as_ref(), &observed)?;
    // Another accounting pass over this stream can move the head past where a
    // hold began. That hold can no longer be accounted, so it goes, and this
    // pass accounts from the stored head while still handing over only the
    // records after the boundary the hold had already handed through.
    let handed = match coalescing.held.get(&key) {
        Some(held) if !decided.begins(&held.batch) => {
            let handed = held.checkpoint.clone();
            coalescing.held.remove(&key);
            Some(handed)
        }
        _ => None,
    };
    let resume = match coalescing.held.get(&key) {
        Some(held) => Resume::held(held),
        None => decided,
    };
    let start = match &handed {
        Some(handed)
            if (handed.next.member, handed.next.offset)
                < (resume.from.member, resume.from.offset) =>
        {
            Some(handed)
        }
        _ => resume.checkpoint.as_ref(),
    };
    let scan =
        listen::scan(&ingestion.root, ingestion.dialect, start).map_err(IngestError::Scan)?;
    let accounted_from = scan
        .records
        .partition_point(|record| !ends_after(record, resume.from.member, resume.from.offset));
    let handed_from = handed.as_ref().map_or(0, |handed| {
        scan.records
            .partition_point(|record| !ends_after(record, handed.next.member, handed.next.offset))
    });
    let carried = coalescing.carried(&key, &resume);
    let pass = fold(
        &ingestion,
        &resume,
        &scan.records[accounted_from..],
        scan.checkpoint,
        carried,
    );
    let folded = scan.records.len() - accounted_from;
    let mut records = scan.records;
    records.drain(..handed_from);
    if folded == 0 {
        return if coalescing
            .held
            .get(&key)
            .is_some_and(|held| bound.reached(held))
        {
            account_held(journal, &ingestion, coalescing, &key, records)
        } else if records.is_empty() {
            Ok(Ingested::Unchanged)
        } else {
            Ok(Ingested::Deferred {
                batch: pass.batch,
                records,
            })
        };
    }
    let batch = coalescing.hold(&key, pass, folded);
    if coalescing
        .held
        .get(&key)
        .is_some_and(|held| bound.reached(held))
    {
        account_held(journal, &ingestion, coalescing, &key, records)
    } else {
        Ok(Ingested::Deferred { batch, records })
    }
}

/// Whether `record` ends after the boundary at `member` and `offset`.
///
/// Heads and checkpoints are record boundaries, so a record ending after one
/// begins at or after it.
fn ends_after(record: &listen::Record, member: u64, offset: u64) -> bool {
    (record.next.next.member, record.next.next.offset) > (member, offset)
}

/// Account one stream's held batch, releasing it either way.
///
/// The hold is taken before the transaction so a refusal cannot leave a span
/// that no head agrees with: the stored head stands, and the next pass reads
/// those records again from it.
fn account_held(
    journal: &mut Journal,
    ingestion: &Ingestion,
    coalescing: &mut Coalescing,
    key: &TranscriptStreamKey,
    records: Vec<listen::Record>,
) -> Result<Ingested, IngestError> {
    let Some(held) = coalescing.held.remove(key) else {
        return Ok(Ingested::Unchanged);
    };
    let batch = held.batch;
    let retained = batch.clone();
    let applied = account(
        journal,
        Accounting {
            actor: ingestion.actor.clone(),
            origin: ingestion.origin.clone(),
            at: ingestion.at,
            batch,
            checkpoint: held.boundary,
        },
    )
    .map_err(IngestError::Account)?;
    Ok(Ingested::Accounted {
        applied,
        batch: retained,
        records,
    })
}

/// Account one stream's held span now, reading nothing further.
///
/// A caller that must cite a record a deferred pass handed it, before the
/// bound would account that record, settles the hold here. The records the
/// span covers were already handed over, so none are returned; a stream that
/// holds nothing writes nothing.
///
/// # Errors
///
/// Returns [`IngestError::Account`] when the batch is refused; the span is
/// released either way, so the next pass reads it again from the stored head.
pub fn settle_held(
    journal: &mut Journal,
    ingestion: &Ingestion,
    coalescing: &mut Coalescing,
) -> Result<Option<Applied>, IngestError> {
    let key = TranscriptStreamKey::new(&ingestion.lane, &ingestion.vendor, &ingestion.stream);
    match account_held(journal, ingestion, coalescing, &key, Vec::new())? {
        Ingested::Accounted { applied, .. } => Ok(Some(applied)),
        Ingested::Deferred { .. } | Ingested::Unchanged => Ok(None),
    }
}

/// The generation the tape currently publishes.
///
/// This is read before the scan rather than taken from it, because the stored
/// head has to be compared against the live generation to tell a continuation
/// from a replacement. Handing a stale checkpoint to the scan would only ever
/// produce a drift refusal, which cannot be told apart from real corruption.
fn observe(root: &Path) -> Result<SourceId, IngestError> {
    Tape::open_existing(root)
        .and_then(|tape| tape.source_id())
        .map_err(|source| IngestError::Identity {
            path: root.to_path_buf(),
            source,
        })
}

/// Where the read resumes, and what the resulting batch claims about identity.
struct Resume {
    checkpoint: Option<Checkpoint>,
    source: CanonicalUuid,
    from: body::TranscriptPosition,
    supersedes: Option<CanonicalUuid>,
}

impl Resume {
    /// Whether a held batch still begins where this resume would start one,
    /// which is what the accounting guard checks against the stored head.
    fn begins(&self, batch: &body::JournalIngested) -> bool {
        batch.source == self.source
            && batch.supersedes == self.supersedes
            && batch.from == self.from
    }

    /// Resume where the held span left the stream, not where the head stands.
    ///
    /// The held span already begins at the stored head, so a pass that
    /// continues it reads only the bytes after the last one held. Its own
    /// `from` is that boundary, which is what lets a caller place a record
    /// against the pass that read it while the batch still spans them all.
    fn held(held: &Held) -> Self {
        Self {
            checkpoint: Some(held.checkpoint.clone()),
            source: held.batch.source,
            from: held.batch.to,
            supersedes: None,
        }
    }

    /// Decide the resume point from the stored head and the live generation.
    ///
    /// The three arms are exactly the ones the accounting guard recognizes: no
    /// head at all, a head on this generation, and a head on a generation the
    /// tape no longer carries. Only the middle arm resumes; the other two read
    /// the generation from its origin, and the third additionally names the
    /// generation it retires so the guard can check the claim rather than infer
    /// it from a source that merely differs.
    fn decide(
        stored: Option<&TranscriptSourceHead>,
        observed: &SourceId,
    ) -> Result<Self, IngestError> {
        let source = canonical(observed)?;
        Ok(match stored {
            Some(head) if head.source == source => Self {
                checkpoint: Some(Checkpoint {
                    source: observed.clone(),
                    next: Position {
                        member: head.next.member,
                        offset: head.next.offset,
                    },
                }),
                source,
                from: head.next,
                supersedes: None,
            },
            Some(head) => Self {
                checkpoint: None,
                source,
                from: ORIGIN,
                supersedes: Some(head.source),
            },
            None => Self {
                checkpoint: None,
                source,
                from: ORIGIN,
                supersedes: None,
            },
        })
    }
}

/// Cross one generation spelling from tape into authority.
fn canonical(source: &SourceId) -> Result<CanonicalUuid, IngestError> {
    source.as_str().parse().map_err(|_| IngestError::Spelling {
        found: source.as_str().to_owned(),
    })
}

/// Fold every complete record the scan yielded into the one batch accounting it.
///
/// A record contributes to exactly one of the three tallies. Records carrying
/// usage contribute their samples; complete records carrying none, and complete
/// residue, are counted rather than dropped, so the batch accounts for every
/// boundary the head crossed and not merely the spending ones.
///
/// The same walk closes the occupant's clean boundaries. A prompt result that
/// completed with an agent run is a turn boundary only when the last settled
/// assistant message of that run stopped by itself: a run that opened since,
/// a tool call, and a length, error, or abort ending all leave it open, and
/// so does a pass that cannot see the message at all. A completed compaction
/// announcement is a compaction boundary. Each is positioned at its record's
/// end, so the batch that carries it accounts every byte it closes, and only
/// the newest one of a pass is kept because it supersedes the others in the
/// same transaction. `carried` is the ending the previous pass left, for a
/// prompt result whose assistant message landed there.
fn fold(
    ingestion: &Ingestion,
    resume: &Resume,
    records: &[listen::Record],
    to: Checkpoint,
    carried: Option<TurnEnding>,
) -> Pass {
    let mut usage = Vec::new();
    let mut non_usage_records = 0;
    let mut residue_records = 0;
    let mut closed = None;
    let mut ending = carried;
    for record in records {
        match &record.payload {
            RecordPayload::Residue(_) => residue_records += 1,
            RecordPayload::Events(events) => {
                let before = usage.len();
                usage.extend(events.iter().filter_map(sample));
                if usage.len() == before {
                    non_usage_records += 1;
                }
            }
        }
        let Some(context) = &record.context else {
            continue;
        };
        if context.turn == Some(TurnMoment::Started) {
            ending = None;
        }
        if context.ending.is_some() {
            ending = context.ending;
        }
        let Some(occupancy) = ingestion.occupancy else {
            continue;
        };
        if (record.locus.member, record.locus.offset)
            < (occupancy.from.member, occupancy.from.offset)
        {
            continue;
        }
        let boundary = match &record.payload {
            RecordPayload::Events(events)
                if events
                    .iter()
                    .any(|sourced| matches!(sourced.event, Event::Compaction(_))) =>
            {
                Some(body::CheckpointBoundary::Compaction)
            }
            _ => context
                .prompt_outcome
                .as_ref()
                .filter(|outcome| {
                    outcome.agent_invoked
                        && outcome.status == PromptStatus::Completed
                        && ending == Some(TurnEnding::Stop)
                })
                .map(|_| body::CheckpointBoundary::Turn),
        };
        if let Some(boundary) = boundary {
            closed = Some(body::LaneCheckpoint {
                lane: ingestion.lane,
                occupant: occupancy.occupant,
                vendor: ingestion.vendor.clone(),
                stream: ingestion.stream.clone(),
                source: resume.source,
                through: body::TranscriptPosition {
                    member: record.next.next.member,
                    offset: record.next.next.offset,
                },
                boundary,
            });
        }
    }
    Pass {
        batch: body::JournalIngested {
            lane: ingestion.lane,
            vendor: ingestion.vendor.clone(),
            stream: ingestion.stream.clone(),
            source: resume.source,
            supersedes: resume.supersedes,
            from: resume.from,
            to: body::TranscriptPosition {
                member: to.next.member,
                offset: to.next.offset,
            },
            usage,
            non_usage_records,
            residue_records,
        },
        checkpoint: to,
        boundary: closed,
        ending,
    }
}

/// The normalized sample one event carries, when it carries one.
fn sample(sourced: &SourcedEvent) -> Option<body::TranscriptUsage> {
    let Event::Usage(usage) = &sourced.event else {
        return None;
    };
    Some(body::TranscriptUsage {
        turns: usage.turns,
        input_tokens: usage.input_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        cache_write_input_tokens: usage.cache_write_input_tokens,
        output_tokens: usage.output_tokens,
        reasoning_output_tokens: usage.reasoning_output_tokens,
        spans: sourced
            .spans
            .iter()
            .map(|span| body::TranscriptSpan {
                member: span.member,
                offset: span.offset,
                length: span.length,
            })
            .collect(),
        ordinal: sourced.ordinal,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use journal::crossing::{Actor, CanonicalUuid, FactKind, TranscriptSourceHead, body};
    use listen::{Dialect, ScanError};
    use tape::Tape;

    use super::{
        Bound, Coalescing, IngestError, Ingested, Ingestion, Journal, TranscriptStreamKey,
        ingest_bounded, settle_held,
    };

    mod checkpoints;

    const LANE: &str = "018f82d4-6f59-7d06-92e3-bce913116305";
    const PROSE: &[u8] =
        br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"working"}],"stopReason":"stop"}}"#;
    const USAGE: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[],"usage":{"input":11,"output":5,"cacheRead":7,"cacheWrite":3},"stopReason":"stop"}}"#;
    const RESIDUE: &[u8] = b"not-json";

    /// A pending record is only pending until its delimiter arrives.
    ///
    /// The trailing bytes belong to no boundary yet, so the head must stop
    /// short of them and the pass must write nothing at all rather than an
    /// empty batch. When the delimiter lands the remainder is accounted once,
    /// continuing from exactly the position the first pass stopped at.
    #[test]
    fn a_pending_record_leaves_the_head_where_the_last_complete_record_ended() {
        let scratch = Scratch::new("pending");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        tape.tee(&line(PROSE)).expect("prose is durable");
        tape.tee(&line(RESIDUE)).expect("residue is durable");
        let complete = (line(PROSE).len() + line(RESIDUE).len()) as u64;
        tape.tee(&USAGE[..20]).expect("a partial record is durable");
        let journal = &mut authority();

        let first = accounted(journal, scratch.path());
        assert_eq!(first.to, position(1, complete), "pending bytes advanced it");
        assert!(first.usage.is_empty(), "no usage record was complete");
        assert_eq!((first.non_usage_records, first.residue_records), (1, 1));

        assert!(
            matches!(
                ingest(journal, ingestion(scratch.path())),
                Ok(Ingested::Unchanged)
            ),
            "a pass with no new complete record still wrote a batch",
        );
        assert_eq!(batches(journal).len(), 1, "the empty pass appended a fact");

        tape.tee(&line(&USAGE[20..])).expect("the record completes");
        let second = accounted(journal, scratch.path());
        assert_eq!(second.from, position(1, complete), "the replay lost bytes");
        assert_eq!(second.to, position(1, complete + line(USAGE).len() as u64));
        assert_eq!(second.usage.len(), 1, "the completed record carried usage");
        scratch.remove();
    }

    /// Sealing a member moves the head forward without re-reading the seal.
    ///
    /// A sealed member cannot grow, so the next record begins the member after
    /// it. The batch still continues from the position the head actually held,
    /// which is what proves the sealed member was not accounted a second time.
    #[test]
    fn a_sealed_member_carries_the_head_into_the_member_after_it() {
        let scratch = Scratch::new("sealing");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        tape.tee(&line(PROSE)).expect("prose is durable");
        let journal = &mut authority();
        let first = accounted(journal, scratch.path());
        assert_eq!(first.to, position(1, line(PROSE).len() as u64));

        tape.seal().expect("the member seals");
        tape.tee(&line(USAGE)).expect("the next member grows");

        let second = accounted(journal, scratch.path());
        assert_eq!(second.from, first.to, "the batch skipped the sealed member");
        assert_eq!(second.to, position(2, line(USAGE).len() as u64));
        assert_eq!(second.usage.len(), 1, "the sealed-past record was lost");
        scratch.remove();
    }

    /// A generation replaced at the same path must be claimed, not assumed.
    ///
    /// The path is unchanged, so nothing about the location says the bytes are
    /// new. Only the generation says so, and the batch has to name the one it
    /// retires: a batch that merely arrived with a different source would be a
    /// silent restart abandoning every byte the old generation accounted.
    #[test]
    fn a_replacement_generation_supersedes_the_one_the_head_holds() {
        let scratch = Scratch::new("replacement");
        let root = scratch.path().join("selected");
        let first = Tape::open(&root).expect("the first generation opens");
        first.tee(&line(USAGE)).expect("the first generation grows");
        let journal = &mut authority();
        let opening = accounted(journal, &root);
        assert_eq!(opening.supersedes, None, "an opening batch retired nothing");

        fs::remove_dir_all(&root).expect("the first generation is removed");
        let replacement = Tape::open(&root).expect("the replacement opens");
        replacement
            .tee(&line(PROSE))
            .expect("the replacement grows");

        let restarted = accounted(journal, &root);
        assert_eq!(
            restarted.supersedes,
            Some(opening.source),
            "the replacement did not name the generation it retired",
        );
        assert_eq!(restarted.from, position(1, 0), "it resumed a dead position");
        assert_ne!(restarted.source, opening.source);
        assert_eq!(
            head(journal).expect("a head stands").source,
            restarted.source
        );
        scratch.remove();
    }

    /// Bytes that retreat behind the head are drift, not a shorter transcript.
    ///
    /// The stored position names bytes the generation is now too short to hold,
    /// so no honest continuation exists. Accounting from the origin instead
    /// would re-spend everything already accounted, so the pass refuses and
    /// leaves the head exactly where the last real batch put it.
    #[test]
    fn bytes_retreating_behind_the_head_refuse_without_moving_it() {
        let scratch = Scratch::new("retreat");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        tape.tee(&line(USAGE)).expect("the record is durable");
        let journal = &mut authority();
        let opening = accounted(journal, scratch.path());

        fs::write(member(scratch.path()), b"short\n").expect("the member retreats");

        match ingest(journal, ingestion(scratch.path())) {
            Err(IngestError::Scan(ScanError::CheckpointRetreated { .. })) => {}
            other => panic!("a retreated member was not refused as drift: {other:?}"),
        }
        assert_eq!(head(journal).expect("a head stands").next, opening.to);
        assert_eq!(batches(journal).len(), 1, "the refusal appended a fact");
        scratch.remove();
    }

    /// A head standing past every member the tape offers is not a resume.
    ///
    /// The stored position names a member the tape no longer supplies at all,
    /// so no boundary exists to continue from. Reading the absence as a fresh
    /// tape would account the whole generation from the origin a second time,
    /// so the pass refuses and leaves the head where the last real batch put it.
    #[test]
    fn a_head_past_every_member_the_tape_supplies_refuses_without_moving_it() {
        let scratch = Scratch::new("gap");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        tape.tee(&line(USAGE)).expect("the record is durable");
        let journal = &mut authority();
        let opening = accounted(journal, scratch.path());

        fs::remove_file(member(scratch.path())).expect("every member is withdrawn");

        match ingest(journal, ingestion(scratch.path())) {
            Err(IngestError::Scan(ScanError::CheckpointGap { .. })) => {}
            other => panic!("a head past every member was not refused: {other:?}"),
        }
        assert_eq!(head(journal).expect("a head stands").next, opening.to);
        assert_eq!(batches(journal).len(), 1, "the refusal appended a fact");
        scratch.remove();
    }

    /// A bound holds the head back without holding the records back.
    ///
    /// Every pass hands its records to the caller, so display never waits for
    /// accounting. What waits is the fact: passes under the bound write none,
    /// and the batch that finally crosses spans all of them exactly once, with
    /// every tally the held passes read.
    #[test]
    fn records_held_under_a_bound_account_once_as_the_span_they_cover() {
        let scratch = Scratch::new("holding");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        tape.tee(&line(PROSE)).expect("prose is durable");
        let journal = &mut authority();
        let coalescing = &mut Coalescing::new();
        let bound = Bound::new(4096, 3);

        match ingest_bounded(journal, ingestion(scratch.path()), coalescing, bound) {
            Ok(Ingested::Deferred { records, .. }) => assert_eq!(records.len(), 1),
            other => panic!("the first pass did not hold its record: {other:?}"),
        }
        assert!(batches(journal).is_empty(), "a held pass wrote a fact");
        assert!(head(journal).is_none(), "a held pass moved the head");

        tape.tee(&line(RESIDUE)).expect("residue is durable");
        match ingest_bounded(journal, ingestion(scratch.path()), coalescing, bound) {
            Ok(Ingested::Deferred { records, .. }) => assert_eq!(records.len(), 1),
            other => panic!("the second pass did not hold its record: {other:?}"),
        }
        assert!(
            batches(journal).is_empty(),
            "two records crossed a bound of three"
        );

        tape.tee(&line(PROSE)).expect("more prose is durable");
        let span = (line(PROSE).len() * 2 + line(RESIDUE).len()) as u64;
        match ingest_bounded(journal, ingestion(scratch.path()), coalescing, bound) {
            Ok(Ingested::Accounted { batch, records, .. }) => {
                assert_eq!(records.len(), 1, "an accounting pass replayed held records");
                assert_eq!(batch.from, position(1, 0), "the batch lost held bytes");
                assert_eq!(batch.to, position(1, span));
                assert_eq!((batch.non_usage_records, batch.residue_records), (2, 1));
            }
            other => panic!("the bound did not account the held span: {other:?}"),
        }
        assert_eq!(
            batches(journal).len(),
            1,
            "the held passes each wrote a fact"
        );
        assert_eq!(
            head(journal).expect("a head stands").next,
            position(1, span)
        );
        scratch.remove();
    }

    /// Spend is never held: a usage record accounts the pass that read it.
    ///
    /// A held prose record is already displayed, so its fact only records a
    /// cursor. A held usage record is spend no reader can see until it is
    /// accounted, so the bound yields to it and the batch lands at once,
    /// carrying the prose held ahead of it.
    #[test]
    fn a_usage_record_accounts_at_once_however_wide_the_bound() {
        let scratch = Scratch::new("spend");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        tape.tee(&line(PROSE)).expect("prose is durable");
        let journal = &mut authority();
        let coalescing = &mut Coalescing::new();
        let bound = Bound::new(1 << 20, 4096);

        let held = ingest_bounded(journal, ingestion(scratch.path()), coalescing, bound);
        assert!(matches!(held, Ok(Ingested::Deferred { .. })), "{held:?}");

        tape.tee(&line(USAGE)).expect("usage is durable");
        match ingest_bounded(journal, ingestion(scratch.path()), coalescing, bound) {
            Ok(Ingested::Accounted { batch, .. }) => {
                assert_eq!(batch.usage.len(), 1, "the accounted batch lost the spend");
                assert_eq!(batch.non_usage_records, 1, "the held prose was dropped");
                assert_eq!(batch.from, position(1, 0));
            }
            other => panic!("spend waited on the bound: {other:?}"),
        }
        scratch.remove();
    }

    /// A bound reached with nothing new still accounts what is held.
    ///
    /// This is the seat-closing and shutdown path: the tape has stopped
    /// growing, so a pass that waited for more records would hold the span
    /// forever. An immediate bound accounts it on the spot, and the pass after
    /// it has nothing left to say.
    #[test]
    fn an_immediate_pass_accounts_a_held_span_the_tape_no_longer_grows() {
        let scratch = Scratch::new("draining");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        tape.tee(&line(PROSE)).expect("the record is durable");
        let journal = &mut authority();
        let coalescing = &mut Coalescing::new();

        let held = ingest_bounded(
            journal,
            ingestion(scratch.path()),
            coalescing,
            Bound::new(4096, 64),
        );
        assert!(matches!(held, Ok(Ingested::Deferred { .. })), "{held:?}");

        match ingest_bounded(
            journal,
            ingestion(scratch.path()),
            coalescing,
            Bound::IMMEDIATE,
        ) {
            Ok(Ingested::Accounted { batch, records, .. }) => {
                assert!(records.is_empty(), "the drain replayed held records");
                assert_eq!(batch.to, position(1, line(PROSE).len() as u64));
                assert_eq!(batch.non_usage_records, 1, "the drain lost the held record");
            }
            other => panic!("the drain did not account the held span: {other:?}"),
        }
        assert!(
            matches!(
                ingest_bounded(
                    journal,
                    ingestion(scratch.path()),
                    coalescing,
                    Bound::IMMEDIATE
                ),
                Ok(Ingested::Unchanged)
            ),
            "a drained stream still had something to account",
        );
        assert_eq!(batches(journal).len(), 1);
        scratch.remove();
    }

    /// Settling accounts exactly the held span and reads nothing past it.
    ///
    /// A caller that must cite a held record settles the hold without taking
    /// records it was never handed: a record that landed after the span stays
    /// for the next pass, which resumes where the settled batch ended, and a
    /// stream holding nothing writes nothing.
    #[test]
    fn settling_a_hold_accounts_its_span_and_leaves_later_records_unread() {
        let scratch = Scratch::new("settling");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        tape.tee(&line(PROSE)).expect("prose is durable");
        let journal = &mut authority();
        let coalescing = &mut Coalescing::new();
        let held = ingest_bounded(journal, ingestion(scratch.path()), coalescing, Bound::LIVE);
        assert!(matches!(held, Ok(Ingested::Deferred { .. })), "{held:?}");
        tape.tee(&line(RESIDUE)).expect("a later record is durable");

        let settled = settle_held(journal, &ingestion(scratch.path()), coalescing);
        assert!(matches!(settled, Ok(Some(_))), "{settled:?}");
        let prose = line(PROSE).len() as u64;
        assert_eq!(
            head(journal).expect("a head stands").next,
            position(1, prose)
        );
        assert!(
            matches!(
                settle_held(journal, &ingestion(scratch.path()), coalescing),
                Ok(None)
            ),
            "a settled stream still held something",
        );

        match ingest_bounded(
            journal,
            ingestion(scratch.path()),
            coalescing,
            Bound::IMMEDIATE,
        ) {
            Ok(Ingested::Accounted { batch, records, .. }) => {
                assert_eq!(records.len(), 1, "the later record was lost or replayed");
                assert_eq!(batch.from, position(1, prose));
                assert_eq!(batch.residue_records, 1);
            }
            other => panic!("the next pass did not resume after the settled span: {other:?}"),
        }
        assert_eq!(batches(journal).len(), 2);
        scratch.remove();
    }

    /// Dropping the held span costs a re-read, never a gap.
    ///
    /// A resident that stops mid-hold leaves the head where the last batch put
    /// it, so the next start reads the held records again and accounts them
    /// from that same position. Nothing is accounted twice, because nothing
    /// was accounted the first time.
    #[test]
    fn a_forgotten_hold_is_read_again_from_the_head_that_never_moved() {
        let scratch = Scratch::new("forgotten");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        tape.tee(&line(PROSE)).expect("prose is durable");
        let journal = &mut authority();
        let opening = accounted(journal, scratch.path());

        tape.tee(&line(RESIDUE)).expect("the record is durable");
        let mut forgotten = Coalescing::new();
        let held = ingest_bounded(
            journal,
            ingestion(scratch.path()),
            &mut forgotten,
            Bound::new(4096, 64),
        );
        assert!(matches!(held, Ok(Ingested::Deferred { .. })), "{held:?}");
        drop(forgotten);

        let batch = accounted(journal, scratch.path());
        assert_eq!(batch.from, opening.to, "the restart lost the held bytes");
        assert_eq!(batch.residue_records, 1, "the held record was never read");
        assert_eq!(batches(journal).len(), 2, "the dropped hold wrote a fact");
        scratch.remove();
    }

    /// A hold another writer overtook accounts only what lies past the head.
    ///
    /// A second accounting pass over the same stream can move the stored head
    /// beyond a hold's start. The hold's next pass then accounts from that
    /// head, so the guard admits it, and still hands over every record the
    /// hold had not handed yet: nothing is refused, replayed, or counted twice.
    #[test]
    fn a_hold_another_writer_overtook_accounts_from_the_stored_head() {
        let scratch = Scratch::new("overtaken");
        let tape = Tape::open(scratch.path()).expect("tape opens");
        tape.tee(&line(PROSE)).expect("prose is durable");
        let journal = &mut authority();
        let live = &mut Coalescing::new();
        let held = ingest_bounded(journal, ingestion(scratch.path()), live, Bound::LIVE);
        assert!(matches!(held, Ok(Ingested::Deferred { .. })), "{held:?}");

        tape.tee(&line(RESIDUE))
            .expect("a record the hold never read is durable");
        let overtaking = accounted(journal, scratch.path());
        tape.tee(&line(USAGE)).expect("a later record is durable");

        match ingest_bounded(journal, ingestion(scratch.path()), live, Bound::IMMEDIATE) {
            Ok(Ingested::Accounted { batch, records, .. }) => {
                assert_eq!(
                    batch.from, overtaking.to,
                    "the batch ignored the stored head"
                );
                assert_eq!(
                    batch.residue_records, 0,
                    "overtaken bytes were counted twice"
                );
                assert_eq!(batch.usage.len(), 1, "the later spend was not accounted");
                assert_eq!(records.len(), 2, "an unhanded record was lost or replayed");
            }
            other => panic!("the overtaken hold was not accounted from the head: {other:?}"),
        }
        assert_eq!(batches(journal).len(), 2);
        assert!(
            matches!(
                settle_held(journal, &ingestion(scratch.path()), live),
                Ok(None)
            ),
            "the overtaken hold survived its pass",
        );
        scratch.remove();
    }

    /// A generation replaced mid-hold accounts the held span before it goes.
    ///
    /// The held bytes belong to the generation that is being retired, and the
    /// head can only carry one generation at a time. Accounting the hold first
    /// keeps that span, and leaves the successor to claim the head with the
    /// supersession the guard checks.
    #[test]
    fn a_replacement_generation_accounts_the_span_the_hold_still_carries() {
        let scratch = Scratch::new("handover");
        let root = scratch.path().join("selected");
        let first = Tape::open(&root).expect("the first generation opens");
        first.tee(&line(PROSE)).expect("the record is durable");
        let journal = &mut authority();
        let coalescing = &mut Coalescing::new();
        let bound = Bound::new(4096, 64);

        let held = ingest_bounded(journal, ingestion(&root), coalescing, bound);
        assert!(matches!(held, Ok(Ingested::Deferred { .. })), "{held:?}");

        fs::remove_dir_all(&root).expect("the generation is withdrawn");
        let second = Tape::open(&root).expect("the replacement opens");
        second.tee(&line(PROSE)).expect("the record is durable");

        match ingest_bounded(journal, ingestion(&root), coalescing, bound) {
            Ok(Ingested::Accounted { batch, records, .. }) => {
                assert!(records.is_empty(), "the handover replayed held records");
                assert_eq!(batch.non_usage_records, 1, "the retired span was dropped");
                assert!(
                    batch.supersedes.is_none(),
                    "the hold claimed a supersession"
                );
            }
            other => panic!("the handover did not account the hold: {other:?}"),
        }

        let replacement = accounted(journal, &root);
        assert_eq!(replacement.from, position(1, 0));
        assert!(
            replacement.supersedes.is_some(),
            "the successor claimed no supersession",
        );
        scratch.remove();
    }

    fn authority() -> Journal {
        Journal::in_memory().expect("in-memory authority")
    }

    fn ingestion(root: &Path) -> Ingestion {
        Ingestion {
            lane: LANE.parse().expect("fixture lane"),
            vendor: "omp".parse().expect("fixture vendor"),
            stream: "session".to_owned(),
            root: root.to_path_buf(),
            dialect: Dialect::Omp,
            actor: Actor::Spice,
            origin: format!("lane:{LANE}").parse().expect("lane origin"),
            at: "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
            occupancy: None,
        }
    }

    /// One unheld pass, the shape every pre-coalescing case is stated in.
    fn ingest(journal: &mut Journal, ingestion: Ingestion) -> Result<Ingested, IngestError> {
        ingest_bounded(journal, ingestion, &mut Coalescing::new(), Bound::IMMEDIATE)
    }

    /// One pass that must have accounted a batch, returned as that batch.
    fn accounted(journal: &mut Journal, root: &Path) -> body::JournalIngested {
        match ingest(journal, ingestion(root)) {
            Ok(Ingested::Accounted { .. }) => batches(journal).pop().expect("the batch is stored"),
            other => panic!("the pass accounted nothing: {other:?}"),
        }
    }

    fn head(journal: &Journal) -> Option<TranscriptSourceHead> {
        let vendor = "omp".parse().expect("fixture vendor");
        let lane: CanonicalUuid = LANE.parse().expect("fixture lane");
        journal
            .read(|read| {
                read.transcript_source_head(&TranscriptStreamKey::new(&lane, &vendor, "session"))
            })
            .expect("read the stored head")
            .map(journal::JournalHead::into_body)
    }

    fn batches(journal: &Journal) -> Vec<body::JournalIngested> {
        journal
            .fact_snapshot()
            .expect("fact snapshot")
            .facts()
            .iter()
            .filter(|envelope| envelope.fact.kind() == FactKind::JournalIngested)
            .map(|envelope| envelope.fact.read().expect("the batch reads back"))
            .collect()
    }

    const fn position(member: u64, offset: u64) -> body::TranscriptPosition {
        body::TranscriptPosition { member, offset }
    }

    fn line(bytes: &[u8]) -> Vec<u8> {
        let mut line = bytes.to_vec();
        line.push(b'\n');
        line
    }

    /// The one plaintext member a single-member fixture tape holds.
    fn member(root: &Path) -> PathBuf {
        fs::read_dir(root)
            .expect("the tape root lists")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "plain")
            })
            .expect("a growing tape has one plaintext member")
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "spice-lane-ingest-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("scratch root is exclusively created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn remove(self) {
            fs::remove_dir_all(&self.0).expect("scratch root is explicitly removed");
            std::mem::forget(self);
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
