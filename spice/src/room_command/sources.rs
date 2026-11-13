//! Cited room sources resolved against the authorities that own their bytes.
//!
//! Admission refuses a contribution whose sources do not resolve, and every
//! read projects the same resolution as an available or typed unavailable
//! link, so a source never reads as text it no longer yields.

use std::collections::BTreeSet;
use std::path::Path;

use wire::rooms::{RoomSource, RoomSourceUnavailable, RoomSourceView};
use wire::{Digest, Seq, SequenceCode, TaskStamp};

/// The authorities outside the journal that own cited bytes.
#[derive(Clone, Copy)]
pub(crate) struct SourceOwners<'a> {
    /// Parent of lane tapes.
    pub(crate) tape: &'a Path,
    /// Task-addressed artifact store.
    pub(crate) artifacts: &'a artifact::Store,
    /// Git directory whose object store owns cited repository source.
    pub(crate) git: &'a Path,
}

/// One Design room's sources, resolved against `owners` when present.
pub(crate) struct RoomSources<'a> {
    owners: Option<SourceOwners<'a>>,
    design: &'a TaskStamp,
    /// Digests retained on the Design, listed at the first artifact citation.
    retained: Option<Result<BTreeSet<Digest>, RoomSourceUnavailable>>,
    /// The repository, opened at the first repository citation.
    repository: Option<Result<gix::Repository, RoomSourceUnavailable>>,
}

impl<'a> RoomSources<'a> {
    /// Sources of `design`'s room; without owners, only journal facts resolve.
    pub(crate) const fn new(owners: Option<SourceOwners<'a>>, design: &'a TaskStamp) -> Self {
        Self {
            owners,
            design,
            retained: None,
            repository: None,
        }
    }

    /// Parent of lane tapes, when this reader can read them.
    pub(crate) fn tape(&self) -> Option<&'a Path> {
        self.owners.map(|owners| owners.tape)
    }

    /// Whether `source` yields its bytes to a record at `before`: a journal
    /// fact earlier than it, a hunk the tape still holds, an artifact retained
    /// on the Design, or a blob and lines the object store still holds at the
    /// cited commit. A cited contribution is an immutable journal record whose
    /// publication the command barrier and journal fold resolve before
    /// admission, so a recorded citation always yields it.
    pub(crate) fn check(
        &mut self,
        source: &RoomSource,
        before: Seq,
    ) -> Result<(), RoomSourceUnavailable> {
        match source {
            RoomSource::Journal { fact } => preceding_fact(*fact, before)
                .map(drop)
                .ok_or(RoomSourceUnavailable::Missing {}),
            RoomSource::Contribution { .. } => Ok(()),
            RoomSource::Transcript(hunk) => {
                let tape = self.tape().ok_or(RoomSourceUnavailable::Unreadable {})?;
                spice::quote::room_hunk_text(tape, hunk).map(drop)
            }
            RoomSource::Artifact { digest } => {
                let Some(owners) = self.owners else {
                    return Err(RoomSourceUnavailable::Unreadable {});
                };
                let design = self.design;
                match self
                    .retained
                    .get_or_insert_with(|| retained(owners.artifacts, design))
                {
                    Ok(retained) if retained.contains(digest) => Ok(()),
                    Ok(_) => Err(RoomSourceUnavailable::Missing {}),
                    Err(unavailable) => Err(unavailable.clone()),
                }
            }
            RoomSource::Git(source) => {
                let Some(owners) = self.owners else {
                    return Err(RoomSourceUnavailable::Unreadable {});
                };
                match self
                    .repository
                    .get_or_insert_with(|| super::git_source::open(owners.git))
                {
                    Ok(repository) => super::git_source::check(repository, source),
                    Err(unavailable) => Err(unavailable.clone()),
                }
            }
        }
    }

    /// `source` as a link a record at `before` can still follow, or why not.
    pub(crate) fn view(&mut self, source: RoomSource, before: Seq) -> RoomSourceView {
        match self.check(&source, before) {
            Ok(()) => RoomSourceView::Available { source },
            Err(unavailable) => RoomSourceView::Unavailable {
                source,
                unavailable,
            },
        }
    }
}

/// The journal position of `fact` when it yields its bytes to a record at
/// `before`: a fact earlier than that record.
pub(crate) fn preceding_fact(fact: SequenceCode, before: Seq) -> Option<Seq> {
    Seq::new(fact.get()).filter(|at| *at < before)
}

/// Digests of every artifact retained on `design`.
fn retained(
    store: &artifact::Store,
    design: &TaskStamp,
) -> Result<BTreeSet<Digest>, RoomSourceUnavailable> {
    store
        .list(design)
        .map(|artifacts| {
            artifacts
                .into_iter()
                .map(|artifact| artifact.digest)
                .collect()
        })
        .map_err(|_| RoomSourceUnavailable::Unreadable {})
}
