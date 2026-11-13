//! The socket opening: one current version, one capability, one epoch.

use serde::{Deserialize, Serialize};

use crate::envelope::{Actor, WireVersion};
use crate::ident::CanonicalUuid;
use crate::reply::{Fault, FaultClass, Refusal, RefusalDiagnostic};

/// HTTP response header carrying the browser/resident protocol generation.
///
/// The header is admitted before [`AppBootstrap`] is decoded, so a document
/// from another strict body generation can offer an explicit reload without
/// attempting to decode that body. The admitted body repeats the same clock in
/// its structured [`crate::Version`] identity.
pub const APP_PROTOCOL_VERSION_HEADER: &str = "x-spice-app-protocol-version";
/// HTTP response header carrying the immutable browser-app generation identity.
pub const APP_GENERATION_HEADER: &str = "x-spice-app-generation";

/// The browser/resident protocol version, independent of durable journal wire.
///
/// An app page can remain alive while an installed resident is replaced. Its
/// request and reply DTOs therefore need a pre-socket clock whose lifetime is
/// the browser application, rather than borrowing [`WireVersion`] from durable
/// facts that must remain readable across upgrades.
///
/// Every generation change is decided by the mandatory bootstrap header before
/// a socket opens or a strict DTO is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppProtocolVersion(pub u32);

impl AppProtocolVersion {
    /// The one app protocol this build speaks.
    pub const CURRENT: Self = Self(1);

    /// True when this is the app protocol this build speaks.
    #[must_use]
    pub const fn is_current(self) -> bool {
        self.0 == Self::CURRENT.0
    }
}

impl std::fmt::Display for AppProtocolVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// An app protocol header that is not its one unsigned decimal spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppProtocolVersionParseError;

impl std::fmt::Display for AppProtocolVersionParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "app protocol version must be a canonical unsigned decimal integer without padding",
        )
    }
}

impl std::error::Error for AppProtocolVersionParseError {}

impl std::str::FromStr for AppProtocolVersion {
    type Err = AppProtocolVersionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parsed = value
            .parse::<u32>()
            .map_err(|_| AppProtocolVersionParseError)?;
        if parsed.to_string() != value {
            return Err(AppProtocolVersionParseError);
        }
        Ok(Self(parsed))
    }
}

/// Authenticated same-origin bootstrap for one embedded app connection.
///
/// The browser obtains this current generation document from the origin that
/// served its exact embedded assets, then presents the capability only inside
/// [`Hello`]. It is not a durable discovery file and carries no historical or
/// fallback endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppBootstrap {
    /// Current HTTP origin the asking browser used and no other endpoint.
    pub endpoint: String,
    /// The connecting principal's capability for the immediately following
    /// opening.
    pub capability: CanonicalUuid,
    /// Resident generation the endpoint and capability address.
    pub epoch: CanonicalUuid,
    /// Product and build identity served at this bootstrap boundary.
    pub version: crate::reply::Version,
}

/// Gravatar's SHA-256 identity for one trimmed, lower-cased email address.
///
/// This is Gravatar's external key, not a WIRE [`crate::Digest`]: it is plain
/// SHA-256 with no domain tag, spelled as 64 lowercase hexadecimal digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GravatarHash(String);

impl GravatarHash {
    /// Character length of the one admitted spelling.
    const SPELLING_LEN: usize = 64;

    /// The hash as Gravatar's avatar path spells it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A Gravatar hash that is not 64 lowercase hexadecimal digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GravatarHashError;

impl std::fmt::Display for GravatarHashError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Gravatar hash must be 64 lowercase hexadecimal digits")
    }
}

impl std::error::Error for GravatarHashError {}

impl std::str::FromStr for GravatarHash {
    type Err = GravatarHashError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let admitted = text.len() == Self::SPELLING_LEN
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
        admitted
            .then(|| Self(text.to_owned()))
            .ok_or(GravatarHashError)
    }
}

impl Serialize for GravatarHash {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for GravatarHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Browser-safe speech path resolved for one admitted agent worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "backend", deny_unknown_fields)]
pub enum SpeechPlayback {
    /// Playback controls are absent.
    Disabled {},
    /// The browser speaks through its system synthesis implementation.
    System {
        /// Voice names in preference order; a device speaks with the first one
        /// it publishes. An empty list spreads agents across the device's own
        /// voices.
        voices: Vec<String>,
        /// Base speaking rate projected into the browser API.
        words_per_minute: u32,
    },
}

/// The first frame of every socket.
///
/// All three fields are required. The socket keeps a current-version-only
/// clock: there is no negotiation and no historical upcast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    /// Socket schema version the client speaks.
    pub wire: WireVersion,
    /// Capability authenticating a principal's or a lane's connection.
    pub capability: CanonicalUuid,
    /// Resident generation the client believes it is addressing.
    pub epoch: CanonicalUuid,
}

/// The resident's answer to an opening it accepted.
///
/// The resident derives actor and rights from the presented capability when it
/// accepts the connection; caller fields may describe provenance but cannot
/// grant it. This frame is how a client learns which principal it is acting
/// as, so it never has to assert one — and so a client that asserted one
/// anyway would be describing provenance the resident already ignored.
///
/// The capability is deliberately absent. It crosses once, inside [`Hello`],
/// and echoing it would place a live credential on the return path of every
/// accepted connection to tell the client something it already knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accepted {
    /// Socket schema version the resident speaks.
    pub wire: WireVersion,
    /// Principal the resident derived from the presented capability.
    pub actor: Actor,
    /// Resident generation this session addresses.
    pub epoch: CanonicalUuid,
    /// Product release the resident runs, so a client can name which side of
    /// a protocol cutover must move when the two do not speak one registry.
    pub release: crate::manifest::ProductVersion,
}

/// Why an opening did not begin a session.
///
/// identity-law: non-crossing - caller-local rejection before a session opens.
///
/// The two failures are deliberately different outcomes: a version the
/// resident does not speak is a refusal the client can act on, while an epoch
/// that has moved is a fault about a resident generation that no longer
/// exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    /// The client speaks a version this resident does not.
    Wire(Refusal),
    /// The client addressed a generation this resident has replaced.
    Epoch(Fault),
}

impl Hello {
    /// Admit this opening against the resident's current epoch.
    ///
    /// A wire mismatch refuses by naming both client and resident versions.
    /// An epoch mismatch faults by naming both epochs.
    ///
    /// # Errors
    ///
    /// Returns [`Rejected::Wire`] when the version is not current, and
    /// [`Rejected::Epoch`] when the addressed generation is not the resident's.
    pub fn admit(&self, resident_epoch: CanonicalUuid) -> Result<(), Rejected> {
        if !self.wire.is_current() {
            return Err(Rejected::Wire(Refusal::bare(RefusalDiagnostic::new(
                "hello::wire-mismatch",
                None,
                Some(WireVersion::CURRENT.to_string()),
                Some(self.wire.to_string()),
            ))));
        }
        if self.epoch != resident_epoch {
            return Err(Rejected::Epoch(Fault {
                class: FaultClass::Protocol,
                reason: "hello::epoch-mismatch".to_owned(),
                detail: format!(
                    "client addressed epoch {}; resident epoch is {resident_epoch}",
                    self.epoch
                ),
            }));
        }
        Ok(())
    }
}
