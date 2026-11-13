//! Durable principals: their optional profile, held roles, and the facts and
//! commands that change them.
//!
//! A principal is born once, by an explicit admission fact, at an immutable
//! journal position. Its identity and birth position never change; a profile
//! edit or a role grant or revocation is its own fact and moves only that
//! principal's head. Git configuration is at most the source an admission
//! copies its profile from. A principal's [`PrincipalCode`] spells its birth
//! position, identifying the external identity as briefly as an occupant address
//! identifies an agent, but it is no addressing route: principals are reached
//! through exact input authority only.
//!
//! [`PrincipalRoles`] is the one predicate deciding what an actor may do as a
//! principal: the resident's command door, its operator-only controls, lane
//! selection, steering wake, operator-direction recognition, and review
//! coverage all ask it rather than comparing an actor against the operator.
//! [`PrincipalDirectory`] is the display counterpart: every surface naming an
//! acting or authoring principal reads that principal's recorded profile through
//! the same resolution, never Git configuration.

mod code;

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub use code::{PrincipalCode, PrincipalCodeError};

use crate::{Actor, CanonicalUuid, Digest, Domain, GravatarHash, Seq};

/// The durable principal named by an actor, absent for a lane or the resident.
#[must_use]
pub const fn actor_principal(actor: &Actor) -> Option<CanonicalUuid> {
    match actor {
        Actor::Principal(principal) => Some(*principal),
        Actor::Spice | Actor::Lane(_) => None,
    }
}

/// Every born principal's current role head at one journal horizon: the one
/// predicate deciding which principal actor holds which role.
///
/// A role change is a fact moving one head, so a predicate read after it
/// answers the changed role for that principal's next command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrincipalRoles {
    /// Born principals, oldest birth first.
    heads: Vec<PrincipalHead>,
}

impl PrincipalRoles {
    /// The predicate over these principal heads, in any order.
    #[must_use]
    pub fn new(heads: impl IntoIterator<Item = PrincipalHead>) -> Self {
        let mut heads: Vec<PrincipalHead> = heads.into_iter().collect();
        heads.sort_by_key(|head| head.born);
        Self { heads }
    }

    /// The current head of the principal `actor` names. Absent for a lane, the
    /// resident, or a principal never born.
    #[must_use]
    pub fn head(&self, actor: &Actor) -> Option<&PrincipalHead> {
        let principal = actor_principal(actor)?;
        self.heads.iter().find(|head| head.principal == principal)
    }

    /// Every born principal's current head, oldest birth first.
    #[must_use]
    pub fn heads(&self) -> &[PrincipalHead] {
        &self.heads
    }

    /// The roles recorded by `actor`'s current principal head.
    /// Debug builds also recognize the fixture principal in an empty book.
    #[must_use]
    pub fn roles(&self, actor: &Actor) -> Option<BTreeSet<PrincipalRole>> {
        #[cfg(debug_assertions)]
        if self.heads.is_empty() && actor == &Actor::TEST_PRINCIPAL {
            return Some(BTreeSet::from([PrincipalRole::Operator]));
        }
        self.head(actor).map(|head| head.roles.clone())
    }

    /// Whether the principal `actor` names currently holds `role`.
    #[must_use]
    pub fn holds(&self, actor: &Actor, role: PrincipalRole) -> bool {
        self.roles(actor).is_some_and(|roles| roles.contains(&role))
    }

    /// Every actor whose principal currently holds `role`, in actor order.
    /// An empty debug-build book also grants the fixture's operator role.
    #[must_use]
    pub fn holders(&self, role: PrincipalRole) -> Vec<Actor> {
        #[cfg(debug_assertions)]
        if self.heads.is_empty() && role == PrincipalRole::Operator {
            return vec![Actor::TEST_PRINCIPAL];
        }
        let mut holders: Vec<Actor> = self
            .heads
            .iter()
            .filter(|head| head.holds(role))
            .map(|head| Actor::Principal(head.principal))
            .collect();
        holders.sort();
        holders
    }

    /// The repository's first-born principal, absent before its birth.
    #[must_use]
    pub fn bootstrap(&self) -> Option<CanonicalUuid> {
        self.heads.first().map(|head| head.principal)
    }

    /// Each principal holding an issued capability, with that capability's
    /// digest.
    pub fn issued(&self) -> impl Iterator<Item = (CanonicalUuid, &Digest)> {
        self.heads.iter().filter_map(|head| {
            head.capability
                .as_ref()
                .map(|capability| (head.principal, capability))
        })
    }
}

