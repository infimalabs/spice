//! Durable taste recognition and fallible app views over admitted lane capture.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use bus::Bus;
use lane::occupant::Seats;
use wire::{AgentActivityKind, CanonicalUuid, Outcome};

use super::{
    CommandResult, RuntimePaths, current_residency, ingest_vendor, lane_origin, lane_surface,
    narration, steering, store_fault, taste_surface,
};

mod acknowledgement;
mod automatic_speech;
mod backlog;
mod direction;
pub(super) mod directive_context;
mod landing;
mod media_hydration;
pub(in crate::resident) mod outbox;
mod pending;
mod projection;
mod prose_stream;
mod sweep;
pub(crate) mod tape_admission;
mod tool_preview;
mod turn_cause;
use acknowledgement::{
    ContextState, Presentation, present_invitation,
    present_steering_acknowledgements_with_directives, scan_presentations,
};
use directive_context::{
    present_invitation_message, present_steering_acknowledgements, steering_keys,
};
pub(in crate::resident) use pending::settle_pending_presentations;
#[cfg(test)]
use projection::publish_agent_event;
#[cfg(test)]
pub(super) use projection::publish_agent_messages;
use projection::{publish_agent_messages_in, publish_agent_record};

/// Reserved wire identity for record context that is not a decoded event.
const TURN_BOUNDARY_EVENT: u32 = u32::MAX;
/// Historical tape bytes one app history request may chain through per
/// source, and the scan time it may spend doing so.
const HISTORY_REQUEST_BYTES: u64 = 32 * 1024 * 1024;
const HISTORY_DURATION: Duration = Duration::from_secs(5);

#[cfg(test)]
pub(super) use backlog::restore_agent_messages;
pub(super) use backlog::{BacklogReader, advance_profile_warming, restore_agent_messages_indexed};
pub(super) use tool_preview::CallPreviews;

/// Captured-generation policy and disposable publishers used after one record
/// passes occupant admission.
pub(super) struct CaptureSurfaces<'a, P> {
    paths: &'a RuntimePaths,
    tastes: &'a taste::Bag,
    steering: &'a steer::Config,
    publisher: &'a P,
    /// What a heard invitation ACK joins against.
    joining: crate::board_command::Joining<'a>,
    /// Tool calls an occupant may run without prose before each prod.
    narration: u32,
}

impl<'a, P> CaptureSurfaces<'a, P> {
    pub(super) fn new(
        paths: &'a RuntimePaths,
        tastes: &'a taste::Bag,
        steering: &'a steer::Config,
        publisher: &'a P,
        joining: crate::board_command::Joining<'a>,
    ) -> Self {
        Self {
            paths,
            tastes,
            steering,
            publisher,
            joining,
            narration: lane::Config::default().narration(),
        }
    }

    /// Replace the compiled narration bound with the configured one.
    pub(super) const fn narration(mut self, bound: u32) -> Self {
        self.narration = bound;
        self
    }

    /// Fold no narration streak: the seat these records belong to has closed,
    /// so a prod could neither be recorded under it nor read back by it.
    pub(super) const fn without_narration(mut self) -> Self {
        self.narration = 0;
        self
    }
}

/// Mutable per-record fold state at the admitted capture boundary.
pub(super) struct CaptureViews<'a> {
    state: &'a Mutex<MessageState>,
    diagnostic: &'a mut dyn io::Write,
}

impl<'a> CaptureViews<'a> {
    pub(super) fn new(state: &'a Mutex<MessageState>, diagnostic: &'a mut dyn io::Write) -> Self {
        Self { state, diagnostic }
    }
}

