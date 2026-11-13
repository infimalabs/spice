//! Canonical transcript quote targets and their resolved wire shape.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::rooms::RoomAuthorityStamp;
use crate::{Actor, CanonicalUuid, Digest, Disposition, MessageLocus, TaskStamp};

const PREFIX: &str = "spice://quote/v1/";
const MARKDOWN_PREFIX: &str = "[quote](";

/// Greatest number of nested quote targets one expansion may follow.
pub const QUOTE_DEPTH_LIMIT: u8 = 8;

/// One durable source of quoted transcript text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum QuoteTarget {
    /// One typed event in an exact lane tape generation.
    Tape {
        /// Lane whose vendor tape owns the record.
        lane: CanonicalUuid,
        /// Opaque tape generation identity.
        source: CanonicalUuid,
        /// Exact record and event coordinates.
        locus: MessageLocus,
        /// Digest of the resolved UTF-8 prose.
        digest: Digest,
    },
    /// One directive in the journal steering-key index.
    Steering {
        /// Human-sized durable directive key.
        key: String,
        /// Digest of the directive's original UTF-8 text.
        digest: Digest,
    },
    /// One conversation publication in a task room.
    Room {
        /// Task whose room owns the publication.
        room: TaskStamp,
        /// The publication's room-authority identity.
        publication: RoomAuthorityStamp,
        /// Digest of the publication's UTF-8 text.
        digest: Digest,
    },

    /// One retained image in an exact tape event.
    TapeMedia {
        /// Lane whose vendor tape owns the record.
        lane: CanonicalUuid,
        /// Opaque tape generation identity.
        source: CanonicalUuid,
        /// Exact record and event coordinates.
        locus: MessageLocus,
        /// Zero-based position in the event's retained media.
        ordinal: u8,
        /// Digest of the selected image's decoded bytes.
        digest: Digest,
    },
    /// One retained image on a journal directive.
    SteeringMedia {
        /// Human-sized durable directive key.
        key: String,
        /// Zero-based position in the directive's retained attachments.
        ordinal: u8,
        /// Digest of the selected image's decoded bytes.
        digest: Digest,
    },
}

/// Why text resembling a quote target is not admitted.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum QuoteTargetError {
    /// The fixed scheme, authority, version, or path shape differs.
    #[error("quote target must use the canonical spice://quote/v1 grammar")]
    Shape,
    /// One typed path coordinate is malformed or non-canonical.
    #[error("invalid quote target {field}: {value:?}")]
    Coordinate {
        /// Coordinate whose parser refused the value.
        field: &'static str,
        /// Refused source text.
        value: String,
    },
    /// A steering key cannot cross a URI path segment.
    #[error("invalid quote target steering key: {0:?}")]
    SteeringKey(String),
}

/// Recorded principal that authored resolved quote text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum QuoteAuthor {
    /// Agent prose emitted by one occupant of a lane.
    Agent {
        /// Logical lane carrying the transcript.
        lane: CanonicalUuid,
        /// Occupant attributed by durable residency authority.
        occupant: CanonicalUuid,
    },
    /// Journal directive created by an authenticated principal.
    Directive {
        /// Actor on the directive's creating fact.
        actor: Actor,
    },
}

/// Who said a quoted message and whom it reached, as the source's own durable
/// record proves. Every structural appearance of one source carries the same
/// value: a directive context, a top-level quote, and a nested quote.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteAttribution {
    /// Durable recorded authorship.
    pub author: QuoteAuthor,
    /// Lanes the source was delivered to, in display order. Room posts and
    /// transcript prose name none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<QuoteRecipient>,
    /// Whether a message delivered the quoted source. The presented text is then
    /// that packet's authenticated body, without the header its recipient
    /// read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub packet: bool,
}

/// One lane a quoted source reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteRecipient {
    /// Lane the source was delivered to.
    pub lane: CanonicalUuid,
    /// Occupant the source's record proves was seated there when it was
    /// published; absent when the record names only the lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occupant: Option<CanonicalUuid>,
}

