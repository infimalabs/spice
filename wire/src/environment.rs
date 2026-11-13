//! Closed environmental facts and the requirements compiled over them.
//!
//! A requirement carries no authored evaluator. Its fact discriminant selects
//! both the value type and the comparison implemented here, so admitting a new
//! fact requires adding its semantics to this crate and recompiling every
//! consumer.

use std::collections::BTreeMap;
use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize};

/// One configured lock-pool name, spelled as one normal path component.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ResourcePoolName(String);

impl ResourcePoolName {
    /// The name as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ResourcePoolName {
    type Err = ResourcePoolNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || matches!(value, "." | "..")
            || value
                .chars()
                .any(|character| matches!(character, '/' | '\\' | '\0'))
        {
            return Err(ResourcePoolNameError(value.to_owned()));
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for ResourcePoolName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ResourcePoolName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

/// A value that does not name one lock pool.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("resource pool name must be one normal path component: {0:?}")]
pub struct ResourcePoolNameError(String);

/// Canonical resource-pool shard counts attached to one task statement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceRequirements(BTreeMap<ResourcePoolName, NonZeroU32>);

impl ResourceRequirements {
    /// Build one canonical resource map from positive shard counts.
    pub fn new(
        resources: impl IntoIterator<Item = (ResourcePoolName, u32)>,
    ) -> Result<Self, ResourceRequirementsError> {
        let mut declared = BTreeMap::new();
        for (name, count) in resources {
            let count =
                NonZeroU32::new(count).ok_or(ResourceRequirementsError::Zero(name.clone()))?;
            if declared.insert(name.clone(), count).is_some() {
                return Err(ResourceRequirementsError::Duplicate(name));
            }
        }
        Ok(Self(declared))
    }

    /// Declared pools in canonical name order.
    pub fn iter(&self) -> impl Iterator<Item = (&ResourcePoolName, u32)> {
        self.0.iter().map(|(name, count)| (name, count.get()))
    }

    /// Whether no pool constrains the task.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether every declared pool currently has its required shard count.
    #[must_use]
    pub fn are_satisfied_by(&self, environment: &Environment) -> bool {
        self.iter().all(|(name, count)| {
            environment
                .resources
                .get(name)
                .is_some_and(|available| *available >= count)
        })
    }
}

/// Why a resource declaration cannot be represented.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceRequirementsError {
    /// One pool was named more than once.
    #[error("duplicate resource pool {0}")]
    Duplicate(ResourcePoolName),
    /// One pool requested no shards.
    #[error("resource pool {0} must request at least one shard")]
    Zero(ResourcePoolName),
}

/// Claim identity retained in the resource lock's owner evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceClaimOwner {
    /// Lane whose claim owns the resource.
    pub lane: crate::CanonicalUuid,
    /// Task whose claim owns the resource.
    pub task: crate::TaskStamp,
}

/// A closed kind of environmental fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvironmentFact {
    /// Finalized schema of the journal authority visible to the evaluator.
    JournalSchema,
    /// Exact executable-configuration digest with a current bound grant.
    TrustGrant,
    /// Product version of the running resident appliance.
    ApplianceVersion,
    /// One configured lock pool observed at allocation time.
    ResourcePool,
}

impl EnvironmentFact {
    /// Every environmental fact, in canonical collection order.
    pub const ALL: &[Self] = &[
        Self::JournalSchema,
        Self::TrustGrant,
        Self::ApplianceVersion,
        Self::ResourcePool,
    ];

    /// Stable wire spelling of this fact.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JournalSchema => "journal-schema",
            Self::TrustGrant => "trust-grant",
            Self::ApplianceVersion => "appliance-version",
            Self::ResourcePool => "resource-pool",
        }
    }
}

