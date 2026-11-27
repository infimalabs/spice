//! Driving a vendor over its stdin: the opening request, interjected
//! steering, and the close that ends its one turn.
//!
//! A vendor launched over a command stream reads nothing from its arguments
//! after start. The supervisor owns the pipe: it writes the opening request,
//! reads the current authenticated steering inbox through the installed
//! executable when kicked, answers dialogs, and drops the pipe once the
//! admitted prompt has settled so the child drains and exits on its own. A
//! drain interrupts the turn over the same pipe before any signal is sent. A
//! confirm dialog becomes one consent request the resident decides; a call on
//! the lane's shell runs in the lane's own terminal and answers with what the
//! command said; a read of a `spice://` URL answers with the resource it
//! names; every other dialog is declined, since nobody at the lane can compose
//! a choice or text for it.
//!
//! Steering never waits on a command: unheard direction arriving while a
//! command runs interrupts the call, which answers with what the command said
//! so far while the command keeps running as a job. The resident reaches the
//! driver through a datagram signal; the payload remains in journal authority.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use listen::{Decoded, Dialog, Event, ProseRole};
use serde::{Deserialize, Serialize};
use wire::CanonicalUuid;

use super::super::RuntimePaths;
use super::Phase;
use super::terminal::{Control, Job, Report, ServiceAuthority, ServiceClaim, Terminal};
use crate::resident::steering::{LineOutcomeNotice, MessageRecordedNotice, MessageRefusalNotice};
use crate::resource::Server;
use steering::{ActivityReminder, InboxDelivery, ReasoningReminder};
#[cfg(test)]
use work::Work;
use work::{ActiveCall, StepState};

/// Bytes one kick datagram may carry, including private occupant-scoped notices.
pub(in crate::resident) const KICK_BYTES: usize = 1024;
const ROOM_CATCH_UP_PURPOSE: &str = "room-catch-up";
/// What an occupied lane is told while room activity it has not read waits.
const ROOM_CATCH_UP_MARKER: &str = concat!(
    "<system-notice>\n",
    "Unread room activity waits in rooms you listen to. ",
    "This notice needs no visible acknowledgment and starts no new task. ",
    "Before parking, read `spice://room/inbox` (or run `spice room inbox`) and ",
    "follow every `more unread activity remains` continuation until the inbox is drained. ",
    "Then remain silent; let a finite job or substantive steering wake the lane.\n",
    "</system-notice>",
);
const CALLBACK_REPLY_BYTES: usize = 4096;
const RETUNE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Correlates the driver's own fast-mode release after a vendor refused it;
/// no live retune waits on its response.
const FAST_REFUSED_REQUEST_ID: &str = "spice-fast-refused";
/// What the occupant is told when its refused turn resumes without fast mode.
const FAST_REFUSED_CONTINUE: &str = "The provider refused fast mode, so Spice turned it off for this session. Continue where you left off.";

/// One child's command stream and what has already been said on it.
type InboxReader = Box<dyn Fn() -> Option<Vec<u8>> + Send + Sync>;
/// Composes the lane's folded briefing; `None` when there is none to give.
type BriefingReader = Box<dyn Fn() -> Option<String> + Send + Sync>;
/// Composes a resumed seat's notice from the lane's current authority.
type ResumeReader = Box<dyn Fn() -> String + Send + Sync>;
/// Whether the journal's residency head seats this driver's occupant.
type SeatedReader = Box<dyn Fn() -> bool + Send + Sync>;
/// Whether room activity waits unread for this lane; `None` when the journal
/// cannot say.
type UnreadReader = Box<dyn Fn() -> Option<bool> + Send + Sync>;

/// What a driver reads of its occupant's durable state.
struct Readers {
    inbox: InboxReader,
    briefing: BriefingReader,
    resume: ResumeReader,
    seated: SeatedReader,
    unread: UnreadReader,
}

/// Reads of an occupant with no steering, briefing, or resume notice, seated.
#[cfg(test)]
impl Default for Readers {
    fn default() -> Self {
        Self {
            inbox: Box::new(|| None),
            briefing: Box::new(|| None),
            resume: Box::new(String::new),
            seated: Box::new(|| true),
            unread: Box::new(|| Some(false)),
        }
    }
}

