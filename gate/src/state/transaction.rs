use super::*;

#[derive(Clone, Debug)]
struct RunContext {
    now: f64,
    base_commit: Option<String>,
    live_paths: BTreeSet<String>,
    reconciled: bool,
    authority: StateLedger,
}

impl RunContext {
    /// The first-over-base origin of a latch this run creates: the commit the
    /// change was staged on, the run clock, and the acting identity.
    fn origin(&self, observation: &LatchObservation, bound: &BoundDecision) -> LatchOrigin {
        LatchOrigin {
            latch: observation.entry.clone(),
            commit: self.base_commit.clone(),
            at: self.now,
            actor: observation
                .actor_inputs
                .require_actor()
                .ok()
                .map(|actor| actor.value().to_owned()),
            measured: observation.measured,
            base_limit: bound.base_limit(),
        }
    }
}

/// One loaded current ledger with an in-memory run in progress.
#[derive(Clone, Debug)]
pub struct CurrentState {
    destination: PathBuf,
    lock: PathBuf,
    before: StateLedger,
    working: StateLedger,
    initially_absent: bool,
    run: Option<RunContext>,
}

/// Read the committed current ledger, empty when the destination does not
/// exist.
///
/// This is the read-only view: it begins no run and holds no lock, so it serves
/// readers that only report what the ledger says.
pub fn read_current_state(destination: &Path) -> Result<StateLedger, StateError> {
    read_ledger(destination).map(|(ledger, _)| ledger)
}

fn read_ledger(destination: &Path) -> Result<(StateLedger, bool), StateError> {
    Ok(match read_optional(destination, CURRENT_STATE_RELEASE)? {
        Some(bytes) => (decode_current(destination, &bytes)?, false),
        None => (
            StateLedger {
                version: STATE_LEDGER_VERSION,
                latches: Vec::new(),
                origins: Vec::new(),
                claims: Vec::new(),
            },
            true,
        ),
    })
}

impl CurrentState {
    /// Load current state, starting from an empty current ledger when the
    /// destination does not exist. `lock` serializes publication separately
    /// from the durable ledger.
    pub fn load(
        destination: impl Into<PathBuf>,
        lock: impl Into<PathBuf>,
    ) -> Result<Self, StateError> {
        let destination = destination.into();
        let (ledger, initially_absent) = read_ledger(&destination)?;
        Ok(Self {
            destination,
            lock: lock.into(),
            before: ledger.clone(),
            working: ledger,
            initially_absent,
            run: None,
        })
    }

    /// Logical ledger loaded at the start of this transaction.
    #[must_use]
    pub const fn before(&self) -> &StateLedger {
        &self.before
    }

    /// Current in-memory ledger after [`Self::begin_run`].
    ///
    /// Production acquisition reads this immediately after beginning the run;
    /// later reconciliation may update it before the proposal is finished.
    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn working_ledger(&self) -> Option<&StateLedger> {
        self.run.as_ref().map(|_| &self.working)
    }