impl fmt::Display for EnvironmentFact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One typed environmental requirement.
///
/// The tagged variant is the fact and its `value` field is the sole typed
/// operand. There is deliberately no operator, expression, evaluator name, or
/// nesting point in this shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "kebab-case", deny_unknown_fields)]
pub enum EnvironmentRequirement {
    /// Require an authority whose observed journal schema is at least `value`.
    JournalSchema {
        /// Inclusive minimum journal schema. Zero is not a schema.
        value: NonZeroU32,
    },
    /// Require one exact executable-configuration digest to be currently granted.
    TrustGrant {
        /// Exact configuration digest whose bound grant is required.
        value: crate::Digest,
    },
    /// Require a running appliance whose SemVer precedence exceeds `value`.
    ///
    /// Build metadata never counts, so restarting the same build, or any
    /// build of the same nominal version, does not satisfy it.
    ApplianceVersion {
        /// Exclusive lower bound on the running appliance's nominal version.
        value: crate::manifest::ProductVersion,
    },
    /// Require `shards` simultaneously available shards from one configured pool.
    ResourcePool {
        /// Configured `lock.<name>` identity.
        name: ResourcePoolName,
        /// Positive number of shards the task needs.
        shards: NonZeroU32,
    },
}

impl EnvironmentRequirement {
    /// Construct a minimum journal-schema requirement.
    ///
    /// # Errors
    ///
    /// Refuses zero because journal schemas begin at one.
    pub const fn journal_schema(value: u32) -> Result<Self, EnvironmentValueError> {
        match NonZeroU32::new(value) {
            Some(value) => Ok(Self::JournalSchema { value }),
            None => Err(EnvironmentValueError),
        }
    }

    /// Construct an exact trust-grant requirement.
    #[must_use]
    pub const fn trust_grant(value: crate::Digest) -> Self {
        Self::TrustGrant { value }
    }

    /// Construct a requirement for an appliance strictly later than `value`.
    #[must_use]
    pub const fn appliance_after(value: crate::manifest::ProductVersion) -> Self {
        Self::ApplianceVersion { value }
    }

    /// Construct a configured resource-pool requirement.
    ///
    /// # Errors
    ///
    /// Refuses zero shards.
    pub fn resource_pool(
        name: ResourcePoolName,
        shards: u32,
    ) -> Result<Self, EnvironmentValueError> {
        NonZeroU32::new(shards)
            .map(|shards| Self::ResourcePool { name, shards })
            .ok_or(EnvironmentValueError)
    }

    /// Fact this requirement constrains.
    #[must_use]
    pub const fn fact(&self) -> EnvironmentFact {
        match self {
            Self::JournalSchema { .. } => EnvironmentFact::JournalSchema,
            Self::TrustGrant { .. } => EnvironmentFact::TrustGrant,
            Self::ApplianceVersion { .. } => EnvironmentFact::ApplianceVersion,
            Self::ResourcePool { .. } => EnvironmentFact::ResourcePool,
        }
    }

    /// Whether one same-kind observation satisfies this requirement.
    ///
    /// The comparison is selected entirely by the fact variant. Callers can
    /// provide observations, but cannot replace or parameterize the evaluator.
    #[must_use]
    pub fn is_satisfied_by(&self, observed: &EnvironmentObservation) -> bool {
        match (self, observed) {
            (
                Self::JournalSchema { value: required },
                EnvironmentObservation::JournalSchema { value: observed },
            ) => observed.get() >= required.get(),
            (
                Self::TrustGrant { value: required },
                EnvironmentObservation::TrustGrant {
                    value: observed,
                    state,
                    worktree,
                },
            ) => {
                observed == required
                    && *state == crate::TrustState::Granted
                    && worktree
                        .as_ref()
                        .is_some_and(|worktree| !worktree.is_empty())
            }
            (
                Self::ApplianceVersion { value: required },
                EnvironmentObservation::ApplianceVersion { value: observed },
            ) => observed.precedence(required).is_gt(),
            (
                Self::ResourcePool {
                    name: required_name,
                    shards: required_shards,
                },
                EnvironmentObservation::ResourcePool {
                    name: observed_name,
                    available,
                },
            ) => required_name == observed_name && *available >= required_shards.get(),
            _ => false,
        }
    }
}

