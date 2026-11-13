//! Keep a lane's held claim parked exactly while its worktree needs an
//! ungranted executable configuration.
//!
//! The configured gate checks trust only when the worktree's configuration
//! declares pre-commit commands, and a lane can neither pend nor grant. So
//! when an occupant's turn ends while its lane holds a live claim, the resident
//! reads that worktree's current trust-gated digest. An ungranted digest is
//! pended for the operator when it has no trust head, becomes the held task's
//! trust requirement, and ices the live lease, exactly as `task require
//! --trust-grant` records them; the wake projection then leaves the claim
//! alone until the grant lands. Once the worktree no longer needs an ungranted
//! digest, for example because the lane backed its change out, a requirement
//! naming a digest this worktree pended is dropped, and the indefinite lease
//! thaws on the claim's next wake.

use super::automatic::{HeldClaimAuthority, guard_held_claim, held_claim_authority};
use super::*;
use journal::{GuardError, JournalHead};
use wire::{Digest, EnvironmentRequirement, EnvironmentRequirements, TrustHead, TrustState};

/// Reconcile the held claim of `lane` on `worktree` with that worktree's
/// current executable-configuration surface.
///
/// Configuration is acquired only once `lane` holds a live claim. The exact
/// capture is revalidated at journal admission, so a concurrent edit cannot
/// park the task on stale executable bytes.
pub(in crate::resident) fn reconcile_trust_park(
    bus: &Bus,
    lane: CanonicalUuid,
    worktree: &str,
    at: wire::Instant,
) -> CommandResult<Option<bus::JournalCommit>> {
    reconcile_observed(
        bus,
        lane,
        worktree,
        || {
            let repository = Repository::discover(worktree)
                .map_err(|error| store_fault("lane::worktree-unresolved", &error.to_string()))?;
            let loaded = spice::config::capture_repository(&repository, &[])
                .map_err(|error| store_fault("trust::surface-unreadable", &error.to_string()))?;
            let gated = !loaded.resolved().root().policy().pre_commit().is_empty();
            let surface = crate::trust_command::Surface::from_loaded(&loaded);
            let need = gated.then(|| surface.digest().clone());
            Ok((
                need,
                Some(SurfaceObservation {
                    repository,
                    surface,
                }),
            ))
        },
        at,
    )
}

#[cfg(test)]
pub(in crate::resident) fn reconcile_trust_park_with_need(
    bus: &Bus,
    lane: CanonicalUuid,
    worktree: &str,
    need: Option<&Digest>,
    at: wire::Instant,
) -> CommandResult<Option<bus::JournalCommit>> {
    reconcile_observed(bus, lane, worktree, || Ok((need.cloned(), None)), at)
}

#[cfg(test)]
pub(in crate::resident) fn reconcile_trust_park_with_surface(
    bus: &Bus,
    lane: CanonicalUuid,
    worktree: &str,
    repository: Repository,
    surface: crate::trust_command::Surface,
    gated: bool,
    at: wire::Instant,
) -> CommandResult<Option<bus::JournalCommit>> {
    reconcile_observed(
        bus,
        lane,
        worktree,
        || {
            let need = gated.then(|| surface.digest().clone());
            Ok((
                need,
                Some(SurfaceObservation {
                    repository,
                    surface,
                }),
            ))
        },
        at,
    )
}

