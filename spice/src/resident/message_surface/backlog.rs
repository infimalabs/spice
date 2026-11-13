//! Startup replay of durable lane tapes into the one app message feed.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

mod history;
mod profile_warm;
mod progress;
#[cfg(test)]
mod tests;
use history::{HistorySources, history_newest, history_tail, history_tail_error};
pub(in crate::resident) use profile_warm::advance as advance_profile_warming;
use progress::{FlightKey, HistoryProgress};

/// Fixed newest-record window for reconstructing live projection state.
///
/// Hidden JSONL activity may fill this window without producing shown cards,
/// so the initial visible page is backfilled separately through durable history.
const STARTUP_WINDOW_BYTES: u64 = 1024 * 1024;
/// Newest-record window for one historical tape read.
///
/// The predecessor read 8 MiB reverse windows and kept reading older ones
/// until the page held its visible cards; a tool-heavy lane can run for
/// megabytes without one, so a smaller window served pages of nothing.
const HISTORY_WINDOW_BYTES: u64 = 8 * 1024 * 1024;
/// Tape index bytes one scan may read; a member row is tens of bytes.
const TAIL_INDEX_BYTES: u64 = 64 * 1024;
const TAIL_RECORD_LIMIT: usize = 256 * 1024;
/// Bounded reads one scan may spend outside its boundary retreat: the tape
/// index, the extents it enumerates, and each extent the window itself reads.
const TAIL_EXTENT_PASSES: usize = 256;
const TAIL_DURATION: Duration = Duration::from_secs(5);
/// Historical windows chained toward one requested page, sized from the
/// shared [`HISTORY_REQUEST_BYTES`] span.
///
/// A page served short keeps its continuation, so the client asks again from
/// where this request stopped rather than rescanning.
const HISTORY_READ_LIMIT: usize = (HISTORY_REQUEST_BYTES / HISTORY_WINDOW_BYTES) as usize;
const STARTUP_LANE_READ_LIMIT: usize = 8;
const STARTUP_SOURCE_BYTE_LIMIT: u64 = STARTUP_SOURCE_BYTES * STARTUP_LANE_READ_LIMIT as u64;

/// Longest record a tape can hold, with the delimiter the tape carries after
/// it.
///
/// OMP capture tees a reassembled logical record: physical RPC frames stay
/// bounded at [`vendor::omp::frame::MAX_FRAME_BYTES`] while the record those
/// chunks carry reaches [`vendor::omp::frame::MAX_REASSEMBLED_BYTES`].
const RECORD_BYTES: u64 = vendor::omp::frame::MAX_REASSEMBLED_BYTES as u64 + 1;

/// Bytes one scan may spend beyond its window: the tape index it reads to
/// find the newest members, then one longest record, when the window it was
/// asked for begins inside that record.
const TAIL_BOUNDARY_BYTES: u64 = TAIL_INDEX_BYTES + RECORD_BYTES;
const STARTUP_SOURCE_BYTES: u64 = STARTUP_WINDOW_BYTES + TAIL_BOUNDARY_BYTES;
const HISTORY_SOURCE_BYTES: u64 = HISTORY_WINDOW_BYTES + TAIL_BOUNDARY_BYTES;

/// Bounded reads one scan may perform: one for every boundary chunk its
/// source allowance can pay for, since a retreat over one longest record
/// spends exactly that many, plus [`TAIL_EXTENT_PASSES`].
const fn tail_passes(source_bytes: u64) -> usize {
    (source_bytes / listen::BOUNDARY_CHUNK_BYTES + 1) as usize + TAIL_EXTENT_PASSES
}

fn startup_budget() -> listen::TailBudget {
    tail_budget(STARTUP_WINDOW_BYTES, STARTUP_SOURCE_BYTES)
}

fn history_budget() -> listen::TailBudget {
    tail_budget(HISTORY_WINDOW_BYTES, HISTORY_SOURCE_BYTES)
}

fn tail_budget(window_bytes: u64, source_bytes: u64) -> listen::TailBudget {
    listen::TailBudget::new(
        window_bytes,
        source_bytes,
        TAIL_RECORD_LIMIT,
        tail_passes(source_bytes),
        TAIL_DURATION,
    )
    .expect("app tail allowances are positive")
}

/// Source-authenticated per-lane continuation state for repeated backlog reads.
#[derive(Debug, Default)]
pub(in crate::resident) struct BacklogReader {
    cursors: BTreeMap<CanonicalUuid, listen::Checkpoint>,
    startup_work: StartupWork,
    state: MessageState,
    steering: PresentationDirectives,
    admissions: Option<bus::BusReader>,
}

impl BacklogReader {
    pub(in crate::resident) fn indexed() -> Self {
        Self::default()
    }

