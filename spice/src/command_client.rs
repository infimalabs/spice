//! The native client's sole authenticated path to the current resident.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde::Serialize;
use sill::{Availability, LockFile, LockName};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::header::ORIGIN;
use tungstenite::{Bytes, Message, WebSocket};
use wire::intent::{CommandAnswer, InboxAnswer};
use wire::{
    Actor, CanonicalUuid, Command, CommandKind, Disposition, Fault, FaultClass, Outcome, Refusal,
    Reply, ReplyPosition, Response, Witness,
};

use crate::cli::{Contract, Guard, Principal, ReplyPositionLaw};
use spice::ambient::Ambient;

mod browser;
mod deadline_stream;
mod discovery;
mod fault;
mod mapping;
mod outbox;
pub(crate) use outbox::execute_outbox;
mod session;
mod steering;
mod typed;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
use browser::browser_command;
pub(crate) use browser::{open_app_result_with, open_app_with, system_browser};
#[cfg(test)]
use deadline_stream::IoFailure;
use deadline_stream::{DeadlineStream, IoDirection, remaining};
#[cfg(all(test, unix))]
use discovery::open_discovery;
use discovery::{
    DiscoveryObserver, DiscoveryRead, DiscoveryWait, resident_lock_path, selected_discovery_path,
};
use fault::{
    OpeningBound, capability_disclosed, delivered_unanswered, discovery_replaced,
    discovery_unavailable, environment_fault, environment_fault_value, opening_fault,
    protocol_fault, protocol_fault_value, transport_fault,
};
use session::{AuthenticatedSession, dialable_endpoint, open_session};
#[cfg(test)]
use session::{decode_opening, open_session_with};
pub(crate) use steering::execute_steering;
pub(crate) use typed::{Host, execute_typed};

/// Bound for an opening that proves a contract and sends no command, so no
/// registry row governs it.
const OPENING_BOUND: Duration = crate::cli::APP_ANSWER_BOUND;
const MAX_DISCOVERY_BYTES: u64 = 16 * 1024;

type ClientResult<T> = Result<T, Outcome<CommandAnswer>>;

/// One generated CLI selection with a landed command-envelope mapping.
pub(crate) enum SelectedCommand<'a> {
    /// The operator requests the current resident's bounded Dark transition.
    Down,
    /// Read the current resident's durable consent queue.
    ConsentRead,
    /// Read the tracked consent policy held by the current resident.
    ConsentPolicy,
    /// Terminally answer one request from an exact queue snapshot.
    ConsentDecide {
        request: CanonicalUuid,
        answer: wire::Answer,
        expected: Vec<Witness>,
    },
    /// Recognize one exact post-proof all-target manifest.
    ProofRecognize {
        manifest: &'a proof::PublishedManifest,
        head: wire::Seq,
    },
    /// Read the current resident-owned lane table.
    LaneRead,
    /// Read the current global fast-mode setting.
    GlobalSettingsRead,
    /// Replace the global fast-mode setting.
    GlobalSettingsSet {
        body: &'a wire::intent::GlobalSettingsSet,
    },
    /// Change a team's runtime switches or configuration binding.
    TeamConfigure {
        body: &'a wire::intent::TeamConfigure,
    },
    /// Seat one new lane and launch its first occupant.
    LaneSeat {
        body: &'a wire::intent::LaneSeat,
    },
    /// Drive one existing lane toward drain, wake, or removal.
    LaneState {
        body: &'a wire::intent::LaneState,
    },
    /// Pin one lane's model spelling and retune its supported running occupant.
    LaneRetune {
        body: &'a wire::intent::LaneRetune,
    },
    /// State the effort spelling one seated lane's next launch runs.
    LaneEffortRetune {
        body: &'a wire::intent::LaneEffortRetune,
    },
    /// State whether one seated lane's next launch runs in fast mode.
    LaneFastRetune {
        body: &'a wire::intent::LaneFastRetune,
    },
    /// Request or clear succession for one lane without starting it.
    LaneRenewal {
        body: &'a wire::intent::LaneRenewal,
    },
    /// Read one bounded continuity account.
    LaneRecall {
        body: &'a wire::intent::LaneRecall,
    },
    /// Resolve one quote target against durable authority.
    QuoteRead {
        body: &'a wire::intent::QuoteRead,
    },
    TopicRead {
        body: &'a wire::topic::TopicRead,
    },
    TopicEdit {
        body: &'a wire::topic::TopicEdit,
    },
    /// The operator appends one durable directive to a seated lane.
    SteerSend {
        lane: CanonicalUuid,
        message: &'a str,
    },
    /// Withdraw a live directive as operator or its task-invitation sender.
    SteerCancel {
        directive: CanonicalUuid,
    },
    /// The operator re-presents one live directive at the urgency now due.
    SteerResend {
        directive: CanonicalUuid,
    },
    /// The operator restores one exact lapsed or dead-lettered directive.
    SteerRecover {
        directive: CanonicalUuid,
    },
    /// The authenticated principal reads visible pending direction.
    SteerInbox {
        lane: Option<CanonicalUuid>,
    },
    /// The operator quarantines the exact current executable digest.
    TrustPend {
        digest: &'a wire::Digest,
        worktree: &'a str,
    },
    /// The operator approves one exact pended executable digest.
    TrustGrant {
        digest: &'a wire::Digest,
    },
    /// The operator rejects one exact pended digest or withdraws approval from
    /// one exact granted digest, witnessing the standing it read.
    TrustRevoke {
        digest: &'a wire::Digest,
        standing: wire::TrustState,
    },
}