    /// Begin one deterministic run, applying staged renames and pruning
    /// missing or expired state before any decisions are made.
    ///
    /// `base_commit` is the commit the staged change is built on; a latch this
    /// run creates records it as the commit its subject first went over base
    /// after.
    pub fn begin_run<I, P>(
        &mut self,
        renames: &RenameMap,
        live_paths: I,
        now: f64,
        base_commit: Option<String>,
    ) -> Result<(), StateError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
    {
        if self.run.is_some() {
            return Err(StateError::current(
                StateErrorKind::Malformed,
                &self.destination,
                "a state transaction may begin only one run",
                "finish or discard the current transaction before starting another",
            ));
        }
        if !now.is_finite() {
            return Err(StateError::current(
                StateErrorKind::Malformed,
                &self.destination,
                "run clock must be finite",
                "supply a finite Unix timestamp from the run clock",
            ));
        }
        let mut live = BTreeSet::new();
        for path in live_paths {
            let raw = path.as_ref();
            let normalized = normalize_repo_path(raw).map_err(|reason| {
                StateError::current(
                    StateErrorKind::Malformed,
                    &self.destination,
                    format!("invalid live path {raw:?}: {reason}"),
                    "provide canonical repository-relative live paths",
                )
            })?;
            live.insert(normalized);
        }

        let mut latches: BTreeSet<LatchEntry> = self.working.latches.iter().cloned().collect();
        let mut origins = origin_map(&self.working.origins);
        for entry in &self.working.latches {
            let renamed = renames.destination(entry.key.repo_path());
            if renamed != entry.key.repo_path() {
                let latch = LatchEntry {
                    kind: entry.kind,
                    key: entry.key.renamed(renamed),
                };
                if let Some(origin) = origin_of(&self.working.origins, entry) {
                    origins.entry(latch.clone()).or_insert_with(|| LatchOrigin {
                        latch: latch.clone(),
                        ..origin.clone()
                    });
                }
                latches.insert(latch);
            }
        }
        latches.retain(|entry| live.contains(entry.key.repo_path()));
        origins.retain(|latch, _| latches.contains(latch));

        let mut claims = Vec::new();
        let mut claim_keys = BTreeSet::new();
        for claim in &self.working.claims {
            if claim.expires_at <= now {
                continue;
            }
            let path = renames.destination(&claim.path).to_owned();
            if !live.contains(&path) {
                continue;
            }
            let key = (path.clone(), claim.actor.clone());
            if !claim_keys.insert(key) {
                return Err(StateError::current(
                    StateErrorKind::Collision,
                    &self.destination,
                    format!(
                        "staged rename collides for claim path {path:?} actor {:?}",
                        claim.actor
                    ),
                    "remove the duplicate claim or resolve the staged rename collision",
                ));
            }
            claims.push(PeerClaim {
                path,
                actor: claim.actor.clone(),
                created_at: claim.created_at,
                expires_at: claim.expires_at,
            });
        }
        sort_claims(&mut claims);
        self.working.latches = latches.into_iter().collect();
        self.working.origins = origins.into_values().collect();
        self.working.claims = claims;
        self.run = Some(RunContext {
            now,
            base_commit,
            live_paths: live,
            reconciled: false,
            authority: self.working.clone(),
        });
        Ok(())
    }

    /// Reconcile a complete current observation inventory.
    ///
    /// Entries omitted from this inventory are absent and therefore pruned.
    /// Current flex breaches are added; prior latches remain only while the
    /// enabled rule is over base and otherwise heal.
    pub fn reconcile(
        &mut self,
        observations: impl IntoIterator<Item = LatchObservation>,
    ) -> Result<Vec<StateDecision>, StateError> {
        self.reconcile_inner(observations, ReconciliationScope::Exhaustive)
    }

    /// Reconcile only the supplied observation inventory.
    ///
    /// Unlike [`Self::reconcile`], omitted live sticky keys are outside this
    /// run's jurisdiction and remain unchanged. Run-start liveness, rename,
    /// and expiry reconciliation still applies to the complete ledger.
    pub fn reconcile_partial(
        &mut self,
        observations: impl IntoIterator<Item = LatchObservation>,
    ) -> Result<Vec<StateDecision>, StateError> {
        self.reconcile_inner(observations, ReconciliationScope::Partial)
    }