/// Browser presentation metadata for one image carrying its own quote action.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaQuote {
    /// Canonical target resolving exactly this image.
    pub target: QuoteTarget,
    /// Admitted source name shown in the composer quote shard.
    pub name: String,
}

/// One successfully resolved retained image.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedMediaQuote {
    /// Exact admitted image bytes and metadata.
    pub media: crate::Media,
    /// Durable recorded authorship of the owning source.
    pub author: QuoteAuthor,
    /// Current directive disposition; absent for tape media.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub disposition: Option<Disposition>,
}

/// Successfully resolved quote text and live journal presentation data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedQuote {
    /// Exact authenticated UTF-8 text.
    pub text: String,
    /// Durable recorded authorship.
    pub author: QuoteAuthor,
    /// Current directive disposition; absent for tape prose.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub disposition: Option<Disposition>,
}

/// Closed refusal reasons exposed instead of invented quote content.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuoteUnavailable {
    /// The target does not follow the admitted URI grammar.
    Malformed,
    /// The named source, event, or steering key does not exist.
    Unknown,
    /// Durable authority could not be read or decoded.
    Unreadable,
    /// Resolved text differs from the target's digest.
    DigestMismatch,
    /// Nested resolution reached [`QUOTE_DEPTH_LIMIT`].
    DepthExceeded,
}

/// One quote lookup result; unavailable values carry no authored content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum QuoteResolution {
    /// Named authority authenticated and supplied the quote.
    Resolved(ResolvedQuote),
    /// Named authority authenticated and supplied one retained image.
    Media(ResolvedMediaQuote),
    /// Resolution refused without fabricating text or authorship.
    Unavailable {
        /// Original target spelling that could not be resolved.
        target: String,
        /// Typed refusal class.
        reason: QuoteUnavailable,
    },
}
/// One recursively expanded fixed-label quote link ready for presentation.
///
/// Every node retains the exact original target. Resolved nodes carry only
/// authenticated source data, while unavailable nodes carry one closed refusal
/// class and no invented text or author.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum QuotePresentation {
    /// Durable authority authenticated this node and any nested quote links.
    Resolved {
        /// Original target spelling embedded in the containing message.
        target: String,
        /// Exact authenticated UTF-8 text.
        text: String,
        /// Who said this source and whom it reached.
        attribution: QuoteAttribution,
        /// Current directive disposition; absent for tape prose.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        disposition: Option<Disposition>,
        /// Quote links found in `text`, in source order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        quotes: Vec<QuotePresentation>,
        /// Images retained by this source, each with its exact quote target.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        media_quotes: Vec<MediaQuote>,
    },
    /// Durable authority authenticated one selected image.
    Media {
        /// Original target spelling embedded in the containing message.
        target: String,
        /// Exact admitted image bytes and metadata.
        media: crate::Media,
        /// Attribution of the owning source.
        attribution: QuoteAttribution,
        /// Current directive disposition; absent for tape media.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::optional::present"
        )]
        disposition: Option<Disposition>,
    },
    /// Resolution refused without fabricating presentation data.
    Unavailable {
        /// Original target spelling embedded in the containing message.
        target: String,
        /// Typed refusal class.
        reason: QuoteUnavailable,
    },
}

impl QuoteTarget {
    /// Digest the exact text a target authenticates.
    #[must_use]
    pub fn text_digest(text: &str) -> Digest {
        Digest::compute(crate::Domain::QuoteText, text.as_bytes())
    }

    /// Tape generation this target authenticates, if it names transcript prose.
    #[must_use]
    pub const fn tape_source(&self) -> Option<CanonicalUuid> {
        match self {
            Self::Tape { source, .. } | Self::TapeMedia { source, .. } => Some(*source),
            Self::Steering { .. } | Self::Room { .. } | Self::SteeringMedia { .. } => None,
        }
    }