fn reconcile_observed(
    bus: &Bus,
    lane: CanonicalUuid,
    worktree: &str,
    observe: impl FnOnce() -> CommandResult<(Option<Digest>, Option<SurfaceObservation>)>,
    at: wire::Instant,
) -> CommandResult<Option<bus::JournalCommit>> {
    let snapshot = bus
        .task_snapshot()
        .map_err(|error| store_fault("lifecycle::claim-heads-unreadable", &error.to_string()))?;
    let Some(authority) = held_claim_authority(bus, Some(&snapshot), lane)? else {
        return Ok(None);
    };
    if authority.lease.body().expiry <= at {
        return Ok(None);
    }
    let (need, surface) = observe()?;
    let requirements = &authority.statement.body().requirements;
    let required = trust_requirement(requirements);
    let Some(change) = decide(required, snapshot.trust(), worktree, need.as_ref()) else {
        return Ok(None);
    };
    let observed = TrustObservations::new(snapshot.trust(), required, need.as_ref());
    let previous = snapshot
        .facts()
        .latest_trust_digest(crate::trust_command::SURFACE_NAME)
        .cloned();
    let (next, pending) = match change {
        Change::Park(digest) => {
            let pending = observed
                .need
                .as_ref()
                .filter(|observation| observation.head.is_none())
                .map(|_| PendingTrust {
                    digest: digest.clone(),
                    previous,
                });
            (with_trust(requirements, Some(digest))?, pending)
        }
        Change::Drop => (with_trust(requirements, None)?, None),
    };
    rewrite(
        bus,
        &authority,
        worktree,
        next,
        RewriteObservation {
            pending,
            trust: observed,
            surface,
        },
        at,
    )
    .map(Some)
}

struct SurfaceObservation {
    repository: Repository,
    surface: crate::trust_command::Surface,
}

impl SurfaceObservation {
    fn guard(self, write: &mut JournalWrite) {
        write.guard(JournalGuard::new(move |_| {
            super::super::require_current_surface(&self.repository, &self.surface)
        }));
    }
}

/// How the held task's trust slot moves at a turn end.
#[derive(Debug, Eq, PartialEq)]
enum Change {
    /// Name the worktree's ungranted current digest.
    Park(Digest),
    /// Remove a digest this worktree pended and no longer needs.
    Drop,
}

/// The move from the task's `required` trust digest given the worktree's
/// current trust-gated digest `need`, or `None` when nothing should change.
///
/// Requirements hold one entry per environmental fact, so the trust slot names
/// one digest. An ungranted need replaces the worktree's own older digest or a
/// granted one, but never an ungranted digest another worktree pended, which
/// already parks the task on a grant this lane did not cause. A need that is
/// granted or absent drops only a digest this worktree pended.
fn decide(
    required: Option<&Digest>,
    trust: &[JournalHead<TrustHead>],
    worktree: &str,
    need: Option<&Digest>,
) -> Option<Change> {
    let head = |digest: &Digest| {
        trust
            .iter()
            .map(JournalHead::body)
            .find(|head| &head.digest == digest)
    };
    let ours = |digest: &Digest| {
        head(digest).is_some_and(|head| head.worktree.as_deref() == Some(worktree))
    };
    let granted =
        |digest: &Digest| head(digest).is_some_and(|head| head.state == TrustState::Granted);
    match need.filter(|digest| !granted(digest)) {
        Some(blocking) => {
            let foreign = required
                .is_some_and(|digest| digest != blocking && !granted(digest) && !ours(digest));
            (required != Some(blocking) && !foreign).then(|| Change::Park(blocking.clone()))
        }
        None => required
            .filter(|digest| Some(*digest) != need && ours(digest))
            .map(|_| Change::Drop),
    }
}

fn trust_requirement(requirements: &EnvironmentRequirements) -> Option<&Digest> {
    requirements
        .as_slice()
        .iter()
        .find_map(|requirement| match requirement {
            EnvironmentRequirement::TrustGrant { value } => Some(value),
            _ => None,
        })
}

/// `requirements` with its trust slot replaced by `trust`.
fn with_trust(
    requirements: &EnvironmentRequirements,
    trust: Option<Digest>,
) -> CommandResult<EnvironmentRequirements> {
    let mut next = requirements
        .as_slice()
        .iter()
        .filter(|requirement| !matches!(requirement, EnvironmentRequirement::TrustGrant { .. }))
        .cloned()
        .chain(trust.map(EnvironmentRequirement::trust_grant))
        .collect::<Vec<_>>();
    next.sort_by_key(EnvironmentRequirement::fact);
    EnvironmentRequirements::new(next)
        .map_err(|error| store_fault("task::trust-park-invalid", &error.to_string()))
}