#[derive(Clone, Copy)]
enum Connection {
    Operator,
    Lane(CanonicalUuid),
}

/// A lane launched from one Git-common authority is addressing another.
///
/// This is only a routing guard. It grants no cross-repository authority and
/// is acted on only after the addressed resident proves that it is live.
#[derive(Debug)]
struct ForeignRepository {
    source: PathBuf,
    addressed: PathBuf,
}

fn foreign_repository(
    operator_discovery: &Path,
    connection: Connection,
    ambient: &Ambient,
) -> Option<ForeignRepository> {
    if matches!(connection, Connection::Operator) {
        return None;
    }
    let wrapper = ambient.get(wire::SpiceVariable::WrapperSpice.as_str())?;
    foreign_repository_from_paths(connection, Path::new(wrapper), operator_discovery)
}

fn foreign_repository_from_paths(
    connection: Connection,
    wrapper: &Path,
    operator_discovery: &Path,
) -> Option<ForeignRepository> {
    if !matches!(connection, Connection::Lane(_)) {
        return None;
    }
    let source = wrapper
        .parent()
        .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some("bin"))?
        .parent()
        .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some(".spice-install"))?
        .parent()?;
    let addressed = operator_discovery
        .parent()
        .filter(|path| path.file_name().and_then(|name| name.to_str()) == Some("runtime"))?
        .parent()
        .filter(|path| {
            path.file_name().and_then(|name| name.to_str()) == Some(wire::STATE_DIRECTORY)
        })?
        .parent()?;
    if source == addressed {
        return None;
    }
    let source = fs::canonicalize(source).ok()?;
    let addressed = fs::canonicalize(addressed).ok()?;
    (source != addressed).then_some(ForeignRepository { source, addressed })
}

/// Submit one contract-bound command to the current repository resident.
///
/// `discovery_path` must be the app-server path returned by the caller's single
/// [`spice::repository::Repository::admit_state`] admission.
pub(crate) fn execute(
    discovery_path: &Path,
    contract: &Contract,
    selected: SelectedCommand<'_>,
    ambient: &Ambient,
) -> Outcome<CommandAnswer> {
    let mapped = MappedCommand::selected(contract, selected);
    let connection = match ambient_lane_identity(ambient) {
        Ok(Some(lane)) => Connection::Lane(lane),
        Ok(None) => Connection::Operator,
        Err(outcome) => return outcome,
    };
    execute_mapped(discovery_path, contract, connection, mapped, ambient)
}

