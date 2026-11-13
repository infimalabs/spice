//! The closed capability-to-actor map one resident generation authenticates
//! sockets by.
//!
//! A capability is minted or issued. The generation mints its local owner's
//! capability and each seated lane's, and knows them raw. An admitted
//! principal's capability was handed off once at admission; the journal's
//! principal heads and this map keep only its digest, so it authenticates
//! across generations without the secret ever being stored. Every external
//! connection authors as its exact principal.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::error::Error;
use std::fmt;
use std::sync::{Arc, RwLock};

use tokio_util::sync::CancellationToken;

use super::OpeningError;

/// Closed capability-to-principal map for one resident generation, with the
/// principal role heads its command door and principal lookups decide by.
#[derive(Clone, Default)]
pub struct Capabilities {
    bindings: Arc<RwLock<Bindings>>,
    owner: Option<wire::CanonicalUuid>,
    roles: crate::RoleBook,
}

#[derive(Default)]
struct Bindings {
    /// Capabilities this generation minted, keyed raw.
    minted: BTreeMap<wire::CanonicalUuid, CapabilityBinding>,
    /// Capabilities admissions issued, keyed by their journaled digest.
    issued: BTreeMap<wire::Digest, CapabilityBinding>,
}

impl Bindings {
    /// The binding `capability` authenticates as. A minted capability is
    /// found without hashing; only a miss digests the presented secret.
    fn get(&self, capability: wire::CanonicalUuid) -> Option<&CapabilityBinding> {
        self.minted.get(&capability).or_else(|| {
            self.issued
                .get(&wire::principal::capability_digest(capability))
        })
    }
}

#[derive(Clone, Debug)]
struct CapabilityBinding {
    actor: wire::Actor,
    retired: CancellationToken,
}

impl CapabilityBinding {
    fn new(actor: wire::Actor) -> Self {
        Self {
            actor,
            retired: CancellationToken::new(),
        }
    }
}

pub(super) struct CapabilityAdmission {
    pub(super) accepted: wire::Accepted,
    pub(super) retired: CancellationToken,
}

impl fmt::Debug for Capabilities {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (minted, issued) = self.bindings.read().map_or((0, 0), |bindings| {
            (bindings.minted.len(), bindings.issued.len())
        });
        formatter
            .debug_struct("Capabilities")
            .field("minted", &minted)
            .field("issued", &issued)
            .field("owned", &self.owner.is_some())
            .finish()
    }
}

impl Capabilities {
    /// A generation whose local owner, the principal `owner`, authenticates
    /// through `capability`: the one local discovery and the app bootstrap
    /// serve when the asking client presents no capability of its own.
    pub fn owned_by(
        capability: wire::CanonicalUuid,
        owner: wire::CanonicalUuid,
    ) -> Result<Self, CapabilityError> {
        let capabilities = Self {
            owner: Some(capability),
            ..Self::default()
        };
        capabilities.insert(capability, wire::Actor::Principal(owner))?;
        Ok(capabilities)
    }

    /// Register a capability this generation minted exactly once.
    pub fn insert(
        &self,
        capability: wire::CanonicalUuid,
        actor: wire::Actor,
    ) -> Result<(), CapabilityError> {
        refuse_noninteractive(&actor)?;
        let mut bindings = self
            .bindings
            .write()
            .map_err(|_| CapabilityError::Unavailable)?;
        match bindings.minted.entry(capability) {
            Entry::Vacant(entry) => {
                entry.insert(CapabilityBinding::new(actor));
                Ok(())
            }
            Entry::Occupied(_) => Err(CapabilityError::Duplicate),
        }
    }

    /// Publish the journal's current principal heads: every later request
    /// decides by their roles, and each issued capability they record binds
    /// its principal. A capability no longer recorded is retired, ending
    /// every session it authenticated.
    pub fn publish(&self, roles: wire::principal::PrincipalRoles) -> Result<(), CapabilityError> {
        let mut bindings = self
            .bindings
            .write()
            .map_err(|_| CapabilityError::Unavailable)?;
        let current: BTreeMap<&wire::Digest, wire::Actor> = roles
            .issued()
            .map(|(principal, capability)| (capability, wire::Actor::Principal(principal)))
            .collect();
        let mut retired = Vec::new();
        bindings.issued.retain(|capability, binding| {
            let held = current.get(capability) == Some(&binding.actor);
            if !held {
                retired.push(binding.retired.clone());
            }
            held
        });
        for (capability, actor) in current {
            bindings
                .issued
                .entry(capability.clone())
                .or_insert_with(|| CapabilityBinding::new(actor));
        }
        let published = self.roles.publish(roles);
        drop(bindings);
        for retired in retired {
            retired.cancel();
        }
        published.map_err(|_| CapabilityError::Unavailable)
    }