pub(in crate::resident) struct Driver<W: Write + Send = std::process::ChildStdin> {
    stdin: Mutex<Option<W>>,
    workers: AtomicUsize,
    retention: Mutex<retention::State>,
    retention_io: retention::Io,
    read_inbox: InboxReader,
    /// Read once per compaction, off the decoding path, since a briefing
    /// scans the transcript and the journal.
    read_briefing: BriefingReader,
    /// Read once, when a resumed seat's first turn is delivered.
    read_resume: ResumeReader,
    /// Read while the first turn is held: it is written only to a seated
    /// occupant, whose commands job admission accepts.
    read_seated: SeatedReader,
    sequence: AtomicU64,
    /// Serialize an inbox read with every transition derived from that exact
    /// rendering, so an older read cannot settle keys published by a later one.
    inbox_delivery: Mutex<()>,
    delivered: Mutex<Option<Vec<u8>>>,
    /// Human directive keys that made the current OMP step stale, and whether
    /// that step has consumed its one replay.
    step: Mutex<StepState>,
    /// Latest queued background notice per purpose. Superseded notices are
    /// coalesced in the extension until omp's next step boundary.
    background: Mutex<BTreeMap<String, String>>,
    /// Read when a commit reaches this lane, at start, and after compaction.
    read_unread: UnreadReader,
    /// Refusal notices an idle occupant could not take, by purpose, held for
    /// its next agent start rather than waking it.
    quiet: Mutex<BTreeMap<String, String>>,
    /// Keys whose original was queued without replacement, or whose recovery
    /// was observed on the transcript. Reminders can point at that telling.
    /// Compaction drops the context and this set.
    told: Mutex<BTreeSet<String>>,
    /// Successfully delivered keys recognized for this occupant's lifetime,
    /// even when compaction drops the verbatim telling.
    recognized: Mutex<BTreeSet<String>>,
    /// Told keys after whose telling the occupant wrote visible prose, so a
    /// resend can say whether it saw a reply at all. Reasoning and tool
    /// arguments are not prose. Cleared with `told`.
    replied: Mutex<BTreeSet<String>>,
    /// Keys the occupant's tape-backed assistant text answered that the
    /// journal still lists. Every composition holds them back; compaction keeps
    /// them, since the journal's lag is not the context's.
    answered: Mutex<answered::Answered>,
    /// Hidden-reasoning attempts already corrected in this context and pending
    /// inbox generation. The text itself is never retained.
    reasoning_reminder: Mutex<ReasoningReminder>,
    /// Response-counted correction state for pending keys whose first reminder
    /// this occupant received. Compaction clears it; a new driver never
    /// inherits it.
    activity_reminder: Mutex<ActivityReminder>,
    /// The seal wrapping this lane's steering, which prose may name without
    /// naming a directive.
    seal: steer::Key,
    /// Tokens a visible acknowledgment header named without holding them, and
    /// already corrected in this context.
    misdirected: Mutex<BTreeSet<String>>,
    /// Latest claimed-task generation and current completion disclosure delivered.
    task_generation: Mutex<Option<(TaskContractNotice, u64)>>,
    /// Exact-claim disclosure and accepted round events for this occupant.
    task_disclosure: Mutex<task_notices::TaskDisclosureState>,
    /// Latest claim lapse privately delivered to this occupant.
    claim_lapse: Mutex<Option<ClaimLapseNotice>>,
    /// Latest taste hold privately delivered to this occupant.
    taste_hold: Mutex<Option<TasteHoldNotice>>,
    /// Latest renewal state observed by this exact occupant.
    renewal: Mutex<Option<renewal::Notice>>,
    /// The confirm dialog each open consent request will answer.
    asks: Mutex<HashMap<CanonicalUuid, String>>,
    /// Where the occupant's commands run, for an occupant that has a shell.
    terminal: Option<Terminal>,
    /// The calls being answered now, by the host tool call each answers.
    calls: Mutex<HashMap<String, ActiveCall>>,
    /// The latest progress each in-flight job wait reported, by its call:
    /// see [`waits`].
    waits: Mutex<HashMap<String, String>>,
    /// The `spice://` server and the host URI requests it is answering now.
    uris: uri::Uris,
    /// Correlated live retunes awaiting an OMP response.
    retunes: Mutex<HashMap<String, PendingRetune>>,
    /// Set once this occupant's vendor refused fast mode and the driver turned
    /// it off to continue; another refusal ends the turn like any vendor
    /// error. The operator turning fast mode back on re-arms it.
    fast_refused: AtomicBool,
    registration: registration::Registration,
}

/// What one observed record raises to the resident.
pub(in crate::resident) enum Raised {
    Ask(Ask),
    /// The vendor refused fast mode in these words; the occupant continues
    /// without it.
    FastRefused(String),
}

/// A confirm dialog stated as the consent request the resident decides.
pub(in crate::resident) struct Ask {
    pub(in crate::resident) request: CanonicalUuid,
    pub(in crate::resident) operation: String,
}

/// What one datagram at the lane's kick door says.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(in crate::resident) enum Kick {
    /// The lane's journal-backed steering inbox changed.
    Inbox,
    /// This already-running lane's unread room activity may have changed.
    RoomCatchUp,
    /// Consent ruled on a request the occupant raised.
    Answer {
        request: CanonicalUuid,
        confirmed: bool,
    },
    /// A committed progress reminder for this exact occupant.
    Narration(wire::body::NarrationProdded),
    /// A claimed task changed after this occupant's claim acknowledged it.
    TaskGeneration(TaskGenerationNotice),
    /// A task the resident proactively assigned to this exact occupant.
    TaskAssignment(TaskAssignmentNotice),
    /// A participant joined or arrived in a shared round held by this occupant.
    TaskRoundUpdate(TaskRoundUpdateNotice),
    /// A claim this exact occupant held ended; an assignment notice for it
    /// that is still queued is withdrawn rather than delivered stale.
    TaskAssignmentEnded(TaskAssignmentEnd),
    /// A current claim's completion contract as it stands now, for this exact
    /// occupant after the claim's initial disclosure.
    TaskContract(TaskContractNotice),
    /// A private background notice whose exact occupant must confirm enqueue.
    BackgroundConfirmed {
        notice: crate::resident::design_input::BackgroundNotice,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<ServiceClaim>,
        request: String,
        reply: PathBuf,
    },
    /// A claim this lane held lapsed; addressed to the lane's current occupant.
    ClaimLapse(ClaimLapseNotice),
    /// A statement write by this lane left a task held for taste review;
    /// addressed to the lane's current occupant.
    TasteHold(TasteHoldNotice),
    /// An authored packet reached someone but missed an address, or its
    /// grammar refused before durable judgment.
    MessageRefused(MessageRefusalNotice),
    /// The receipt for a SYN packet this exact occupant authored and sent.
    MessageRecorded(MessageRecordedNotice),
    /// A peer message this exact occupant's context sent ended with no reply
    /// text reaching it.
    LineOutcome(LineOutcomeNotice),
    /// Current renewal intent for this exact occupant.
    Renewal(renewal::Notice),
    /// Change one setting on this exact running occupant and answer through a
    /// one-shot callback door after the correlated provider response.
    Retune {
        occupant: CanonicalUuid,
        request: String,
        retune: LiveRetune,
        reply: PathBuf,
    },
    /// Resolve one model spelling against this exact occupant's retained
    /// served-model catalog without changing its runtime state.
    ResolveModel {
        occupant: CanonicalUuid,
        spelling: String,
        reply: PathBuf,
    },
    /// Reconcile an idle process's exact task owner.
    ReconcileClaim,
    /// Update the held-claim parking projection for this exact owner.
    ClaimActivation { owner: ServiceClaim, admitted: bool },
    /// The lane is being drained: interrupt the turn and end the input.
    Abort,
}

