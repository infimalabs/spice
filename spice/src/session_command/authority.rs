use std::error::Error;
use std::fmt;

use board::Entry;
use journal::{JournalRead, JournalReader};
use spice::repository::Repository;
use wire::{Actor, ResidencyHead};

use super::model::{
    AuthorityUnknown, BoardGlanceView, HeldClaimView, HeldWorkView, LanePostureView, OwnAddress,
    PeerView, PostureView, RepositoryGlanceView, RetiredDirectionView,
};
use super::{Environment, capture, glance, source};
use crate::gate_command::LatchDebtView;

/// What currently binds this lane, read once from the journal.
pub(crate) struct RecallAuthorities {
    /// Exact repository capture selected for this lane session.
    pub(crate) capture: Option<capture::Authority>,
    /// Pending asks, or `None` when the steering ledger could not be read.
    pub(crate) asks: Option<Vec<PendingAsk>>,
    pub(crate) held_work: HeldWorkView,
    pub(crate) posture: Option<PostureView>,
    pub(crate) peers: Vec<PeerView>,
    pub(crate) directions: Vec<RetiredDirectionView>,
    pub(crate) board: Option<BoardGlanceView>,
    pub(crate) latch_debt: Option<LatchDebtView>,
    pub(crate) repository: Option<RepositoryGlanceView>,
    pub(crate) unknowns: Vec<AuthorityUnknown>,
}

/// One steering directive still awaiting this lane's acknowledgment.
pub(crate) struct PendingAsk {
    pub(crate) directive: String,
    pub(crate) key: String,
    pub(crate) stage: String,
    pub(crate) text: String,
    pub(crate) attachments: Vec<wire::AgentMediaReference>,
    pub(crate) authored: String,
    pub(crate) expires: String,
}

/// How the reader names the seat whose authority it reads: the live vendor
/// thread an agent's own tools carry, or the lane its supervisor drives.
#[derive(Clone, Copy)]
enum Seat<'a> {
    Thread(&'a str),
    Lane(wire::CanonicalUuid),
}

impl RecallAuthorities {
    fn empty() -> Self {
        Self {
            capture: None,
            asks: None,
            held_work: HeldWorkView {
                thread: None,
                lane: None,
                own_claim: None,
                own_claim_unread: Some("the claim probe did not run".to_owned()),
                peer_claims: None,
                renewed_from: None,
                address: None,
            },
            posture: None,
            peers: Vec::new(),
            directions: Vec::new(),
            board: None,
            latch_debt: None,
            repository: None,
            unknowns: Vec::new(),
        }
    }
}

pub(crate) fn observe(environment: &Environment) -> RecallAuthorities {
    let mut recall = RecallAuthorities::empty();

    let thread = live_thread(environment, &mut recall);
    recall.held_work.thread.clone_from(&thread);

    if let Some(thread) = thread.as_deref() {
        observe_current_authorities(Seat::Thread(thread), environment, &mut recall);
    } else {
        unavailable_probes(
            &mut recall,
            "no admitted live vendor thread identity is available",
        );
    }
    recall
}

/// Read one lane's authority by its identity rather than a live thread.
///
/// A supervisor knows its lane before the vendor declares a session, so the
/// thread it reports is whichever session the residency head records for the
/// lane: the predecessor's at a fresh start, the current one afterwards.
pub(crate) fn observe_lane(
    environment: &Environment,
    lane: wire::CanonicalUuid,
) -> RecallAuthorities {
    let mut recall = RecallAuthorities::empty();
    observe_current_authorities(Seat::Lane(lane), environment, &mut recall);
    recall
}

