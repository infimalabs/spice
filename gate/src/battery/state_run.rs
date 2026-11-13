//! Shared durable-state transaction for one enclosing production gate run.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use super::BatteryError;
use crate::core::ActorCapture;
use crate::state::{
    BoundDecision, CurrentState, JudgedLatches, LatchObservation, RenameMap, StateLedger,
    StateReceipt, current_state_lock_path, current_state_path,
};
use crate::tree::git_directories;

/// Whether one accepted run observed all sticky keys or only a selected subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateRunScope {
    /// Preserve live sticky keys outside the accumulated observation set.
    Partial,
    /// Treat the accumulated observations as the complete current inventory.
    Exhaustive,
}

/// One shareable, single-publication state transaction.
///
/// Focused captures in an ordinary change gate clone this handle. Each capture
/// judges the same run-start ledger and contributes its exact observations;
/// only the enclosing owner may accept or reject the combined transaction.
#[derive(Clone, Debug)]
pub struct BatteryStateRun {
    inner: Arc<Mutex<StateRunInner>>,
}

#[derive(Debug)]
struct StateRunInner {
    current: Option<CurrentState>,
    now: f64,
    judged: Option<StateLedger>,
    live_paths: Option<Vec<String>>,
    renames: Option<RenameMap>,
    publishes_state: Option<bool>,
    observations: Vec<(LatchObservation, BoundDecision)>,
}