/// A zero value supplied for an environmental fact whose domain begins at one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentValueError;

impl fmt::Display for EnvironmentValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("environment value must be a positive integer")
    }
}

impl std::error::Error for EnvironmentValueError {}

/// The complete observed environment supplied to requirement evaluation.
///
/// Each fact has one private slot, so callers cannot create duplicate or
/// order-sensitive observations. Extending the vocabulary adds another typed
/// slot and another compiled match arm rather than opening an evaluator API.
///
/// identity-law: non-crossing - process-local input to compiled requirement evaluation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    journal_schema: Option<NonZeroU32>,
    trust: BTreeMap<crate::Digest, crate::TrustHead>,
    appliance: Option<crate::manifest::ProductVersion>,
    resources: BTreeMap<ResourcePoolName, u32>,
}

impl Environment {
    /// Observe one finalized journal schema.
    ///
    /// # Errors
    ///
    /// Refuses zero because journal schemas begin at one.
    pub const fn journal_schema(value: u32) -> Result<Self, EnvironmentValueError> {
        match NonZeroU32::new(value) {
            Some(value) => Ok(Self {
                journal_schema: Some(value),
                trust: BTreeMap::new(),
                appliance: None,
                resources: BTreeMap::new(),
            }),
            None => Err(EnvironmentValueError),
        }
    }

    /// Add current trust heads observed at the same authority barrier.
    #[must_use]
    pub fn with_trust_heads(mut self, heads: impl IntoIterator<Item = crate::TrustHead>) -> Self {
        self.trust
            .extend(heads.into_iter().map(|head| (head.digest.clone(), head)));
        self
    }

    /// Current authority head for one exact executable digest.
    #[must_use]
    pub fn trust_head(&self, digest: &crate::Digest) -> Option<&crate::TrustHead> {
        self.trust.get(digest)
    }

    /// Add the running resident appliance's product version.
    #[must_use]
    pub fn with_appliance(mut self, version: crate::manifest::ProductVersion) -> Self {
        self.appliance = Some(version);
        self
    }

    /// Add one lock pool's currently available shard count.
    #[must_use]
    pub fn with_resource_pool(mut self, name: ResourcePoolName, available: u32) -> Self {
        self.resources.insert(name, available);
        self
    }

    /// Resource-pool observations carried by this environment.
    pub fn resource_pools(&self) -> impl Iterator<Item = (&ResourcePoolName, u32)> {
        self.resources
            .iter()
            .map(|(name, available)| (name, *available))
    }

    fn observation(&self, requirement: &EnvironmentRequirement) -> Option<EnvironmentObservation> {
        match requirement {
            EnvironmentRequirement::JournalSchema { .. } => self
                .journal_schema
                .map(|value| EnvironmentObservation::JournalSchema { value }),
            EnvironmentRequirement::TrustGrant { value } => {
                self.trust
                    .get(value)
                    .map(|head| EnvironmentObservation::TrustGrant {
                        value: head.digest.clone(),
                        state: head.state,
                        worktree: head.worktree.clone(),
                    })
            }
            EnvironmentRequirement::ApplianceVersion { .. } => self
                .appliance
                .clone()
                .map(|value| EnvironmentObservation::ApplianceVersion { value }),
            EnvironmentRequirement::ResourcePool { name, .. } => {
                self.resources.get(name).copied().map(|available| {
                    EnvironmentObservation::ResourcePool {
                        name: name.clone(),
                        available,
                    }
                })
            }
        }
    }

    /// Whether this environment contains a same-kind observation satisfying
    /// one compiled requirement.
    #[must_use]
    pub fn satisfies(&self, requirement: &EnvironmentRequirement) -> bool {
        self.observation(requirement)
            .is_some_and(|observation| requirement.is_satisfied_by(&observation))
    }
}