struct ObservedAuthorities {
    capture: Option<Result<Option<capture::Authority>, AuthorityReadError>>,
    lane: Option<wire::LaneResidency>,
    renewed_from: Option<String>,
    residency_error: Option<AuthorityReadError>,
    hold: Option<Result<Option<wire::body::LaneHeld>, AuthorityReadError>>,
    ledger: Option<Result<steer::Ledger, AuthorityReadError>>,
    own_claim: Option<Result<Option<HeldClaimView>, AuthorityReadError>>,
    peer_claims: Option<Result<usize, AuthorityReadError>>,
    directions: Option<Result<Vec<journal::RetiredDirection>, AuthorityReadError>>,
    board: Option<Result<BoardGlanceView, AuthorityReadError>>,
    team: Option<Result<TeamAddresses, AuthorityReadError>>,
}

/// This lane's team in residency order and the address each seat offers.
type TeamAddresses = (
    Vec<wire::LaneResidency>,
    std::collections::BTreeMap<wire::CanonicalUuid, wire::SeatAddress>,
);

fn observe_current_authorities(
    seat: Seat<'_>,
    environment: &Environment,
    recall: &mut RecallAuthorities,
) {
    let Some(current) = environment.cwd.clone() else {
        unknown(
            recall,
            "repository",
            "current-directory",
            &"the reading process has no working directory",
        );
        unavailable_probes(recall, "current directory is unavailable");
        return;
    };
    let repository = match Repository::discover(&current) {
        Ok(repository) => repository,
        Err(error) => {
            unknown(recall, "repository", "discover", &error);
            unavailable_probes(recall, "repository discovery failed");
            return;
        }
    };
    recall.posture = Some(PostureView {
        cwd: current.display().to_string(),
        worktree: repository.worktree_root().display().to_string(),
        head: std::fs::read_to_string(repository.git_dir().join("HEAD"))
            .ok()
            .map(|head| {
                let head = head.trim();
                head.strip_prefix("ref: ")
                    .map_or(head, |name| {
                        name.strip_prefix("refs/heads/").unwrap_or(name)
                    })
                    .to_owned()
            }),
        lane: None,
    });
    recall.repository = Some((environment.checkout)(
        repository.worktree_root(),
        &environment.ambient,
    ));
    match LatchDebtView::read(repository.common_dir()) {
        Ok(debt) => recall.latch_debt = Some(debt),
        Err(error) => unknown(recall, "gate", "latch-ledger", &error),
    }
    let state = match repository.admit_state() {
        Ok(state) => state,
        Err(error) => {
            unknown(recall, "repository", "admit-state", &error);
            unavailable_probes(recall, "repository state is unavailable");
            return;
        }
    };
    let reader = match JournalReader::open(state.journal()) {
        Ok(reader) => reader,
        Err(error) => {
            unknown(recall, "journal", "open-read-only", &error);
            unavailable_probes(recall, "read-only journal open failed");
            return;
        }
    };
    match reader.read_with(|read| read_authorities(read, seat)) {
        Ok(observed) => apply_observed(recall, seat, observed),
        Err(error) => {
            unknown(recall, "journal", "read-transaction", &error);
            unavailable_probes(recall, "journal read transaction failed");
        }
    }
}

#[derive(Debug)]
enum AuthorityReadError {
    ResidencyMissing,
    Journal(journal::GuardError),
    Steering(steer::directives::Refusal),
    Capture(String),
    Board(board::TaskError),
}

impl fmt::Display for AuthorityReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResidencyMissing => {
                formatter.write_str("journal snapshot has no current residency head")
            }
            Self::Journal(error) => write!(formatter, "journal heads are unavailable: {error}"),
            Self::Steering(error) => {
                write!(formatter, "indexed steering heads are invalid: {error}")
            }
            Self::Board(error) => write!(formatter, "board entry is invalid: {error}"),
            Self::Capture(error) => {
                write!(formatter, "captured session authority is invalid: {error}")
            }
        }
    }
}

impl Error for AuthorityReadError {}