impl BatteryStateRun {
    /// Load the Git-common ledger and fix this run's wall clock.
    pub fn capture(repository: &Path) -> Result<Self, BatteryError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| BatteryError::new(format!("battery state clock: {error}")))?
            .as_secs_f64();
        Self::capture_at(repository, now)
    }

    /// Load with an explicit clock for deterministic production-path tests.
    pub fn capture_at(repository: &Path, now: f64) -> Result<Self, BatteryError> {
        let (_, common_git) = git_directories(repository).map_err(|error| {
            BatteryError::new(format!("cannot locate battery state authority: {error}"))
        })?;
        let current = CurrentState::load(
            current_state_path(&common_git),
            current_state_lock_path(&common_git),
        )
        .map_err(|error| BatteryError::new(format!("cannot load battery state: {error}")))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(StateRunInner {
                current: Some(current),
                now,
                judged: None,
                live_paths: None,
                renames: None,
                publishes_state: None,
                observations: Vec::new(),
            })),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, StateRunInner>, BatteryError> {
        self.inner
            .lock()
            .map_err(|_| BatteryError::new("battery state transaction lock was poisoned"))
    }

    /// Bind the first exact selected-path inventory and return its judged
    /// ledger with that ledger's latches read at the run clock.
    pub(super) fn bind(
        &self,
        live_paths: &[String],
        renames: &RenameMap,
        base_commit: Option<String>,
        publishes_state: bool,
        actor: &ActorCapture,
    ) -> Result<(StateLedger, JudgedLatches, ActorCapture), BatteryError> {
        let mut inner = self.lock()?;
        if let Some(judged) = &inner.judged {
            let expected = inner
                .live_paths
                .as_ref()
                .expect("a judged state run retains its path inventory");
            if expected != live_paths {
                return Err(BatteryError::new(
                    "focused captures in one gate state run selected different live path inventories",
                ));
            }
            if inner.renames.as_ref() != Some(renames) {
                return Err(BatteryError::new(
                    "focused captures in one gate state run selected different rename lineages",
                ));
            }
            if inner.publishes_state != Some(publishes_state) {
                return Err(BatteryError::new(
                    "focused captures in one gate state run selected different state publication authorities",
                ));
            }
            validate_bound_claims(judged, actor)?;
            return Ok((
                judged.clone(),
                JudgedLatches::new(judged, inner.now),
                actor_with_claims(actor, judged),
            ));
        }

        let now = inner.now;
        let current = inner
            .current
            .as_mut()
            .ok_or_else(|| BatteryError::new("battery state transaction is already terminal"))?;
        current
            .begin_run(renames, live_paths, now, base_commit)
            .map_err(|error| BatteryError::new(format!("cannot begin battery state: {error}")))?;
        let judged = current
            .working_ledger()
            .expect("begin_run exposes the judged ledger")
            .clone();
        validate_bound_claims(&judged, actor)?;
        inner.live_paths = Some(live_paths.to_vec());
        inner.renames = Some(renames.clone());
        inner.publishes_state = Some(publishes_state);
        inner.judged = Some(judged.clone());
        let latches = JudgedLatches::new(&judged, now);
        let bound_actor = actor_with_claims(actor, &judged);
        Ok((judged, latches, bound_actor))
    }

    /// Accumulate one capture's exact observation/decision pairs.
    pub(super) fn observe(
        &self,
        observations: &[LatchObservation],
        decisions: &[BoundDecision],
    ) -> Result<(), BatteryError> {
        if observations.len() != decisions.len() {
            return Err(BatteryError::new(format!(
                "battery state captured {} observations for {} bound decisions",
                observations.len(),
                decisions.len()
            )));
        }
        let mut inner = self.lock()?;
        if inner.current.is_none() {
            return Err(BatteryError::new(
                "battery state transaction is already terminal",
            ));
        }
        inner
            .observations
            .extend(observations.iter().cloned().zip(decisions.iter().cloned()));
        Ok(())
    }

    /// Finish the complete accumulated proposal, publishing only staged sources.
    ///
    /// Revision-selected proposals are deliberately reconciled to preserve the
    /// same validation path, then rejected before the durable compare-and-swap.
    pub fn accept(&self, scope: StateRunScope) -> Result<StateReceipt, BatteryError> {
        let mut inner = self.lock()?;
        let mut current = inner
            .current
            .take()
            .ok_or_else(|| BatteryError::new("battery state transaction is already terminal"))?;
        let mut observed = std::mem::take(&mut inner.observations);
        let publishes_state = inner.publishes_state.ok_or_else(|| {
            BatteryError::new("battery state run has not bound a publication authority")
        })?;
        drop(inner);

        observed.sort_by(|left, right| left.0.entry().cmp(right.0.entry()));
        for pair in observed.windows(2) {
            if pair[0].0.entry() == pair[1].0.entry() {
                return Err(BatteryError::new(format!(
                    "battery state repeated observation {:?}",
                    pair[0].0.entry()
                )));
            }
        }
        let observations = observed
            .iter()
            .map(|(observation, _)| observation.clone())
            .collect::<Vec<_>>();
        let reconciled = match scope {
            StateRunScope::Partial => current.reconcile_partial(observations),
            StateRunScope::Exhaustive => current.reconcile(observations),
        }
        .map_err(|error| BatteryError::new(format!("cannot reconcile battery state: {error}")))?;
        for (actual, (observation, expected)) in reconciled.iter().zip(&observed) {
            if actual.entry() != observation.entry() || actual.bound() != expected {
                return Err(BatteryError::new(
                    "battery state reconciliation disagreed with a captured bound decision",
                ));
            }
        }

        let mut claims = BTreeMap::new();
        for (observation, decision) in &observed {
            if decision.over_base() {
                claims
                    .entry(observation.actor_inputs().candidate_path().to_owned())
                    .or_insert_with(|| observation.actor_inputs().clone());
            }
        }
        for (path, actors) in claims {
            current.claim([path], &actors).map_err(|error| {
                BatteryError::new(format!("cannot claim battery state: {error}"))
            })?;
        }
        let pending = current
            .finish()
            .map_err(|error| BatteryError::new(format!("cannot finish battery state: {error}")))?;
        if publishes_state {
            pending.accept().map_err(BatteryError::state_publication)
        } else {
            Ok(pending.reject())
        }
    }

    /// Make rejection terminal without touching durable bytes.
    pub fn reject(&self) -> Result<(), BatteryError> {
        let mut inner = self.lock()?;
        inner.current.take();
        inner.observations.clear();
        Ok(())
    }
}

fn actor_with_claims(actor: &ActorCapture, state: &StateLedger) -> ActorCapture {
    match actor {
        ActorCapture::Acting { actor, .. } => ActorCapture::Acting {
            actor: actor.clone(),
            bound_claims: state.claims().to_vec(),
        },
        ActorCapture::Absent => ActorCapture::Absent,
    }
}

fn validate_bound_claims(state: &StateLedger, actor: &ActorCapture) -> Result<(), BatteryError> {
    let ActorCapture::Acting { bound_claims, .. } = actor else {
        return Ok(());
    };
    for claim in bound_claims {
        if !state.claims().contains(claim) {
            return Err(BatteryError::new(format!(
                "battery bound claim on {:?} held by {:?} is not one the state authority holds: \
                 the run clock only drops claims, so these are two different readings",
                claim.path(),
                claim.actor()
            )));
        }
    }
    Ok(())
}