/// Submit one contract-bound command through the operator connection even
/// while a lane identity is ambient, for roots that never seat lanes: the
/// ambient identity names a seat in the repository that launched the agent,
/// and honoring it against another root would select a lane discovery path
/// that root cannot publish.
pub(crate) fn execute_operator(
    discovery_path: &Path,
    contract: &Contract,
    selected: SelectedCommand<'_>,
    ambient: &Ambient,
) -> Outcome<CommandAnswer> {
    let mapped = MappedCommand::selected(contract, selected);
    execute_mapped(
        discovery_path,
        contract,
        Connection::Operator,
        mapped,
        ambient,
    )
}

fn execute_mapped(
    operator_discovery: &Path,
    contract: &Contract,
    connection: Connection,
    mapped: ClientResult<MappedCommand>,
    ambient: &Ambient,
) -> Outcome<CommandAnswer> {
    execute_mapped_with(
        operator_discovery,
        contract,
        connection,
        mapped,
        ambient,
        |_| {},
    )
}

fn execute_mapped_with(
    operator_discovery: &Path,
    contract: &Contract,
    connection: Connection,
    mapped: ClientResult<MappedCommand>,
    ambient: &Ambient,
    after_transient: impl FnMut(&Fault),
) -> Outcome<CommandAnswer> {
    let mapped = match mapped {
        Ok(mapped) => mapped,
        Err(outcome) => return outcome,
    };
    let foreign = foreign_repository(operator_discovery, connection, ambient);
    let discovery_path = selected_discovery_path(operator_discovery, connection);
    let bound = crate::cli::answer_bound(mapped.kind);
    execute_mapped_within(
        operator_discovery,
        &discovery_path,
        foreign.as_ref(),
        contract,
        mapped,
        bound,
        after_transient,
    )
}

/// Submit one mapped command and outlast one resident handoff inside `bound`.
fn execute_mapped_within(
    operator_discovery: &Path,
    discovery_path: &Path,
    foreign: Option<&ForeignRepository>,
    contract: &Contract,
    mapped: MappedCommand,
    bound: Duration,
    mut after_transient: impl FnMut(&Fault),
) -> Outcome<CommandAnswer> {
    let deadline = Instant::now() + bound;
    let resident_lock = resident_lock_path(operator_discovery);
    let route = SessionRoute {
        contract,
        operator_discovery,
        discovery_path,
        resident_lock: &resident_lock,
        foreign,
    };
    let window = Window { deadline, bound };
    let session = match acquire_session_until(&route, window, false, &mut after_transient) {
        Ok(session) => session,
        Err(outcome) => return outcome,
    };
    let dialed = session.discovery.value.epoch;
    let outcome = submit(contract, &mapped, session);
    let ending = if let Some(fault) = interrupted_submission(&outcome) {
        after_transient(fault);
        None
    } else if refused_admission(&outcome) && mapped.outlasts_restart() {
        // The dialed resident is ending and refused before it enqueued
        // anything, so nothing was committed and the same envelope may be
        // presented to its successor. This is the one moment a restart is
        // observable from a client: a resident that refuses admission is on
        // its way out, and whatever the lock says while it leaves is part of
        // the same handoff, not a resident that is gone for good.
        after_transient(&environment_fault_value(
            NOT_ADMITTING,
            format!("resident epoch {dialed} stopped admitting the command"),
        ));
        Some(dialed)
    } else {
        return outcome;
    };

    // The resident may have committed before its socket disappeared, or it
    // refused before enqueueing at all. Present the exact command identity
    // again: a replacement generation reconciles a durable receipt or admits
    // the envelope fresh, while moved authority is returned as its current
    // refusal. Both attempts share one client bound, and a second transport
    // interruption or refusal is terminal rather than an unbounded replay loop.
    let session = match acquire_session_until(&route, window, true, &mut after_transient) {
        Ok(session) => session,
        Err(outcome) => {
            return match ending {
                Some(epoch) => restart_unfinished(outcome, epoch, bound),
                None => outcome,
            };
        }
    };
    submit(contract, &mapped, session)
}

/// The refusal a resident sends while it is ending and no longer admits.
const NOT_ADMITTING: &str = "resident::not-admitting";