impl<W: Write + Send + 'static> Driver<W> {
    /// Own a child's stdin, the occupant's supervisor state path, lane
    /// identity, and the terminal its commands run in.
    ///
    /// # Errors
    ///
    /// Returns the error naming why this supervisor cannot launch reads
    /// through its executable's currently installed image.
    pub(in crate::resident::supervisor) fn new(
        stdin: W,
        state: &Path,
        lane: CanonicalUuid,
        worktree: PathBuf,
        terminal: Option<Terminal>,
        authority: ServiceAuthority,
        report: impl Fn(Phase) + Send + Sync + 'static,
    ) -> io::Result<Arc<Self>> {
        let inbox = inbox::Reader::new(state, lane, &worktree)?;
        let sessions = current::Sessions::new(lane, worktree.clone())?;
        let uri_server = current::resource_server(lane, worktree, terminal.clone())?;
        let briefing = Arc::clone(&sessions);
        let seating = terminal.clone();
        let rooms = authority.clone();
        Ok(Self::with_runtime(
            stdin,
            Readers {
                inbox: Box::new(move || inbox.read()),
                briefing: Box::new(move || briefing.briefing()),
                resume: Box::new(move || sessions.resume()),
                seated: Box::new(move || seating.as_ref().is_none_or(Terminal::seated)),
                unread: Box::new(move || rooms.rooms_unread().ok()),
            },
            uri_server,
            steer::Key::for_identity(lane),
            terminal,
            retention::Io::new(state.to_path_buf(), authority, report),
        ))
    }

    #[cfg(test)]
    fn with_server(
        stdin: W,
        readers: Readers,
        uri_server: Server,
        seal: steer::Key,
        terminal: Option<Terminal>,
    ) -> Arc<Self> {
        Self::with_runtime(
            stdin,
            readers,
            uri_server,
            seal,
            terminal,
            retention::Io::default(),
        )
    }

    fn with_runtime(
        stdin: W,
        readers: Readers,
        uri_server: Server,
        seal: steer::Key,
        terminal: Option<Terminal>,
        retention_io: retention::Io,
    ) -> Arc<Self> {
        let Readers {
            inbox,
            briefing,
            resume,
            seated,
            unread,
        } = readers;
        Arc::new(Self {
            retention: Mutex::new(retention::State::Active),
            retention_io,
            stdin: Mutex::new(Some(stdin)),
            workers: AtomicUsize::new(0),
            read_inbox: inbox,
            read_briefing: briefing,
            read_resume: resume,
            read_seated: seated,
            read_unread: unread,
            sequence: AtomicU64::new(0),
            inbox_delivery: Mutex::new(()),
            delivered: Mutex::new(None),
            step: Mutex::new(StepState::default()),
            background: Mutex::new(BTreeMap::new()),
            told: Mutex::new(BTreeSet::new()),
            recognized: Mutex::new(BTreeSet::new()),
            replied: Mutex::new(BTreeSet::new()),
            quiet: Mutex::new(BTreeMap::new()),
            answered: Mutex::new(answered::Answered::default()),
            reasoning_reminder: Mutex::new(ReasoningReminder::default()),
            activity_reminder: Mutex::new(ActivityReminder::default()),
            seal,
            misdirected: Mutex::new(BTreeSet::new()),
            task_generation: Mutex::new(None),
            task_disclosure: Mutex::new(task_notices::TaskDisclosureState::default()),
            claim_lapse: Mutex::new(None),
            taste_hold: Mutex::new(None),
            renewal: Mutex::new(None),
            asks: Mutex::new(HashMap::new()),
            terminal,
            calls: Mutex::new(HashMap::new()),
            waits: Mutex::new(HashMap::new()),
            uris: uri::Uris::new(uri_server),
            retunes: Mutex::new(HashMap::new()),
            fast_refused: AtomicBool::new(false),
            registration: registration::Registration::default(),
        })
    }

    /// Read and interject the current authenticated inbox unless it is empty
    /// or exactly what the agent already received. Every call being answered
    /// is interrupted before unheard direction is written. The resident sends
    /// this kick right after it commits a start, so a first turn held for that
    /// seating is released first.
    pub(in crate::resident) fn deliver_inbox(&self) {
        if !self.release_first_turn() {
            self.interject(InboxDelivery::Update);
        }
    }

    /// Queue, keep, or withdraw the one fixed marker of unread room activity
    /// without interrupting work. The marker never carries the activity: the
    /// lane renders it by reading its room inbox. An unchanged marker is not
    /// written again, heard or not; `renew` writes it once more, since a
    /// provider may discard an unheard aside with the context a compaction
    /// drops.
    fn deliver_room_catch_up(&self, renew: bool) {
        let Some(unread) = (self.read_unread)() else {
            return;
        };
        if renew {
            self.background
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(ROOM_CATCH_UP_PURPOSE);
        }
        let _ = self.background(
            ROOM_CATCH_UP_PURPOSE,
            if unread { ROOM_CATCH_UP_MARKER } else { "" },
        );
    }

    /// Interrupt current host calls into kept jobs and deliver the progress
    /// reminder through steering now. The status line it asks for is progress
    /// inside the turn, so it says to keep working after giving it.
    fn deliver_narration(&self, prod: &wire::body::NarrationProdded) {
        let urgency = if prod.count > 1 {
            "Progress update overdue"
        } else {
            "Progress update due"
        };
        let message = format!(
            "<system-notice>\n{urgency}: {} tool calls since your last visible response \
             (reminder {}). Give the operator one short status line now, then continue \
             the current turn without stopping: the status line reports progress and \
             does not end your work. Keep healthy work running; do not cancel it to \
             answer this reminder. No acknowledgment key is required.\n</system-notice>",
            prod.moments, prod.count,
        );
        let (calls, _) = self.delivery_calls(true, false);
        for handle in calls {
            handle.interrupt();
        }
        let _sent = self.steer(&message);
    }

    /// Tell this exact occupant which addresses a partly delivered packet it
    /// authored missed, or which grammar refusal prevented durable judgment,
    /// without waking it.
    fn deliver_message_refusal(&self, notice: &MessageRefusalNotice) {
        self.refuse_quietly(&notice.purpose(), &notice.text());
    }

    /// Tell this exact occupant what a SYN packet it authored and sent reached.
    fn deliver_message_recorded(&self, notice: &MessageRecordedNotice) {
        let _ = self.background(&format!("syn-recorded:{}", notice.packet), &notice.text());
    }

    /// Tell this exact occupant, without waking it, how a peer message it sent
    /// ended when no reply text reached it.
    fn deliver_line_outcome(&self, notice: &LineOutcomeNotice) {
        let _ = self.background(&format!("line-outcome:{}", notice.input), &notice.text());
    }

    /// Replace one queued notice without interrupting model or tool work. A
    /// notice queued while the first turn is still unwritten is held and
    /// written right after that turn's prompt.
    fn background(&self, purpose: &str, message: &str) -> io::Result<()> {
        self.update_background(purpose, |_| message.to_owned())
    }

    fn update_background(
        &self,
        purpose: &str,
        update: impl FnOnce(&str) -> String,
    ) -> io::Result<()> {
        let mut queued = self
            .background
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let previous = queued.get(purpose).map_or("", String::as_str);
        let message = update(previous);
        if previous == message {
            return if message.is_empty() {
                Ok(())
            } else {
                self.activate()
            };
        }
        if !self.registration.holds_first_turn() {
            self.activate()?;
            let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
            self.write(&vendor::omp::aside_request(sequence, purpose, &message))?;
        }
        if message.is_empty() {
            queued.remove(purpose);
        } else {
            queued.insert(purpose.to_owned(), message);
        }
        Ok(())
    }

    /// Record one settled visible assistant response and clear narration.
    /// Streamed revisions and tool-only records are not complete responses.
    fn observe_visible_reply(&self, decoded: &Decoded) -> bool {
        let replied = decoded.events().iter().any(|event| {
            matches!(event, Event::Prose(prose)
                if prose.role == ProseRole::Assistant && !prose.text.trim().is_empty())
        });
        if replied {
            let told = self.told.lock().unwrap_or_else(PoisonError::into_inner);
            self.replied
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(told.iter().cloned());
            let _cancelled = self.background("narration", "");
        }
        replied
    }

    /// Dispatch a host operation only after its exact record reached durable
    /// tape. An authored write, the outbox or an invitation's message, and an
    /// outbox receipt read carry that record's provenance.
    pub(in crate::resident) fn observe_capture(
        self: &Arc<Self>,
        record: &lane::capture::CaptureRecord,
    ) -> Option<Raised> {
        if let Some(listen::Dialog::HostUri {
            id,
            operation,
            url,
            content,
        }) = record
            .decoded
            .context()
            .and_then(|context| context.dialog.as_ref())
            && (spice::outbox_source::authored_write(operation, url)
                || (operation == "read" && url.starts_with("spice://outbox/")))
        {
            self.serve_uri_at(
                id,
                operation,
                url,
                content.as_deref(),
                Some(wire::outbox::Carrier {
                    occupant: record.occupant,
                    locus: wire::TapeLocus {
                        member: record.locus.member,
                        offset: record.locus.offset,
                        length: record.locus.length,
                    },
                }),
            );
            return None;
        }
        self.observe(&record.decoded)
    }

    /// Let one decoded record shape delivery, and state what it raises to the
    /// resident: the consent request a confirm dialog asks for, or the
    /// vendor's refusal of fast mode.
    ///
    /// A confirm holds the turn until consent answers it through
    /// [`Self::answer`]; a call on the lane's shell holds it until the
    /// command ends; any other dialog is declined at once, since no operator
    /// sees a lane's prompts and the turn would otherwise hold. A withdrawn
    /// call ends its command. A compaction discards what the agent was told,
    /// so the standing inbox is interjected again; the end of the agent's
    /// turn ends its input, so the child drains and exits rather than idling
    /// for a prompt nobody sends. A turn the vendor failed only because it
    /// refused fast mode instead continues on the same session without it.
    pub(in crate::resident) fn observe(self: &Arc<Self>, decoded: &Decoded) -> Option<Raised> {
        self.observe_step(decoded);
        self.observe_retune(decoded);
        let fast = self.observe_opening(decoded);
        // A telling seen on the transcript is now held verbatim. Ordinary
        // background delivery releases its replacement slot; the unread-room
        // marker holds its slot until the journal shows nothing unread.
        for prose in decoded.events().iter().filter_map(|event| match event {
            Event::Prose(prose) if prose.role == ProseRole::Operator => Some(prose),
            _ => None,
        }) {
            let mut told = self.told.lock().unwrap_or_else(PoisonError::into_inner);
            let recognized = self
                .recognized
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            told.extend(
                steer::inbox::keys(&prose.text)
                    .filter(|key| recognized.contains(*key))
                    .map(str::to_owned),
            );
            drop(recognized);
            drop(told);
            self.observe_activity_reminder(&prose.text);
            self.background
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|purpose, message| {
                    purpose == ROOM_CATCH_UP_PURPOSE || !prose.text.contains(message.as_str())
                });
        }
        // Record visible acknowledgements before a host-tool worker can
        // immediately return and replay the current inbox.
        self.cancel_answered_reminders(decoded);
        let context = decoded.context();
        let asked = match context.and_then(|context| context.dialog.as_ref()) {
            Some(Dialog::Confirm { id, title, message }) => Some(self.ask(id, title, message)),
            Some(Dialog::HostTool {
                id,
                tool,
                arguments,
            }) => {
                self.run(id, tool, arguments);
                None
            }
            Some(Dialog::HostUri {
                id,
                operation,
                url,
                content,
            }) => {
                self.serve_uri(id, operation, url, content.as_deref());
                None
            }
            Some(dialog) => {
                let _declined = self.write(&vendor::omp::dialog_declined(dialog.id()));
                None
            }
            None => None,
        };
        if let Some(withdrawn) = context.and_then(|context| context.withdrawn.as_deref()) {
            self.cancel(withdrawn);
        }
        self.restate_waits(decoded);
        let replied = self.observe_visible_reply(decoded);
        let compacted = decoded
            .events()
            .iter()
            .any(|event| matches!(event, Event::Compaction(_)));
        if compacted {
            // Match interject's lock order: no delivery may observe only one reset.
            let mut delivered = self
                .delivered
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let mut told = self.told.lock().unwrap_or_else(PoisonError::into_inner);
            told.clear();
            self.replied
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
            drop(told);
            delivered.take();
            drop(delivered);
            self.interject(InboxDelivery::PostCompaction);
            self.deliver_briefing();
            self.deliver_room_catch_up(true);
            self.reasoning_reminder
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .nudged
                .clear();
            self.misdirected
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
            self.reset_activity_reminder();
        }
        let hidden_correction = self.nudge_hidden_acknowledgements(decoded);
        let misdirected_correction = self.nudge_misdirected_acknowledgements(decoded);
        if replied && !compacted && !hidden_correction && !misdirected_correction {
            self.nudge_unanswered_activity();
        }
        if let Some(refusal) = self.finish_settled_prompt(decoded) {
            return Some(Raised::FastRefused(refusal));
        }
        asked.map(Raised::Ask).or(fast.map(Raised::FastRefused))
    }

    fn finish_settled_prompt(&self, decoded: &Decoded) -> Option<String> {
        let outcome = decoded
            .context()
            .and_then(|context| context.prompt_outcome.as_ref());
        let settled = decoded.context().is_some_and(|context| {
            context.session_settled || outcome.is_some_and(|outcome| outcome.session_settled)
        });
        if !settled {
            return None;
        }
        if let Some(refusal) = self.continue_without_fast(decoded) {
            return Some(refusal);
        }
        if outcome.is_some_and(|outcome| outcome.status == listen::PromptStatus::Error) {
            // A claim keeps a completed turn's process parked for its
            // continuation. A failed turn must instead reach EOF so exit
            // accounting can hold the lane on the provider's words.
            self.finish();
        } else {
            self.settle();
        }
        None
    }

    /// Turn fast mode off and resume a turn whose vendor failure refused it,
    /// once until the operator turns it back on, answering the vendor's words.
    fn continue_without_fast(&self, decoded: &Decoded) -> Option<String> {
        let failed = decoded
            .context()
            .and_then(|context| context.prompt_outcome.as_ref())
            .is_some_and(|outcome| outcome.status == listen::PromptStatus::Error);
        let refusal = decoded.events().iter().find_map(|event| match event {
            Event::Failure(failure)
                if failed && vendor::omp::refuses_fast_mode(&failure.message) =>
            {
                Some(failure.message.clone())
            }
            _ => None,
        })?;
        if self.fast_refused.swap(true, Ordering::Relaxed) {
            return None;
        }
        self.write(&vendor::omp::set_fast_mode_request(
            FAST_REFUSED_REQUEST_ID,
            false,
        ))
        .and_then(|()| self.steer(FAST_REFUSED_CONTINUE))
        .ok()?;
        Some(refusal)
    }

    /// Answer the confirm dialog one consent request stands for.
    ///
    /// A request this driver never raised, or one it already answered, is
    /// nobody's dialog here and says nothing.
    pub(in crate::resident) fn answer(&self, request: CanonicalUuid, confirmed: bool) {
        let dialog = self
            .asks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&request);
        if let Some(dialog) = dialog {
            let _answered = self.write(&vendor::omp::dialog_confirmed(&dialog, confirmed));
        }
    }

    /// Follow a kept command to its ending, delivering its watcher's match
    /// as it comes and its ending after it.
    fn hear(&self, terminal: &Terminal, job: &Job, control: &Control) {
        let report = terminal.follow(job, control, &mut || self.deliver_match(job));
        self.deliver(job, report.as_ref());
    }

    /// Deliver the line a watched job matched, unless the lane heard it, as
    /// one background notice. The lane records hearing it only once its
    /// notice is written, under the input's lock, so a closed seat hears
    /// nothing and the next seat's catch-up owes exactly what this one did
    /// not say. Until the first prompt is written the match waits behind it.
    fn deliver_match(&self, job: &Job) {
        let Some(pattern) = job.until() else {
            return;
        };
        if self.hold_job(job, None) || self.activate().is_err() {
            return;
        }
        let mut stdin = self.stdin.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(stream) = stdin.as_mut() else {
            return;
        };
        let Ok(Some(line)) = job.unheard_match() else {
            return;
        };
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let notice = vendor::omp::aside_request(
            sequence,
            &format!("job:{}:match", job.id()),
            &vendor::omp::job_matched(job.id(), pattern, &line),
        );
        if written(stream, &notice).is_ok() {
            let _ = job.hear_match();
        }
    }

    /// Deliver a kept command's ending as steering, in omp's own notice,
    /// unless the lane already heard it: a job answer that reported the
    /// ending claimed the telling first. The claim precedes the notice, so a
    /// racing answer and notice never both stay silent. It is taken under the
    /// input's lock, so a closed seat claims nothing and the next seat's
    /// catch-up owes the ending, and a notice that fails to write gives the
    /// claim back. A stopped job has nothing to deliver; whoever stopped it
    /// was answered. Until the first prompt is written the ending waits
    /// behind it, unclaimed.
    fn deliver(&self, job: &Job, report: Option<&Report>) {
        if self.hold_job(job, Some(report)) {
            return;
        }
        if self.activate().is_err() {
            return;
        }
        let mut stdin = self.stdin.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(stream) = stdin.as_mut() else {
            return;
        };
        // A record that cannot be created is not a telling anyone owns, so
        // the notice goes out rather than risk silence.
        if !job.claim_heard().unwrap_or(true) {
            return;
        }
        let Some(report) = report else {
            return;
        };
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let notice = vendor::omp::aside_request(
            sequence,
            &format!("job:{}", job.id()),
            &vendor::omp::job_delivery(job.id(), &report.output),
        );
        if written(stream, &notice).is_err() {
            let _released = job.release_heard();
        }
    }

    /// Say what the lane is owed at its start: the standing inbox, and the
    /// match and ending of every job the lane has not heard, each followed
    /// to its end on a thread of its own.
    pub(in crate::resident) fn catch_up(self: &Arc<Self>) {
        self.deliver_inbox();
        self.deliver_room_catch_up(true);
        let Some(terminal) = self.terminal.clone() else {
            return;
        };
        for job in terminal.undelivered() {
            let driver = Arc::clone(self);
            let terminal = terminal.clone();
            let _followed = std::thread::Builder::new()
                .name("spice-catch-up".to_owned())
                .spawn(move || driver.hear(&terminal, &job, &Control::new()));
        }
    }

    /// End the work one withdrawn call is being answered with, if it still
    /// runs, and silence the answer to a withdrawn host URI request.
    fn cancel(&self, id: &str) {
        self.withdraw_uri(id);
        let handle = self
            .calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        if let Some(call) = handle {
            call.handle.cancel();
        }
    }

    fn ask(&self, dialog: &str, title: &str, message: &str) -> Ask {
        let request = super::super::new_uuid();
        self.asks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(request, dialog.to_owned());
        Ask {
            request,
            operation: match (title.trim(), message.trim()) {
                (title, "") => title.to_owned(),
                ("", message) => message.to_owned(),
                (title, message) => format!("{title}: {message}"),
            },
        }
    }

    /// Interrupt the agent's turn and end its input: how a drain begins.
    ///
    /// omp stops the turn where it stands and records the assistant message
    /// as aborted, then reads the end of its input and exits on its own. The
    /// resident's signal is only for an occupant that does neither.
    pub(in crate::resident) fn abort(&self) {
        let _aborted = self.write(&vendor::omp::abort_request());
        let running =
            std::mem::take(&mut *self.calls.lock().unwrap_or_else(PoisonError::into_inner));
        for call in running.into_values() {
            call.handle.cancel();
        }
        self.close();
    }

    /// End the child's input; further deliveries are dropped.
    pub(in crate::resident) fn close(&self) {
        self.finish();
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        // The activity state and stdin are one write boundary: settlement,
        // reconciliation, and drain cannot interleave after activation but
        // before the command reaches OMP.
        let state = self
            .retention
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *state != retention::State::Active {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the OMP command stream is idle",
            ));
        }
        let mut stdin = self.stdin.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(stream) = stdin.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the child's command stream is closed",
            ));
        };
        written(stream, bytes)
    }
}

