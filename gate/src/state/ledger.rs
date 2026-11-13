use super::*;

/// Which sticky ledger owns an entry.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatchKind {
    FileLoc,
    FileBytes,
    ComplexityCcn,
    ComplexityLength,
    RepoDocChars,
}

impl LatchKind {
    /// Ledger spelling of this kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileLoc => "file_loc",
            Self::FileBytes => "file_bytes",
            Self::ComplexityCcn => "complexity_ccn",
            Self::ComplexityLength => "complexity_length",
            Self::RepoDocChars => "repo_doc_chars",
        }
    }

    pub(super) const fn admits(self, key: &StateKey) -> bool {
        match self {
            Self::FileLoc | Self::FileBytes | Self::RepoDocChars => key.is_path(),
            Self::ComplexityCcn | Self::ComplexityLength => key.is_function(),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Serialized canonical path-key body.
pub struct PathKey {
    path: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Serialized canonical path/function-key body.
pub struct FunctionKey {
    path: String,
    function: String,
}

/// A sticky key in one of the two ledger shapes.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StateKey {
    Function(FunctionKey),
    Path(PathKey),
}

impl StateKey {
    /// Construct a canonical path key.
    pub fn path(path: impl AsRef<str>) -> Result<Self, KeyError> {
        Ok(Self::Path(PathKey {
            path: normalize_repo_path(path.as_ref()).map_err(|reason| KeyError::InvalidPath {
                path: path.as_ref().to_owned(),
                reason,
            })?,
        }))
    }

    /// Construct a canonical path/function key.
    pub fn function(path: impl AsRef<str>, function: impl Into<String>) -> Result<Self, KeyError> {
        let function = function.into();
        if function.is_empty() {
            return Err(KeyError::EmptyFunction);
        }
        Ok(Self::Function(FunctionKey {
            path: normalize_repo_path(path.as_ref()).map_err(|reason| KeyError::InvalidPath {
                path: path.as_ref().to_owned(),
                reason,
            })?,
            function,
        }))
    }

    /// Canonical repository-relative path component.
    #[must_use]
    pub fn repo_path(&self) -> &str {
        match self {
            Self::Function(key) => &key.path,
            Self::Path(key) => &key.path,
        }
    }

    /// Function component when this is a path/function key.
    #[must_use]
    pub fn function_name(&self) -> Option<&str> {
        match self {
            Self::Function(key) => Some(&key.function),
            Self::Path(_) => None,
        }
    }

    const fn is_path(&self) -> bool {
        matches!(self, Self::Path(_))
    }

    const fn is_function(&self) -> bool {
        matches!(self, Self::Function(_))
    }

    pub(super) fn renamed(&self, path: &str) -> Self {
        match self {
            Self::Function(key) => Self::Function(FunctionKey {
                path: path.to_owned(),
                function: key.function.clone(),
            }),
            Self::Path(_) => Self::Path(PathKey {
                path: path.to_owned(),
            }),
        }
    }
}

/// One ledger-qualified sticky key.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatchEntry {
    pub(super) kind: LatchKind,
    pub(super) key: StateKey,
}

impl LatchEntry {
    /// Validate that the key shape belongs to this ledger kind.
    pub fn new(kind: LatchKind, key: StateKey) -> Result<Self, KeyError> {
        if !kind.admits(&key) {
            return Err(KeyError::WrongShape { kind });
        }
        Ok(Self { kind, key })
    }

    /// Owning sticky ledger class.
    #[must_use]
    pub const fn kind(&self) -> LatchKind {
        self.kind
    }

    /// Path or path/function key.
    #[must_use]
    pub const fn key(&self) -> &StateKey {
        &self.key
    }
}

/// The fingerprint spelled as `KIND path="…"`, with ` function="…"` for a
/// function key.
impl fmt::Display for LatchEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} path={:?}",
            self.kind.as_str(),
            self.key.repo_path()
        )?;
        match self.key.function_name() {
            Some(function) => write!(formatter, " function={function:?}"),
            None => Ok(()),
        }
    }
}

/// A malformed state key or rename map.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyError {
    InvalidPath { path: String, reason: String },
    EmptyFunction,
    WrongShape { kind: LatchKind },
    RenameCollision { path: String },
}