#[derive(Debug, Default)]
pub(crate) struct MessageState {
    previews: CallPreviews,
    outbox_activity: outbox::Activity,
    media_hydration: media_hydration::MediaHydration,
    observations: MessageObservations,
    prose: BTreeMap<CanonicalUuid, ProseState>,
    pending: pending::PendingPresentations,
    pub(crate) profiles: spice::rooms::profile::LaneProfiles,
    pub(in crate::resident) narration: super::narration::Streaks,
    presented: BTreeMap<(CanonicalUuid, CanonicalUuid, Option<CanonicalUuid>), listen::Position>,
    /// Last admission horizon already projected; startup tape replay owns older cards.
    outbox_cursor: Option<wire::Seq>,
    /// Current OMP message creation time, including silent message-start records.
    messages: BTreeMap<CanonicalUuid, OpenMessage>,
    /// Designs whose request reached the current turn of one seated occupant:
    /// the union over every record of the turn whose operator prose carried a
    /// room catch-up, whether the prompt that opened the turn or an aside that
    /// landed inside it, kept until the record that completes the turn.
    profile_turn_causes: BTreeMap<(CanonicalUuid, CanonicalUuid), Vec<wire::TaskStamp>>,
    /// Transcript spans read and presented but not yet accounted.
    ///
    /// Held here because the accounting bound spans sweeps: the state that
    /// outlives one record is the only place a later sweep can find what an
    /// earlier one held.
    accounting: lane::ingest::Coalescing,
    /// Operator directions whose first landing this resident already measured.
    landings: landing::Measured,
}

#[cfg(test)]
impl MessageState {
    /// State whose live accounting already holds spans an earlier sweep read.
    pub(super) fn holding(accounting: lane::ingest::Coalescing) -> Self {
        Self {
            accounting,
            ..Self::default()
        }
    }
}

pub(crate) fn lock_state(
    state: &Mutex<MessageState>,
) -> CommandResult<MutexGuard<'_, MessageState>> {
    state
        .lock()
        .map_err(|error| store_fault("serve::message-state-unavailable", &error.to_string()))
}

/// Name a card the app server refused instead of raising it: the app is a projection
/// of durable tapes, so its refusal is a diagnostic, never a resident fault.
pub(in crate::resident) fn app_diagnostic(
    presented: CommandResult<()>,
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    match presented {
        Err(Outcome::Fault(fault)) => {
            let _ = writeln!(
                diagnostic,
                "message unavailable: {}: {}",
                fault.reason, fault.detail
            );
            Ok(())
        }
        outcome => outcome,
    }
}

#[derive(Debug)]
struct ProseState {
    published: wire::AgentMessage,
    terminal: wire::AgentMessage,
}

#[derive(Debug)]
struct OpenMessage {
    author: CanonicalUuid,
    source: Option<CanonicalUuid>,
    id: String,
    timestamp: u64,
    /// Content blocks whose completed streamed revision was presented.
    streamed: BTreeSet<u32>,
}

#[derive(Clone, Debug)]
struct MessageObservation {
    lane: CanonicalUuid,
    source: CanonicalUuid,
    from: wire::body::TranscriptPosition,
    to: wire::body::TranscriptPosition,
    at: wire::Instant,
}

#[derive(Clone, Debug, Default)]
struct MessageObservations(Vec<MessageObservation>);

impl MessageObservations {
    fn from_facts<'a>(facts: impl IntoIterator<Item = &'a wire::Envelope<wire::AnyFact>>) -> Self {
        let mut observations = Self::default();
        for envelope in facts {
            if let Some(batch) = envelope.fact.read::<wire::body::JournalIngested>() {
                observations.push(&batch, batch.from, envelope.at);
            }
        }
        observations
    }

    fn append(&mut self, mut observations: Self) {
        self.0.append(&mut observations.0);
    }

    /// The batches one lane's reads can match, without every other lane's.
    fn for_lane(&self, lane: CanonicalUuid) -> Self {
        Self(
            self.0
                .iter()
                .filter(|observation| observation.lane == lane)
                .cloned()
                .collect(),
        )
    }

    fn extend(&mut self, observations: Vec<super::TranscriptObservation>) {
        for observation in observations {
            self.push(&observation.batch, observation.from, observation.at);
        }
    }

    fn push(
        &mut self,
        batch: &wire::body::JournalIngested,
        from: wire::body::TranscriptPosition,
        at: wire::Instant,
    ) {
        self.0.push(MessageObservation {
            lane: batch.lane,
            source: batch.source,
            from,
            to: batch.to,
            at,
        });
    }

    fn at(&self, lane: CanonicalUuid, locus: listen::Locus) -> Option<wire::Instant> {
        self.find(lane, locus).map(|observation| observation.at)
    }

    fn source(&self, lane: CanonicalUuid, locus: listen::Locus) -> Option<CanonicalUuid> {
        self.find(lane, locus).map(|observation| observation.source)
    }

    /// Whether a known batch already spans one tape position on this lane.
    fn covers(&self, lane: CanonicalUuid, position: (u64, u64)) -> bool {
        self.0
            .iter()
            .any(|observation| observation.spans(lane, position))
    }

    fn find(&self, lane: CanonicalUuid, locus: listen::Locus) -> Option<&MessageObservation> {
        let position = (locus.member, locus.offset);
        self.0
            .iter()
            .rev()
            .find(|observation| observation.spans(lane, position))
    }
}