fn refused_admission(outcome: &Outcome<CommandAnswer>) -> bool {
    matches!(outcome, Outcome::Refusal(refusal) if refusal.reason() == NOT_ADMITTING)
}

/// Keep one answer for a restart the client witnessed and outlasted.
///
/// After the dialed resident refused admission, every reason the handoff
/// loop can name at its deadline describes a successor that never published:
/// discovery absent, a lock nobody holds, or a holder still publishing. Naming
/// any of those would send the caller to repair an environment that is merely
/// between residents. The refusal the resident actually gave stands, extended
/// with the epoch that ended, the bound the client waited, and the one action
/// left: run the command again once a resident is seated. Any other outcome
/// is a different defect and passes through unchanged.
fn restart_unfinished(
    outcome: Outcome<CommandAnswer>,
    epoch: CanonicalUuid,
    bound: Duration,
) -> Outcome<CommandAnswer> {
    match &outcome {
        Outcome::Fault(fault) if fault.reason == "serve::deadline-elapsed" => {}
        _ => return outcome,
    }
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            NOT_ADMITTING,
            Some("resident-state".to_owned()),
            Some("seated".to_owned()),
            Some("quiescing".to_owned()),
        )
        .with_detail(format!(
            "resident epoch {epoch} stopped admitting during this command and no successor published an authenticated surface within the {bound:?} client bound; run the command again once a resident is seated"
        )),
    ))
}

/// Prove the selected read contract through the resident's sole authenticated
/// opening path and return the generation that accepted it.
pub(crate) fn authenticate(
    discovery_path: &Path,
    contract: &Contract,
    ambient: &Ambient,
) -> ClientResult<CanonicalUuid> {
    authenticate_with(discovery_path, contract, ambient, |_| {})
}

fn authenticate_with(
    discovery_path: &Path,
    contract: &Contract,
    ambient: &Ambient,
    after_transient: impl FnMut(&Fault),
) -> ClientResult<CanonicalUuid> {
    if !contract.guards.contains(&Guard::Resident) || matches!(contract.principal, Principal::Local)
    {
        return Err(protocol_fault(
            "command::mapping-unsupported",
            "selected CLI contract has no authenticated resident read mapping",
        ));
    }
    let connection = ambient_lane_identity(ambient)?.map_or(Connection::Operator, Connection::Lane);
    let foreign = foreign_repository(discovery_path, connection, ambient);
    let selected = selected_discovery_path(discovery_path, connection);
    let resident_lock = resident_lock_path(discovery_path);
    let session = acquire_session_with(
        &SessionRoute {
            contract,
            operator_discovery: discovery_path,
            discovery_path: &selected,
            resident_lock: &resident_lock,
            foreign: foreign.as_ref(),
        },
        OPENING_BOUND,
        after_transient,
    )?;
    Ok(session.accepted.epoch)
}

struct MappedCommand {
    id: CanonicalUuid,
    kind: CommandKind,
    expected: Vec<Witness>,
    body: serde_json::Value,
}

struct InboxSnapshot {
    answer: InboxAnswer,
    position: wire::Seq,
}

fn read_inbox(
    operator_discovery: &Path,
    connection: Connection,
    lane: Option<CanonicalUuid>,
    ambient: &Ambient,
) -> Result<InboxSnapshot, Outcome<CommandAnswer>> {
    let contract = crate::cli::contract(&["inbox"])
        .expect("the generated CLI registry carries the inbox contract");
    let mapped = MappedCommand::new(
        contract,
        &["inbox"],
        CommandKind::SteerInbox,
        Vec::new(),
        &wire::intent::SteerInbox { lane },
    )?;
    let outcome = execute_mapped(
        operator_discovery,
        contract,
        connection,
        Ok(mapped),
        ambient,
    );
    match outcome {
        Outcome::Reply(Reply {
            position: ReplyPosition::Snapshot(position),
            body: CommandAnswer::Inbox(answer),
        }) => Ok(InboxSnapshot { answer, position }),
        Outcome::Reply(_) => Err(protocol_fault(
            "steer::inbox-answer-invalid",
            "resident inbox reply did not retain its snapshot position and body",
        )),
        Outcome::Refusal(refusal) => Err(Outcome::Refusal(refusal)),
        Outcome::Fault(fault) => Err(Outcome::Fault(fault)),
    }
}
pub(crate) fn read_lane_inbox(
    operator_discovery: &Path,
    lane: CanonicalUuid,
    ambient: &Ambient,
) -> Result<InboxAnswer, Outcome<CommandAnswer>> {
    read_inbox(operator_discovery, Connection::Lane(lane), None, ambient)
        .map(|snapshot| snapshot.answer)
}

