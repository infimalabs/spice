//! The operator's settlement of a task, as its detail reads it back.

use wire::intent::TaskDetailAnswer;
use wire::{Actor, Instant, Phase, Seq};

/// How the operator settled a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlementAct<'a> {
    /// Closed by the operator while its workflow held `phase`.
    Closed { phase: &'a Phase },
    /// Withdrawn from the board.
    Deleted,
}

/// The one operator settlement a task's detail records, closure or
/// withdrawal, with the text the operator settled it on. Neither writes a room
/// observation, so this record is where that text reads back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettlementRecord<'a> {
    /// Closed or withdrawn.
    pub act: SettlementAct<'a>,
    /// Position of the settling fact.
    pub position: Seq,
    /// Commit time recorded by the journal.
    pub at: &'a Instant,
    /// Principal that settled the task.
    pub author: &'a Actor,
    /// The closure's validation or the withdrawal's reason.
    pub text: &'a str,
    /// Whose live claim the settlement overrode, when it overrode one.
    pub claimed: Option<&'a Actor>,
}

impl<'a> SettlementRecord<'a> {
    /// The settlement `detail` reads back, if the task was closed or deleted.
    #[must_use]
    pub fn of(detail: &'a TaskDetailAnswer) -> Option<Self> {
        if let Some(closure) = &detail.closure {
            return Some(Self {
                act: SettlementAct::Closed {
                    phase: &closure.phase,
                },
                position: closure.position,
                at: &closure.at,
                author: &closure.author,
                text: &closure.validation,
                claimed: closure.claimed.as_ref(),
            });
        }
        detail.deletion.as_ref().map(|deletion| Self {
            act: SettlementAct::Deleted,
            position: deletion.position,
            at: &deletion.at,
            author: &deletion.author,
            text: &deletion.reason,
            claimed: deletion.claimed.as_ref(),
        })
    }

    /// The card heading naming who settled the task, by `author`, and how.
    #[must_use]
    pub fn heading(&self, author: &str) -> String {
        match self.act {
            SettlementAct::Closed { phase } => format!("Closed by {author} at {phase}"),
            SettlementAct::Deleted => format!("Deleted by {author}"),
        }
    }
}