impl MessageObservation {
    /// Whether this batch read that tape position on that lane.
    fn spans(&self, lane: CanonicalUuid, position: (u64, u64)) -> bool {
        self.lane == lane
            && (self.from.member, self.from.offset) <= position
            && position < (self.to.member, self.to.offset)
    }
}

pub(super) trait AgentMessagePublisher {
    fn publish(
        &self,
        message: wire::AgentMessage,
    ) -> Result<bool, serve::stream::MessagePublishError>;

    fn revise_final(
        &self,
        message: wire::AgentMessage,
    ) -> Result<bool, serve::stream::MessagePublishError>;
}

pub(super) enum TasteRecognition<'a> {
    Direct,
    Deferred(&'a mut taste_surface::AdjudicationSubmitter),
}

impl AgentMessagePublisher for serve::stream::MessagePublisher {
    fn publish(
        &self,
        message: wire::AgentMessage,
    ) -> Result<bool, serve::stream::MessagePublishError> {
        Self::publish(self, message)
    }

    fn revise_final(
        &self,
        message: wire::AgentMessage,
    ) -> Result<bool, serve::stream::MessagePublishError> {
        Self::revise_final(self, message)
    }
}

impl AgentMessagePublisher for serve::stream::MessageBatch {
    fn publish(
        &self,
        message: wire::AgentMessage,
    ) -> Result<bool, serve::stream::MessagePublishError> {
        Self::publish(self, message)
    }

    fn revise_final(
        &self,
        message: wire::AgentMessage,
    ) -> Result<bool, serve::stream::MessagePublishError> {
        Self::revise_final(self, message)
    }
}

pub(super) fn accept_capture_record<P: AgentMessagePublisher>(
    bus: &Bus,
    epoch: CanonicalUuid,
    seats: &mut Seats,
    surfaces: &CaptureSurfaces<'_, P>,
    mut recognition: TasteRecognition<'_>,
    record: &lane::capture::CaptureRecord,
    mut views: CaptureViews<'_>,
) -> CommandResult<()> {
    if !lane_surface::capture_record(bus, epoch, seats, record)? {
        return Ok(());
    }
    if record.declaration.is_some() {
        let residency = current_residency(bus, epoch)?;
        if let Some(session) = residency
            .body()
            .lanes
            .iter()
            .find(|row| row.lane == record.lane)
            .and_then(|row| row.session.as_ref())
            .filter(|session| session.occupant == record.occupant)
        {
            lock_state(views.state)?.profiles.bind(
                record.occupant,
                spice::rooms::profile::SessionKey::new(
                    session.lane,
                    session.vendor.clone(),
                    session.identity.clone(),
                ),
            );
        }
    }
    // The bytes behind this record are already durable, so accounting them here
    // keeps the transcript head level with the records the resident has taken
    // responsibility for, rather than leaving the gap for the next start to
    // find. A record the resident did not accept above does not account either.
    //
    // Accounting and live presentation share this sweep, so nothing publishes
    // before capture admission. Scanning through the live head also catches
    // records whose finite-queue notice never arrived.
    let held = std::mem::take(&mut lock_state(views.state)?.accounting);
    let (mut observations, held) = super::ingest_transcripts(
        bus,
        surfaces.paths,
        Some(record.lane),
        lane::ingest::Bound::LIVE,
        held,
        views.diagnostic,
    )?;
    // A noticed record the sweep did not cover was accounted, and its packets
    // read, by an earlier pass; only the sweep's own records are new to SYN.
    let noticed_new = observations.iter().any(|observation| {
        observation.batch.lane == record.lane
            && observation
                .records
                .iter()
                .any(|swept| swept.locus == record.locus)
    });
    let swept = sweep::swept_records(record, &mut observations);
    {
        let mut state = lock_state(views.state)?;
        state.accounting = held;
        state.observations.extend(observations);
    }
    sweep::present_swept(bus, epoch, surfaces, &mut recognition, &swept, &mut views)?;
    let authored = if noticed_new { &swept[..] } else { &swept[1..] };
    publish_swept(
        bus,
        epoch,
        surfaces,
        authored,
        steering::Refusals::Notify,
        &mut views,
    )?;
    measure_landings(bus, record.lane, &swept, &mut views)?;
    hear_swept(
        bus,
        surfaces.paths,
        surfaces.steering,
        (epoch, surfaces.joining),
        record.lane,
        &swept,
        views.diagnostic,
    )
}