/// One born principal's durable attribution and optional profile decoration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalDisplay {
    /// The principal displayed.
    pub principal: CanonicalUuid,
    /// The code its birth position spells.
    pub code: PrincipalCode,
    /// Recorded display name, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Recorded email address, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Uppercase badge derived from the recorded name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initials: Option<String>,
    /// Gravatar hash derived from the recorded email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gravatar: Option<GravatarHash>,
}

/// Every born principal's display identity, oldest birth first, at one
/// journal horizon: the one source naming an acting or authoring principal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrincipalDirectory(Vec<PrincipalDisplay>);

impl PrincipalDirectory {
    /// Each current head in birth order, with the complete display identity
    /// derived from its recorded profile.
    #[must_use]
    pub fn display(heads: &[PrincipalHead]) -> Self {
        Self(
            heads
                .iter()
                .map(|head| {
                    head.profile
                        .display(head.principal, PrincipalCode::of(head))
                })
                .collect(),
        )
    }

    /// Each head of `roles` by its immutable code alone, for an agent reader
    /// that shows no profile decoration.
    #[must_use]
    pub fn codes(roles: &PrincipalRoles) -> Self {
        Self(
            roles
                .heads()
                .iter()
                .map(|head| PrincipalDisplay {
                    principal: head.principal,
                    code: PrincipalCode::of(head),
                    name: None,
                    email: None,
                    initials: None,
                    gravatar: None,
                })
                .collect(),
        )
    }

    /// The principal `actor` names, absent for a lane, the resident, or a
    /// principal this directory does not hold.
    #[must_use]
    pub fn principal(&self, actor: &Actor) -> Option<&PrincipalDisplay> {
        let principal = actor_principal(actor)?;
        self.0.iter().find(|display| display.principal == principal)
    }

    /// How the principal `actor` names presents.
    #[must_use]
    pub fn identity(&self, actor: &Actor) -> Option<&PrincipalDisplay> {
        self.principal(actor)
    }

    /// `actor` as an agent reads it: by its principal's immutable code when
    /// it resolves, by [`Self::principal`], to a born principal.
    #[must_use]
    pub fn named<'a>(&'a self, actor: &'a Actor) -> NamedActor<'a> {
        NamedActor {
            actor,
            code: self.principal(actor).map(|display| display.code),
        }
    }
}

/// One actor as an agent reads it. A born principal reads as the immutable
/// code its birth position spells, never profile decoration or an
/// identity-class label, so renaming a profile never changes attribution.
/// Every other actor, including an unknown principal, reads as its identity
/// alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamedActor<'a> {
    /// The actor itself.
    pub actor: &'a Actor,
    /// The code of the born principal it resolves to.
    pub code: Option<PrincipalCode>,
}

impl fmt::Display for NamedActor<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            Some(code) => write!(formatter, "{code}"),
            None => write!(formatter, "{}", self.actor),
        }
    }
}

/// A principal's recorded display profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalProfile {
    /// Display name, absent when none was recorded.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub name: Option<String>,
    /// Email address, absent when none was recorded.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub email: Option<String>,
}

impl PrincipalProfile {
    /// The short name a recorded name yields: the first letters of its first
    /// two words, uppercased. Absent when no name, or only whitespace, was
    /// recorded.
    #[must_use]
    pub fn initials(&self) -> Option<String> {
        self.name.as_deref().and_then(|name| {
            let badge = name
                .split_whitespace()
                .take(2)
                .filter_map(|word| word.chars().next())
                .flat_map(char::to_uppercase)
                .collect::<String>();
            (!badge.is_empty()).then_some(badge)
        })
    }

    /// Complete presentation derived only from this recorded profile.
    #[must_use]
    pub fn display(&self, principal: CanonicalUuid, code: PrincipalCode) -> PrincipalDisplay {
        let initials = self.initials();
        let gravatar = self.email.as_ref().map(|email| {
            format!(
                "{:x}",
                Sha256::digest(email.trim().to_lowercase().as_bytes())
            )
            .parse()
            .expect("a SHA-256 lowercase hex spelling is a Gravatar hash")
        });
        PrincipalDisplay {
            principal,
            code,
            name: self.name.clone(),
            email: self.email.clone(),
            initials,
            gravatar,
        }
    }
}

/// One authority a principal may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrincipalRole {
    /// Operator authority over the repository's lanes, board, and policy.
    Operator,
}

impl PrincipalRole {
    /// The role as it is spelled on the wire and the command line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
        }
    }
}

impl std::str::FromStr for PrincipalRole {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "operator" => Ok(Self::Operator),
            other => Err(format!(
                "unknown principal role {other:?}; expected operator"
            )),
        }
    }
}

