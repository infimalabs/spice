//! Source-backed room messages.
//!
//! A lane destination is delivered by exactly one native steering directive
//! opened in the message's own write, and a principal destination by one
//! actor-addressed steering input. Room authority adds write-once message
//! records.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{CanonicalUuid, Instant, SequenceCode, TaskStamp};

use super::{RoomAnchor, RoomAuthorityStamp, RoomSource, TranscriptHunk, all_unique};

/// Most independently qualified destinations one room publication may carry.
pub const ROOM_ROUTE_FANOUT_LIMIT: usize = 32;
/// Most source bytes one substantive room publication may account.
pub const ROOM_PUBLICATION_BYTES_LIMIT: usize = 64 * 1024;

/// A frozen room route destination.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RoomDestination {
    /// One durable external principal.
    Principal { principal: CanonicalUuid },
    /// One logical lane; later occupants of that lane inherit live duties.
    Lane { lane: CanonicalUuid },
    /// One task room, with no directed recipient duty.
    Task { task: TaskStamp },
}

impl RoomDestination {
    /// Whether this destination can owe a directed receipt.
    pub const fn is_audience(&self) -> bool {
        matches!(self, Self::Principal { .. } | Self::Lane { .. })
    }
}

/// One selector after admission resolved its audience and its own optional context.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomRoute {
    /// Frozen principal, lane, or task destination.
    pub destination: RoomDestination,
    /// This selector's task context, never borrowed from another route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<RoomAnchor>,
    /// Key of the principal direction a lane route concerns.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub direction: Option<String>,
}

/// Exact steering input addressed to one principal.
///
/// A publication route's input derives its directive from the publication
/// and principal; a later line body addressed to the principal carries its
/// own native directive. This is admission authority only; constructing it
/// does not mint or settle steering input.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrincipalInputRef {
    /// Steering directive of the input.
    pub directive: CanonicalUuid,
    /// Exact external principal the input addresses.
    pub principal: CanonicalUuid,
}

impl PrincipalInputRef {
    /// The input a publication route to `principal` mints.
    ///
    /// Deterministic in the publication and principal because a publication
    /// admits each principal at most once, so every reader maps the route to
    /// its steering head without a lookup.
    #[must_use]
    pub fn published(publication: &RoomAuthorityStamp, principal: CanonicalUuid) -> Self {
        let identity = format!("spice:principal-input:{publication}:{principal}");
        let digest = blake3::hash(identity.as_bytes());
        let mut bytes = [0_u8; 16];
        bytes.copy_from_slice(&digest.as_bytes()[..16]);
        Self {
            directive: CanonicalUuid::new(uuid::Uuid::from_bytes(bytes)),
            principal,
        }
    }

    /// Exact native steering directive.
    #[must_use]
    pub const fn directive(&self) -> CanonicalUuid {
        self.directive
    }
}

/// Semantic message verb.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "lowercase", deny_unknown_fields)]
pub enum RoomPublicationKind {
    /// Ordinary message publication.
    Message {},
}

/// One source-backed substantive publication shared by principal and lane admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RoomPublicationWire", into = "RoomPublicationWire")]
pub struct RoomPublication {
    publication: RoomAuthorityStamp,
    author: RoomDestination,
    source: RoomSource,
    content_bytes: u32,
    kind: RoomPublicationKind,
    routes: Vec<RoomRoute>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomPublicationWire {
    publication: RoomAuthorityStamp,
    author: RoomDestination,
    source: RoomSource,
    content_bytes: u32,
    kind: RoomPublicationKind,
    routes: Vec<RoomRoute>,
}

impl TryFrom<RoomPublicationWire> for RoomPublication {
    type Error = &'static str;

    fn try_from(value: RoomPublicationWire) -> Result<Self, Self::Error> {
        if !value.author.is_audience() {
            return Err("room publication author must be a principal or lane");
        }
        if value.content_bytes == 0 || value.content_bytes as usize > ROOM_PUBLICATION_BYTES_LIMIT {
            return Err("room publication content exceeds its positive byte bound");
        }
        if value.routes.len() > ROOM_ROUTE_FANOUT_LIMIT || !all_unique(&value.routes) {
            return Err("room publication routes must be unique and within the fanout bound");
        }
        for route in &value.routes {
            if matches!(route.destination, RoomDestination::Task { .. }) && route.context.is_some()
            {
                return Err("task destinations already name their context");
            }
            if route.direction.is_some()
                && (!matches!(route.destination, RoomDestination::Principal { .. })
                    || !matches!(value.author, RoomDestination::Lane { .. }))
            {
                return Err("only a lane's principal route names the direction it concerns");
            }
        }
        let principals = value
            .routes
            .iter()
            .filter_map(|route| match route.destination {
                RoomDestination::Principal { principal } => Some(principal),
                _ => None,
            });
        let principal_count = principals.clone().count();
        if principals.collect::<BTreeSet<_>>().len() != principal_count {
            return Err("room publication may route to each principal once");
        }
        if value.routes.is_empty() {
            return Err("message must name at least one independently qualified route");
        }
        Ok(Self {
            publication: value.publication,
            author: value.author,
            source: value.source,
            content_bytes: value.content_bytes,
            kind: value.kind,
            routes: value.routes,
        })
    }
}

impl From<RoomPublication> for RoomPublicationWire {
    fn from(value: RoomPublication) -> Self {
        Self {
            publication: value.publication,
            author: value.author,
            source: value.source,
            content_bytes: value.content_bytes,
            kind: value.kind,
            routes: value.routes,
        }
    }
}

impl RoomPublication {
    /// Admit one validated source-backed publication.
    pub fn try_new(
        publication: RoomAuthorityStamp,
        author: RoomDestination,
        source: RoomSource,
        content_bytes: u32,
        kind: RoomPublicationKind,
        routes: Vec<RoomRoute>,
    ) -> Result<Self, &'static str> {
        RoomPublicationWire {
            publication,
            author,
            source,
            content_bytes,
            kind,
            routes,
        }
        .try_into()
    }

