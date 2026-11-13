//! Source-qualified transcript sessions, reconstructed once and advanced by delta.

use std::collections::BTreeMap;
use std::path::PathBuf;

use journal::crossing::{AnyFact, CanonicalUuid, Envelope, FactKind, Seq, body};
use journal::{FactReadBudget, FactSnapshot, GuardError, Journal};

type Position = (u64, u64);
type Sessions = BTreeMap<(Position, Seq), TranscriptSession>;

/// Author and worktree authenticated by a source-qualified session declaration.
#[derive(Clone, Debug)]
pub struct TranscriptSession {
    /// Occupant that authored this session.
    pub occupant: CanonicalUuid,
    /// Vendor whose dialect decodes this session.
    pub vendor: String,
    /// Exact resumable vendor-session identity.
    pub identity: String,
    /// Worktree bound to this session.
    pub worktree: PathBuf,
}

/// Disposable session lookup over source-qualified declarations.
#[derive(Clone, Debug, Default)]
pub struct TranscriptSessions {
    sources: BTreeMap<(CanonicalUuid, CanonicalUuid), Sessions>,
}

#[cfg(test)]
mod tests;

impl TranscriptSessions {
    /// Recover source-qualified session bindings in journal order.
    #[must_use]
    pub fn from_snapshot(snapshot: &FactSnapshot) -> Self {
        let mut sessions = Self::default();
        for fact in snapshot.facts() {
            sessions.apply(fact);
        }
        sessions
    }

    /// Advance this disposable index with one fact in journal order.
    /// Facts other than source-qualified session declarations have no effect.
    pub fn apply(&mut self, envelope: &Envelope<AnyFact>) {
        let Some(session) = envelope.fact.read::<body::LaneSessionDeclared>() else {
            return;
        };
        let position = (session.member, session.offset);
        self.sources
            .entry((session.lane, session.source))
            .or_default()
            .insert(
                (position, envelope.seq),
                TranscriptSession {
                    occupant: session.occupant,
                    vendor: session.vendor.to_string(),
                    identity: session.identity,
                    worktree: session.worktree.into(),
                },
            );
    }

    /// Latest session at or before one position in an exact source generation.
    #[must_use]
    pub fn at(
        &self,
        lane: CanonicalUuid,
        source: CanonicalUuid,
        member: u64,
        offset: u64,
    ) -> Option<&TranscriptSession> {
        self.sources
            .get(&(lane, source))?
            .range(
                ..=(
                    (member, offset),
                    Seq::new(u64::MAX).expect("nonzero sequence"),
                ),
            )
            .next_back()
            .map(|(_, session)| session)
    }
}

/// Owned by one writer generation; successful pages advance the recovery cursor.
#[derive(Default)]
pub(super) struct TranscriptSessionState {
    sessions: TranscriptSessions,
    cursor: Option<Seq>,
}

impl TranscriptSessionState {
    pub(super) fn lookup(
        &mut self,
        journal: &Journal,
        lane: CanonicalUuid,
        source: CanonicalUuid,
        member: u64,
        offset: u64,
    ) -> Result<Option<TranscriptSession>, GuardError> {
        journal.read(|read| {
            while self.cursor != read.cursor()? {
                let page = read.selected_fact_page(
                    &[FactKind::LaneSessionDeclared],
                    self.cursor,
                    None,
                    FactReadBudget::RETAINED,
                )?;
                for fact in &page.rows {
                    self.sessions.apply(fact);
                }
                self.cursor = page.continuation.or(page.horizon);
            }
            Ok(self.sessions.at(lane, source, member, offset).cloned())
        })
    }
}