    fn reconcile_inner(
        &mut self,
        observations: impl IntoIterator<Item = LatchObservation>,
        scope: ReconciliationScope,
    ) -> Result<Vec<StateDecision>, StateError> {
        let run = self.run.as_ref().ok_or_else(|| {
            StateError::current(
                StateErrorKind::Malformed,
                &self.destination,
                "state reconciliation requires begin_run",
                "begin the transaction with its fixed clock, renames, and live paths",
            )
        })?;
        if run.reconciled {
            return Err(StateError::current(
                StateErrorKind::Malformed,
                &self.destination,
                "state reconciliation may run exactly once per transaction",
                "start a new transaction for a different observation inventory",
            ));
        }
        let prior: BTreeSet<LatchEntry> = self.working.latches.iter().cloned().collect();
        let mut observations: Vec<_> = observations.into_iter().collect();
        observations.sort_by(|left, right| left.entry.cmp(&right.entry));
        for pair in observations.windows(2) {
            if pair[0].entry == pair[1].entry {
                return Err(StateError::current(
                    StateErrorKind::Collision,
                    &self.destination,
                    format!("duplicate current observation for {:?}", pair[0].entry),
                    "emit exactly one observation for each current sticky key",
                ));
            }
        }

        let mut updated = match scope {
            ReconciliationScope::Exhaustive => BTreeSet::new(),
            ReconciliationScope::Partial => prior.clone(),
        };
        let mut origins = origin_map(&self.working.origins);
        let judged = JudgedLatches::new(&run.authority, run.now);
        let mut decisions = Vec::with_capacity(observations.len());
        for observation in observations {
            if !run.live_paths.contains(observation.entry.key.repo_path()) {
                return Err(StateError::current(
                    StateErrorKind::Malformed,
                    &self.destination,
                    format!(
                        "observation path {:?} is absent from the live inventory",
                        observation.entry.key.repo_path()
                    ),
                    "include the path in the run inventory or omit its observation",
                ));
            }
            let durable_latched = prior.contains(&observation.entry);
            let latched = observation.prior_latched.unwrap_or(durable_latched);
            if durable_latched && !latched {
                return Err(StateError::current(
                    StateErrorKind::Malformed,
                    &self.destination,
                    format!(
                        "observation for {:?} ignored its durable prior latch",
                        observation.entry
                    ),
                    "judge each observation against the run ledger's durable latch inventory",
                ));
            }
            let bound = judged.attribute(
                &observation.entry,
                observation.bound.decide(observation.measured, latched),
            );
            match bound.latch_transition {
                LatchTransition::Latched => {
                    origins.insert(observation.entry.clone(), run.origin(&observation, &bound));
                    updated.insert(observation.entry.clone());
                }
                LatchTransition::Held => {
                    updated.insert(observation.entry.clone());
                }
                LatchTransition::Clear | LatchTransition::Healed => {
                    updated.remove(&observation.entry);
                }
            }
            decisions.push(StateDecision {
                entry: observation.entry,
                bound,
            });
        }
        origins.retain(|latch, _| updated.contains(latch));
        self.working.latches = updated.into_iter().collect();
        self.working.origins = origins.into_values().collect();
        self.run
            .as_mut()
            .expect("the run remains active through reconciliation")
            .reconciled = true;
        Ok(decisions)
    }

