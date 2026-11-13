//! The fact envelope: version, position, instant, attribution, and cause.

use std::cmp::Ordering;
use std::fmt;
use std::num::NonZeroU64;
use std::str::FromStr;

use serde::de::{Deserializer, Error as DeError};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

use crate::ident::{CanonicalUuid, Instant};

/// The crossing wire generation writers emit and readers admit.
///
/// This is a transparent number rather than a type-level constant because a
/// mismatch must be able to name the version it observed. A version that
/// refused to decode could not be reported back to the peer that sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WireVersion(pub u32);

impl WireVersion {
    /// The one version this build speaks.
    pub const CURRENT: Self = Self(1);

    /// True when this is the version this build speaks.
    #[must_use]
    pub const fn is_current(self) -> bool {
        self.0 == Self::CURRENT.0
    }
}

impl fmt::Display for WireVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A monotone journal position assigned by the committing transaction.
///
/// Positions are non-zero so that the "zero and null sentinels are invalid"
/// rule for `cause` is carried by the type rather than re-checked by callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(pub NonZeroU64);

impl Seq {
    /// Build a position, refusing the zero sentinel.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(inner) => Some(Self(inner)),
            None => None,
        }
    }

    /// The first position a journal assigns: the first four-character
    /// sequence code, so no position spells shorter.
    pub const FIRST: Self =
        Self(NonZeroU64::new(52 * 52 * 52).expect("first position is non-zero"));

    /// The position as a plain number.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Every text rendering of a position, for humans and agents alike, is its
/// condensed base52 sequence code. Serde keeps the transparent integer.
impl fmt::Display for Seq {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&crate::SequenceCode::new(self.get()), formatter)
    }
}

/// Text names a position only in its canonical base52 spelling: decimal
/// digits are themselves base52 digits, so no second spelling is admitted.
impl FromStr for Seq {
    type Err = crate::SequenceCodeError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::new(text.parse::<crate::SequenceCode>()?.get()).ok_or(crate::SequenceCodeError)
    }
}

/// Greatest journal position covered by one complete observation.
///
/// identity-law: non-crossing - derived from a journal Seq inside board and bus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct JournalCursor(Option<Seq>);

impl JournalCursor {
    #[must_use]
    pub const fn empty() -> Self {
        Self(None)
    }

    #[must_use]
    pub const fn at(position: Seq) -> Self {
        Self(Some(position))
    }

    #[must_use]
    pub const fn position(self) -> Option<Seq> {
        self.0
    }
}

/// The authenticated endpoint that caused an append.
///
/// The resident derives this from the accepted capability; client text cannot
/// choose it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Actor {
    /// The resident acting on its own behalf.
    Spice,
    /// A seated lane.
    Lane(CanonicalUuid),
    /// A durable external principal acting through its own capability.
    Principal(CanonicalUuid),
}
impl Actor {
    /// Deterministic principal used only by debug-build fixtures.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub const TEST_PRINCIPAL: Self = Self::Principal(CanonicalUuid::new(uuid::Uuid::nil()));

    /// The logical lane endpoint, when this actor is a lane.
    #[must_use]
    pub const fn lane(&self) -> Option<CanonicalUuid> {
        match self {
            Self::Lane(lane) => Some(*lane),
            Self::Spice | Self::Principal(_) => None,
        }
    }

    /// The durable principal endpoint, when this actor is external.
    #[must_use]
    pub const fn principal(&self) -> Option<CanonicalUuid> {
        match self {
            Self::Principal(principal) => Some(*principal),
            Self::Spice | Self::Lane(_) => None,
        }
    }
}

impl Actor {
    /// Prefix introducing a lane actor.
    const LANE: &'static str = "lane:";
    /// Prefix introducing an external principal.
    const PRINCIPAL: &'static str = "principal:";

    const fn rank(&self) -> u8 {
        match self {
            Self::Lane(_) => 0,
            Self::Principal(_) => 1,
            Self::Spice => 2,
        }
    }
}