/// Resolve one quote target over the lane's own authenticated connection.
pub(crate) fn read_lane_quote(
    operator_discovery: &Path,
    lane: CanonicalUuid,
    target: &str,
    ambient: &Ambient,
) -> Result<wire::QuoteResolution, Outcome<CommandAnswer>> {
    let contract = crate::cli::contract(&["quote"])
        .expect("the generated CLI registry carries the quote contract");
    let body = wire::intent::QuoteRead {
        target: target.to_owned(),
    };
    let mapped = MappedCommand::selected(contract, SelectedCommand::QuoteRead { body: &body });
    match execute_mapped(
        operator_discovery,
        contract,
        Connection::Lane(lane),
        mapped,
        ambient,
    ) {
        Outcome::Reply(Reply {
            body: CommandAnswer::Quote(resolution),
            ..
        }) => Ok(*resolution),
        Outcome::Reply(_) => Err(protocol_fault(
            "quote::answer-invalid",
            "resident quote reply did not carry a quote resolution",
        )),
        Outcome::Refusal(refusal) => Err(Outcome::Refusal(refusal)),
        Outcome::Fault(fault) => Err(Outcome::Fault(fault)),
    }
}

/// Where one session is acquired: the contract it opens, the discovery it
/// dials, and the repository route it must still hold.
struct SessionRoute<'a> {
    contract: &'a Contract,
    operator_discovery: &'a Path,
    discovery_path: &'a Path,
    resident_lock: &'a Path,
    foreign: Option<&'a ForeignRepository>,
}

/// The one client bound an acquisition spends, and the instant it ends.
#[derive(Clone, Copy)]
struct Window {
    deadline: Instant,
    bound: Duration,
}

/// Acquire one authenticated session across a resident publication handoff.
///
/// A synchronous re-read admits a replacement that has already published.
/// Otherwise the observer is armed and followed by another read, so an atomic
/// discovery rename cannot land between the failed read and the wait. Only
/// opening is retried; the command envelope remains untouched until this
/// function succeeds.
fn acquire_session_with(
    route: &SessionRoute<'_>,
    bound: Duration,
    mut after_transient: impl FnMut(&Fault),
) -> ClientResult<AuthenticatedSession> {
    let window = Window {
        deadline: Instant::now() + bound,
        bound,
    };
    acquire_session_until(route, window, false, &mut after_transient)
}

fn acquire_session_until(
    route: &SessionRoute<'_>,
    window: Window,
    recovering_submission: bool,
    after_transient: &mut impl FnMut(&Fault),
) -> ClientResult<AuthenticatedSession> {
    require_repository_route(route)?;
    let attempt = acquire_session_attempt(route, window);
    let Err(outcome) = attempt else {
        return attempt;
    };
    let fault = match refreshable_fault(&outcome, route.resident_lock, recovering_submission) {
        Ok(Some(fault)) => fault,
        Ok(None) => return Err(outcome),
        Err(fault) => return Err(Outcome::Fault(fault)),
    };
    after_transient(fault);

    // A replacement may already have published. Re-read before constructing
    // the observer so a present authenticated surface has no watcher latency.
    require_repository_route(route)?;
    let retry = acquire_session_attempt(route, window);
    let Err(outcome) = retry else {
        return retry;
    };
    match refreshable_fault(&outcome, route.resident_lock, recovering_submission) {
        Ok(Some(_)) => {}
        Ok(None) => return Err(outcome),
        Err(fault) => return Err(Outcome::Fault(fault)),
    }
    let Some(mut observer) = DiscoveryObserver::arm(route.resident_lock) else {
        return Err(outcome);
    };
    loop {
        // The first iteration closes the interval between the last read and
        // arming the observer; later iterations consume signalled changes.
        require_repository_route(route)?;
        let attempt = acquire_session_attempt(route, window);
        let Err(outcome) = attempt else {
            return attempt;
        };
        let fault = match refreshable_fault(&outcome, route.resident_lock, recovering_submission) {
            Ok(Some(fault)) => fault,
            Ok(None) => return Err(outcome),
            Err(fault) => return Err(Outcome::Fault(fault)),
        };
        match observer.wait(window.deadline) {
            DiscoveryWait::Changed => {}
            DiscoveryWait::Deadline => {
                return Err(acquisition_deadline(window.bound, &fault.reason));
            }
            DiscoveryWait::Failed => return Err(outcome),
        }
    }
}