impl fmt::Display for KeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path, reason } => {
                write!(formatter, "invalid repository path {path:?}: {reason}")
            }
            Self::EmptyFunction => formatter.write_str("function key must be non-empty"),
            Self::WrongShape { kind } => {
                write!(formatter, "key shape does not belong to {kind:?} ledger")
            }
            Self::RenameCollision { path } => {
                write!(formatter, "staged rename destination collides at {path:?}")
            }
        }
    }
}

impl Error for KeyError {}

/// One active shared flex claim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerClaim {
    pub(super) path: String,
    pub(super) actor: String,
    pub(super) created_at: f64,
    pub(super) expires_at: f64,
}

impl PeerClaim {
    /// Canonical claimed path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Claim owner.
    #[must_use]
    pub fn actor(&self) -> &str {
        &self.actor
    }

    /// Original ownership timestamp.
    #[must_use]
    pub const fn created_at(&self) -> f64 {
        self.created_at
    }

    /// Exclusive expiry timestamp.
    #[must_use]
    pub const fn expires_at(&self) -> f64 {
        self.expires_at
    }
}

/// What the accepted run that created one latch recorded about its subject
/// first going over base: the commit its change was staged on, the run clock,
/// the acting actor, and the measured value against the base bound.
///
/// The latch it names is its fingerprint. Renames carry an origin with its
/// latch, healing drops it with the latch, and a latch written before origins
/// were recorded has none rather than an invented one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatchOrigin {
    pub(super) latch: LatchEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) commit: Option<String>,
    pub(super) at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) actor: Option<String>,
    pub(super) measured: u64,
    pub(super) base_limit: u64,
}

impl LatchOrigin {
    /// Fingerprint of the debt this origin belongs to.
    #[must_use]
    pub const fn latch(&self) -> &LatchEntry {
        &self.latch
    }

    /// Commit the latching change was staged on; absent on an unborn branch.
    #[must_use]
    pub fn commit(&self) -> Option<&str> {
        self.commit.as_deref()
    }

    /// Unix time of the accepted run that created the latch.
    #[must_use]
    pub const fn at(&self) -> f64 {
        self.at
    }

    /// Actor of the creating run, when it named one.
    #[must_use]
    pub fn actor(&self) -> Option<&str> {
        self.actor.as_deref()
    }

    /// Value measured when the subject first went over base.
    #[must_use]
    pub const fn measured(&self) -> u64 {
        self.measured
    }

    /// Base bound the subject first went over.
    #[must_use]
    pub const fn base_limit(&self) -> u64 {
        self.base_limit
    }

    /// Whole seconds since the subject first went over base.
    #[must_use]
    pub fn age_seconds(&self, now: f64) -> u64 {
        seconds_since(self.at, now)
    }
}

/// What a held latch's ledger row knows of when its subject first went over
/// base, so a refusal it holds reads as inherited debt rather than a verdict
/// on the current change. Both parts are absent for a latch whose origin was
/// never recorded.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LatchInheritance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    age_seconds: Option<u64>,
}

impl LatchInheritance {
    /// Commit the latching change was staged on, when recorded.
    #[must_use]
    pub fn commit(&self) -> Option<&str> {
        self.commit.as_deref()
    }

    /// Whole seconds since first over base, when recorded.
    #[must_use]
    pub const fn age_seconds(&self) -> Option<u64> {
        self.age_seconds
    }
}

impl fmt::Display for LatchInheritance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("inherited debt: first over base after ")?;
        match &self.commit {
            Some(commit) => write!(formatter, "commit {commit}, ")?,
            None => formatter.write_str("an unknown commit, ")?,
        }
        match self.age_seconds {
            Some(seconds) => write!(formatter, "{} ago", latch_age(seconds)),
            None => formatter.write_str("at an unknown time"),
        }
    }
}

fn seconds_since(at: f64, now: f64) -> u64 {
    (now - at).max(0.0) as u64
}

/// Spell an age in its largest whole unit: days, hours, minutes, or seconds.
#[must_use]
pub fn latch_age(seconds: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match seconds {
        seconds if seconds >= DAY => format!("{}d", seconds / DAY),
        seconds if seconds >= HOUR => format!("{}h", seconds / HOUR),
        seconds if seconds >= MINUTE => format!("{}m", seconds / MINUTE),
        seconds => format!("{seconds}s"),
    }
}