    /// Claim live flex paths for the required actor, preserving an existing
    /// same-actor creation time and redirecting when a peer owns the path.
    pub fn claim<I, P>(
        &mut self,
        paths: I,
        actors: &ActorInputs,
    ) -> Result<Vec<ClaimDecision>, StateError>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
    {
        let run = self.run.as_ref().ok_or_else(|| {
            StateError::current(
                StateErrorKind::Malformed,
                &self.destination,
                "peer claims require begin_run",
                "begin the transaction with its fixed clock and live inventory",
            )
        })?;
        let claimant = actors.require_actor().map_err(|error| {
            StateError::current(
                StateErrorKind::MissingActor,
                &self.destination,
                error.to_string(),
                "provide an active actor",
            )
        })?;
        let mut requested = BTreeSet::new();
        for path in paths {
            let raw = path.as_ref();
            let normalized = normalize_repo_path(raw).map_err(|reason| {
                StateError::current(
                    StateErrorKind::Malformed,
                    &self.destination,
                    format!("invalid claim path {raw:?}: {reason}"),
                    "claim canonical repository-relative paths",
                )
            })?;
            if !run.live_paths.contains(&normalized) {
                return Err(StateError::current(
                    StateErrorKind::Malformed,
                    &self.destination,
                    format!("claim path {normalized:?} is not live"),
                    "claim only paths in the fixed run inventory",
                ));
            }
            requested.insert(normalized);
        }

        let active = self.working.claims.clone();
        let mut updated = active.clone();
        let mut decisions = Vec::new();
        for path in requested {
            let owner = owning_claim(&active, &path);
            if let Some(owner) = owner
                && owner.actor != claimant.value()
            {
                decisions.push(ClaimDecision {
                    path,
                    claimant: claimant.clone(),
                    claim: owner.clone(),
                    status: ClaimStatus::PeerHeld,
                });
                continue;
            }
            let claim = PeerClaim {
                path: path.clone(),
                actor: claimant.value().to_owned(),
                created_at: owner.map_or(run.now, |claim| claim.created_at),
                expires_at: run.now + PEER_CLAIM_TTL_SECONDS as f64,
            };
            let status = if owner.is_some() {
                ClaimStatus::Owned
            } else {
                ClaimStatus::Claimed
            };
            updated.retain(|existing| existing.path != path || existing.actor != claimant.value());
            updated.push(claim.clone());
            decisions.push(ClaimDecision {
                path,
                claimant: claimant.clone(),
                claim,
                status,
            });
        }
        sort_claims(&mut updated);
        self.working.claims = updated;
        Ok(decisions)
    }

