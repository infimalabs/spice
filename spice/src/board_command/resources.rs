//! Claim-lifetime possession of configured resource-pool shards.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use board::Entry;
use sill::{HeldShard, LockAttempt, LockAvailability, LockTopology};
use wire::intent::CommandAnswer;
use wire::{Actor, Environment, Outcome, TaskStamp};

use super::{environment_fault, refusal, store_fault};

/// Resident-owned shards retained for as long as their task claim is active.
pub(crate) struct ResourceClaims {
    topology: LockTopology,
    held: Mutex<BTreeMap<TaskStamp, Vec<HeldShard>>>,
}

impl ResourceClaims {
    pub(crate) fn new(topology: LockTopology) -> Self {
        Self {
            topology,
            held: Mutex::new(BTreeMap::new()),
        }
    }

    /// Reconstruct claim-held shards before the command surface accepts work.
    pub(crate) fn restore(
        &self,
        bus: &bus::Bus,
        repository: Option<&spice::repository::Repository>,
    ) -> Result<(), Outcome<CommandAnswer>> {
        if self.topology.is_empty() {
            return Ok(());
        }
        let observed = bus
            .task_snapshot()
            .map_err(|error| store_fault("board::heads-unreadable", error.to_string()))?;
        let residency = observed.residency().ok_or_else(|| {
            store_fault(
                "board::residency-missing",
                "resource restoration requires current residency",
            )
        })?;
        let active: BTreeSet<_> = residency
            .body()
            .lanes
            .iter()
            .filter_map(|lane| {
                let actor = Actor::Lane(lane.lane);
                let task = observed.facts().latest_claim_task(&actor)?;
                observed
                    .task_heads(task)?
                    .active_claim_of(&actor)
                    .map(|_| task.clone())
            })
            .collect();
        if active.is_empty() {
            self.reconcile(&[]);
            return Ok(());
        }
        let snapshot = super::read_guard::validated_snapshot_for_selected_tasks(
            &observed,
            repository,
            Actor::Spice,
            &active,
        )?;
        self.reconcile(&snapshot.entries);
        Ok(())
    }

    /// Reconcile retained possessions, then expose every configured pool's
    /// current availability to one allocation decision.
    pub(crate) fn observe(
        &self,
        base: Environment,
        entries: &[Entry],
    ) -> Result<Environment, Outcome<CommandAnswer>> {
        self.reconcile(entries);
        let report = self.topology.report().map_err(resource_fault)?;
        let mut environment = base;
        for pool in report.pools {
            let available = pool
                .entries
                .iter()
                .filter(|entry| entry.availability == LockAvailability::Available)
                .count() as u32;
            let name: wire::ResourcePoolName =
                pool.name
                    .parse()
                    .map_err(|error: wire::ResourcePoolNameError| {
                        environment_fault("board::resource-pool-invalid", error.to_string())
                    })?;
            environment = environment.with_resource_pool(name, available);
        }
        Ok(environment)
    }

    /// Refuse an explicit declaration that names no configured pool.
    pub(crate) fn validate(
        &self,
        requirements: &wire::EnvironmentRequirements,
    ) -> Result<(), Outcome<CommandAnswer>> {
        let report = self.topology.report().map_err(resource_fault)?;
        for (name, _) in requirements.resource_pools() {
            if !report.pools.iter().any(|pool| pool.name == name.as_str()) {
                return Err(unconfigured_pool(name.as_str()));
            }
        }
        Ok(())
    }

    /// Reserve the selected task's requirement before its claim is published.
    pub(crate) fn reserve<'a>(
        &'a self,
        entry: &Entry,
        actor: &Actor,
        entries: &[Entry],
    ) -> Result<ResourceReservation<'a>, Outcome<CommandAnswer>> {
        self.reconcile(entries);
        if entry.statement().resources.is_empty() {
            return Ok(ResourceReservation::empty(self));
        }
        let task = entry.born().clone();
        let context = owner_context(actor, &task)?;
        let retained = self.held.lock().unwrap_or_else(|error| error.into_inner());
        if retained.contains_key(&task) {
            return Ok(ResourceReservation::transfer(self, task, context));
        }
        let shards = acquire_all(
            &self.topology,
            &entry.statement().resources,
            context.as_deref(),
        )?;
        Ok(ResourceReservation::acquired(self, task, shards))
    }

    fn reconcile(&self, entries: &[Entry]) {
        let active = entries
            .iter()
            .flat_map(|entry| {
                entry
                    .active_claims()
                    .map(move |held| (entry, &held.claim().actor))
            })
            .collect::<Vec<_>>();
        let active_tasks = active
            .iter()
            .map(|(entry, _)| entry.born().clone())
            .collect::<BTreeSet<_>>();
        let mut retained = self.held.lock().unwrap_or_else(|error| error.into_inner());
        retained.retain(|task, _| active_tasks.contains(task));
        for (entry, actor) in active {
            let task = entry.born();
            if retained.contains_key(task) || entry.statement().resources.is_empty() {
                continue;
            }
            let Ok(context) = owner_context(actor, task) else {
                continue;
            };
            if let Ok(shards) = acquire_all(
                &self.topology,
                &entry.statement().resources,
                context.as_deref(),
            ) {
                retained.insert(task.clone(), shards);
            }
        }
    }
}