/// The durable latches one run judges against, read at its fixed clock.
///
/// Acquisition decides each bound against this inventory and reconciliation
/// rebuilds it from the same run authority, so a held latch reports the same
/// inheritance on both routes. Sweep covers are verdict policy rather than
/// bound state: they decide who answers for a breach, never the limit, so
/// reconciliation judges without them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JudgedLatches {
    latches: Vec<LatchEntry>,
    origins: Vec<LatchOrigin>,
    covers: BTreeMap<LatchEntry, TaskHandle>,
    now: f64,
}

impl JudgedLatches {
    /// Judge one ledger's latches at the run clock.
    #[must_use]
    pub fn new(ledger: &StateLedger, now: f64) -> Self {
        Self {
            latches: ledger.latches.clone(),
            origins: ledger.origins.clone(),
            covers: BTreeMap::new(),
            now,
        }
    }

    /// Hold each covered fingerprint for the pending sweep task that covers
    /// it, as the rim read the board for this run.
    #[must_use]
    pub fn with_covers(mut self, covers: BTreeMap<LatchEntry, TaskHandle>) -> Self {
        self.covers = covers;
        self
    }

    /// Whether this subject is durably latched.
    #[must_use]
    pub fn contains(&self, entry: &LatchEntry) -> bool {
        self.latches.binary_search(entry).is_ok()
    }

    /// The pending sweep task that holds this breach, when one covers it.
    ///
    /// Only a breach is held: a subject inside its bound answers to nobody.
    #[must_use]
    pub fn cover(&self, entry: &LatchEntry, decision: &BoundDecision) -> Option<&TaskHandle> {
        decision
            .over_limit()
            .then(|| self.covers.get(entry))
            .flatten()
    }

    /// Attach what the ledger knows of a held latch's origin to its decision.
    ///
    /// A latch is held only when it predates the current change, whether the
    /// ledger recorded it or the committed base was already over; any other
    /// transition is returned unchanged.
    #[must_use]
    pub fn attribute(&self, entry: &LatchEntry, mut decision: BoundDecision) -> BoundDecision {
        if decision.latch_transition == LatchTransition::Held {
            let origin = origin_of(&self.origins, entry);
            decision.inherited = Some(LatchInheritance {
                commit: origin.and_then(|origin| origin.commit.clone()),
                age_seconds: origin.map(|origin| origin.age_seconds(self.now)),
            });
        }
        decision
    }
}

pub(super) fn origin_of<'a>(
    origins: &'a [LatchOrigin],
    entry: &LatchEntry,
) -> Option<&'a LatchOrigin> {
    origins
        .binary_search_by(|origin| origin.latch.cmp(entry))
        .ok()
        .map(|index| &origins[index])
}

/// Complete current Git-common state.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StateLedger {
    pub(super) version: u64,
    pub(super) latches: Vec<LatchEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) origins: Vec<LatchOrigin>,
    pub(super) claims: Vec<PeerClaim>,
}

impl<'de> Deserialize<'de> for StateLedger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let ledger = Self::from(RawStateLedger::deserialize(deserializer)?);
        validate_ledger(&ledger, Path::new("<state-ledger>"), CURRENT_STATE_RELEASE)
            .map_err(serde::de::Error::custom)?;
        Ok(ledger)
    }
}

impl StateLedger {
    /// Current schema number.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Sticky entries in canonical order.
    #[must_use]
    pub fn latches(&self) -> &[LatchEntry] {
        &self.latches
    }

    /// Recorded latch origins in canonical latch order.
    #[must_use]
    pub fn origins(&self) -> &[LatchOrigin] {
        &self.origins
    }

    /// Every latched fingerprint beside its origin, unknown when unrecorded.
    pub fn catalogue(&self) -> impl Iterator<Item = (&LatchEntry, Option<&LatchOrigin>)> {
        self.latches
            .iter()
            .map(|entry| (entry, origin_of(&self.origins, entry)))
    }