fn written(stream: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(bytes)?;
    stream.flush()
}

/// Where this lane's driver listens for republished-inbox kicks: one
/// content-addressed leaf beneath the door root.
pub(in crate::resident) fn kick_path(paths: &RuntimePaths, lane: CanonicalUuid) -> PathBuf {
    let mut identity = blake3::Hasher::new();
    identity.update(paths.common.as_os_str().as_encoded_bytes());
    identity.update(lane.to_string().as_bytes());
    kick_path_beneath(door_root(paths), &identity.finalize().to_string())
}

/// Where ephemeral command doors live, each as one content-addressed leaf.
///
/// `sockaddr_un.sun_path` is only 104 bytes on Darwin, and repository and
/// worktree roots are not bounded that tightly, so Unix doors sit beneath the
/// canonical short system temporary path. A Windows door endpoint is an
/// ordinary file naming its pipe, so it stays in the private runtime
/// directory. Durable state remains repository-local either way.
fn door_root(paths: &RuntimePaths) -> &Path {
    #[cfg(unix)]
    {
        let _ = paths;
        Path::new("/tmp")
    }
    #[cfg(windows)]
    {
        &paths.live
    }
}

fn kick_path_beneath(root: &Path, identity: &str) -> PathBuf {
    root.join(format!("spice-k-{identity}.sock"))
}