    /// Freeze the proposed after-ledger for acceptance or rejection.
    pub fn finish(self) -> Result<PendingState, StateError> {
        match self.run.as_ref() {
            None => {
                return Err(StateError::current(
                    StateErrorKind::Malformed,
                    &self.destination,
                    "cannot finish a state transaction before begin_run",
                    "begin and complete the run before finishing its state",
                ));
            }
            Some(run) if !run.reconciled => {
                return Err(StateError::current(
                    StateErrorKind::Malformed,
                    &self.destination,
                    "cannot finish before reconciling the complete observation inventory",
                    "replay the run and reconcile exactly once before finishing its state",
                ));
            }
            Some(_) => {}
        }
        Ok(PendingState {
            destination: self.destination,
            lock: self.lock,
            before: self.before,
            after: self.working,
            initially_absent: self.initially_absent,
            authority: self
                .run
                .expect("a finished transaction retains its run authority")
                .authority,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReconciliationScope {
    Exhaustive,
    Partial,
}

/// Claim outcome for one requested path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClaimStatus {
    Claimed,
    Owned,
    PeerHeld,
}

/// One claimant's deterministic disposition for a path.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimDecision {
    path: String,
    claimant: ActorResolution,
    claim: PeerClaim,
    status: ClaimStatus,
}

impl ClaimDecision {
    /// Requested canonical path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Required actor that requested ownership.
    #[must_use]
    pub const fn claimant(&self) -> &ActorResolution {
        &self.claimant
    }

    /// Effective owner claim.
    #[must_use]
    pub const fn claim(&self) -> &PeerClaim {
        &self.claim
    }

    /// Claimed, refreshed ownership, or peer-held redirection.
    #[must_use]
    pub const fn status(&self) -> ClaimStatus {
        self.status
    }
}

/// Frozen state mutation awaiting the run's acceptance decision.
#[derive(Clone, Debug)]
pub struct PendingState {
    destination: PathBuf,
    lock: PathBuf,
    before: StateLedger,
    after: StateLedger,
    initially_absent: bool,
    authority: StateLedger,
}

impl PendingState {
    /// Exact proposed before-ledger.
    #[must_use]
    pub const fn before(&self) -> &StateLedger {
        &self.before
    }

    /// Exact proposed after-ledger.
    #[must_use]
    pub const fn after(&self) -> &StateLedger {
        &self.after
    }

    /// Reject the run without touching the destination.
    #[must_use]
    pub fn reject(self) -> StateReceipt {
        StateReceipt::new(&self.before, &self.authority, &self.after, false)
    }

    /// Atomically publish the after-ledger at the sole accepted-run boundary.
    /// A concurrent ledger is composed under the same lock when its latch and
    /// claim keys are disjoint from this proposal, or when both sides only
    /// refresh the same claim for the same actor.
    ///
    /// The first accepted run always writes its current ledger, even when
    /// reconciliation did not otherwise move it.
    pub fn accept(self) -> Result<StateReceipt, StateError> {
        let parent = self.destination.parent().ok_or_else(|| {
            StateError::current(
                StateErrorKind::Persist,
                &self.destination,
                "current state destination has no parent directory",
                "select the fixed Git-common state path",
            )
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            StateError::current(
                StateErrorKind::Persist,
                &self.destination,
                format!("could not create current state directory: {error}"),
                "repair the Git-common state directory and retry the accepted run",
            )
        })?;
        let lock_parent = self.lock.parent().ok_or_else(|| {
            StateError::current(
                StateErrorKind::Persist,
                &self.lock,
                "current state lock has no parent directory",
                "select the fixed Git-common state lock path",
            )
        })?;
        fs::create_dir_all(lock_parent).map_err(|error| {
            StateError::current(
                StateErrorKind::Persist,
                &self.lock,
                format!("could not create current state lock directory: {error}"),
                "repair the Git-common runtime lock directory and retry the accepted run",
            )
        })?;
        let lock_name = LockName::new("gate-state")
            .expect("the fixed current state lock name is one normal component");
        let lock = LockFile::new(lock_name, self.lock);
        let _possession = lock.acquire().map_err(|error| {
            StateError::current(
                StateErrorKind::Persist,
                lock.path(),
                format!("could not acquire current state publication lock: {error}"),
                "repair the Git-common state lock and retry the accepted run",
            )
        })?;
        let observed = read_optional(&self.destination, CURRENT_STATE_RELEASE)?
            .map(|bytes| decode_current(&self.destination, &bytes))
            .transpose()?;
        let (published, changed) = match observed {
            None if self.initially_absent => (self.after.clone(), true),
            None => {
                return Err(StateError::current(
                    StateErrorKind::Stale,
                    &self.destination,
                    "current state disappeared after this run loaded it",
                    "restore or reload current state before retrying the run",
                ));
            }
            Some(current) if current.digest() == self.before.digest() => {
                let changed = current != self.after;
                (self.after.clone(), changed)
            }
            Some(current) => {
                let composed =
                    compose_disjoint(&self.destination, &self.before, &self.after, &current)?;
                let changed = current != composed;
                (composed, changed)
            }
        };
        let bytes = changed
            .then(|| published.encode())
            .transpose()
            .map_err(|error| {
                StateError::current(
                    StateErrorKind::Encode,
                    &self.destination,
                    format!("could not encode current state: {error}"),
                    "report the schema defect; do not replace the prior ledger",
                )
            })?;
        if let Some(bytes) = bytes {
            atomic::Destination::new(&self.destination)
                .map_err(|error| {
                    StateError::current(
                        StateErrorKind::Persist,
                        &self.destination,
                        format!("could not prepare atomic state publication: {error}"),
                        "repair the Git-common state directory and retry the accepted run",
                    )
                })?
                .publish(|staged| staged.write_all(&bytes))
                .map_err(|error| {
                    StateError::current(
                        StateErrorKind::Persist,
                        &self.destination,
                        format!("could not atomically publish current state: {error}"),
                        "repair the Git-common state directory and retry the accepted run",
                    )
                })?;
        }
        Ok(StateReceipt::new(
            &self.before,
            &self.authority,
            &published,
            true,
        ))
    }
}

fn compose_disjoint(
    destination: &Path,
    before: &StateLedger,
    proposed: &StateLedger,
    current: &StateLedger,
) -> Result<StateLedger, StateError> {
    let proposed_latches = changed_latches(before, proposed);
    let concurrent_latches = changed_latches(before, current);
    if let Some(entry) = proposed_latches.intersection(&concurrent_latches).next() {
        return Err(StateError::current(
            StateErrorKind::Conflict,
            destination,
            format!("concurrent gate-state conflict kind=latch key={entry:?}"),
            "recapture and rerun against the accepted latch authority",
        ));
    }

    let proposed_claims = changed_claim_paths(before, proposed);
    let concurrent_claims = changed_claim_paths(before, current);
    let mut merged_claims = BTreeMap::new();
    for path in proposed_claims.intersection(&concurrent_claims) {
        let Some(claim) = merge_same_actor_claim(path, proposed, current) else {
            return Err(StateError::current(
                StateErrorKind::Conflict,
                destination,
                format!("concurrent gate-state conflict kind=claim path={path:?}"),
                "recapture and rerun against the accepted actor/claim authority",
            ));
        };
        merged_claims.insert(path.clone(), claim);
    }

    let mut composed = current.clone();
    let proposed_latch_set = proposed.latches.iter().cloned().collect::<BTreeSet<_>>();
    let mut composed_latches = composed.latches.iter().cloned().collect::<BTreeSet<_>>();
    let mut composed_origins = origin_map(&composed.origins);
    for entry in proposed_latches {
        composed_origins.remove(&entry);
        if proposed_latch_set.contains(&entry) {
            if let Some(origin) = origin_of(&proposed.origins, &entry) {
                composed_origins.insert(entry.clone(), origin.clone());
            }
            composed_latches.insert(entry);
        } else {
            composed_latches.remove(&entry);
        }
    }
    composed.latches = composed_latches.into_iter().collect();
    composed.origins = composed_origins.into_values().collect();

    for path in proposed_claims {
        composed.claims.retain(|claim| claim.path != path);
        if let Some(claim) = merged_claims.remove(&path) {
            composed.claims.push(claim);
        } else {
            composed.claims.extend(
                proposed
                    .claims
                    .iter()
                    .filter(|claim| claim.path == path)
                    .cloned(),
            );
        }
    }
    sort_claims(&mut composed.claims);
    Ok(composed)
}

fn origin_map(origins: &[LatchOrigin]) -> BTreeMap<LatchEntry, LatchOrigin> {
    origins
        .iter()
        .map(|origin| (origin.latch.clone(), origin.clone()))
        .collect()
}

fn merge_same_actor_claim(
    path: &str,
    proposed: &StateLedger,
    current: &StateLedger,
) -> Option<PeerClaim> {
    let exactly_one = |ledger: &StateLedger| {
        let mut claims = ledger.claims.iter().filter(|claim| claim.path == path);
        let claim = claims.next()?.clone();
        claims.next().is_none().then_some(claim)
    };
    let proposed = exactly_one(proposed)?;
    let current = exactly_one(current)?;
    (proposed.actor == current.actor).then(|| PeerClaim {
        path: path.to_owned(),
        actor: proposed.actor,
        created_at: proposed.created_at.min(current.created_at),
        expires_at: proposed.expires_at.max(current.expires_at),
    })
}

fn changed_latches(left: &StateLedger, right: &StateLedger) -> BTreeSet<LatchEntry> {
    let left = left.latches.iter().cloned().collect::<BTreeSet<_>>();
    let right = right.latches.iter().cloned().collect::<BTreeSet<_>>();
    left.symmetric_difference(&right).cloned().collect()
}

fn changed_claim_paths(left: &StateLedger, right: &StateLedger) -> BTreeSet<String> {
    let mut paths = left
        .claims
        .iter()
        .chain(&right.claims)
        .map(|claim| claim.path.clone())
        .collect::<BTreeSet<_>>();
    paths.retain(|path| {
        left.claims
            .iter()
            .filter(|claim| claim.path == *path)
            .collect::<Vec<_>>()
            != right
                .claims
                .iter()
                .filter(|claim| claim.path == *path)
                .collect::<Vec<_>>()
    });
    paths
}

/// Immutable evidence for one proposed ledger transition.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StateReceipt {
    version: u64,
    before: Digest,
    authority: Digest,
    after: Digest,
    accepted: bool,
    authority_ledger: StateLedger,
    published_ledger: StateLedger,
}

impl StateReceipt {
    fn new(
        before: &StateLedger,
        authority: &StateLedger,
        published: &StateLedger,
        accepted: bool,
    ) -> Self {
        Self {
            version: STATE_RECEIPT_VERSION,
            before: before.digest(),
            authority: authority.digest(),
            after: published.digest(),
            accepted,
            authority_ledger: authority.clone(),
            published_ledger: published.clone(),
        }
    }