/// Write down how long each operator direction a live sweep first lands on
/// `lane` waited: in spice before omp received it, then in omp's steering
/// queue before the transcript carried it. A closing sweep measures nothing,
/// since its records may have landed long before it read them.
fn measure_landings(
    bus: &Bus,
    lane: CanonicalUuid,
    swept: &[std::borrow::Cow<'_, lane::capture::CaptureRecord>],
    views: &mut CaptureViews<'_>,
) -> CommandResult<()> {
    let mut state = lock_state(views.state)?;
    let mut landed = Vec::new();
    for record in swept {
        let received_millis = record
            .decoded
            .context()
            .and_then(|context| context.message.as_ref())
            .and_then(|message| message.timestamp);
        for prose in record
            .decoded
            .events()
            .iter()
            .filter_map(|event| match event {
                listen::Event::Prose(prose) if prose.role == listen::ProseRole::Operator => {
                    Some(prose)
                }
                _ => None,
            })
        {
            for key in state.landings.first_landings(lane, &prose.text) {
                let observed = state.observations.at(lane, prose.locus);
                landed.push((key, received_millis, observed));
            }
        }
    }
    drop(state);
    if landed.is_empty() {
        return Ok(());
    }
    let pending = match bus.read(move |read| crate::steering_read::pending(read, Some(lane))) {
        Ok(pending) => pending,
        Err(error) => {
            let _ = writeln!(
                views.diagnostic,
                "lane {lane} steering landing unmeasured: steer::index-unreadable: {error}"
            );
            return Ok(());
        }
    };
    for (key, received_millis, observed) in landed {
        let Some(head) = pending.iter().find(|head| {
            head.body().key == key
                && head.body().sender.lane().is_none()
                && head.body().fanout.is_some()
        }) else {
            continue;
        };
        let clocks = landing::Clocks {
            published: head.body().published,
            received_millis,
            landed: observed,
        };
        if let Some(line) = landing::describe(lane, &key, clocks) {
            let _ = writeln!(views.diagnostic, "{line}");
        }
    }
    Ok(())
}

/// Publish the SYN packets each swept record's author wrote.
///
/// Authorship is the sweep's capture binding: a live sweep's records belong
/// to the noticed occupant, a closing sweep's to the occupant that left.
fn publish_swept<P>(
    bus: &Bus,
    epoch: CanonicalUuid,
    surfaces: &CaptureSurfaces<'_, P>,
    swept: &[std::borrow::Cow<'_, lane::capture::CaptureRecord>],
    refusals: steering::Refusals,
    views: &mut CaptureViews<'_>,
) -> CommandResult<()> {
    let sources = {
        let state = lock_state(views.state)?;
        swept
            .iter()
            .map(|record| state.observations.source(record.lane, record.locus))
            .collect::<Vec<_>>()
    };
    for (record, source) in swept.iter().zip(sources) {
        let Some(source) = source else {
            continue;
        };
        steering::publish_authored(
            bus,
            surfaces.paths,
            epoch,
            surfaces.steering,
            &steering::AuthoredRecord {
                lane: record.lane,
                author: record.occupant,
                source,
                locus: record.locus,
                events: record.decoded.events(),
            },
            refusals,
            views.diagnostic,
        );
        let _ = super::room_catch_up::recognize(bus, record, source)?;
        retain_conversation_evidence(record, source, views)?;
    }
    Ok(())
}

fn retain_conversation_evidence(
    record: &lane::capture::CaptureRecord,
    source: CanonicalUuid,
    views: &mut CaptureViews<'_>,
) -> CommandResult<()> {
    let mut state = lock_state(views.state)?;
    let turn = record.decoded.context().and_then(|context| context.turn);
    let owner = (record.lane, record.occupant);
    let requested = spice::rooms::profile::design_request_causes(record.decoded.events().iter());
    if !requested.is_empty() {
        let current = state.profile_turn_causes.entry(owner).or_default();
        current.extend(requested);
        current.sort();
        current.dedup();
    }
    let caused_by = state
        .profile_turn_causes
        .get(&owner)
        .cloned()
        .unwrap_or_default();
    for prose in record
        .decoded
        .events()
        .iter()
        .filter_map(|event| match event {
            listen::Event::Prose(prose) if prose.role == listen::ProseRole::Assistant => {
                Some(prose)
            }
            _ => None,
        })
    {
        let Some(conversation) = spice::rooms::evidence::Conversation::read(&prose.text) else {
            continue;
        };
        let key = spice::rooms::profile::EvidenceKey::conversation(
            source,
            prose.locus.member,
            prose.locus.offset,
            prose.locus.length,
            caused_by.clone(),
        );
        state.profiles.insert(
            record.lane,
            record.occupant,
            spice::rooms::evidence::Evidence::conversation(key, &conversation),
        );
    }
    if turn == Some(listen::TurnMoment::Completed) {
        state.profile_turn_causes.remove(&owner);
    }
    Ok(())
}

/// Retire the directives the swept prose answered.
///
/// What a lane says is its answer, and the accounting sweep hears it even if
/// live projection displayed it first.
fn hear_swept(
    bus: &Bus,
    paths: &RuntimePaths,
    steering: &steer::Config,
    (epoch, joining): (CanonicalUuid, crate::board_command::Joining<'_>),
    lane: CanonicalUuid,
    swept: &[std::borrow::Cow<'_, lane::capture::CaptureRecord>],
    diagnostic: &mut dyn io::Write,
) -> CommandResult<()> {
    let keys = steering_keys(swept.iter().flat_map(|record| {
        record
            .decoded
            .events()
            .iter()
            .filter_map(listen::Event::assistant_text)
    }));
    if keys.is_empty() {
        return Ok(());
    }
    let residency = current_residency(bus, epoch)?;
    let Some(row) = residency.body().lanes.iter().find(|row| row.lane == lane) else {
        return Ok(());
    };
    if ingest_vendor(&row.vendor).is_some() {
        steering::dispose_heard(
            bus,
            paths,
            (steering, epoch, joining),
            lane,
            &row.vendor,
            &keys,
            diagnostic,
        );
    }
    Ok(())
}

/// Account and present whatever a lane's tape gained before its seat closed,
/// then forget the occupant's binding.
///
/// An occupant writes its last records moments before it exits, and the exit
/// can be accounted before their notices are read; a notice read afterwards
/// no longer names the seated occupant and is refused. The closing sweep
/// runs once per vacated seat, before a successor starts, and projects any
/// remaining records in tape order under that occupant.
///
/// The seat is gone whether or not its records were presented, so its exact
/// binding leaves [`LaneProfiles`] either way. The lane's profile keeps every
/// card it retained, so the next background fit still counts its
/// conversation and every later occupant adds to it.
///
/// [`LaneProfiles`]: spice::rooms::profile::LaneProfiles
pub(super) fn present_closed_transcript<P: AgentMessagePublisher>(
    bus: &Bus,
    epoch: CanonicalUuid,
    surfaces: &CaptureSurfaces<'_, P>,
    recognition: TasteRecognition<'_>,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    mut views: CaptureViews<'_>,
) -> CommandResult<()> {
    let presented = present_closing_records(
        bus,
        epoch,
        surfaces,
        recognition,
        lane,
        occupant,
        &mut views,
    );
    let mut state = lock_state(views.state)?;
    state.profiles.vacate(lane, occupant);
    state.profile_turn_causes.remove(&(lane, occupant));
    presented
}

fn present_closing_records<P: AgentMessagePublisher>(
    bus: &Bus,
    epoch: CanonicalUuid,
    surfaces: &CaptureSurfaces<'_, P>,
    mut recognition: TasteRecognition<'_>,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    views: &mut CaptureViews<'_>,
) -> CommandResult<()> {
    // A seat that closed writes nothing more, so nothing may stay held: the
    // closing sweep accounts every span this lane still owes before its
    // successor opens a generation against the same head.
    let held = std::mem::take(&mut lock_state(views.state)?.accounting);
    let (mut observations, held) = super::ingest_transcripts(
        bus,
        surfaces.paths,
        Some(lane),
        lane::ingest::Bound::IMMEDIATE,
        held,
        views.diagnostic,
    )?;
    let swept = sweep::closing_records(lane, occupant, &mut observations);
    {
        let mut state = lock_state(views.state)?;
        state.accounting = held;
        state.observations.extend(observations);
    }
    sweep::present_swept(bus, epoch, surfaces, &mut recognition, &swept, views)?;
    publish_swept(
        bus,
        epoch,
        surfaces,
        &swept,
        steering::Refusals::Record,
        views,
    )?;
    lock_state(views.state)?.media_hydration.clear_lane(lane);
    Ok(())
}

struct AgentRecord<'a> {
    lane: CanonicalUuid,
    author: CanonicalUuid,
    source: Option<CanonicalUuid>,
    locus: listen::Locus,
    context: &'a listen::RecordContext,
    events: &'a [(u32, &'a listen::Event)],
    observed: Option<wire::Instant>,
    media_source: Option<&'a std::path::Path>,
    /// The addressing of an admitted invitation's message, whose whole prose
    /// body is one message to that lane in that task's room.
    invitation: Option<&'a wire::outbox::Invitation>,
}

struct AgentEvent<'a> {
    lane: CanonicalUuid,
    author: CanonicalUuid,
    source: Option<CanonicalUuid>,
    ordinal: u32,
    decoded: &'a listen::Event,
    authored: Option<&'a str>,
    observed: Option<wire::Instant>,
    observations: &'a MessageObservations,
    prose_origin: Option<wire::MessageProseOrigin>,
    suppress_answer: bool,
    superseded_answers: &'a [steer::Key],
    invitation: Option<&'a wire::outbox::Invitation>,
}