    /// Remove one exact minted mapping during a failed or retired seat.
    pub fn remove(&self, capability: wire::CanonicalUuid, actor: &wire::Actor) -> bool {
        let Ok(mut bindings) = self.bindings.write() else {
            return false;
        };
        if bindings
            .minted
            .get(&capability)
            .is_none_or(|binding| &binding.actor != actor)
        {
            return false;
        }
        let retired = bindings
            .minted
            .remove(&capability)
            .expect("matching capability remains under the registry lock")
            .retired;
        drop(bindings);
        retired.cancel();
        true
    }

    /// Admit a current hello and derive its principal server-side.
    pub fn admit(
        &self,
        hello: &wire::Hello,
        epoch: wire::CanonicalUuid,
        release: &wire::manifest::ProductVersion,
    ) -> Result<wire::Accepted, OpeningError> {
        self.admit_session(hello, epoch, release)
            .map(|admission| admission.accepted)
    }

    /// Admit one socket and retain the exact binding that authenticated it.
    pub(super) fn admit_session(
        &self,
        hello: &wire::Hello,
        epoch: wire::CanonicalUuid,
        release: &wire::manifest::ProductVersion,
    ) -> Result<CapabilityAdmission, OpeningError> {
        hello.admit(epoch).map_err(|rejected| match rejected {
            wire::Rejected::Wire(refusal) => OpeningError::Refused(refusal),
            wire::Rejected::Epoch(fault) => OpeningError::Fault(fault),
        })?;
        let (actor, retired) = self
            .bindings
            .read()
            .map_err(|_| {
                OpeningError::Fault(wire::Fault {
                    class: wire::FaultClass::Environment,
                    reason: "serve::capability-unavailable".to_owned(),
                    detail: "resident capability registry is unavailable".to_owned(),
                })
            })?
            .get(hello.capability)
            .map(|binding| (binding.actor.clone(), binding.retired.clone()))
            .ok_or_else(|| {
                OpeningError::Refused(wire::Refusal::bare(wire::RefusalDiagnostic::new(
                    "hello::capability-unknown",
                    None,
                    None,
                    None,
                )))
            })?;
        Ok(CapabilityAdmission {
            accepted: wire::Accepted {
                wire: wire::WireVersion::CURRENT,
                actor,
                epoch,
                release: release.clone(),
            },
            retired,
        })
    }

    /// The principal role heads this generation decides authority by. The
    /// resident publishes them before serving and after every principal
    /// change, so a role change governs that principal's next request.
    #[must_use]
    pub fn roles(&self) -> &crate::RoleBook {
        &self.roles
    }

    /// These capabilities deciding by `roles` instead of an unpublished book.
    #[cfg(test)]
    pub(super) fn with_roles(self, roles: crate::RoleBook) -> Self {
        Self { roles, ..self }
    }

    /// Whether `capability` authenticates a born principal.
    pub(super) fn is_principal(&self, capability: wire::CanonicalUuid) -> bool {
        let Ok(roles) = self.roles.current() else {
            return false;
        };
        self.bindings.read().ok().is_some_and(|bindings| {
            bindings
                .get(capability)
                .is_some_and(|binding| roles.roles(&binding.actor).is_some())
        })
    }

    pub(super) fn matches(&self, capability: wire::CanonicalUuid, actor: &wire::Actor) -> bool {
        self.bindings.read().ok().is_some_and(|bindings| {
            bindings
                .get(capability)
                .is_some_and(|binding| &binding.actor == actor)
        })
    }

    pub(super) fn contains(&self, capability: wire::CanonicalUuid) -> bool {
        self.bindings
            .read()
            .ok()
            .is_some_and(|bindings| bindings.get(capability).is_some())
    }

    /// The capability an app bootstrap hands the asking client. A presented
    /// capability must currently authenticate a born principal; only an opener
    /// presenting no capability may receive the local owner's.
    pub(super) fn serving(
        &self,
        presented: Option<wire::CanonicalUuid>,
    ) -> Option<wire::CanonicalUuid> {
        let capability = match presented {
            Some(capability) => capability,
            None => self.owner?,
        };
        self.is_principal(capability).then_some(capability)
    }
}

/// Only a lane or a born principal authenticates a connection.
fn refuse_noninteractive(actor: &wire::Actor) -> Result<(), CapabilityError> {
    match actor {
        wire::Actor::Spice => Err(CapabilityError::ResidentIsNotInteractive),
        wire::Actor::Lane(_) | wire::Actor::Principal(_) => Ok(()),
    }
}

/// Capability registration was structurally invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityError {
    Duplicate,
    ResidentIsNotInteractive,
    NotPrincipal,
    /// The shared registry could not be acquired.
    Unavailable,
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Duplicate => "app capability is already registered",
            Self::ResidentIsNotInteractive => "resident actor cannot own an interactive capability",
            Self::NotPrincipal => "resident discovery requires a born principal's capability",
            Self::Unavailable => "app capability registry is unavailable",
        })
    }
}

impl Error for CapabilityError {}