    /// Receipt schema admitted by this build.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Exact logical ledger identity before the run.
    #[must_use]
    pub const fn before(&self) -> &Digest {
        &self.before
    }

    /// Exact normalized ledger every arm judged before execution.
    #[must_use]
    pub const fn authority(&self) -> &Digest {
        &self.authority
    }

    /// Exact ledger identity published by the accepted run.
    #[must_use]
    pub const fn after(&self) -> &Digest {
        &self.after
    }

    /// Whether the after-ledger crossed the persistence boundary.
    #[must_use]
    pub const fn accepted(&self) -> bool {
        self.accepted
    }

    /// Canonical immutable receipt bytes.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        wire::encode(self)
    }

    /// Decode only the current canonical receipt spelling.
    pub fn decode(bytes: &[u8]) -> Result<Self, StateError> {
        let source = Path::new("<state-receipt>");
        let version: VersionEvidence = wire::decode(bytes).map_err(|error| {
            StateError::current(
                StateErrorKind::Decode,
                source,
                format!("malformed state receipt JSON: {error}"),
                "retain the bytes as unsupported evidence or regenerate the receipt",
            )
        })?;
        if version.version != STATE_RECEIPT_VERSION {
            return Err(StateError::current(
                StateErrorKind::UnsupportedVersion,
                source,
                format!("unsupported state receipt version {}", version.version),
                "use a build that admits this receipt version; do not rewrite the bytes",
            ));
        }
        let raw: RawStateReceipt = wire::decode(bytes).map_err(|error| {
            StateError::current(
                StateErrorKind::Decode,
                source,
                format!("malformed state receipt JSON: {error}"),
                "retain the original bytes and repair the receipt producer",
            )
        })?;
        let receipt = Self {
            version: raw.version,
            before: raw.before,
            authority: raw.authority,
            after: raw.after,
            accepted: raw.accepted,
            authority_ledger: raw.authority_ledger,
            published_ledger: raw.published_ledger,
        };
        validate_ledger(&receipt.authority_ledger, source, CURRENT_STATE_RELEASE)?;
        validate_ledger(&receipt.published_ledger, source, CURRENT_STATE_RELEASE)?;
        if receipt.authority_ledger.digest() != receipt.authority
            || receipt.published_ledger.digest() != receipt.after
        {
            return Err(StateError::current(
                StateErrorKind::Malformed,
                source,
                "state receipt ledger bodies do not match their declared identities",
                "retain the original bytes and regenerate the receipt from one accepted transition",
            ));
        }
        let canonical = receipt.encode().map_err(|error| {
            StateError::current(
                StateErrorKind::Encode,
                source,
                format!("could not encode decoded state receipt: {error}"),
                "report the current receipt schema defect",
            )
        })?;
        if canonical != bytes {
            return Err(StateError::current(
                StateErrorKind::NonCanonical,
                source,
                "state receipt JSON is not canonical",
                "retain the original bytes as evidence; only current producers mint receipts",
            ));
        }
        Ok(receipt)
    }

    /// Domain-separated identity of the frozen receipt.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let bytes = self
            .encode()
            .expect("the state receipt contains infallible JSON values");
        Digest::compute(Domain::GateStateReceipt, &bytes)
    }
}