/// The digest the journal keeps of a capability issued to a principal; the
/// capability itself is handed off once and never journaled.
#[must_use]
pub fn capability_digest(capability: CanonicalUuid) -> Digest {
    Digest::compute(Domain::PrincipalCapability, capability.get().as_bytes())
}

/// One principal's current standing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalHead {
    /// Identity fixed at birth.
    pub principal: CanonicalUuid,
    /// Journal position of the admission fact that bore this principal.
    pub born: Seq,
    /// Current recorded profile.
    pub profile: PrincipalProfile,
    /// Roles currently held.
    pub roles: BTreeSet<PrincipalRole>,
    /// Digest of the capability this principal's admission issued, absent
    /// once revoked. The bootstrap principal has none: each resident
    /// generation mints its capability and publishes it locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<Digest>,
}

impl PrincipalHead {
    /// Whether this principal currently holds `role`.
    #[must_use]
    pub fn holds(&self, role: PrincipalRole) -> bool {
        self.roles.contains(&role)
    }
}

/// A principal was admitted with its first profile and roles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalAdmitted {
    /// Identity of the principal born here.
    pub principal: CanonicalUuid,
    /// Profile recorded at birth.
    pub profile: PrincipalProfile,
    /// Roles held from birth.
    pub roles: BTreeSet<PrincipalRole>,
    /// Digest of the capability handed off to this principal, absent for the
    /// bootstrap birth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<Digest>,
}

/// One principal's recorded profile changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalProfiled {
    /// Principal whose profile changed.
    pub principal: CanonicalUuid,
    /// Profile replaced by this fact.
    pub from: PrincipalProfile,
    /// Profile recorded by this fact.
    pub profile: PrincipalProfile,
}

/// One principal gained a role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalGranted {
    /// Principal gaining the role.
    pub principal: CanonicalUuid,
    /// Role granted.
    pub role: PrincipalRole,
}

/// One principal lost a role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalRevoked {
    /// Principal losing the role.
    pub principal: CanonicalUuid,
    /// Role revoked.
    pub role: PrincipalRole,
}

/// One principal's issued capability was retired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalDeauthorized {
    /// Principal whose capability was retired.
    pub principal: CanonicalUuid,
    /// Digest of the retired capability.
    pub capability: Digest,
}

/// Read every born principal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalRead {}
/// Read every principal's display identity and the connected principal's roles.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalDirectoryRead {}

/// Connected-readable principal identities and only the reader's current roles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalDirectoryAnswer {
    /// Every born principal's display identity, oldest birth first.
    pub principals: PrincipalDirectory,
    /// Roles held by the principal that issued this read.
    pub roles: BTreeSet<PrincipalRole>,
}

/// Bear one new principal.
///
/// With neither field given, the profile is copied from the repository's Git
/// `user.name` and `user.email`; otherwise exactly the given fields are
/// recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalAdmit {
    /// Explicit display name.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub name: Option<String>,
    /// Explicit email address.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub email: Option<String>,
    /// Roles held from birth.
    pub roles: BTreeSet<PrincipalRole>,
}

/// Change one principal's recorded profile. An absent field keeps its current
/// value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalEdit {
    /// Principal whose profile changes.
    pub principal: CanonicalUuid,
    /// Replacement display name.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub name: Option<String>,
    /// Replacement email address.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub email: Option<String>,
}

/// Grant or revoke one role of one principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalRoleRef {
    /// Principal whose role changes.
    pub principal: CanonicalUuid,
    /// Role granted or revoked.
    pub role: PrincipalRole,
}

/// Retire the capability one principal's admission issued, ending every
/// session it opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalDeauthorize {
    /// Principal whose capability is retired.
    pub principal: CanonicalUuid,
}

/// One born principal as a listing presents it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalListing {
    /// Current standing.
    pub head: PrincipalHead,
    /// Commit instant of the admission fact that bore this principal.
    pub admitted: crate::Instant,
}

/// Every born principal in birth order, as of the answering position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalsAnswer {
    /// Current principals, oldest birth first.
    pub principals: Vec<PrincipalListing>,
}

/// One principal's standing as an admission, profile edit, role change, or
/// capability revocation left it. Birth position is assigned at commit, so a
/// write answers without it; [`PrincipalsAnswer`] carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalStanding {
    /// Principal the write named or bore.
    pub principal: CanonicalUuid,
    /// Profile recorded after the write.
    pub profile: PrincipalProfile,
    /// Roles held after the write.
    pub roles: BTreeSet<PrincipalRole>,
    /// The capability an admission issued, handed off in that admission's
    /// reply alone. The command receipt and every fact keep only its digest,
    /// so a replayed admission never hands it off again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<CanonicalUuid>,
}