/// Stop a lane from presenting its repository-bound identity to another
/// repository once that repository's own resident is provably serving.
///
/// An absent or changing operator record is not proof of a foreign resident:
/// the local resident may be between its lock acquisition and lane-surface
/// publication, so that state keeps the ordinary bounded wait.
fn require_repository_route(route: &SessionRoute<'_>) -> ClientResult<()> {
    let Some(foreign) = route.foreign else {
        return Ok(());
    };
    let Ok(discovery) = DiscoveryRead::open_fault(route.operator_discovery, route.resident_lock)
    else {
        return Ok(());
    };
    if discovery.require_live_publisher().is_err() {
        return Ok(());
    }
    Err(Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            "serve::foreign-repository".to_owned(),
            Some("repository-authority".to_owned()),
            Some(foreign.source.display().to_string()),
            Some(foreign.addressed.display().to_string()),
        )
        .with_detail(format!(
            "the resident serving repository authority {} does not own this lane; ask the \
             operator to run the command there or exchange an explicit document handoff",
            foreign.addressed.display()
        )),
    )))
}

fn acquire_session_attempt(
    route: &SessionRoute<'_>,
    window: Window,
) -> ClientResult<AuthenticatedSession> {
    DiscoveryRead::open(route.discovery_path, route.resident_lock).and_then(|discovery| {
        remaining(window.deadline)
            .map_err(|_| acquisition_deadline(window.bound, "opening the current discovery"))?;
        open_session(route.contract, discovery, window.deadline, window.bound)
    })
}

fn refreshable_fault<'a>(
    outcome: &'a Outcome<CommandAnswer>,
    resident_lock: &Path,
    recovering_submission: bool,
) -> Result<Option<&'a Fault>, Fault> {
    let Outcome::Fault(fault) = outcome else {
        return Ok(None);
    };
    match fault.reason.as_str() {
        "serve::publisher-replaced" => Ok(Some(fault)),
        "serve::discovery-unavailable" | "serve::discovery-replaced" => {
            if recovering_submission {
                return Ok(Some(fault));
            }
            match publisher_availability(resident_lock)? {
                Availability::Held => Ok(Some(fault)),
                Availability::Available => Err(environment_fault_value(
                    "serve::publisher-gone",
                    "no resident holds the runtime lock or publishes its discovery; run `spice up` from this repository to restore resident commands",
                )),
            }
        }
        "serve::publisher-gone"
        | "serve::connect-failed"
        | "serve::opening-failed"
        | "serve::send-failed"
        | "serve::receive-failed"
            if recovering_submission =>
        {
            Ok(Some(fault))
        }
        _ => Ok(None),
    }
}

fn interrupted_submission(outcome: &Outcome<CommandAnswer>) -> Option<&Fault> {
    let Outcome::Fault(fault) = outcome else {
        return None;
    };
    matches!(
        fault.reason.as_str(),
        "serve::send-failed" | "serve::receive-failed" | "serve::closed-without-answer"
    )
    .then_some(fault)
}

fn publisher_availability(resident_lock: &Path) -> Result<Availability, Fault> {
    let name = LockName::new("resident").expect("resident is a non-empty lock name");
    LockFile::new(name, resident_lock)
        .availability()
        .map_err(|error| environment_fault_value("serve::publisher-unknown", error.to_string()))
}

