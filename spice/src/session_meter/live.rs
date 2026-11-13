//! Read-only live-board collection for the native command wrapper.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use journal::JournalReader;
use serde::{Deserialize, Serialize};
use wire::{Actor, ClaimDisposition, ResidentState, TaskHandle};

use super::{ClaimState, Decision, Meter, NarrationState, WorkingState};

const RUNTIME_DIRECTORY: &str = "runtime";
const METER_DIRECTORY: &str = "meter";
const METER_STATE: &str = "session-meter.json";
const OWNER_STATE: &str = "session-meter-owner.json";
const LOSS_STATE: &str = "session-meter-loss.json";
const CLAIM_RECONCILIATION_REFRESH: Duration = Duration::from_secs(1);

/// One read-only live meter bound to the current worktree and process identity.
#[derive(Clone, Debug)]
pub struct LiveMeter {
    journal: PathBuf,
    meter: Meter,
    owner_path: PathBuf,
    loss_path: PathBuf,
    actor: ProcessActor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProcessActor {
    vendor: String,
    identity: String,
}

#[derive(Clone, Debug)]
struct BoardObservation {
    state: WorkingState,
    current: Option<StoredOwner>,
    prior_active: Option<AuthorityKey>,
    claim_reconciliation_pending: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredOwner {
    vendor: String,
    identity: String,
    authority: AuthorityKey,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorityKey {
    handle: String,
    claim: u64,
    lane: String,
    occupant: String,
    epoch: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredLoss {
    owner: StoredOwner,
    observed: AuthorityKey,
}

impl LiveMeter {
    /// Discover one repository and bind the sole ambient vendor identity.
    ///
    /// Values must be captured by the binary rim before this call. Missing,
    /// non-Unicode, or ambiguous identities disable the meter.
    pub fn open(
        root: &Path,
        identities: Vec<(&'static crate::extension::Vendor, Option<String>)>,
    ) -> Result<Self, String> {
        let mut actors = identities.into_iter().filter_map(|(vendor, identity)| {
            identity
                .filter(|identity| !identity.trim().is_empty())
                .map(|identity| ProcessActor {
                    vendor: vendor.name().to_owned(),
                    identity,
                })
        });
        let actor = actors
            .next()
            .filter(|_| actors.next().is_none())
            .ok_or_else(|| "the current process does not prove one vendor identity".to_owned())?;
        let repository = crate::repository::Repository::discover(root)
            .map_err(|error| format!("working-state repository is unavailable: {error}"))?;
        let state = repository
            .admit_state()
            .map_err(|error| format!("working-state paths are unavailable: {error}"))?;
        let meter = state
            .worktree_runtime()
            .join(RUNTIME_DIRECTORY)
            .join(METER_DIRECTORY);
        fs::create_dir_all(&meter)
            .map_err(|error| format!("working-state directory is unavailable: {error}"))?;
        Ok(Self {
            journal: state.journal().to_path_buf(),
            meter: Meter::new(meter.join(METER_STATE)),
            owner_path: meter.join(OWNER_STATE),
            loss_path: meter.join(LOSS_STATE),
            actor,
        })
    }

    /// Collect and reduce one current board observation.
    ///
    /// Every error is intended to become silence at the wrapper boundary.
    pub fn evaluate(&self) -> Result<Decision, String> {
        let prior = read_json::<StoredOwner>(&self.owner_path)?;
        let reader = JournalReader::open(&self.journal)
            .map_err(|error| format!("working-state journal is unavailable: {error}"))?;
        let actor = self.actor.clone();
        let prior_authority = prior
            .as_ref()
            .filter(|owner| owner.vendor == actor.vendor && owner.identity == actor.identity)
            .map(|owner| owner.authority.clone());
        let observation = reader
            .read(move |read| collect(read, &actor, prior_authority.as_ref()))
            .map_err(|error| format!("working-state board is unavailable: {error}"))?;

        if let Some(current) = observation.current.as_ref() {
            write_json(&self.owner_path, current)?;
        }
        let loss = claim_loss(prior, &observation, &self.actor)
            .map(|loss| self.loss_announcement(loss))
            .transpose()?
            .flatten();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("working-state clock is unavailable: {error}"))?;
        let mut decision = self
            .meter
            .evaluate_time_sensitive(&observation.state, now)
            .map_err(|error| error.to_string())?;
        if loss.is_some() {
            decision.announcement = loss;
            decision.refresh_after = None;
        } else if observation.claim_reconciliation_pending {
            // A replacement resident deliberately publishes a non-seated
            // residency before its successor can atomically seat and rebound
            // live claims. Treat that interval as unfinished observation, not
            // as ownership movement, and let the wrapper's existing callback
            // worker re-read durable authority until it settles.
            decision.refresh_after = Some(CLAIM_RECONCILIATION_REFRESH);
        }
        Ok(decision)
    }

    fn loss_announcement(&self, loss: StoredLoss) -> Result<Option<String>, String> {
        if read_json::<StoredLoss>(&self.loss_path)?.as_ref() == Some(&loss) {
            return Ok(None);
        }
        write_json(&self.loss_path, &loss)?;
        Ok(Some(format!(
            "Claim moved: {} is no longer held by this process; run spice task status",
            loss.owner.authority.handle
        )))
    }
}

fn claim_loss(
    prior: Option<StoredOwner>,
    observation: &BoardObservation,
    actor: &ProcessActor,
) -> Option<StoredLoss> {
    let owner = prior?;
    let observed = observation.prior_active.clone()?;
    (owner.vendor == actor.vendor
        && owner.identity == actor.identity
        && observation.current.is_none()
        && !observation.claim_reconciliation_pending)
        .then_some(StoredLoss { owner, observed })
}

fn collect(
    read: &journal::JournalRead<'_>,
    actor: &ProcessActor,
    prior: Option<&AuthorityKey>,
) -> Result<BoardObservation, journal::GuardError> {
    let residency = read.residency_head()?;
    let prior_active = match prior.and_then(|prior| {
        let born = prior.handle.parse::<TaskHandle>().ok()?.born().ok()?;
        Some((prior, born))
    }) {
        Some((prior, born)) => read
            .task_heads_for(&born)?
            .as_ref()
            .and_then(|heads| active_authority(heads, prior)),
        None => None,
    };
    let Some(residency) = residency.filter(|head| head.body().state == ResidentState::Seated)
    else {
        return Ok(BoardObservation {
            state: WorkingState::default(),
            current: None,
            claim_reconciliation_pending: prior_active.is_some(),
            prior_active,
        });
    };
    let mut matching = residency.body().lanes.iter().filter(|lane| {
        lane.session.as_ref().is_some_and(|session| {
            session.vendor.to_string() == actor.vendor && session.identity == actor.identity
        })
    });
    let Some(lane) = matching.next().filter(|_| matching.next().is_none()) else {
        return Ok(BoardObservation {
            state: WorkingState::default(),
            current: None,
            prior_active,
            claim_reconciliation_pending: false,
        });
    };
    collect_lane(read, &residency, lane, actor, prior_active)
}

fn collect_lane(
    read: &journal::JournalRead<'_>,
    residency: &journal::JournalHead<wire::ResidencyHead>,
    lane: &wire::LaneResidency,
    actor: &ProcessActor,
    prior_active: Option<AuthorityKey>,
) -> Result<BoardObservation, journal::GuardError> {
    let pending_inbox_count = crate::steering_read::pending(read, Some(lane.lane))?.len();
    let mut subscriptions = residency
        .body()
        .subscriptions
        .iter()
        .filter(|subscription| subscription.team == lane.team);
    let drain_queue = subscriptions
        .next()
        .is_some_and(|subscription| subscription.drain)
        && subscriptions.next().is_none();
    let lane_actor = Actor::Lane(lane.lane);
    let heads = read.active_claim_for_actor(&lane_actor)?;
    let held = heads
        .as_ref()
        .and_then(|heads| Some((heads, heads.active_claim_of(&lane_actor)?)));
    let current = held.and_then(|(heads, held)| {
        current_owner(heads, held, residency.body().epoch, lane.occupant, actor)
    });
    let claim = held
        .and_then(|(heads, held)| {
            current.as_ref()?;
            Some((heads, held.lease()?.body()))
        })
        .map(|(heads, lease)| {
            let now = jiff::Timestamp::now();
            let remaining = duration_since(lease.expiry.get(), now);
            // The claim only reaches here once its epoch and occupant match the
            // lane's seated occupant, which is the resident's own renewal
            // eligibility; an iced lease is the one live claim it never rolls.
            let rolling = residency.body().state == wire::ResidentState::Seated
                && !crate::claim_lease::is_indefinite(lease);
            Ok(ClaimState {
                handle: heads
                    .statement()
                    .body()
                    .handle(heads.workflow().body().phase)
                    .to_string(),
                phase: heads.workflow().body().phase.to_string(),
                elapsed: duration_since(now, lease.start.get()),
                remaining: remaining.or(Some(Duration::ZERO)),
                rolling,
            })
        })
        .transpose()?;
    let narration = open_prod(read, lane)?;
    Ok(BoardObservation {
        state: WorkingState {
            pending_inbox_count,
            drain_queue,
            claim,
            last_taste: String::new(),
            narration,
        },
        current,
        prior_active,
        claim_reconciliation_pending: false,
    })
}

/// The lane's open prod: its newest `narration::prodded` row addressed to the
/// seated occupant with no `narration::cleared` row for that occupant after it.
///
/// Both kinds are read newest-first through one bounded page each; a prod
/// older than a page of other lanes' rows is treated as gone, which the next
/// prod on this lane corrects by itself.
fn open_prod(
    read: &journal::JournalRead<'_>,
    lane: &wire::LaneResidency,
) -> Result<Option<NarrationState>, journal::GuardError> {
    let Some(occupant) = lane.occupant else {
        return Ok(None);
    };
    let budget = journal::FactReadBudget::new(NARRATION_PAGE_ROWS, NARRATION_PAGE_BYTES)?;
    let prodded = read
        .recent_fact_page(wire::FactKind::NarrationProdded, None, budget)?
        .rows
        .into_iter()
        .find_map(|envelope| {
            let prodded = envelope.fact.read::<wire::body::NarrationProdded>()?;
            (prodded.lane == lane.lane && prodded.occupant == occupant)
                .then_some((envelope.seq, prodded))
        });
    let Some((seq, prodded)) = prodded else {
        return Ok(None);
    };
    let cleared = read
        .recent_fact_page(wire::FactKind::NarrationCleared, None, budget)?
        .rows
        .into_iter()
        .filter(|envelope| envelope.seq > seq)
        .any(|envelope| {
            envelope
                .fact
                .read::<wire::body::NarrationCleared>()
                .is_some_and(|cleared| cleared.lane == lane.lane && cleared.occupant == occupant)
        });
    Ok((!cleared).then_some(NarrationState {
        moments: prodded.moments,
        count: prodded.count,
    }))
}

const NARRATION_PAGE_ROWS: usize = 64;
const NARRATION_PAGE_BYTES: usize = 64 * 1024;

fn current_owner(
    heads: &journal::TaskHeads,
    held: &journal::ClaimHeads,
    residency_epoch: wire::CanonicalUuid,
    seated_occupant: Option<wire::CanonicalUuid>,
    actor: &ProcessActor,
) -> Option<StoredOwner> {
    let claim = held.claim();
    let lease = held.lease()?;
    if claim.body().disposition != ClaimDisposition::Active
        || claim.body().epoch != residency_epoch
        || lease.body().epoch != residency_epoch
        || seated_occupant != Some(claim.body().occupant)
    {
        return None;
    }
    Some(StoredOwner {
        vendor: actor.vendor.clone(),
        identity: actor.identity.clone(),
        authority: authority(heads, claim),
    })
}

/// The prior owner's claim, when it is still active on the lane it held.
fn active_authority(heads: &journal::TaskHeads, prior: &AuthorityKey) -> Option<AuthorityKey> {
    let candidate = heads
        .statement()
        .body()
        .handle(heads.workflow().body().phase);
    if candidate.to_string() != prior.handle {
        return None;
    }
    let held = heads.claims().iter().find(|held| {
        held.claim().body().is_active() && held.claim().body().lane.to_string() == prior.lane
    })?;
    held.lease()?;
    Some(authority(heads, held.claim()))
}

fn authority(
    heads: &journal::TaskHeads,
    claim: &journal::JournalHead<wire::ClaimHead>,
) -> AuthorityKey {
    AuthorityKey {
        handle: heads
            .statement()
            .body()
            .handle(heads.workflow().body().phase)
            .to_string(),
        claim: claim.position().get(),
        lane: claim.body().lane.to_string(),
        occupant: claim.body().occupant.to_string(),
        epoch: claim.body().epoch.to_string(),
    }
}

fn duration_since(later: jiff::Timestamp, earlier: jiff::Timestamp) -> Option<Duration> {
    (later >= earlier).then(|| later.duration_since(earlier).unsigned_abs())
}

fn read_json<T>(path: &Path) -> Result<Option<T>, String>
where
    T: for<'de> Deserialize<'de>,
{
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("working-state record is unreadable: {error}")),
    };
    Ok(serde_json::from_slice(&bytes).ok())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("working-state record is unencodable: {error}"))?;
    let destination = atomic::Destination::new(path)
        .map_err(|error| format!("working-state record is unpublished: {error}"))?;
    destination
        .publish(|staged| staged.write_all(&bytes))
        .map_err(|error| format!("working-state record is unpublished: {error}"))
}

#[cfg(test)]
mod tests;