/// Tell whichever driver listens for this lane that its inbox changed.
///
/// Nothing listens for a lane without a command-driven occupant, and a
/// driver that misses a kick reads the current inbox on the next one, so
/// the send is fire-and-forget.
pub(in crate::resident) fn kick(paths: &RuntimePaths, lane: CanonicalUuid) {
    send(paths, lane, &Kick::Inbox);
}

/// Reconcile an idle process's exact task owner at the task-service boundary.
pub(in crate::resident) fn reconcile_claim(paths: &RuntimePaths, lane: CanonicalUuid) {
    send(paths, lane, &Kick::ReconcileClaim);
}

/// Publish whether the exact idle claim may consume a new unit of work.
pub(in crate::resident) fn claim_activation(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    owner: ServiceClaim,
    admitted: bool,
) {
    send(paths, lane, &Kick::ClaimActivation { owner, admitted });
}

/// Tell an already-running lane's driver to look again for unread room activity.
/// A vacant lane has no kick door, so this operation has no wake authority.
pub(in crate::resident) fn room_catch_up(paths: &RuntimePaths, lane: CanonicalUuid) {
    send(paths, lane, &Kick::RoomCatchUp);
}

/// A committed prod is also available at the next shell/hook meter boundary.
/// Command-driven occupants can hear it now without cancelling their jobs.
pub(in crate::resident) fn narrate(paths: &RuntimePaths, prod: &wire::body::NarrationProdded) {
    send(paths, prod.lane, &Kick::Narration(prod.clone()));
}