struct ProjectedMessage {
    published: wire::AgentMessage,
    terminal: Option<wire::AgentMessage>,
    /// The keys this prose acknowledged before the journal retired them.
    pending: Option<pending::PendingPresentation>,
}

/// Whether `texts` hold a placed, well-formed packet and every occupant address
/// those packets name.
fn packet_destinations<'a>(
    texts: impl IntoIterator<Item = &'a str>,
) -> (bool, BTreeSet<wire::OccupantAddress>) {
    let mut packets = false;
    let mut addresses = BTreeSet::new();
    for publication in texts
        .into_iter()
        .filter(|text| text.contains(steer::ack::scan::message::MARKER))
        .flat_map(steer::ack::scan::scan_messages)
        .filter_map(|message| message.publication.ok())
    {
        packets = true;
        addresses.extend(publication.addresses());
    }
    (packets, addresses)
}

fn message_addresses<'a>(
    texts: impl IntoIterator<Item = &'a str>,
) -> BTreeSet<wire::OccupantAddress> {
    texts
        .into_iter()
        .filter(|text| text.contains(steer::ack::scan::message::MARKER))
        .flat_map(steer::ack::scan::scan_messages)
        .filter_map(|message| message.publication.ok())
        .flat_map(|publication| publication.addresses().collect::<Vec<_>>())
        .collect()
}