    /// Render this target as the ordinary Markdown link stored in message text.
    #[must_use]
    pub fn markdown(&self) -> String {
        format!("{MARKDOWN_PREFIX}{self})")
    }

    /// Parse one whole fixed-label Markdown quote link.
    pub fn from_markdown(text: &str) -> Result<Self, QuoteTargetError> {
        let target = text
            .strip_prefix(MARKDOWN_PREFIX)
            .and_then(|text| text.strip_suffix(')'))
            .ok_or(QuoteTargetError::Shape)?;
        target.parse()
    }

    /// Digest carried by every target kind.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        match self {
            Self::Tape { digest, .. }
            | Self::Steering { digest, .. }
            | Self::Room { digest, .. }
            | Self::TapeMedia { digest, .. }
            | Self::SteeringMedia { digest, .. } => digest,
        }
    }

    /// Whether this target resolves one retained image rather than prose.
    #[must_use]
    pub const fn is_media(&self) -> bool {
        matches!(self, Self::TapeMedia { .. } | Self::SteeringMedia { .. })
    }
}

impl fmt::Display for QuoteTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(PREFIX)?;
        match self {
            Self::Tape {
                lane,
                source,
                locus,
                digest,
            } => write!(
                formatter,
                "tape/{lane}/{source}/{}/{}/{}/{}/{}",
                locus.member, locus.offset, locus.length, locus.event, digest
            ),
            Self::Steering { key, digest } => {
                write!(formatter, "steer/{key}/{digest}")
            }
            Self::Room {
                room,
                publication,
                digest,
            } => write!(formatter, "room/{room}/{publication}/{digest}"),
            Self::TapeMedia {
                lane,
                source,
                locus,
                ordinal,
                digest,
            } => write!(
                formatter,
                "tape-media/{lane}/{source}/{}/{}/{}/{}/{ordinal}/{digest}",
                locus.member, locus.offset, locus.length, locus.event
            ),
            Self::SteeringMedia {
                key,
                ordinal,
                digest,
            } => write!(formatter, "steer-media/{key}/{ordinal}/{digest}"),
        }
    }
}

impl FromStr for QuoteTarget {
    type Err = QuoteTargetError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let path = text.strip_prefix(PREFIX).ok_or(QuoteTargetError::Shape)?;
        let segments = path.split('/').collect::<Vec<_>>();
        match segments.as_slice() {
            ["tape", lane, source, member, offset, length, event, digest] => Ok(Self::Tape {
                lane: coordinate("lane", lane)?,
                source: coordinate("source", source)?,
                locus: MessageLocus {
                    member: number("member", member)?,
                    offset: number("offset", offset)?,
                    length: number("length", length)?,
                    event: number("event", event)?,
                },
                digest: coordinate("digest", digest)?,
            }),
            ["steer", key, digest] if steering_key(key) => Ok(Self::Steering {
                key: (*key).to_owned(),
                digest: coordinate("digest", digest)?,
            }),
            ["steer", key, _] => Err(QuoteTargetError::SteeringKey((*key).to_owned())),
            ["room", room, publication, digest] => Ok(Self::Room {
                room: coordinate("room", room)?,
                publication: coordinate("publication", publication)?,
                digest: coordinate("digest", digest)?,
            }),
            ["tape-media", fields @ ..] => tape_media(fields),
            ["steer-media", key, ordinal, digest] if steering_key(key) => Ok(Self::SteeringMedia {
                key: (*key).to_owned(),
                ordinal: number("ordinal", ordinal)?,
                digest: coordinate("digest", digest)?,
            }),
            ["steer-media", key, _, _] => Err(QuoteTargetError::SteeringKey((*key).to_owned())),
            _ => Err(QuoteTargetError::Shape),
        }
    }
}