fn acquisition_deadline(bound: Duration, prior: &str) -> Outcome<CommandAnswer> {
    environment_fault(
        "serve::deadline-elapsed",
        format!(
            "resident did not publish an authenticated surface within the {bound:?} client bound after {prior}"
        ),
    )
}

fn submit(
    contract: &Contract,
    mapped: &MappedCommand,
    mut session: AuthenticatedSession,
) -> Outcome<CommandAnswer> {
    let command = mapped.envelope(session.discovery.value.epoch);
    if let Err(outcome) = send(&mut session.socket, &command) {
        return outcome;
    }
    let response = match receive(&mut session.socket) {
        Ok(response) => response,
        Err(outcome) => return delivered_unanswered(outcome, contract),
    };
    decode_response(
        &response,
        contract,
        mapped.id,
        mapped.kind,
        &session.accepted,
        session.discovery.value.capability,
    )
}

thread_local! {
    /// The lane this thread's commands authenticate as, set by [`as_lane`].
    static SEATED_LANE: std::cell::Cell<Option<CanonicalUuid>> = const { std::cell::Cell::new(None) };
}

/// Run `commands` with every command this thread submits authenticated over
/// `lane`'s own connection, exactly as a CLI run in that lane's shell would,
/// whatever the process environment names.
pub(crate) fn as_lane<T>(lane: CanonicalUuid, commands: impl FnOnce() -> T) -> T {
    struct Restore(Option<CanonicalUuid>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SEATED_LANE.set(self.0);
        }
    }
    let _restore = Restore(SEATED_LANE.replace(Some(lane)));
    commands()
}

fn ambient_lane_identity(ambient: &Ambient) -> ClientResult<Option<CanonicalUuid>> {
    if let Some(lane) = SEATED_LANE.get() {
        return Ok(Some(lane));
    }
    let Some(value) = ambient.get(steer::hook::LANE) else {
        return Ok(None);
    };
    let Some(value) = value.to_str() else {
        return Err(protocol_fault(
            "hello::lane-identity-invalid",
            format!("{} is not UTF-8", steer::hook::LANE),
        ));
    };
    value.parse().map(Some).map_err(|_| {
        protocol_fault(
            "hello::lane-identity-invalid",
            format!("{} is not one canonical lane UUID", steer::hook::LANE),
        )
    })
}

/// The lane this invocation runs as, when its environment names exactly one.
pub(crate) fn seated_lane(ambient: &Ambient) -> Option<CanonicalUuid> {
    ambient_lane_identity(ambient).ok().flatten()
}

fn decode_response(
    bytes: &[u8],
    contract: &Contract,
    id: CanonicalUuid,
    kind: CommandKind,
    accepted: &wire::Accepted,
    capability: CanonicalUuid,
) -> Outcome<CommandAnswer> {
    let response = match wire::decode::<Response<serde_json::Value>>(bytes) {
        Ok(response) => response,
        Err(_) => {
            return protocol_fault(
                "command::answer-undecodable",
                "resident returned no current command response envelope",
            );
        }
    };
    if discloses_capability(bytes, capability) {
        return capability_disclosed();
    }
    if !response.v.is_current() || response.id != id || response.epoch != accepted.epoch {
        return protocol_fault(
            "command::answer-mismatch",
            "resident response does not bind the current command identity and epoch",
        );
    }
    match response.outcome {
        Outcome::Reply(reply) => {
            if !position_matches(contract.reply_position, reply.position) {
                return protocol_fault(
                    "command::position-mismatch",
                    "resident reply position violates the selected CLI contract",
                );
            }
            let Ok(body) = kind.binding().answer.decode_value(reply.body) else {
                return answer_binding_fault(spice::version::product(), &accepted.release);
            };
            Outcome::Reply(Reply {
                position: reply.position,
                body,
            })
        }
        Outcome::Refusal(refusal) => Outcome::Refusal(refusal),
        Outcome::Fault(fault) => Outcome::Fault(fault),
    }
}