#[derive(Clone)]
struct TrustObservation {
    digest: Digest,
    head: Option<JournalHead<TrustHead>>,
}

struct TrustObservations {
    required: Option<TrustObservation>,
    need: Option<TrustObservation>,
}

impl TrustObservations {
    fn new(
        trust: &[JournalHead<TrustHead>],
        required: Option<&Digest>,
        need: Option<&Digest>,
    ) -> Self {
        let observe = |digest: &Digest| TrustObservation {
            digest: digest.clone(),
            head: trust
                .iter()
                .find(|head| head.body().digest == *digest)
                .cloned(),
        };
        Self {
            required: required
                .filter(|required| Some(*required) != need)
                .map(observe),
            need: need.map(observe),
        }
    }

    fn guard(self, write: &mut JournalWrite) {
        write.guard(JournalGuard::new(move |read| {
            for observation in [self.required.as_ref(), self.need.as_ref()]
                .into_iter()
                .flatten()
            {
                if read.trust_head(&observation.digest)? != observation.head {
                    return Err(GuardError::refused("task::trust-park-standing-moved"));
                }
            }
            Ok(())
        }));
    }
}

struct PendingTrust {
    digest: Digest,
    previous: Option<Digest>,
}

struct RewriteObservation {
    pending: Option<PendingTrust>,
    trust: TrustObservations,
    surface: Option<SurfaceObservation>,
}

/// Record `requirements` on the held task, icing a finite lease when they
/// carry an unsatisfied trust grant. An absent trust digest is pended in the
/// same transaction, so no pending-only intermediate state can wake the lane.
fn rewrite(
    bus: &Bus,
    authority: &HeldClaimAuthority,
    worktree: &str,
    requirements: EnvironmentRequirements,
    observed: RewriteObservation,
    at: wire::Instant,
) -> CommandResult<bus::JournalCommit> {
    let lease = authority.lease.body();
    let iced = (trust_requirement(&requirements).is_some()
        && !spice::claim_lease::is_indefinite(lease))
    .then(|| spice::claim_lease::iced(lease));
    let origin = format!("task:{}", authority.task)
        .parse::<wire::Origin>()
        .map_err(|error| store_fault("task::origin-invalid", &error.to_string()))?;
    let mut write = JournalWrite::new(Actor::Spice, origin, at, Producer::CommandDoor);
    guard_held_claim(&mut write, authority);
    observed.trust.guard(&mut write);
    if let Some(surface) = observed.surface {
        surface.guard(&mut write);
    }
    if let Some(PendingTrust { digest, previous }) = observed.pending {
        let pended = write
            .fact(
                wire::body::TrustPended {
                    surface: crate::trust_command::SURFACE_NAME.to_owned(),
                    digest: digest.clone(),
                    worktree: Some(worktree.to_owned()),
                    previous,
                },
                FactCause::Root,
            )
            .map_err(|error| store_fault("trust::fact-invalid", &error.to_string()))?;
        let worktree = worktree.to_owned();
        write.trust_head(pended, move |_| TrustHead {
            digest,
            state: TrustState::Pended,
            worktree: Some(worktree),
        });
    }
    let revision = u32::try_from(authority.statement.position().get())
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    let edited = write
        .fact(
            wire::body::TaskEdited {
                intent: None,
                task: authority.task.clone(),
                revision,
                dependencies: None,
                flow: None,
            },
            FactCause::Root,
        )
        .map_err(|error| store_fault("task::trust-park-build", &error.to_string()))?;
    let statement = wire::TaskHead {
        requirements,
        ..authority.statement.body().clone()
    };
    write.task_head(edited, move |position| wire::TaskHead {
        revision: position,
        ..statement
    });
    if let Some(iced) = iced {
        write.lease_head(authority.task.clone(), edited, move |_| iced);
    }
    commit(bus, write)
}
