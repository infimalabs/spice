//! Lane-authored conversation text shown by principal-addressed inputs. A
//! lane's bytes stay in its transcript, so the runtime reads each open
//! input's source once; a publication's bytes never change, so a settled read
//! stands.

use std::collections::BTreeSet;

use wire::intent::{RoomConcernedDirection, RoomConversationContent, RoomConversationTextAnswer};
use wire::rooms::RoomAuthorityStamp;
use wire::{Actor, Outcome, Response};

use super::AppRuntime;
use super::rooms::PrincipalInboxItem;
use crate::socket::Admitted;

/// One lane-authored conversation publication's text, read on demand from
/// the transcript that owns its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversationText {
    /// A read is queued or in flight.
    Loading,
    /// What the owning source supplied at the read barrier, with the
    /// principal direction the publication's route concerns and the exact
    /// actor that route reaches.
    Read {
        content: RoomConversationContent,
        direction: Option<Box<RoomConcernedDirection>>,
        recipient: Option<Actor>,
    },
    /// The resident refused or failed the read, with its stable reason.
    Failed(String),
}

impl AppRuntime {
    /// The on-demand text of one lane-authored conversation publication.
    #[must_use]
    pub fn conversation_text(&self, publication: &RoomAuthorityStamp) -> Option<&ConversationText> {
        self.conversation_texts.get(publication)
    }

    /// Open inputs addressed to this runtime's exact principal.
    #[must_use]
    pub fn principal_inbox(&self) -> Vec<PrincipalInboxItem<'_>> {
        let Some(principal) = self.principal.actor().principal() else {
            return Vec::new();
        };
        self.rooms.conversation().principal_inbox(principal)
    }

    /// Open principal inputs whose source text this runtime has not yet asked
    /// for, each now marked loading.
    pub fn begin_principal_input_texts(&mut self) -> Vec<RoomAuthorityStamp> {
        let wanted = self
            .principal_inbox()
            .into_iter()
            .filter_map(|entry| entry.conversation)
            .map(|conversation| conversation.publication.publication())
            .filter(|publication| !self.conversation_texts.contains_key(*publication))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        for publication in &wanted {
            self.conversation_texts
                .insert(publication.clone(), ConversationText::Loading);
        }
        wanted
    }

    /// Apply one correlated conversation text read without crossing
    /// capability scope or publication identity.
    pub fn apply_conversation_text(
        &mut self,
        publication: &RoomAuthorityStamp,
        admitted: &Admitted<Response<RoomConversationTextAnswer>>,
    ) {
        if admitted.scope() != self.accepted.scope() {
            return;
        }
        let text = match &admitted.get().outcome {
            Outcome::Reply(reply) if reply.body.publication == *publication => {
                ConversationText::Read {
                    content: reply.body.content.clone(),
                    direction: reply.body.direction.clone().map(Box::new),
                    recipient: reply.body.recipient.clone(),
                }
            }
            Outcome::Reply(_) => {
                ConversationText::Failed("room::conversation-text-identity-mismatch".to_owned())
            }
            Outcome::Refusal(refusal) => ConversationText::Failed(
                refusal
                    .diagnostic()
                    .map_or(
                        "room::conversation-text-refused",
                        wire::RefusalDiagnostic::reason,
                    )
                    .to_owned(),
            ),
            Outcome::Fault(fault) => ConversationText::Failed(fault.reason.clone()),
        };
        self.conversation_texts.insert(publication.clone(), text);
    }

    /// A read in flight at a dropped connection never answers; ask again.
    pub(super) fn forget_conversation_text_reads(&mut self) {
        self.conversation_texts
            .retain(|_, text| *text != ConversationText::Loading);
    }
}