    /// Advisory bound debt: how many subjects are latched and when the oldest
    /// recorded one first went over base.
    #[must_use]
    pub fn debt(&self) -> LatchDebt {
        LatchDebt {
            latched: self.latches.len(),
            oldest_at: self.origins.iter().map(LatchOrigin::at).reduce(f64::min),
        }
    }

    /// Active claims in canonical order.
    #[must_use]
    pub fn claims(&self) -> &[PeerClaim] {
        &self.claims
    }

    /// Canonical current bytes.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        wire::encode(self)
    }

    /// Domain-separated identity of the complete ledger.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let bytes = self
            .encode()
            .expect("the state ledger contains only finite JSON values");
        Digest::compute(Domain::GateStateLedger, &bytes)
    }
}

/// The ledger's advisory bound-debt signal; it never refuses or files work.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatchDebt {
    latched: usize,
    oldest_at: Option<f64>,
}

impl LatchDebt {
    /// Latched subjects.
    #[must_use]
    pub const fn latched(&self) -> usize {
        self.latched
    }

    /// Age of the oldest recorded latch, unknown when none recorded an origin.
    #[must_use]
    pub fn oldest_age_seconds(&self, now: f64) -> Option<u64> {
        self.oldest_at.map(|at| seconds_since(at, now))
    }
}

/// Canonical staged old-to-new paths.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenameMap {
    paths: BTreeMap<String, String>,
}

impl RenameMap {
    /// Normalize a complete staged rename set and refuse ambiguous targets.
    pub fn new<I, O, N>(renames: I) -> Result<Self, KeyError>
    where
        I: IntoIterator<Item = (O, N)>,
        O: AsRef<str>,
        N: AsRef<str>,
    {
        let mut paths = BTreeMap::new();
        let mut destinations = BTreeSet::new();
        for (old, new) in renames {
            let old = StateKey::path(old)?.repo_path().to_owned();
            let new = StateKey::path(new)?.repo_path().to_owned();
            if old == new {
                continue;
            }
            if paths.insert(old.clone(), new.clone()).is_some() {
                return Err(KeyError::RenameCollision { path: old });
            }
            if !destinations.insert(new.clone()) {
                return Err(KeyError::RenameCollision { path: new });
            }
        }
        Ok(Self { paths })
    }

    pub(crate) fn destination<'a>(&'a self, path: &'a str) -> &'a str {
        self.paths.get(path).map_or(path, String::as_str)
    }
}

/// One complete current observation used to reconcile a sticky entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LatchObservation {
    pub(super) entry: LatchEntry,
    pub(super) bound: BoundRule,
    pub(super) measured: u64,
    pub(super) actor_inputs: ActorInputs,
    pub(super) prior_latched: Option<bool>,
}

impl LatchObservation {
    /// Bind one measured key to its rule and claim actor evidence.
    pub fn new(
        entry: LatchEntry,
        bound: BoundRule,
        measured: u64,
        actor_inputs: ActorInputs,
    ) -> Result<Self, BoundError> {
        if entry.key().repo_path() != actor_inputs.candidate_path() {
            return Err(BoundError::InvalidPath {
                path: actor_inputs.candidate_path().to_owned(),
                reason: format!(
                    "actor candidate path must match observed key path {:?}",
                    entry.key().repo_path()
                ),
            });
        }
        Ok(Self {
            entry,
            bound,
            measured,
            actor_inputs,
            prior_latched: None,
        })
    }

    /// Actor evidence retained only for peer-claim arbitration.
    #[must_use]
    pub(crate) const fn actor_inputs(&self) -> &ActorInputs {
        &self.actor_inputs
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn with_prior_latched(mut self, prior_latched: bool) -> Self {
        self.prior_latched = Some(prior_latched);
        self
    }

    /// Sticky key this observation reconciles.
    #[must_use]
    pub const fn entry(&self) -> &LatchEntry {
        &self.entry
    }
}

/// One latch-qualified decision returned by reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateDecision {
    pub(super) entry: LatchEntry,
    pub(super) bound: BoundDecision,
}

impl StateDecision {
    /// Sticky entry this decision controls.
    #[must_use]
    pub const fn entry(&self) -> &LatchEntry {
        &self.entry
    }

    /// Complete bound decision.
    #[must_use]
    pub const fn bound(&self) -> &BoundDecision {
        &self.bound
    }
}