fn tape_media(fields: &[&str]) -> Result<QuoteTarget, QuoteTargetError> {
    let [lane, source, member, offset, length, event, ordinal, digest] = fields else {
        return Err(QuoteTargetError::Shape);
    };
    Ok(QuoteTarget::TapeMedia {
        lane: coordinate("lane", lane)?,
        source: coordinate("source", source)?,
        locus: MessageLocus {
            member: number("member", member)?,
            offset: number("offset", offset)?,
            length: number("length", length)?,
            event: number("event", event)?,
        },
        ordinal: number("ordinal", ordinal)?,
        digest: coordinate("digest", digest)?,
    })
}

fn coordinate<T>(field: &'static str, text: &str) -> Result<T, QuoteTargetError>
where
    T: FromStr + fmt::Display,
{
    let parsed = text
        .parse::<T>()
        .map_err(|_| QuoteTargetError::Coordinate {
            field,
            value: text.to_owned(),
        })?;
    if parsed.to_string() != text {
        return Err(QuoteTargetError::Coordinate {
            field,
            value: text.to_owned(),
        });
    }
    Ok(parsed)
}

fn number<T>(field: &'static str, text: &str) -> Result<T, QuoteTargetError>
where
    T: FromStr + fmt::Display,
{
    coordinate(field, text)
}

/// Whether text has the shape of a current steering acknowledgment key.
#[must_use]
pub fn steering_key(key: &str) -> bool {
    crate::base52::is_moment_code(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(last: u8) -> CanonicalUuid {
        format!("018f82d4-6f59-7d06-92e3-bce9131163{last:02x}")
            .parse()
            .expect("canonical UUID")
    }

    #[test]
    fn every_target_round_trips_through_canonical_markdown() {
        let text = "the quoted words";
        let targets = [
            QuoteTarget::Tape {
                lane: uuid(1),
                source: uuid(2),
                locus: MessageLocus {
                    member: 3,
                    offset: 5,
                    length: 8,
                    event: 13,
                },
                digest: QuoteTarget::text_digest(text),
            },
            QuoteTarget::Steering {
                key: "dTVLR0qL0".to_owned(),
                digest: QuoteTarget::text_digest(text),
            },
            QuoteTarget::Room {
                room: "dVrD59Y8N".parse().expect("fixture room"),
                publication: "dVsYdDj7d".parse().expect("fixture publication"),
                digest: QuoteTarget::text_digest(text),
            },
            QuoteTarget::TapeMedia {
                lane: uuid(1),
                source: uuid(2),
                locus: MessageLocus {
                    member: 3,
                    offset: 5,
                    length: 8,
                    event: 13,
                },
                ordinal: 2,
                digest: Digest::compute(crate::Domain::MediaContent, b"image"),
            },
            QuoteTarget::SteeringMedia {
                key: "dTVLR0qL0".to_owned(),
                ordinal: 1,
                digest: Digest::compute(crate::Domain::MediaContent, b"image"),
            },
        ];
        for target in targets {
            let markdown = target.markdown();
            assert_eq!(QuoteTarget::from_markdown(&markdown), Ok(target));
        }
    }

    #[test]
    fn parser_refuses_noncanonical_coordinates_and_unknown_shapes() {
        let digest = QuoteTarget::text_digest("quote");
        let lane = uuid(1);
        let source = uuid(2);
        for target in [
            format!("spice://quote/v1/tape/{lane}/{source}/03/5/8/13/{digest}"),
            format!("spice://quote/v2/tape/{lane}/{source}/3/5/8/13/{digest}"),
            format!("spice://other/v1/tape/{lane}/{source}/3/5/8/13/{digest}"),
            format!("spice://quote/v1/steer/not-a-key/{digest}"),
            format!("spice://quote/v1/steer/dTVLR0qL/{digest}"),
            format!("spice://quote/v1/steer/dTVLR0qLA/{digest}"),
            format!("spice://quote/v1/room/not-a-stamp/dVsYdDj7d/{digest}"),
            "spice://quote/v1/room/dVrD59Y8N/dVsYdDj7d".to_owned(),
        ] {
            assert!(target.parse::<QuoteTarget>().is_err(), "{target}");
        }
    }
}