pub(super) fn load_steering_presentations(
    reader: &bus::BusReader,
    keys: &BTreeSet<steer::Key>,
) -> CommandResult<Vec<bus::SteeringPresentation>> {
    let keys = keys.iter().map(ToString::to_string).collect::<Vec<_>>();
    let mut presentations = Vec::new();
    for batch in keys.chunks(bus::MAX_STEERING_PRESENTATION_KEYS) {
        presentations.extend(
            reader
                .steering_presentations(batch.to_vec())
                .map_err(|error| {
                    store_fault("serve::message-steering-unavailable", &error.to_string())
                })?,
        );
    }
    Ok(presentations)
}

/// The role heads deciding which loaded directives the operator authored.
pub(super) fn load_principal_roles(
    reader: &bus::BusReader,
) -> CommandResult<wire::principal::PrincipalRoles> {
    reader
        .principal_roles()
        .map_err(|error| store_fault("serve::message-roles-unavailable", &error.to_string()))
}

pub(crate) fn quote_agent_prose(
    reader: &bus::BusReader,
    lane: CanonicalUuid,
    source: CanonicalUuid,
    ordinal: u32,
    prose: &listen::Prose,
) -> Result<String, String> {
    let serve::stream::MessagePresentation {
        mut text,
        directive_contexts,
        ..
    } = present_acknowledged_prose(reader, lane, source, ordinal, prose)?;
    for context in directive_contexts.iter().rev() {
        let start = usize::try_from(context.start).expect("wire directive start always fits usize");
        let end = usize::try_from(context.end).expect("wire directive end always fits usize");
        text.replace_range(start..end, &context.quote_target.markdown());
    }
    Ok(text)
}

