//! Reply body decoding for each closed answer shape.

use super::{AnswerShape, CommandAnswer};

impl AnswerShape {
    /// Decode a successful reply body into the answer this shape names.
    ///
    /// # Errors
    ///
    /// Returns the decoder error when the bytes are not that shape. Answers
    /// refuse unknown fields, so a body spelled for the wrong command is a
    /// decode failure rather than a silently truncated value.
    pub fn decode(self, bytes: &[u8]) -> Result<CommandAnswer, serde_json::Error> {
        self.decode_from(bytes)
    }

    /// Decode a reply body that was already parsed as part of its envelope.
    ///
    /// Equivalent to [`Self::decode`] of the value's encoding, without
    /// serializing a possibly megabyte-sized body only to parse it again.
    ///
    /// # Errors
    ///
    /// Returns the decoder error when the value is not that shape.
    pub fn decode_value(
        self,
        value: serde_json::Value,
    ) -> Result<CommandAnswer, serde_json::Error> {
        self.decode_from(value)
    }

    fn decode_from(self, source: impl AnswerSource) -> Result<CommandAnswer, serde_json::Error> {
        match self {
            Self::Outbox => Ok(CommandAnswer::Outbox(Box::new(source.read()?))),
            Self::LaneJobs => Ok(CommandAnswer::LaneJobs(Box::new(source.read()?))),
            Self::PrincipalDirectory => Ok(CommandAnswer::PrincipalDirectory(source.read()?)),
            Self::Principals => Ok(CommandAnswer::Principals(source.read()?)),
            Self::Principal => Ok(CommandAnswer::Principal(source.read()?)),
            Self::LaneJob => Ok(CommandAnswer::LaneJob(Box::new(source.read()?))),
            Self::Rack => Ok(CommandAnswer::Rack(Box::new(source.read()?))),
            Self::RoomScore
            | Self::RoomPost
            | Self::RoomLeave
            | Self::RoomContribute
            | Self::RoomContributionRead
            | Self::RoomConversationText => self.decode_room(source),
            Self::Board
            | Self::TaskDetail
            | Self::TaskRelations
            | Self::TaskTimeline
            | Self::Text
            | Self::Task
            | Self::TaskPlace
            | Self::Claim
            | Self::TaskInvite => self.decode_primary(source),
            Self::Directive
            | Self::Inbox
            | Self::SteerLine
            | Self::Consent
            | Self::ConsentQueue => self.decode_coordination(source),
            Self::Trust | Self::TrustReport | Self::TrustShow => self.decode_trust(source),
            Self::ConsentPolicy
            | Self::GlobalSettings
            | Self::VendorCatalog
            | Self::Lane
            | Self::LaneChange
            | Self::LaneRenewal
            | Self::State
            | Self::Recall
            | Self::Taste
            | Self::Recognition
            | Self::Quote
            | Self::Topic => self.decode_secondary(source),
        }
    }

    fn decode_primary(self, body: impl AnswerSource) -> Result<CommandAnswer, serde_json::Error> {
        Ok(match self {
            Self::Board => CommandAnswer::Board(Box::new(body.read()?)),
            Self::TaskDetail => CommandAnswer::TaskDetail(Box::new(body.read()?)),
            Self::TaskRelations => CommandAnswer::TaskRelations(Box::new(body.read()?)),
            Self::TaskTimeline => CommandAnswer::TaskTimeline(Box::new(body.read()?)),
            Self::Text => CommandAnswer::Text(body.read()?),
            Self::Task => CommandAnswer::Task(body.read()?),
            Self::TaskPlace => CommandAnswer::TaskPlace(Box::new(body.read()?)),
            Self::Claim => CommandAnswer::Claim(Box::new(body.read()?)),
            Self::TaskInvite => CommandAnswer::TaskInvite(Box::new(body.read()?)),
            _ => unreachable!("answer-shape decode partition is closed"),
        })
    }
    fn decode_coordination(
        self,
        body: impl AnswerSource,
    ) -> Result<CommandAnswer, serde_json::Error> {
        Ok(match self {
            Self::Directive => CommandAnswer::Directive(body.read()?),
            Self::Inbox => CommandAnswer::Inbox(body.read()?),
            Self::SteerLine => CommandAnswer::SteerLine(Box::new(body.read()?)),
            Self::Consent => CommandAnswer::Consent(body.read()?),
            Self::ConsentQueue => CommandAnswer::ConsentQueue(body.read()?),
            _ => unreachable!("coordination answer-shape partition is closed"),
        })
    }

    fn decode_trust(self, body: impl AnswerSource) -> Result<CommandAnswer, serde_json::Error> {
        Ok(match self {
            Self::Trust => CommandAnswer::Trust(body.read()?),
            Self::TrustReport => CommandAnswer::TrustReport(Box::new(body.read()?)),
            Self::TrustShow => CommandAnswer::TrustShow(Box::new(body.read()?)),
            _ => unreachable!("trust answer-shape partition is closed"),
        })
    }

    fn decode_secondary(self, body: impl AnswerSource) -> Result<CommandAnswer, serde_json::Error> {
        Ok(match self {
            Self::ConsentPolicy => CommandAnswer::ConsentPolicy(body.read()?),
            Self::GlobalSettings => CommandAnswer::GlobalSettings(body.read()?),
            Self::VendorCatalog => CommandAnswer::VendorCatalog(body.read()?),
            Self::Lane => CommandAnswer::Lane(Box::new(body.read()?)),
            Self::LaneChange => CommandAnswer::LaneChange(Box::new(body.read()?)),
            Self::LaneRenewal => CommandAnswer::LaneRenewal(Box::new(body.read()?)),
            Self::State => CommandAnswer::State(body.read()?),
            Self::Recall => CommandAnswer::Recall(Box::new(body.read()?)),
            Self::Taste => CommandAnswer::Taste(body.read()?),
            Self::Recognition => CommandAnswer::Recognition(body.read()?),
            Self::Quote => CommandAnswer::Quote(body.read()?),
            Self::Topic => CommandAnswer::Topic(body.read()?),
            _ => unreachable!("answer-shape decode partition is closed"),
        })
    }
}

/// Where one answer body's JSON comes from: received bytes, or a value its
/// envelope already parsed.
pub(super) trait AnswerSource {
    fn read<T: serde::de::DeserializeOwned>(self) -> Result<T, serde_json::Error>;
}

impl AnswerSource for &[u8] {
    fn read<T: serde::de::DeserializeOwned>(self) -> Result<T, serde_json::Error> {
        crate::decode(self)
    }
}

impl AnswerSource for serde_json::Value {
    fn read<T: serde::de::DeserializeOwned>(self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self)
    }
}