    pub(in crate::resident) fn into_state(self) -> MessageState {
        self.state
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct StartupWork {
    lanes: usize,
    source_bytes: u64,
    records: usize,
    passes: usize,
}

impl StartupWork {
    /// Whether one more lane's allowance fits the startup ceiling.
    fn admits_lane(self) -> bool {
        self.lanes < STARTUP_LANE_READ_LIMIT
            && self.source_bytes.saturating_add(STARTUP_SOURCE_BYTES) <= STARTUP_SOURCE_BYTE_LIMIT
    }

    fn begin_lane(&mut self) {
        self.lanes = self.lanes.saturating_add(1);
    }

    fn add(&mut self, work: listen::TailWork) {
        self.source_bytes = self.source_bytes.saturating_add(work.source_bytes);
        self.records = self.records.saturating_add(work.records);
        self.passes = self.passes.saturating_add(work.passes);
        debug_assert!(self.lanes <= STARTUP_LANE_READ_LIMIT);
        debug_assert!(self.source_bytes <= STARTUP_SOURCE_BYTE_LIMIT);
    }
}

/// Seed each current lane's bounded app backlog before discovery is published.
///
/// Live capture and restart replay both cross [`publish_agent_event`]. A record
/// captured during startup is replayed or queued live, then deduplicated.
/// Fixed-tail reconstruction and durable-page reads keep independent bounds.
#[cfg(test)]
pub(in crate::resident) fn restore_agent_messages(
    paths: &RuntimePaths,
    residency: &wire::ResidencyHead,
    publisher: &serve::stream::MessagePublisher,
    reader: &mut BacklogReader,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    restore_agent_messages_with(None, paths, residency, publisher, reader, diagnostic)
}

pub(in crate::resident) fn restore_agent_messages_indexed(
    bus: &bus::BusReader,
    paths: &RuntimePaths,
    residency: &wire::ResidencyHead,
    publisher: &serve::stream::MessagePublisher,
    reader: &mut BacklogReader,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    restore_agent_messages_with(Some(bus), paths, residency, publisher, reader, diagnostic)
}

fn restore_agent_messages_with(
    bus: Option<&bus::BusReader>,
    paths: &RuntimePaths,
    residency: &wire::ResidencyHead,
    publisher: &serve::stream::MessagePublisher,
    reader: &mut BacklogReader,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    reader.admissions = bus.cloned();
    if let Some(bus) = bus {
        reader.state.outbox_activity.bind(bus, &paths.tape);
    }
    reader.state.outbox_cursor = bus
        .map(bus::BusReader::cursor)
        .transpose()
        .map_err(|error| store_fault("outbox::projection-unreadable", &format!("{error:?}")))?
        .flatten();
    seed_session_profiles(residency, reader);
    reader.startup_work = StartupWork::default();
    let generation = publisher
        .batch()
        .map_err(|error| store_fault("serve::message-publish-failed", &error.to_string()))?;
    let mut available = Vec::new();
    for row in &residency.lanes {
        if !reader.startup_work.admits_lane() {
            break;
        }
        reader.startup_work.begin_lane();
        available.push(row.lane);
        if let Some(work) = restore_lane(bus, paths, row, &generation, reader, diagnostic)? {
            reader.startup_work.add(work);
        }
    }
    if let Some(bus) = bus {
        match profile_warm::read_task_page(bus, None) {
            Ok(page) => profile_warm::fold_task_page(&mut reader.state.profiles, page),
            Err(error) => {
                profile_warm::fail_task_history(&mut reader.state.profiles, &error, diagnostic);
            }
        }
    }
    let current = residency
        .lanes
        .iter()
        .map(|row| row.lane)
        .collect::<Vec<_>>();
    publisher
        .set_lane_availability(current, available.iter().copied())
        .map_err(|error| store_fault("serve::message-history-unavailable", &error.to_string()))?;
    let history = Arc::new(DurableHistory::new(bus, paths, residency, reader));
    let history_open = history
        .lanes
        .keys()
        .filter(|lane| available.contains(lane))
        .copied()
        .collect::<Vec<_>>();
    for (lane, page) in initial_history_pages(&history, &history_open) {
        match page {
            Ok(Some(page)) => {
                for message in page.messages {
                    // Historical projections have the same failure boundary as live replay.
                    app_diagnostic(
                        generation.publish(message).map(|_| ()).map_err(|error| {
                            store_fault("serve::message-publish-failed", &error.to_string())
                        }),
                        diagnostic,
                    )?;
                }
            }
            Ok(None) => {}
            Err(error) => {
                let _ = writeln!(diagnostic, "backlog unavailable for lane {lane}: {error}",);
            }
        }
    }
    publisher
        .set_history_provider(history)
        .map_err(|error| store_fault("serve::message-history-unavailable", &error.to_string()))?;
    publisher
        .set_history_open(history_open)
        .map_err(|error| store_fault("serve::message-history-unavailable", &error.to_string()))?;
    generation
        .commit()
        .map_err(|error| store_fault("serve::message-publish-failed", &error.to_string()))?;
    publisher
        .bound_initial_history()
        .map_err(|error| store_fault("serve::message-history-unavailable", &error.to_string()))?;
    Ok(())
}

fn seed_session_profiles(residency: &wire::ResidencyHead, reader: &mut BacklogReader) {
    for session in residency
        .lanes
        .iter()
        .filter_map(|row| row.session.as_ref())
    {
        reader.state.profiles.bind(
            session.occupant,
            spice::rooms::profile::SessionKey::new(
                session.lane,
                session.vendor.clone(),
                session.identity.clone(),
            ),
        );
        reader.state.profiles.begin_warming(session.lane);
    }
}

/// Read every admitted lane's newest durable card page before app readiness.
///
/// The fixed startup tape tail bounds reconstruction work but can contain only
/// hidden tool records. Durable history already owns the wider bounded walk
/// needed to find shown cards. Reading lanes concurrently preserves that bound
/// per lane without making a sparse sibling delay every other initial view.
fn initial_history_pages(
    history: &Arc<DurableHistory>,
    lanes: &[CanonicalUuid],
) -> Vec<(
    CanonicalUuid,
    Result<Option<serve::stream::MessageHistoryPage>, String>,
)> {
    std::thread::scope(|scope| {
        let reads = lanes
            .iter()
            .copied()
            .map(|lane| {
                let history = Arc::clone(history);
                let read = scope.spawn(move || {
                    serve::stream::MessageHistoryProvider::history(
                        history.as_ref(),
                        lane,
                        None,
                        serve::stream::MESSAGE_HISTORY_PAGE_LIMIT,
                    )
                });
                (lane, read)
            })
            .collect::<Vec<_>>();
        reads
            .into_iter()
            .map(|(lane, read)| {
                let page = read.join().unwrap_or_else(|_| {
                    Err("the durable history reader stopped before answering".to_owned())
                });
                (lane, page)
            })
            .collect()
    })
}

#[derive(Debug)]
struct DurableHistory {
    lanes: BTreeMap<CanonicalUuid, DurableLane>,
    progress: HistoryProgress,
    hydration: Mutex<Hydration>,
    bus: Option<bus::BusReader>,
    steering: PresentationDirectives,
}

/// Observation times known to this provider, and how far each lane's walk got.
///
/// Startup hydrates only the window it scanned, so an older page names loci no
/// startup fact covers. The provider therefore keeps walking the same lane's
/// ingested facts backwards across pages: each walk resumes where the last one
/// stopped and what it reads stays, so no page pays for a position an earlier
/// page already crossed.
#[derive(Debug, Default)]
struct Hydration {
    observations: MessageObservations,
    walks: HashMap<CanonicalUuid, LaneWalk>,
}

/// Where one lane's backward observation walk stands.
#[derive(Clone, Copy, Debug)]
enum LaneWalk {
    /// The walk resumes strictly before this fact.
    At(wire::Seq),
    /// The lane has no older ingested fact to read.
    Exhausted,
}

#[derive(Debug)]
struct DurableLane {
    tape: std::path::PathBuf,
    dialect: listen::Dialect,
    author: CanonicalUuid,
    source: Option<listen::Checkpoint>,
}

impl DurableHistory {
    fn new(
        bus: Option<&bus::BusReader>,
        paths: &RuntimePaths,
        residency: &wire::ResidencyHead,
        reader: &BacklogReader,
    ) -> Self {
        let lanes = residency
            .lanes
            .iter()
            .filter_map(|row| {
                let author = row
                    .occupant
                    .or_else(|| row.session.as_ref().map(|session| session.occupant))
                    .or_else(|| row.lineage.last().copied())?;
                let dialect = super::super::ingest_vendor(&row.vendor)?.dialect();
                Some((
                    row.lane,
                    DurableLane {
                        tape: paths.lane_tape(row.lane).join(row.vendor.as_str()),
                        dialect,
                        author,
                        source: reader.cursors.get(&row.lane).cloned(),
                    },
                ))
            })
            .collect();
        Self {
            lanes,
            progress: HistoryProgress::default(),
            hydration: Mutex::new(Hydration {
                observations: reader.state.observations.clone(),
                walks: HashMap::new(),
            }),
            bus: bus.cloned(),
            steering: reader.steering.clone(),
        }
    }
}

impl serve::stream::MessageHistoryProvider for DurableHistory {
    fn history(
        &self,
        lane: CanonicalUuid,
        before: Option<&wire::AgentMessage>,
        limit: usize,
    ) -> Result<Option<serve::stream::MessageHistoryPage>, String> {
        let Some(source) = self.lanes.get(&lane) else {
            return Ok(None);
        };
        let tape = self.progress.tape_start(lane, source, before)?;
        let key = FlightKey {
            lane,
            anchor: before.map(|message| message.locus),
            limit,
            tape: tape.clone(),
        };
        self.progress
            .single_flight(key, || {
                self.read_page(lane, source, before, limit, tape.as_ref())
                    .inspect_err(|_| self.progress.forget_tape(lane))
            })
            .map(Some)
    }
}

impl DurableHistory {
    fn read_page(
        &self,
        lane: CanonicalUuid,
        source: &DurableLane,
        before: Option<&wire::AgentMessage>,
        limit: usize,
        tape: Option<&listen::Checkpoint>,
    ) -> Result<serve::stream::MessageHistoryPage, String> {
        let sources = self.history_sources(lane, source, before, limit, tape)?;
        let (messages, more) = sources.page(before, limit);
        if more && let (Some(scan), Some(anchor)) = (&sources.tape, messages.first().or(before)) {
            self.progress
                .remember_tape(lane, anchor, tape_continuation(scan, anchor))?;
        }
        Ok(serve::stream::MessageHistoryPage { messages, more })
    }

    fn history_sources(
        &self,
        lane: CanonicalUuid,
        source: &DurableLane,
        before: Option<&wire::AgentMessage>,
        limit: usize,
        tape: Option<&listen::Checkpoint>,
    ) -> Result<HistorySources, String> {
        let tape = if source.tape.is_dir() {
            let newest = match tape {
                Some(end) => self.tape_messages(lane, source, before, end, limit)?,
                None => self.initial_tape_messages(lane, source, before, limit)?,
            };
            Some(self.fill_tape_messages(lane, source, before, limit, newest)?)
        } else if source.source.is_some() {
            return Err("the transcript source disappeared after app bootstrap".to_owned());
        } else {
            None
        };
        Ok(HistorySources { tape })
    }

    fn tape_messages(
        &self,
        lane: CanonicalUuid,
        source: &DurableLane,
        before: Option<&wire::AgentMessage>,
        end: &listen::Checkpoint,
        limit: usize,
    ) -> Result<TapeHistoryScan, String> {
        let tail = history_tail(&source.tape, source.dialect, end).map_err(history_tail_error)?;
        self.collect_tape_messages(lane, source, before, limit, tail)
    }

    fn initial_tape_messages(
        &self,
        lane: CanonicalUuid,
        source: &DurableLane,
        before: Option<&wire::AgentMessage>,
        limit: usize,
    ) -> Result<TapeHistoryScan, String> {
        let tail =
            history_newest(&source.tape, source.dialect).map_err(|error| error.to_string())?;
        self.collect_tape_messages(lane, source, before, limit, tail)
    }

    fn collect_tape_messages(
        &self,
        lane: CanonicalUuid,
        source: &DurableLane,
        before: Option<&wire::AgentMessage>,
        limit: usize,
        tail: listen::TailScan,
    ) -> Result<TapeHistoryScan, String> {
        let source_start = tail.start;
        let end = tail.scan.checkpoint.clone();
        let work = tail.work;
        let messages = CollectedMessages::new(limit, before);
        let scan = tail.scan;
        let mut steering = self.steering.clone();
        if let Some(bus) = &self.bus {
            hydrate_steering_presentations(bus, &scan, &mut steering)
                .map_err(|error| format!("{error:?}"))?;
        }
        let mut reader = BacklogReader {
            cursors: BTreeMap::new(),
            startup_work: StartupWork::default(),
            state: MessageState {
                observations: self.window_observations(lane, &scan)?,
                ..MessageState::default()
            },
            steering,
            admissions: self.bus.clone(),
        };
        let mut diagnostic = Vec::new();
        publish_scan(
            scan,
            lane,
            source.author,
            Some(&source.tape),
            &messages,
            &mut reader,
            &mut diagnostic,
        )
        .map_err(|error| format!("{error:?}"))?;
        if !diagnostic.is_empty() {
            return Err(String::from_utf8_lossy(&diagnostic).trim().to_owned());
        }
        let collected = messages
            .inner
            .into_inner()
            .map_err(|_| "durable history collection is unavailable".to_owned())?;
        let mut messages = collected.messages;
        if let Some(before) = before {
            messages.retain(|message| message.chronology_key() < before.chronology_key());
        }
        let selection_start = messages.len().saturating_sub(limit);
        let selected = messages.split_off(selection_start);
        let complete = !collected.dropped && selection_start == 0;
        Ok(TapeHistoryScan {
            messages: selected,
            more: !complete || source_start.next != listen::Position::start(),
            complete,
            start: source_start,
            end,
            work,
        })
    }

    /// Observation times covering one window's records, reading only what the
    /// already-walked facts do not answer.
    ///
    /// Batches tile a source generation without gaps, so a fact covering the
    /// oldest locus of this window means every newer locus in it is covered
    /// too. That makes one containment test the whole decision, and leaves the
    /// walk to run exactly when a page reaches past what earlier pages read.
    ///
    /// The walk runs while the lock is held so two concurrent pages cannot
    /// read the same facts twice and retain both copies. That costs no real
    /// concurrency: every fact read crosses the one writer queue anyway.
    fn window_observations(
        &self,
        lane: CanonicalUuid,
        scan: &listen::Scan,
    ) -> Result<MessageObservations, String> {
        let mut hydration = self
            .hydration
            .lock()
            .map_err(|_| "durable history observations are unavailable".to_owned())?;
        let (Some(bus), Some(oldest)) = (self.bus.as_ref(), oldest_scan_locus(scan)) else {
            return Ok(hydration.observations.for_lane(lane));
        };
        if hydration.observations.covers(lane, oldest)
            || matches!(hydration.walks.get(&lane), Some(LaneWalk::Exhausted))
        {
            return Ok(hydration.observations.for_lane(lane));
        }
        let source = scan_source(scan).map_err(|error| format!("{error:?}"))?;
        let resume = match hydration.walks.get(&lane) {
            Some(LaneWalk::At(seq)) => Some(*seq),
            Some(LaneWalk::Exhausted) | None => None,
        };
        let walk = walk_observations(bus, lane, source, oldest, scan.records.len(), resume)
            .map_err(|error| format!("{error:?}"))?;
        hydration
            .walks
            .insert(lane, walk.resume.map_or(LaneWalk::Exhausted, LaneWalk::At));
        hydration.observations.append(walk.observations);
        Ok(hydration.observations.for_lane(lane))
    }
}

/// Visible cards one history request collected from the tape.
///
/// `start` is the oldest boundary any chained window reached and `end` the
/// newest, while `work` sums every window read for the request. `complete`
/// says `messages` holds every visible card older than the anchor between
/// them, so nothing the page left behind lies physically after `start`.
struct TapeHistoryScan {
    messages: Vec<wire::AgentMessage>,
    more: bool,
    complete: bool,
    start: listen::Checkpoint,
    end: listen::Checkpoint,
    work: listen::TailWork,
}

fn tape_continuation(scan: &TapeHistoryScan, before: &wire::AgentMessage) -> listen::Checkpoint {
    debug_assert!(scan.work.source_bytes <= HISTORY_SOURCE_BYTES * HISTORY_READ_LIMIT as u64);
    debug_assert!(scan.work.records <= TAIL_RECORD_LIMIT * HISTORY_READ_LIMIT);
    debug_assert!(scan.work.passes <= tail_passes(HISTORY_SOURCE_BYTES) * HISTORY_READ_LIMIT);
    let end = scan.end.clone();
    let anchor = before.chronology_key();
    // Physical progress, not the anchor, is what the next page resumes from:
    // a complete scan whose every card the page already took proved the whole
    // range behind the anchor empty down to `start`.
    if scan.complete
        && scan
            .messages
            .iter()
            .all(|message| message.chronology_key() >= anchor)
    {
        return scan.start.clone();
    }
    let boundary = scan
        .messages
        .iter()
        .filter(|message| message.chronology_key() >= anchor)
        .min_by_key(|message| message.chronology_key());
    let Some(message) = boundary else {
        return end;
    };
    listen::Checkpoint {
        source: end.source,
        next: listen::Position {
            member: message.locus.member,
            offset: message.locus.offset,
        },
    }
}

/// Visible cards older than the page anchor, newest `limit` kept.
///
/// The anchor filters before the limit does: a window whose newest cards
/// all stand at or after the anchor must not crowd out the older ones this
/// page exists to serve.
#[derive(Debug)]
struct CollectedMessages {
    limit: usize,
    before: Option<(i64, CanonicalUuid, u64, u64, u64, u32)>,
    inner: Mutex<CollectedHistory>,
}

#[derive(Debug, Default)]
struct CollectedHistory {
    messages: Vec<wire::AgentMessage>,
    dropped: bool,
}

impl CollectedMessages {
    fn new(limit: usize, before: Option<&wire::AgentMessage>) -> Self {
        Self {
            limit,
            before: before.map(wire::AgentMessage::chronology_key),
            inner: Mutex::new(CollectedHistory::default()),
        }
    }

    fn precedes_anchor(&self, message: &wire::AgentMessage) -> bool {
        self.before
            .is_none_or(|before| message.chronology_key() < before)
    }
}

impl AgentMessagePublisher for CollectedMessages {
    fn publish(
        &self,
        message: wire::AgentMessage,
    ) -> Result<bool, serve::stream::MessagePublishError> {
        if !visible_message(&message) || !self.precedes_anchor(&message) {
            return Ok(true);
        }
        let mut collected = self
            .inner
            .lock()
            .map_err(|_| serve::stream::MessagePublishError::Unavailable)?;
        collected.messages.push(message);
        if collected.messages.len() > self.limit {
            collected.messages.remove(0);
            collected.dropped = true;
        }
        Ok(true)
    }

    fn revise_final(
        &self,
        message: wire::AgentMessage,
    ) -> Result<bool, serve::stream::MessagePublishError> {
        let mut collected = self
            .inner
            .lock()
            .map_err(|_| serve::stream::MessagePublishError::Unavailable)?;
        let Some(existing) = collected
            .messages
            .iter_mut()
            .find(|existing| existing.lane == message.lane && existing.locus == message.locus)
        else {
            // A card at or after the anchor was never collected.
            if !self.precedes_anchor(&message) {
                return Ok(true);
            }
            return Err(serve::stream::MessagePublishError::IdentityConflict);
        };
        *existing = message;
        Ok(true)
    }
}

fn restore_lane(
    bus: Option<&bus::BusReader>,
    paths: &RuntimePaths,
    row: &wire::LaneResidency,
    publisher: &impl AgentMessagePublisher,
    reader: &mut BacklogReader,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<Option<listen::TailWork>> {
    let Some(author) = row
        .occupant
        .or_else(|| row.session.as_ref().map(|session| session.occupant))
        .or_else(|| row.lineage.last().copied())
    else {
        return Ok(None);
    };
    let Some(vendor) = super::super::ingest_vendor(&row.vendor) else {
        return Ok(None);
    };
    let tape = paths.lane_tape(row.lane).join(row.vendor.as_str());
    if !tape_available(&tape, row.lane, diagnostic) {
        reader
            .state
            .profiles
            .warm_failed(row.lane, "lane transcript is unavailable");
        return Ok(None);
    }
    let tail = match read_backlog(&tape, row.lane, vendor.dialect(), reader) {
        Ok(tail) => tail,
        Err(error) => {
            let work = error.tail_work().copied();
            reader
                .state
                .profiles
                .warm_failed(row.lane, error.to_string());
            forget_changed_source(error, row.lane, reader, diagnostic);
            return Ok(work);
        }
    };
    let work = tail.work;
    let scan = tail.scan;
    let source_start = tail.start.clone();
    if let Some(bus) = bus {
        hydrate_observations(bus, row.lane, &scan, reader)?;
        hydrate_steering_presentations(bus, &scan, &mut reader.steering)?;
        if row.session.is_some() {
            match profile_warm::read_transcript_window(row.lane, &scan, &source_start) {
                Ok(window) => {
                    profile_warm::fold_transcript_window(&mut reader.state.profiles, window);
                }
                Err(error) => {
                    reader
                        .state
                        .profiles
                        .warm_failed(row.lane, format!("{error:?}"));
                    let _ = writeln!(
                        diagnostic,
                        "lane profile transcript unavailable for lane {}: {error:?}",
                        row.lane
                    );
                }
            }
        }
    }
    publish_scan(
        scan,
        row.lane,
        author,
        Some(&tape),
        publisher,
        reader,
        diagnostic,
    )?;
    Ok(Some(work))
}

fn hydrate_observations(
    bus: &bus::BusReader,
    lane: CanonicalUuid,
    scan: &listen::Scan,
    reader: &mut BacklogReader,
) -> CommandResult<()> {
    let Some(oldest) = oldest_scan_locus(scan) else {
        return Ok(());
    };
    let source = scan_source(scan)?;
    let walk = walk_observations(bus, lane, source, oldest, scan.records.len(), None)?;
    reader.state.observations.append(walk.observations);
    Ok(())
}

/// Oldest tape position any decoded event in one scan names.
fn oldest_scan_locus(scan: &listen::Scan) -> Option<(u64, u64)> {
    scan.records
        .iter()
        .flat_map(|record| match &record.payload {
            listen::RecordPayload::Events(events) => events
                .iter()
                .map(|event| (event.event.locus().member, event.event.locus().offset))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .min()
}

/// Source generation the scan read, as the accounting facts name it.
fn scan_source(scan: &listen::Scan) -> CommandResult<CanonicalUuid> {
    scan.checkpoint
        .source
        .as_str()
        .parse::<CanonicalUuid>()
        .map_err(|error| store_fault("serve::message-source-invalid", &error.to_string()))
}

/// One bounded backward walk of a lane's accounting facts.
struct ObservationWalk {
    observations: MessageObservations,
    /// Fact the next walk resumes strictly before, or `None` once the lane's
    /// oldest accounting fact has been read.
    resume: Option<wire::Seq>,
}

/// Read this lane's accounting facts backwards until one covers `oldest`.
///
/// The walk is capped rather than exhaustive: a window deep in a long tape may
/// stop before it reaches its own batch, and then reports where it stopped so
/// the next request continues from there instead of paying for the same facts
/// again. Only batches naming this lane and this source generation can end the
/// walk, so a replaced generation never lends its times to the current one.
fn walk_observations(
    bus: &bus::BusReader,
    lane: CanonicalUuid,
    source: CanonicalUuid,
    oldest: (u64, u64),
    records: usize,
    resume: Option<wire::Seq>,
) -> CommandResult<ObservationWalk> {
    let origin = lane_origin(lane)?;
    let mut remaining = records.saturating_mul(4).clamp(32, 4_096);
    let mut before = resume;
    let mut facts = Vec::new();
    while remaining > 0 {
        let limit = remaining.min(128);
        let budget = journal::FactReadBudget::new(limit, journal::MAX_RETAINED_FACT_PAGE_BYTES)
            .expect("positive capped backlog work is a repository budget");
        let page = bus
            .recent_origin_facts(
                origin.clone(),
                wire::FactKind::JournalIngested,
                before,
                budget,
            )
            .map_err(|error| store_fault("serve::message-time-unavailable", &error.to_string()))?;
        if page.rows.is_empty() {
            before = None;
            break;
        }
        before = page.rows.last().map(|fact| fact.seq);
        let reached = page.rows.iter().any(|envelope| {
            envelope
                .fact
                .read::<wire::body::JournalIngested>()
                .is_some_and(|batch| {
                    batch.lane == lane
                        && batch.source == source
                        && (batch.from.member, batch.from.offset) <= oldest
                })
        });
        remaining = remaining.saturating_sub(page.rows.len());
        facts.extend(page.rows);
        if reached {
            break;
        }
    }
    facts.sort_by_key(|fact| fact.seq);
    Ok(ObservationWalk {
        observations: MessageObservations::from_facts(&facts),
        resume: before,
    })
}

fn scan_events(scan: &listen::Scan) -> impl Iterator<Item = &listen::Event> {
    scan.records
        .iter()
        .flat_map(|record| match &record.payload {
            listen::RecordPayload::Events(events) => events.as_slice(),
            _ => &[],
        })
        .map(|sourced| &sourced.event)
}

fn scan_assistant_texts(scan: &listen::Scan) -> impl Iterator<Item = &str> {
    scan.records.iter().flat_map(|record| {
        let events = match &record.payload {
            listen::RecordPayload::Events(events) => events.as_slice(),
            _ => &[],
        };
        let outbox = record
            .context
            .as_ref()
            .and_then(|context| match context.dialog.as_ref()? {
                listen::Dialog::HostUri {
                    operation,
                    url,
                    content,
                    ..
                } if spice::outbox_source::authored_write(operation, url) => content.as_deref(),
                _ => None,
            });
        events
            .iter()
            .filter_map(|event| event.event.assistant_text())
            .chain(outbox)
    })
}

fn hydrate_steering_presentations(
    bus: &bus::BusReader,
    scan: &listen::Scan,
    steering: &mut PresentationDirectives,
) -> CommandResult<()> {
    let mut keys = steering_keys(scan_assistant_texts(scan));
    keys.extend(direction::delivered(scan_events(scan)));
    keys.retain(|key| !steering.directives.contains_key(key));
    let rows = load_steering_presentations(bus, &keys)?;
    steering
        .extend_presentations(bus, &rows, super::load_principal_roles(bus)?)
        .map_err(|error| store_fault("serve::message-steering-invalid", &error))?;
    steering.prove_recipients(bus, message_addresses(scan_assistant_texts(scan)))
}

fn tape_available(tape: &Path, lane: CanonicalUuid, diagnostic: &mut dyn io::Write) -> bool {
    match fs::metadata(tape) {
        Ok(metadata) if metadata.is_dir() => true,
        Ok(_) => {
            let _ = writeln!(
                diagnostic,
                "backlog unavailable for lane {lane}: {} is not a tape directory",
                tape.display(),
            );
            false
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            let _ = writeln!(diagnostic, "backlog unavailable for lane {lane}: {error}",);
            false
        }
    }
}

fn forget_changed_source(
    error: listen::ScanError,
    lane: CanonicalUuid,
    reader: &mut BacklogReader,
    diagnostic: &mut dyn io::Write,
) {
    if matches!(&error, listen::ScanError::SourceChanged { .. }) {
        reader.cursors.remove(&lane);
        reader.state.previews.clear_lane(lane);
        reader.state.outbox_activity.clear_lane(lane);
        reader.state.media_hydration.clear_lane(lane);
        reader.state.prose.remove(&lane);
    }
    let _ = writeln!(diagnostic, "backlog unavailable for lane {lane}: {error}",);
}

fn publish_scan(
    scan: listen::Scan,
    lane: CanonicalUuid,
    author: CanonicalUuid,
    media_source: Option<&Path>,
    publisher: &impl AgentMessagePublisher,
    reader: &mut BacklogReader,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let checkpoint = scan.checkpoint.clone();
    let source = scan
        .checkpoint
        .source
        .as_str()
        .parse::<CanonicalUuid>()
        .map_err(|error| store_fault("serve::message-source-invalid", &error.to_string()))?;
    let mut published = true;
    for mut record in scan.records {
        let mut record_author = author;
        let mut authored = None;
        let mut observed = None;
        let mut invitation = None;
        if let Some(bus) = &reader.admissions
            && record.context.as_ref().is_some_and(|context| {
                matches!(
                    context.dialog.as_ref(),
                    Some(listen::Dialog::HostUri { operation, url, .. })
                        if spice::outbox_source::authored_write(operation, url)
                )
            })
        {
            let accepted = bus
                .outbox(bus::OutboxQuery::Source {
                    lane,
                    source,
                    member: record.locus.member,
                    offset: record.locus.offset,
                })
                .map_err(|error| store_fault("outbox::index-unreadable", &format!("{error:?}")))?
                .pop();
            if let Some(accepted) = accepted {
                let decoded = listen::Decoded::Events {
                    context: Box::new(record.context.take().expect("host context was checked")),
                    events: Vec::new(),
                };
                let decoded = spice::outbox_source::materialize(decoded, &accepted.body)
                    .map_err(|error| store_fault("outbox::source-invalid", error))?;
                let listen::Decoded::Events { context, events } = decoded else {
                    unreachable!("outbox materialization returns authored events");
                };
                record.context = Some(*context);
                record_author = accepted.body.occupant;
                observed = Some(accepted.at);
                invitation.clone_from(&accepted.body.invitation);
                authored = Some(events);
            }
        }
        let Some(context) = record.context.as_ref() else {
            continue;
        };
        let listen::RecordPayload::Events(events) = &record.payload else {
            continue;
        };
        let decoded = match &authored {
            Some(events) => events
                .iter()
                .enumerate()
                .map(|(ordinal, event)| (ordinal as u32, event))
                .collect(),
            None => events
                .iter()
                .map(|sourced| {
                    let event = u32::try_from(sourced.ordinal).map_err(|error| {
                        store_fault("serve::message-locus-invalid", &error.to_string())
                    })?;
                    Ok((event, &sourced.event))
                })
                .collect::<CommandResult<Vec<_>>>()?,
        };
        if let Err(outcome) = publish_agent_record(
            publisher,
            AgentRecord {
                lane,
                author: record_author,
                source: Some(source),
                locus: record.locus,
                context,
                events: &decoded,
                observed,
                media_source,
                invitation: invitation.as_ref(),
            },
            &mut reader.state,
            Some(&reader.steering),
        ) {
            match outcome {
                Outcome::Fault(fault) => {
                    published = false;
                    let _ = writeln!(
                        diagnostic,
                        "backlog unavailable for lane {lane}: {}: {}",
                        fault.reason, fault.detail,
                    );
                }
                other => return Err(other),
            }
        }
    }
    if published {
        reader.cursors.insert(lane, checkpoint);
    }
    Ok(())
}

fn read_backlog(
    tape: &Path,
    lane: CanonicalUuid,
    dialect: listen::Dialect,
    reader: &BacklogReader,
) -> Result<listen::TailScan, listen::ScanError> {
    let tail = tail_backlog(tape, dialect)?;
    if let Some(cursor) = reader.cursors.get(&lane)
        && cursor.source != tail.scan.checkpoint.source
    {
        return Err(listen::ScanError::SourceChanged {
            expected: cursor.source.clone(),
            observed: tail.scan.checkpoint.source.clone(),
            next: cursor.next,
        });
    }
    Ok(tail)
}

fn tail_backlog(
    tape: &Path,
    dialect: listen::Dialect,
) -> Result<listen::TailScan, listen::ScanError> {
    listen::scan_tail(tape, dialect, startup_budget())
}

fn visible_message(message: &wire::AgentMessage) -> bool {
    message.activity.is_visible() || !message.media.is_empty()
}