fn read_design_research(
    read: &JournalRead<'_>,
    statement: &wire::TaskHead,
    workflow: &wire::WorkflowHead,
) -> Result<Option<String>, AuthorityReadError> {
    if !statement.intent.is_some_and(wire::TaskIntent::is_design) {
        return Ok(None);
    }
    let maturity = read
        .design_maturity(&statement.born)
        .map_err(AuthorityReadError::Journal)?;
    Ok(crate::board_command::design_research(
        statement,
        workflow,
        Some(&maturity.items),
    ))
}

/// Every current direct neighbor of `statement` in either dependency direction.
fn read_neighbors(
    read: &JournalRead<'_>,
    statement: &wire::TaskHead,
) -> Result<Vec<journal::TaskHeads>, AuthorityReadError> {
    let mut neighbors = Vec::new();
    let mut after = None;
    loop {
        let page = read
            .task_neighbor_head_page(
                &statement.born,
                &statement.dependencies,
                after.as_ref(),
                journal::FactReadBudget::RETAINED,
            )
            .map_err(AuthorityReadError::Journal)?;
        neighbors.extend(page.rows);
        match page.continuation {
            Some(cursor) => after = Some(cursor),
            None => return Ok(neighbors),
        }
    }
}

fn read_capture(
    read: &JournalRead<'_>,
    lane: &wire::LaneResidency,
) -> Option<Result<Option<capture::Authority>, AuthorityReadError>> {
    let session = lane.session.clone()?;
    let key = wire::TranscriptStreamKey::new(
        &session.lane,
        &session.vendor,
        wire::body::TASK_CREATION_CONTEXT_STREAM,
    );
    Some(
        read.transcript_source_head(&key)
            .map_err(AuthorityReadError::Journal)
            .and_then(|head| {
                head.map(journal::JournalHead::into_body)
                    .map(|head| capture::Authority::new(session, head))
                    .transpose()
                    .map_err(AuthorityReadError::Capture)
            }),
    )
}