/// One typed observation from the environment being evaluated.
///
/// identity-law: non-crossing - process-local input to compiled requirement evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentObservation {
    /// Finalized journal schema observed from authority.
    JournalSchema {
        /// Positive observed schema value.
        value: NonZeroU32,
    },
    /// Current trust head observed for one exact digest.
    TrustGrant {
        /// Digest naming this trust head.
        value: crate::Digest,
        /// Current approval standing.
        state: crate::TrustState,
        /// Worktree that produced the executable configuration, when bound.
        worktree: Option<String>,
    },
    /// Product version of the running resident appliance.
    ApplianceVersion {
        /// Observed nominal version, with its build metadata.
        value: crate::manifest::ProductVersion,
    },
    /// Available shards in one configured resource pool.
    ResourcePool {
        /// Pool whose topology was observed.
        name: ResourcePoolName,
        /// Number of shards available at the observation instant.
        available: u32,
    },
}

impl EnvironmentObservation {
    /// Construct an observed journal schema.
    ///
    /// # Errors
    ///
    /// Refuses zero because journal schemas begin at one.
    pub const fn journal_schema(value: u32) -> Result<Self, EnvironmentValueError> {
        match NonZeroU32::new(value) {
            Some(value) => Ok(Self::JournalSchema { value }),
            None => Err(EnvironmentValueError),
        }
    }

    /// Fact this observation describes.
    #[must_use]
    pub const fn fact(&self) -> EnvironmentFact {
        match self {
            Self::JournalSchema { .. } => EnvironmentFact::JournalSchema,
            Self::TrustGrant { .. } => EnvironmentFact::TrustGrant,
            Self::ApplianceVersion { .. } => EnvironmentFact::ApplianceVersion,
            Self::ResourcePool { .. } => EnvironmentFact::ResourcePool,
        }
    }
}

/// Canonical AND-composed environmental requirements for one task.
///
/// Entries are strictly ordered by [`EnvironmentFact`]. That simultaneously
/// makes the encoding unique and prevents two requirements for one fact from
/// acquiring accidental precedence semantics.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct EnvironmentRequirements(Vec<EnvironmentRequirement>);

impl EnvironmentRequirements {
    /// Build a canonical collection in the exact supplied order.
    ///
    /// Different fact kinds follow [`EnvironmentFact`] order. Resource-pool
    /// requirements may repeat for distinct pools and sort by pool name.
    ///
    /// # Errors
    ///
    /// Refuses duplicate facts, duplicate resource names, and out-of-order
    /// entries. It never sorts or deduplicates caller input.
    pub fn new(requirements: Vec<EnvironmentRequirement>) -> Result<Self, RequirementsError> {
        for pair in requirements.windows(2) {
            match (&pair[0], &pair[1]) {
                (
                    EnvironmentRequirement::ResourcePool { name: previous, .. },
                    EnvironmentRequirement::ResourcePool { name: current, .. },
                ) => match previous.cmp(current) {
                    std::cmp::Ordering::Less => {}
                    std::cmp::Ordering::Equal => {
                        return Err(RequirementsError::DuplicateResource);
                    }
                    std::cmp::Ordering::Greater => {
                        return Err(RequirementsError::ResourceOutOfOrder);
                    }
                },
                (previous, current) if previous.fact() == current.fact() => {
                    return Err(RequirementsError::Duplicate(current.fact()));
                }
                (previous, current) if previous.fact() > current.fact() => {
                    return Err(RequirementsError::OutOfOrder {
                        previous: previous.fact(),
                        current: current.fact(),
                    });
                }
                _ => {}
            }
        }
        Ok(Self(requirements))
    }

    /// Build the necessarily canonical one-entry collection.
    #[must_use]
    pub fn one(requirement: EnvironmentRequirement) -> Self {
        Self(vec![requirement])
    }

    /// Requirements in canonical fact order.
    #[must_use]
    pub fn as_slice(&self) -> &[EnvironmentRequirement] {
        &self.0
    }