/// Refuse an answer outside this client's command registry, naming the
/// releases on both sides of the session and which one must move.
///
/// The foreign body is never interpreted: the session handshake already
/// proved which release the resident runs, so that is the whole diagnosis.
fn answer_binding_fault(
    client: &wire::manifest::ProductVersion,
    resident: &wire::manifest::ProductVersion,
) -> Outcome<CommandAnswer> {
    if client == resident {
        return protocol_fault(
            "command::answer-binding",
            "resident reply body violates the command registry binding",
        );
    }
    let boundary = if client.precedence(resident).is_lt() {
        format!("run the spice executable installed with release {resident}")
    } else {
        format!("restart the resident on release {client} with `spice down`, then `spice up`")
    };
    protocol_fault(
        "command::answer-binding",
        format!(
            "client release {client} cannot read an answer from resident release {resident}; {boundary}"
        ),
    )
}

fn position_matches(law: ReplyPositionLaw, position: ReplyPosition) -> bool {
    matches!(
        (law, position),
        (ReplyPositionLaw::Direct, ReplyPosition::Direct {})
            | (ReplyPositionLaw::Snapshot, ReplyPosition::Snapshot(_))
            | (ReplyPositionLaw::Committed, ReplyPosition::Committed(_))
            | (
                ReplyPositionLaw::SnapshotOrCommitted,
                ReplyPosition::Snapshot(_)
            )
            | (
                ReplyPositionLaw::SnapshotOrCommitted,
                ReplyPosition::Committed(_)
            )
            | (
                ReplyPositionLaw::DirectOrCommitted,
                ReplyPosition::Direct {}
            )
            | (
                ReplyPositionLaw::DirectOrCommitted,
                ReplyPosition::Committed(_)
            )
    )
}

fn principal_matches(principal: Principal, actor: &Actor) -> bool {
    match principal {
        Principal::Local => false,
        Principal::Principal => matches!(actor, Actor::Principal(_)),
        Principal::Lane => matches!(actor, Actor::Lane(_)),
        Principal::PrincipalOrLane => {
            matches!(actor, Actor::Lane(_) | Actor::Principal(_))
        }
    }
}

/// Whether one received frame carries the discovery capability.
///
/// The frame is searched as received: a board answer runs to megabytes, and
/// re-encoding it only to scan the copy doubled the cost of every reply.
fn discloses_capability(frame: &[u8], capability: CanonicalUuid) -> bool {
    let needle = capability.to_string();
    std::str::from_utf8(frame).is_ok_and(|text| text.contains(&needle))
}

fn send<T: Serialize>(socket: &mut WebSocket<DeadlineStream>, value: &T) -> ClientResult<()> {
    let bytes = wire::encode(value).map_err(|_| {
        protocol_fault(
            "serve::request-unencodable",
            "current client request did not retain its wire representation",
        )
    })?;
    socket
        .send(Message::Binary(bytes.into()))
        .map_err(|error| transport_fault(socket.get_ref(), error.to_string(), IoDirection::Write))
}

fn receive(socket: &mut WebSocket<DeadlineStream>) -> ClientResult<Bytes> {
    loop {
        let message = match socket.read() {
            Ok(message) => message,
            Err(error) => {
                return Err(transport_fault(
                    socket.get_ref(),
                    error.to_string(),
                    IoDirection::Read,
                ));
            }
        };
        match message {
            Message::Binary(_) | Message::Text(_) => return Ok(message.into_data()),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            Message::Close(_) => {
                return Err(protocol_fault(
                    "serve::closed-without-answer",
                    "resident closed the socket before answering",
                ));
            }
        }
    }
}

fn steering_refusal(
    reason: &str,
    observed: Option<String>,
    detail: &str,
) -> Outcome<CommandAnswer> {
    Outcome::Refusal(Refusal::bare(
        wire::RefusalDiagnostic::new(
            reason.to_owned(),
            Some("steering-inbox".to_owned()),
            Some("one live directive in the authenticated lane".to_owned()),
            observed,
        )
        .with_detail(detail),
    ))
}

#[cfg(test)]
mod tests;