fn read_authorities(
    read: &JournalRead<'_>,
    seat: Seat<'_>,
) -> Result<ObservedAuthorities, std::convert::Infallible> {
    let residency = read
        .residency_head()
        .map_err(AuthorityReadError::Journal)
        .and_then(|head| {
            head.map(journal::JournalHead::into_body)
                .ok_or(AuthorityReadError::ResidencyMissing)
        });
    let lane = residency
        .as_ref()
        .ok()
        .and_then(|residency| match seat {
            Seat::Thread(thread) => lane_for_thread(residency, thread),
            Seat::Lane(lane) => residency.lanes.iter().find(|row| row.lane == lane),
        })
        .cloned();
    let Some(lane) = lane else {
        return Ok(ObservedAuthorities {
            capture: None,
            lane: None,
            renewed_from: None,
            residency_error: residency.err(),
            ledger: None,
            hold: None,
            own_claim: None,
            peer_claims: None,
            directions: None,
            board: None,
            team: None,
        });
    };
    let renewed_from = residency
        .as_ref()
        .ok()
        .and_then(|residency| handoff_predecessor(&residency.renewals, lane.lane));
    let actor = Actor::Lane(lane.lane);
    let capture = read_capture(read, &lane);
    let hold = read
        .current_lane_hold(lane.lane)
        .map_err(AuthorityReadError::Journal);
    let ledger = crate::steering_read::pending(read, Some(lane.lane))
        .map_err(AuthorityReadError::Journal)
        .and_then(|heads| {
            steer::Ledger::from_authority_heads(heads.iter().map(|head| head.body()))
                .map_err(AuthorityReadError::Steering)
        });
    let own_claim = read
        .active_claim_for_actor(&actor)
        .map_err(AuthorityReadError::Journal)
        .and_then(|heads| {
            heads.map_or(Ok(None), |heads| {
                let statement = heads.statement().body();
                let neighbors = if crate::board_command::substantive_term(&statement.term) {
                    Vec::new()
                } else {
                    read_neighbors(read, statement)?
                };
                let contract = crate::board_command::completion_contract(
                    &statement.term,
                    &statement.born,
                    &statement.dependencies,
                    statement.intent,
                    neighbors.iter(),
                );
                let research = read_design_research(read, statement, heads.workflow().body())?;
                let bodies = heads.into_bodies();
                Entry::new(
                    bodies.statement,
                    bodies.workflow,
                    bodies.completion,
                    bodies.claims,
                )
                .map_err(AuthorityReadError::Board)
                .map(|entry| Some(held_claim(&entry, &actor, contract, research)))
            })
        });
    let peer_claims = read
        .active_claim_count_excluding(&actor)
        .map_err(AuthorityReadError::Journal);
    let directions = read
        .retired_directions(lane.lane, glance::DIRECTION_ROWS)
        .map_err(AuthorityReadError::Journal);
    let board = glance::board(read).map_err(AuthorityReadError::Journal);
    let team = residency
        .as_ref()
        .map(|residency| {
            residency
                .lanes
                .iter()
                .filter(|row| row.team == lane.team)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let team = bus::journal_seat_addresses(read, &team)
        .map(|addresses| (team, addresses))
        .map_err(AuthorityReadError::Journal);
    Ok(ObservedAuthorities {
        lane: Some(lane),
        capture,
        renewed_from,
        residency_error: residency.err(),
        ledger: Some(ledger),
        hold: Some(hold),
        own_claim: Some(own_claim),
        peer_claims: Some(peer_claims),
        directions: Some(directions),
        board: Some(board),
        team: Some(team),
    })
}

/// The predecessor thread a lane's handoff-pending renewal hands its next
/// fresh occupant; the mark settles to successor-started once one starts.
pub(super) fn handoff_predecessor(
    renewals: &[wire::LaneRenewalRecord],
    lane: wire::CanonicalUuid,
) -> Option<String> {
    renewals.iter().find_map(|record| match &record.state {
        wire::LaneRenewalState::HandoffPending { predecessor, .. } if record.lane == lane => {
            Some(predecessor.thread.clone())
        }
        _ => None,
    })
}

fn apply_observed(recall: &mut RecallAuthorities, seat: Seat<'_>, observed: ObservedAuthorities) {
    if let Some(error) = observed.residency_error {
        unknown(recall, "journal", "residency-head", &error);
        unavailable_probes(recall, "the current residency head is unavailable");
        return;
    }
    let Some(lane) = observed.lane else {
        let detail = match seat {
            Seat::Thread(thread) => format!("no current lane session matches live thread {thread}"),
            Seat::Lane(lane) => format!("the residency head records no lane {lane}"),
        };
        unavailable_probe(recall, "steering", "thread-lane", &detail);
        unavailable_probe(recall, "board", "thread-actor", &detail);
        unavailable_probe(recall, "board", "active-peer-count", &detail);
        recall.held_work.own_claim_unread = Some(detail);
        return;
    };
    if let Some(capture) = observed.capture {
        match capture {
            Ok(binding) => recall.capture = binding,
            Err(error) => unknown(recall, "journal", "lane-capture", &error),
        }
    }
    recall.held_work.lane = Some(lane.lane.to_string());
    recall.held_work.renewed_from = observed.renewed_from;
    if recall.held_work.thread.is_none() {
        recall.held_work.thread = lane
            .session
            .as_ref()
            .and_then(|session| source::canonical_thread(&session.identity));
    }
    let hold = observed.hold.and_then(|hold| match hold {
        Ok(Some(held)) => Some(wire::intent::LaneHold {
            evidence: held.evidence,
            until: held.until,
        }),
        Ok(None) => None,
        Err(error) => {
            unknown(recall, "journal", "current-lane-hold", &error);
            None
        }
    });
    if let Some(posture) = &mut recall.posture {
        posture.lane = Some(LanePostureView {
            worktree: lane.worktree.clone(),
            disposition: lane.disposition.clone(),
            life: lane.life.clone(),
            vendor: lane.vendor.to_string(),
            model: lane.model.clone(),
            hold,
        });
    }

    if let Some(ledger) = observed.ledger {
        match ledger {
            Ok(ledger) => {
                recall.asks = Some(
                    ledger
                        .pending(lane.lane)
                        .into_iter()
                        .map(|directive| PendingAsk {
                            directive: directive.directive().to_string(),
                            key: directive.key().to_string(),
                            stage: format!("{:?}", directive.stage()).to_ascii_lowercase(),
                            text: wire::media::without_media_payloads(
                                directive.text(),
                                directive.attachments(),
                            ),
                            attachments: directive.media_references().expect(
                                "the admitted steering ledger derives bounded media references",
                            ),
                            authored: directive
                                .published()
                                .map_or_else(String::new, |at| at.to_string()),
                            expires: directive.expires().to_string(),
                        })
                        .collect(),
                );
            }
            Err(error) => unknown(recall, "steering", "pending-thread-lane", &error),
        }
    }

    if let Some(own_claim) = observed.own_claim {
        match own_claim {
            Ok(entry) => {
                recall.held_work.own_claim = entry;
                recall.held_work.own_claim_unread = None;
            }
            Err(error) => {
                recall.held_work.own_claim_unread = Some(error.to_string());
                unknown(recall, "board", "exact-actor-claim", &error);
            }
        }
    }
    if let Some(peer_claims) = observed.peer_claims {
        match peer_claims {
            Ok(count) => recall.held_work.peer_claims = Some(count),
            Err(error) => unknown(recall, "board", "active-peer-count", &error),
        }
    }
    if let Some(directions) = observed.directions {
        match directions {
            Ok(retired) => recall.directions = glance::directions(retired),
            Err(error) => unknown(recall, "steering", "retired-direction", &error),
        }
    }
    if let Some(board) = observed.board {
        match board {
            Ok(board) => recall.board = Some(board),
            Err(error) => unknown(recall, "board", "glance", &error),
        }
    }
    if let Some(team) = observed.team {
        match team {
            Ok(team) => apply_team(recall, lane.lane, team),
            Err(error) => unknown(recall, "journal", "seat-addresses", &error),
        }
    }
}

/// This lane's own address, and every teammate with the address it offers.
fn apply_team(
    recall: &mut RecallAuthorities,
    own: wire::CanonicalUuid,
    (team, mut addresses): TeamAddresses,
) {
    recall.held_work.address = addresses.remove(&own).map(OwnAddress::Seat);
    recall.peers = team
        .into_iter()
        .filter_map(|row| {
            let address = addresses.remove(&row.lane)?;
            Some(PeerView {
                lane: row.lane.to_string(),
                name: row.name,
                vendor: row.vendor.to_string(),
                model: row.model,
                address,
            })
        })
        .collect();
}

fn unavailable_probes(recall: &mut RecallAuthorities, detail: &str) {
    unavailable_probe(recall, "steering", "pending-thread-lane", detail);
    unavailable_probe(recall, "board", "exact-actor-claim", detail);
    recall.held_work.own_claim_unread = Some(detail.to_owned());
    unavailable_probe(recall, "board", "active-peer-count", detail);
}

fn unavailable_probe(recall: &mut RecallAuthorities, authority: &str, probe: &str, detail: &str) {
    recall.unknowns.push(AuthorityUnknown {
        authority: authority.to_owned(),
        probe: probe.to_owned(),
        detail: detail.to_owned(),
    });
}

fn held_claim(
    entry: &Entry,
    actor: &wire::Actor,
    completion_contract: String,
    design_research: Option<String>,
) -> HeldClaimView {
    let statement = entry.statement();
    HeldClaimView {
        completion_contract,
        design_research,
        handle: entry.handle().to_string(),
        topic: wire::topic_label(statement.topic.path.as_ref()).to_owned(),
        phase: entry.workflow().phase.to_string(),
        status: entry.workflow().status.as_str().to_owned(),
        title: statement.title.clone(),
        pitch: statement.pitch.clone(),
        lease_expiry: entry
            .claim_of(actor)
            .and_then(board::TaskClaim::lease)
            .map(|lease| lease.expiry.to_string()),
        flow: entry
            .workflow()
            .flow
            .iter()
            .map(ToString::to_string)
            .collect(),
        description: statement
            .description
            .as_deref()
            .map(crate::read_page::collapse),
        term: {
            let items = wire::StatementItem::each(&statement.term).collect::<Vec<_>>();
            items
                .iter()
                .zip(&statement.term)
                .map(|(item, row)| {
                    format!(
                        "{} {}",
                        item.handle(&items),
                        crate::read_page::collapse(row)
                    )
                })
                .collect()
        },
    }
}

fn lane_for_thread<'a>(
    residency: &'a ResidencyHead,
    thread: &str,
) -> Option<&'a wire::LaneResidency> {
    residency.lanes.iter().find(|lane| {
        lane.session.as_ref().is_some_and(|session| {
            source::canonical_thread(&session.identity)
                .as_deref()
                .is_some_and(|identity| identity == thread)
        })
    })
}