    /// Stable identity of this admitted publication.
    #[must_use]
    pub const fn publication(&self) -> &RoomAuthorityStamp {
        &self.publication
    }

    /// Authenticated author frozen at admission.
    #[must_use]
    pub const fn author(&self) -> &RoomDestination {
        &self.author
    }

    /// Owning source link; content bytes remain with that authority.
    #[must_use]
    pub const fn source(&self) -> &RoomSource {
        &self.source
    }

    /// Accounted source-byte extent.
    #[must_use]
    pub const fn content_bytes(&self) -> u32 {
        self.content_bytes
    }

    /// Semantic publication operation.
    #[must_use]
    pub const fn kind(&self) -> &RoomPublicationKind {
        &self.kind
    }

    /// Independently qualified routes frozen at admission.
    #[must_use]
    pub fn routes(&self) -> &[RoomRoute] {
        &self.routes
    }

    /// Lane routes in route order, each delivered by one native directive.
    pub fn lane_routes(&self) -> impl Iterator<Item = (CanonicalUuid, Option<&RoomAnchor>)> {
        self.routes
            .iter()
            .filter_map(|route| match route.destination {
                RoomDestination::Lane { lane } => Some((lane, route.context.as_ref())),
                _ => None,
            })
    }

    /// Principal routes in route order.
    pub fn principal_routes(&self) -> impl Iterator<Item = (CanonicalUuid, Option<&RoomAnchor>)> {
        self.routes
            .iter()
            .filter_map(|route| match route.destination {
                RoomDestination::Principal { principal } => {
                    Some((principal, route.context.as_ref()))
                }
                _ => None,
            })
    }

    /// Deterministic input identity admitted by `principal`'s frozen route.
    ///
    /// No steering input is created by this projection.
    #[must_use]
    pub fn principal_input(&self, principal: CanonicalUuid) -> Option<PrincipalInputRef> {
        self.routes
            .iter()
            .any(|route| route.destination == RoomDestination::Principal { principal })
            .then(|| PrincipalInputRef::published(&self.publication, principal))
    }
}

/// A lane's authored assistant message, admitted from the exact transcript hunk
/// that owns its bytes.
///
/// The hunk's lane is the author and the hunk is the source, so the admitted
/// [`RoomPublication`] is derived from this record rather than restated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomConversed {
    /// Native moment identity of this publication.
    pub publication: RoomAuthorityStamp,
    /// The publication's exact body bytes in its author's transcript.
    pub hunk: TranscriptHunk,
    /// Semantic verb; `kind` is the fact discriminant.
    pub operation: RoomPublicationKind,
    /// Independently qualified routes frozen at admission.
    pub routes: Vec<RoomRoute>,
    /// Directives the same write opened for this message's lane deliveries.
    pub directed: Vec<CanonicalUuid>,
    /// Canonical deadline inherited by actor-addressed inputs this publication
    /// creates.
    pub expires: Instant,
}

impl RoomConversed {
    /// The publication this lane record admits.
    pub fn publication(&self) -> Result<RoomPublication, &'static str> {
        let bytes = self
            .hunk
            .text
            .end
            .checked_sub(self.hunk.text.start)
            .and_then(|bytes| u32::try_from(bytes).ok())
            .ok_or("room publication hunk is not a byte range")?;
        RoomPublication::try_new(
            self.publication.clone(),
            RoomDestination::Lane {
                lane: self.hunk.head.lane,
            },
            RoomSource::Transcript(self.hunk.clone()),
            bytes,
            self.operation.clone(),
            self.routes.clone(),
        )
    }
}

/// An authenticated principal root post whose text this fact owns.
///
/// Every lane delivery it makes is a principal direction its own write opens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomPosted {
    /// Native moment identity of this publication.
    pub publication: RoomAuthorityStamp,
    /// The principal's exact composed text.
    pub text: String,
    /// Semantic verb; `kind` is the fact discriminant.
    pub operation: RoomPublicationKind,
    /// Independently qualified routes frozen at admission.
    pub routes: Vec<RoomRoute>,
    /// Directives the same write opened for this message's lane deliveries.
    pub directed: Vec<CanonicalUuid>,
    /// Canonical deadline inherited by actor-addressed inputs this publication
    /// creates.
    pub expires: Instant,
}

impl RoomPosted {
    /// The publication this fact admits at its own journal position `at`,
    /// which owns the text.
    pub fn publication(
        &self,
        at: SequenceCode,
        principal: CanonicalUuid,
    ) -> Result<RoomPublication, &'static str> {
        let bytes = u32::try_from(self.text.len())
            .map_err(|_| "room publication content exceeds its positive byte bound")?;
        RoomPublication::try_new(
            self.publication.clone(),
            RoomDestination::Principal { principal },
            RoomSource::Journal { fact: at },
            bytes,
            self.operation.clone(),
            self.routes.clone(),
        )
    }
}

#[cfg(test)]
mod tests;