impl Ord for Actor {
    /// Rank exactly as the wire spelling without rendering or allocation.
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank()
            .cmp(&other.rank())
            .then_with(|| match (self, other) {
                (Self::Lane(left), Self::Lane(right))
                | (Self::Principal(left), Self::Principal(right)) => left.cmp(right),
                _ => Ordering::Equal,
            })
    }
}

impl PartialOrd for Actor {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Text that does not name an actor.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("actor must be `spice`, `lane:<canonical-uuid>`, or `principal:<canonical-uuid>`: {0:?}")]
pub struct ActorError(String);

impl FromStr for Actor {
    type Err = ActorError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "spice" => Ok(Self::Spice),
            _ => text
                .strip_prefix(Self::LANE)
                .and_then(|lane| lane.parse().ok())
                .map(Self::Lane)
                .or_else(|| {
                    text.strip_prefix(Self::PRINCIPAL)
                        .and_then(|principal| principal.parse().ok())
                        .map(Self::Principal)
                })
                .ok_or_else(|| ActorError(text.to_owned())),
        }
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spice => formatter.write_str("spice"),
            Self::Lane(lane) => write!(formatter, "{}{lane}", Self::LANE),
            Self::Principal(principal) => write!(formatter, "{}{principal}", Self::PRINCIPAL),
        }
    }
}

impl Serialize for Actor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Actor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// Root attribution carried through the work.
///
/// An origin names the root of a causal chain, never the principal writing
/// one link of it. The fact that opens a chain mints its origin; every fact
/// descending from it through `cause` repeats that origin unchanged, however
/// many principals or resident generations the chain crosses. The journal
/// holds this as law rather than convention: the commit door refuses a cause
/// that crosses an origin boundary, and its thread index names one origin per
/// root. A `steer::sent` roots its chain at the resident that admitted it, so
/// the `steer::canceled`, `steer::escalated`, `steer::acked`, and
/// `steer::declined` facts that advance or retire the directive carry that same
/// `resident:<epoch>` even when a later generation writes them. Who wrote a
/// link is read from the envelope `actor` and `cause`, and from the body
/// `epoch` where a lane authenticated the fact. A fact with no live directive
/// to descend from, such as a `steer::missed`, opens a chain of its own at the
/// writing resident.
///
/// WIRE names ACK keys, task handles, lane ids, and repository revisions as
/// examples rather than a closed set, so the shape law is the one thing
/// enforced here: a non-empty class, a colon, and a non-empty value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Origin(String);

/// Text that does not name a root attribution.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("origin must be `<class>:<value>` with both parts non-empty: {0:?}")]
pub struct OriginError(String);

impl Origin {
    /// The attribution as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Origin {
    type Error = OriginError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        let valid = text
            .split_once(':')
            .is_some_and(|(class, value)| !class.is_empty() && !value.is_empty());
        if valid {
            Ok(Self(text))
        } else {
            Err(OriginError(text))
        }
    }
}

impl FromStr for Origin {
    type Err = OriginError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::try_from(text.to_owned())
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for Origin {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Origin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// A fact as it rests in the journal and crosses the socket.
///
/// `seq`, `at`, `actor`, and any inherited `origin` are assigned inside the
/// commit door. A client may propose domain data but cannot mint journal
/// position, time, attribution, or causality.
///
/// The parameter is the fact, not the body, because the two readers of a stored
/// row want different things from it. A consumer that knows which kind it asked
/// for names [`crate::Fact`] and gets the body typed and the discriminant checked
/// against it. A consumer sweeping rows it did not write learns the kind at
/// runtime and names [`crate::AnyFact`], which keeps the header meaningful without
/// pretending to a body type it cannot know. Both spell the same header once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope<T> {
    /// Current wire version.
    pub v: WireVersion,
    /// Durable observation cursor for this fact.
    pub seq: Seq,
    /// Commit instant.
    pub at: Instant,
    /// Principal that caused the append.
    pub actor: Actor,
    /// Root attribution carried through the work: the origin of the chain root
    /// this fact descends from, not the writer of this link.
    pub origin: Origin,
    /// Position of the immediately provoking fact; absent at a chain root.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub cause: Option<Seq>,
    /// Discriminant and body, typed or opaque per the parameter.
    pub fact: T,
}