fn live_thread(environment: &Environment, recall: &mut RecallAuthorities) -> Option<String> {
    let populated = environment
        .vendors
        .iter()
        .filter(|vendor| vendor.identity.is_some())
        .collect::<Vec<_>>();
    let vendor = if let Some(preferred) = environment.preferred_vendor.as_deref() {
        let preferred = preferred.to_string_lossy();
        let Some(vendor) = environment
            .vendors
            .iter()
            .find(|vendor| vendor.vendor.name() == preferred)
        else {
            unavailable_probe(
                recall,
                "environment",
                "live-thread-identity",
                &format!(
                    "SPICE_AGENT_VENDOR names unknown vendor {preferred:?}; transcript selectors cannot replace the admitted live vendor identity"
                ),
            );
            return None;
        };
        if vendor.identity.is_none() {
            unavailable_probe(
                recall,
                "environment",
                "live-thread-identity",
                &format!(
                    "SPICE_AGENT_VENDOR selects {} but {} is absent; another vendor identity cannot replace it",
                    vendor.vendor.name(),
                    vendor.vendor.thread_id_environment()
                ),
            );
            return None;
        }
        vendor
    } else {
        match populated.as_slice() {
            [vendor] => *vendor,
            [] => {
                unavailable_probe(
                    recall,
                    "environment",
                    "live-thread-identity",
                    "no live vendor thread identity is present; transcript selectors are evidence, not current-actor credentials",
                );
                return None;
            }
            vendors => {
                unavailable_probe(
                    recall,
                    "environment",
                    "live-thread-identity",
                    &format!(
                        "multiple live vendor identities are present without SPICE_AGENT_VENDOR: {}; current authority remains unknown",
                        vendors
                            .iter()
                            .map(|vendor| vendor.vendor.thread_id_environment())
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                );
                return None;
            }
        }
    };
    let raw = vendor
        .identity
        .as_deref()
        .expect("the selected vendor environment carries an identity")
        .to_string_lossy();
    match source::canonical_thread(&raw) {
        Some(thread) => Some(thread),
        None => {
            unavailable_probe(
                recall,
                "environment",
                "live-thread-identity",
                &format!(
                    "{} contains malformed live vendor identity {raw:?}; transcript selectors cannot replace it",
                    vendor.vendor.thread_id_environment()
                ),
            );
            None
        }
    }
}

fn unknown(
    recall: &mut RecallAuthorities,
    authority: &str,
    probe: &str,
    error: &(dyn fmt::Display + '_),
) {
    recall.unknowns.push(AuthorityUnknown {
        authority: authority.to_owned(),
        probe: probe.to_owned(),
        detail: error.to_string(),
    });
}
