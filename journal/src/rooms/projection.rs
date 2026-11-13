//! How each admitted change moves the room projection.

use wire::TaskStamp;

use super::{Change, Projection, Refusal};

impl Projection {
    pub(super) fn apply(&mut self, change: Change) -> Result<(), Refusal> {
        match change {
            Change::Publication {
                room,
                publication,
                seq,
            } => {
                self.publications.insert((room, publication), seq);
            }
            Change::OopsGeneration { seq, body } => {
                self.oops_generation = Some((seq, body));
            }
            Change::Proposal { subject, row } => {
                self.proposals.insert(subject, row);
            }
            Change::Authorization { outputs, row } => {
                for output in outputs {
                    self.authorizations
                        .insert((output, row.decision.clone()), row.clone());
                }
            }
            Change::Cursor { lane, seq, body } => {
                self.cursors.insert(lane, (seq, body));
            }
            Change::Conversation(edit) => self.conversation.apply(edit),
            Change::MaturityNotice { seq, notice } => {
                self.notices.insert((notice.design.to_string(), seq), ());
                self.update_maturity(&notice.design, |maturity| {
                    maturity.observe_request(&notice);
                })?;
            }
            Change::MaturityContribution(contribution) => {
                self.update_maturity(&contribution.room, |maturity| {
                    maturity.observe_contribution(&contribution);
                })?;
            }
            Change::MaturityLeave(leave) => {
                if self.maturity.contains_key(leave.room.as_str()) {
                    self.update_maturity(&leave.room, |maturity| maturity.observe_leave(&leave))?;
                }
            }
        }
        Ok(())
    }

    fn update_maturity(
        &mut self,
        design: &TaskStamp,
        update: impl FnOnce(&mut wire::rooms::DesignMaturity),
    ) -> Result<(), Refusal> {
        let mut maturity = self
            .maturity
            .get(design.as_str())
            .map(|body| wire::decode(body))
            .transpose()?
            .unwrap_or_default();
        update(&mut maturity);
        self.maturity
            .insert(design.to_string(), wire::encode(&maturity)?);
        Ok(())
    }
}