/// Tell whichever driver listens for this lane how consent ruled on one of
/// its requests.
///
/// An occupant that has since left took its dialog with it, so the answer is
/// as fire-and-forget as an inbox kick.
pub(in crate::resident) fn answer(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    request: CanonicalUuid,
    confirmed: bool,
) {
    send(paths, lane, &Kick::Answer { request, confirmed });
}

/// Queue one keyless background notice for the exact occupant, retained for
/// `owner` as one exact-owner operation when one is named, and wait until that
/// supervisor confirms its driver accepted the notice.
///
/// # Errors
///
/// Returns an I/O error when no matching supervisor receives the request, its
/// callback cannot be read, or its answer names another request.
pub(in crate::resident) fn confirmed_background_notice(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    notice: crate::resident::design_input::BackgroundNotice,
    owner: Option<ServiceClaim>,
) -> io::Result<bool> {
    confirmed_background::send(&kick_path(paths, lane), &paths.common, notice, owner)
}

/// Privately tell the exact authoring occupant about a partial-delivery miss
/// or grammar refusal.
pub(in crate::resident) fn message_refused(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    notice: MessageRefusalNotice,
) {
    send(paths, lane, &Kick::MessageRefused(notice));
}

/// Privately give the exact authoring occupant the receipt for a SYN packet
/// it sent: how many recipients it reached and which rooms it recorded in.
pub(in crate::resident) fn message_recorded(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    notice: MessageRecordedNotice,
) {
    send(paths, lane, &Kick::MessageRecorded(notice));
}