    /// Declared resource-pool needs in canonical pool-name order.
    pub fn resource_pools(&self) -> impl Iterator<Item = (&ResourcePoolName, u32)> {
        self.0.iter().filter_map(|requirement| match requirement {
            EnvironmentRequirement::ResourcePool { name, shards } => Some((name, shards.get())),
            _ => None,
        })
    }

    /// Compile authored resource entries into their canonical task map.
    #[must_use]
    pub fn compiled_resources(&self) -> ResourceRequirements {
        ResourceRequirements(
            self.0
                .iter()
                .filter_map(|requirement| match requirement {
                    EnvironmentRequirement::ResourcePool { name, shards } => {
                        Some((name.clone(), *shards))
                    }
                    _ => None,
                })
                .collect(),
        )
    }

    /// Retain only scalar environmental facts.
    #[must_use]
    pub fn without_resources(&self) -> Self {
        Self(
            self.0
                .iter()
                .filter(|requirement| {
                    !matches!(requirement, EnvironmentRequirement::ResourcePool { .. })
                })
                .cloned()
                .collect(),
        )
    }

    /// Whether no environment fact constrains this task.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether every requirement has a same-kind observation satisfying its
    /// compiled comparison.
    #[must_use]
    pub fn are_satisfied_by(&self, environment: &Environment) -> bool {
        self.0
            .iter()
            .all(|requirement| environment.satisfies(requirement))
    }

    /// Whether this conjunction contains a trust grant the environment does not satisfy.
    #[must_use]
    pub fn has_unsatisfied_trust_grant(&self, environment: &Environment) -> bool {
        self.0.iter().any(|requirement| {
            matches!(requirement, EnvironmentRequirement::TrustGrant { .. })
                && !environment.satisfies(requirement)
        })
    }

    /// Deserialize a present task field.
    ///
    /// Empty collections are represented only by omission on task facts and
    /// heads, so an explicit empty array is a noncanonical spelling.
    pub(crate) fn present<'de, D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let requirements = Self::deserialize(deserializer)?;
        if requirements.is_empty() {
            Err(D::Error::custom(
                "present task requirements must not be empty",
            ))
        } else {
            Ok(requirements)
        }
    }
}

impl<'de> Deserialize<'de> for EnvironmentRequirements {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let requirements = Vec::<EnvironmentRequirement>::deserialize(deserializer)?;
        Self::new(requirements).map_err(D::Error::custom)
    }
}

impl TryFrom<Vec<EnvironmentRequirement>> for EnvironmentRequirements {
    type Error = RequirementsError;

    fn try_from(requirements: Vec<EnvironmentRequirement>) -> Result<Self, Self::Error> {
        Self::new(requirements)
    }
}

impl AsRef<[EnvironmentRequirement]> for EnvironmentRequirements {
    fn as_ref(&self) -> &[EnvironmentRequirement] {
        self.as_slice()
    }
}

/// Why an environmental requirement collection is not canonical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequirementsError {
    /// One fact kind appeared more than once.
    Duplicate(EnvironmentFact),
    /// One resource pool appeared more than once.
    DuplicateResource,
    /// Resource pools were not in canonical name order.
    ResourceOutOfOrder,
    /// Fact kinds were not in their declared canonical order.
    OutOfOrder {
        /// Fact immediately preceding the inversion.
        previous: EnvironmentFact,
        /// Fact that should have appeared earlier.
        current: EnvironmentFact,
    },
}

impl fmt::Display for RequirementsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate(fact) => write!(formatter, "duplicate environment fact {fact}"),
            Self::OutOfOrder { previous, current } => write!(
                formatter,
                "environment fact {current} must sort before {previous}"
            ),
            Self::DuplicateResource => formatter.write_str("duplicate resource pool requirement"),
            Self::ResourceOutOfOrder => {
                formatter.write_str("resource pool requirements are not in canonical name order")
            }
        }
    }
}

impl std::error::Error for RequirementsError {}
