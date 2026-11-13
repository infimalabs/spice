//! The room family's rows of the command binding partitions.

use super::{AnswerShape, AnswerSource, Binding, BodyShape, CommandAnswer, CommandKind, Intent};

impl AnswerShape {
    pub(super) fn decode_room(
        self,
        body: impl AnswerSource,
    ) -> Result<CommandAnswer, serde_json::Error> {
        Ok(match self {
            Self::RoomScore => CommandAnswer::RoomScore(body.read()?),
            Self::RoomPost => CommandAnswer::RoomPost(body.read()?),
            Self::RoomLeave => CommandAnswer::RoomLeave(body.read()?),
            Self::RoomContribute => CommandAnswer::RoomContribute(body.read()?),
            Self::RoomContributionRead => CommandAnswer::RoomContributionRead(body.read()?),
            Self::RoomConversationText => CommandAnswer::RoomConversationText(body.read()?),
            _ => unreachable!("room answer decode partition is closed"),
        })
    }
}

impl CommandKind {
    pub(super) const fn room_binding(self) -> Binding {
        use AnswerShape as A;
        use BodyShape as B;

        match self {
            Self::RoomScore => Binding::of(B::RoomScore, A::RoomScore),
            Self::RoomPost => Binding::of(B::RoomPost, A::RoomPost),
            Self::RoomConversationText => {
                Binding::of(B::RoomConversationText, A::RoomConversationText)
            }
            Self::RoomLeave => Binding::of(B::RoomLeave, A::RoomLeave),
            Self::RoomContribute => Binding::of(B::RoomContribute, A::RoomContribute),
            Self::RoomContributionRead => {
                Binding::of(B::RoomContributionRead, A::RoomContributionRead)
            }
            _ => panic!("room command binding partition is closed"),
        }
    }
}

impl Intent {
    pub(super) const fn room_shape(&self) -> BodyShape {
        match self {
            Self::RoomScore(_) => BodyShape::RoomScore,
            Self::RoomPost(_) => BodyShape::RoomPost,
            Self::RoomConversationText(_) => BodyShape::RoomConversationText,
            Self::RoomLeave(_) => BodyShape::RoomLeave,
            Self::RoomContribute(_) => BodyShape::RoomContribute,
            Self::RoomContributionRead(_) => BodyShape::RoomContributionRead,
            _ => panic!("room intent shape partition is closed"),
        }
    }
}

impl BodyShape {
    pub(super) fn decode_room_body(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        Ok(match self {
            Self::RoomScore => Intent::RoomScore(crate::decode(bytes)?),
            Self::RoomPost => Intent::RoomPost(crate::decode(bytes)?),
            Self::RoomConversationText => Intent::RoomConversationText(crate::decode(bytes)?),
            Self::RoomLeave => Intent::RoomLeave(crate::decode(bytes)?),
            Self::RoomContribute => Intent::RoomContribute(crate::decode(bytes)?),
            Self::RoomContributionRead => Intent::RoomContributionRead(crate::decode(bytes)?),
            _ => unreachable!("only room shapes decode as room bodies"),
        })
    }
}