/// Privately tell the sending occupant how one of its peer messages ended
/// when no reply text reached it.
pub(in crate::resident) fn line_outcome(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    notice: LineOutcomeNotice,
) {
    send(paths, lane, &Kick::LineOutcome(notice));
}

fn background_accepted<W: Write + Send + 'static>(
    driver: &Driver<W>,
    occupant: CanonicalUuid,
    notice: &crate::resident::design_input::BackgroundNotice,
) -> bool {
    notice.occupant == occupant && driver.background(&notice.purpose, &notice.text).is_ok()
}

/// Interrupt the driver listening at `door`, if one does.
///
/// A lane without a command-driven occupant has no door, and a door whose
/// supervisor has gone refuses the datagram; either way the error says
/// nobody is there to interrupt, so a signal is the only word left.
///
/// # Errors
///
/// Returns the send error when no driver takes the kick.
pub(in crate::resident) fn abort(door: &Path) -> io::Result<()> {
    send_to(door, &Kick::Abort)
}

fn send(paths: &RuntimePaths, lane: CanonicalUuid, kick: &Kick) {
    let _sent = send_to(&kick_path(paths, lane), kick);
}

fn send_to(path: &Path, message: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(message).expect("a door message is representable as JSON");
    door::send(path, &bytes)
}

/// One-shot reply door name, unique to `source` and `request`.
fn reply_name(source: &Path, kind: char, request: &str) -> String {
    let mut identity = blake3::Hasher::new();
    identity.update(source.as_os_str().as_encoded_bytes());
    identity.update(request.as_bytes());
    format!("spice-{kind}-{}.sock", identity.finalize())
}