fn acquire_all(
    topology: &LockTopology,
    resources: &wire::ResourceRequirements,
    context: Option<&str>,
) -> Result<Vec<HeldShard>, Outcome<CommandAnswer>> {
    let mut shards = Vec::new();
    for (name, count) in resources.iter() {
        shards.extend(acquire(topology, name.as_str(), count, context)?);
    }
    Ok(shards)
}

fn acquire(
    topology: &LockTopology,
    name: &str,
    count: u32,
    context: Option<&str>,
) -> Result<Vec<HeldShard>, Outcome<CommandAnswer>> {
    let mut shards = Vec::with_capacity(count as usize);
    for _ in 0..count {
        match topology.try_acquire(name, None).map_err(resource_fault)? {
            LockAttempt::Acquired(mut shard) => {
                shard
                    .set_context(context.map(ToOwned::to_owned))
                    .map_err(|error| {
                        environment_fault("board::resource-owner-unwritable", error.to_string())
                    })?;
                shards.push(shard);
            }
            LockAttempt::Contended { .. } => {
                return Err(refusal(
                    "board::resource-unavailable",
                    Some(name.to_owned()),
                    "another claim or process holds the required resource pool",
                ));
            }
        }
    }
    Ok(shards)
}

fn owner_context(
    actor: &Actor,
    task: &TaskStamp,
) -> Result<Option<String>, Outcome<CommandAnswer>> {
    let Actor::Lane(lane) = actor else {
        return Ok(None);
    };
    serde_json::to_string(&wire::ResourceClaimOwner {
        lane: *lane,
        task: task.clone(),
    })
    .map(Some)
    .map_err(|error| environment_fault("board::resource-owner-invalid", error.to_string()))
}

fn resource_fault(error: sill::LockSurfaceError) -> Outcome<CommandAnswer> {
    match error {
        sill::LockSurfaceError::UnknownName(name) => unconfigured_pool(&name),
        error => environment_fault("board::resource-pool-unavailable", error.to_string()),
    }
}

fn unconfigured_pool(name: &str) -> Outcome<CommandAnswer> {
    refusal(
        "board::resource-pool-unconfigured",
        Some(name.to_owned()),
        "configure the named lock pool before declaring it as a task resource",
    )
}

/// Shards acquired before the claim commit, transferred to resident retention
/// only after the journal accepts that claim.
pub(crate) struct ResourceReservation<'a> {
    owner: &'a ResourceClaims,
    task: Option<TaskStamp>,
    acquired: Vec<HeldShard>,
    transfer_context: Option<String>,
}

impl<'a> ResourceReservation<'a> {
    fn empty(owner: &'a ResourceClaims) -> Self {
        Self {
            owner,
            task: None,
            acquired: Vec::new(),
            transfer_context: None,
        }
    }

    fn acquired(owner: &'a ResourceClaims, task: TaskStamp, acquired: Vec<HeldShard>) -> Self {
        Self {
            owner,
            task: Some(task),
            acquired,
            transfer_context: None,
        }
    }

    fn transfer(owner: &'a ResourceClaims, task: TaskStamp, context: Option<String>) -> Self {
        Self {
            owner,
            task: Some(task),
            acquired: Vec::new(),
            transfer_context: context,
        }
    }

    pub(crate) fn commit(mut self) -> Result<(), Outcome<CommandAnswer>> {
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        let mut retained = self
            .owner
            .held
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(context) = self.transfer_context.take()
            && let Some(shards) = retained.get_mut(&task)
        {
            for shard in shards {
                shard.set_context(Some(context.clone())).map_err(|error| {
                    environment_fault("board::resource-owner-unwritable", error.to_string())
                })?;
            }
            return Ok(());
        }
        retained.insert(task, std::mem::take(&mut self.acquired));
        Ok(())
    }
}