/// Present one lane's settled prose against the journal steering its
/// acknowledgement headers name, as its card shows it. No authoring occupant
/// is known here, so its SYN packets do not present as sends.
pub(super) fn present_acknowledged_prose(
    reader: &bus::BusReader,
    lane: CanonicalUuid,
    source: CanonicalUuid,
    ordinal: u32,
    prose: &listen::Prose,
) -> Result<serve::stream::MessagePresentation, String> {
    let keys = steering_keys([prose.text.as_str()]);
    if keys.is_empty() {
        return Ok(serve::stream::MessagePresentation::plain(
            prose.text.clone(),
        ));
    }
    let rows = load_steering_presentations(reader, &keys).map_err(|error| format!("{error:?}"))?;
    let roles = load_principal_roles(reader).map_err(|error| format!("{error:?}"))?;
    let directives = PresentationDirectives::from_presentations(reader, &rows, roles)?;
    present_steering_acknowledgements(
        &prose.text,
        Some(&directives),
        PresentationEvent {
            lane,
            author: None,
            source: Some(source),
            locus: prose.locus,
            ordinal,
            superseded_answers: &[],
        },
    )
    .map_err(|error| format!("{error:?}"))
}

/// The journal context one card's presentation reads: the steering directives
/// its acknowledgement headers name, the occupant births its packet addresses
/// name, and the role heads deciding which directives the operator sent.
#[derive(Clone, Debug, Default)]
pub(super) struct PresentationDirectives {
    directives: BTreeMap<steer::Key, PresentationDirective>,
    /// An address names one birth forever, so a proved recipient never
    /// changes and departure never redirects it to the lane's next occupant.
    recipients: BTreeMap<wire::OccupantAddress, wire::MessageRecipient>,
    roles: wire::principal::PrincipalRoles,
    /// Line identity of each presented peer line input.
    lines: BTreeMap<steer::Key, wire::intent::LineStanding>,
}

#[derive(Clone, Debug)]
struct PresentationDirective {
    lane: CanonicalUuid,
    text: String,
    authored: bool,
    taste: bool,
    review: Option<wire::MessageReview>,
    disposition: wire::Disposition,
    quote_target: wire::QuoteTarget,
    /// Images retained by this directive, each with its exact quote target.
    media_quotes: Vec<wire::MediaQuote>,
    /// Actor on the creating fact, whose principal roles grant authority.
    actor: wire::Actor,
    /// Who said the directive and whom it reached, exactly as a quote of it
    /// names them. A packet's body already stands in `text`.
    attribution: wire::QuoteAttribution,
    /// Journal instant at which the directive's creating fact was committed.
    published: wire::Instant,
    /// Operator submit action this directive's send shares with its siblings.
    fanout: Option<CanonicalUuid>,
    retirement: Option<wire::body::TranscriptLocus>,
}

#[derive(Clone, Copy, Debug)]
struct PresentationEvent<'a> {
    lane: CanonicalUuid,
    /// Occupant whose transcript wrote the event, when the presenter knows it;
    /// only an event with an author presents its SYN packets as sends.
    author: Option<CanonicalUuid>,
    source: Option<CanonicalUuid>,
    locus: listen::Locus,
    ordinal: u32,
    /// Keys whose answer a later event in this captured record owns.
    superseded_answers: &'a [steer::Key],
}

struct EventDirectives<'a> {
    directives: &'a PresentationDirectives,
    event: PresentationEvent<'a>,
}