/// Bind the one-shot door at `reply`, send `door` the kick built around it,
/// and decode the first answer arriving within `timeout`.
fn call<T: serde::de::DeserializeOwned>(
    door: &Path,
    reply: PathBuf,
    timeout: std::time::Duration,
    kick: impl FnOnce(PathBuf) -> Kick,
) -> io::Result<T> {
    let callback = door::Door::bind(&reply, CALLBACK_REPLY_BYTES)?;
    send_to(door, &kick(reply))?;
    let bytes = callback.recv(Some(timeout))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Bind this lane's kick door and let every kick reach the driver.
///
/// A stale door from an earlier supervisor is replaced. The listening thread
/// lives as long as the supervisor process; the door closes once the child
/// has exited. A message that says nothing this door understands is dropped,
/// since the sender never waits for its effect.
///
/// # Errors
///
/// Returns the bind error when the door cannot be opened.
pub(in crate::resident) fn listen_for_kicks(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    driver: Arc<Driver>,
) -> io::Result<KickDoor> {
    let door = Arc::new(door::Door::bind(&kick_path(paths, lane), KICK_BYTES)?);
    let listening = Arc::clone(&door);
    std::thread::Builder::new()
        .name("spice-kick".to_owned())
        .spawn(move || {
            driver.catch_up();
            while let Ok(kick) = listening.recv(None) {
                match serde_json::from_slice(&kick) {
                    Ok(Kick::Inbox) => driver.deliver_inbox(),
                    Ok(Kick::RoomCatchUp) => driver.deliver_room_catch_up(false),
                    Ok(Kick::Answer { request, confirmed }) => {
                        driver.answer(request, confirmed);
                    }
                    Ok(Kick::Narration(prod)) if prod.occupant == occupant => {
                        driver.deliver_narration(&prod);
                    }
                    Ok(Kick::Narration(_)) => {}
                    Ok(
                        kick @ (Kick::TaskGeneration(_)
                        | Kick::TaskContract(_)
                        | Kick::TaskAssignment(_)
                        | Kick::TaskRoundUpdate(_)
                        | Kick::TaskAssignmentEnded(_)
                        | Kick::ClaimLapse(_)
                        | Kick::TasteHold(_)),
                    ) => driver.deliver_task_notice(occupant, kick),
                    Ok(Kick::BackgroundConfirmed {
                        notice,
                        owner,
                        request,
                        reply,
                    }) if notice.occupant == occupant => {
                        let exact = owner
                            .as_ref()
                            .is_none_or(|owner| driver.idle_claim_matches(owner));
                        let accepted = exact && background_accepted(&driver, occupant, &notice);
                        confirmed_background::answer(&reply, request, accepted);
                    }
                    Ok(Kick::BackgroundConfirmed { request, reply, .. }) => {
                        confirmed_background::answer(&reply, request, false);
                    }
                    Ok(Kick::MessageRefused(notice)) if notice.occupant == occupant => {
                        driver.deliver_message_refusal(&notice);
                    }
                    Ok(Kick::MessageRefused(_)) => {}
                    Ok(Kick::MessageRecorded(notice)) if notice.occupant == occupant => {
                        driver.deliver_message_recorded(&notice);
                    }
                    Ok(Kick::MessageRecorded(_)) => {}
                    Ok(Kick::LineOutcome(notice)) if notice.occupant == occupant => {
                        driver.deliver_line_outcome(&notice);
                    }
                    Ok(Kick::LineOutcome(_)) => {}
                    Ok(Kick::Renewal(notice)) if notice.occupant == occupant => {
                        driver.deliver_renewal(notice);
                    }
                    Ok(Kick::Renewal(_)) => {}
                    Ok(Kick::Retune {
                        occupant: target,
                        request,
                        retune,
                        reply,
                    }) if target == occupant => driver.retune(request, retune, reply),
                    Ok(Kick::Retune { .. }) => {}
                    Ok(Kick::ResolveModel {
                        occupant: target,
                        spelling,
                        reply,
                    }) if target == occupant => driver.resolve_model(&spelling, &reply),
                    Ok(Kick::ResolveModel { reply, .. }) => {
                        let _sent = send_to(
                            &reply,
                            &ModelResolutionReply {
                                result: ModelResolution::Refused {
                                    detail: "the lane now runs another occupant".to_owned(),
                                },
                            },
                        );
                    }
                    Ok(Kick::ReconcileClaim) => driver.reconcile_claim(),
                    Ok(Kick::ClaimActivation { owner, admitted }) => {
                        if driver.claim_activation(owner, admitted) {
                            driver.catch_up();
                        }
                    }
                    Ok(Kick::Abort) => driver.abort(),
                    Err(_) => {}
                }
            }
        })?;
    Ok(KickDoor { door })
}

/// The bound kick door, closed when the supervisor is done with it.
pub(in crate::resident) struct KickDoor {
    door: Arc<door::Door>,
}

impl Drop for KickDoor {
    fn drop(&mut self) {
        self.door.close();
    }
}

#[cfg(test)]
mod admission_tests;
mod answered;
mod confirmed_background;
pub(in crate::resident::supervisor) mod current;
pub(in crate::resident::supervisor) mod inbox;
mod live_retune;
#[cfg(test)]
mod match_tests;
use live_retune::PendingRetune;
#[cfg(test)]
use live_retune::RetuneReply;
pub(in crate::resident) use live_retune::{LiveRetune, LiveRetuneResult, retune};
mod model;
use model::ModelResolutionReply;
pub(in crate::resident) use model::{ModelResolution, resolve_model};
mod registration;
mod retention;
pub(in crate::resident) use registration::FirstTurn;
pub(in crate::resident) mod renewal;
mod task_notices;
pub(in crate::resident) use task_notices::{
    ClaimLapseNotice, TaskAssignmentEnd, TaskAssignmentNotice, TaskContractNotice,
    TaskGenerationNotice, TaskRoundUpdateKind, TaskRoundUpdateNotice, TasteHoldNotice, TasteState,
    claim_lapse, task_assignment, task_assignment_ended, task_contract, task_generation,
    task_round_update, taste_hold,
};
mod quiet;
mod steering;
#[cfg(test)]
mod steering_tests;
#[cfg(test)]
mod tests;
mod uri;
mod waits;
mod work;
