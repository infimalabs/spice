//! The owner record a holder writes into its lock file, and what it says.
//!
//! The record's shape, the single rewrite that replaces it, and the reading that
//! decides whether a refusal the kernel returned is worth waiting out all live
//! here, because they share one invariant: a reader arriving at any instant
//! finds a whole record, or the empty file that spells release, and never a
//! record half replaced.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::process;

use serde::{Deserialize, Serialize};

use crate::lock::replacement_record;
use crate::protected::ProtectedGroup;

use super::LockOwnerReport;
#[cfg(unix)]
use super::process_state::{process_absent, process_group_terminal, process_terminal};

const OWNER_BYTES_LIMIT: u64 = 64 * 1024;

/// What one lock file says about who holds it and what outlives them.
///
/// The field order is load-bearing. Everything ahead of `protected_group` is
/// fixed for as long as the process writing it lives, so the group's slot sits
/// at the same offset in every record this owner will ever write, and the
/// guardian publishing into that offset cannot be made to write into the wrong
/// place by a record rewritten under it. `owner_released` follows the slot for
/// exactly that reason: it is the one field ahead of it that would otherwise
/// change width mid-life, and it changes width by appearing at all.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LockOwnerEvidence {
    pub(super) pid: u32,
    pub(super) cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) protected_group: Option<ProtectedGroup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) protected_guardian: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) owner_released: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) context: Option<String>,
}

impl LockOwnerEvidence {
    pub(super) fn report(&self) -> Option<LockOwnerReport> {
        (!self.owner_released).then(|| LockOwnerReport {
            pid: self.pid,
            cwd: self.cwd.clone(),
            context: self.context.clone(),
        })
    }

    /// What this record says about possession that outlived the owner naming it.
    ///
    /// A slot still holding no group answers the same way an absent one does:
    /// unbounded, the refusing answer. It cannot answer any other way, because
    /// a group nobody has named yet is not a group that has finished.
    #[cfg(unix)]
    pub(super) fn guardian_state(&self) -> GuardianState {
        let finished = self
            .protected_group
            .and_then(ProtectedGroup::group)
            .is_some_and(process_group_terminal)
            && (self.owner_released || process_terminal(self.pid));
        if !finished {
            return GuardianState::Unbounded;
        }
        match self.protected_guardian {
            Some(guardian) if !process_absent(guardian) => GuardianState::Pending,
            Some(_) => GuardianState::Departed,
            None => GuardianState::Unbounded,
        }
    }
}

/// How a record's protected topology bounds a reader that found the lock held.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GuardianState {
    /// A guardian is still holding possession for work that has already ended,
    /// so waiting for it costs only the moment it takes to notice.
    Pending,
    /// The guardian named here is gone, and a guardian releases what it held on
    /// its way out, so whatever the probe saw was not this record's possession.
    Departed,
    /// Nothing here bounds a wait: the protected work may still be running, or
    /// no guardian was ever named to end it.
    Unbounded,
}

/// Answer a refusal the kernel already returned against what the record bounds.
///
/// A guardian still holding possession for work that has finished is worth
/// waiting out. A guardian that departed between the probe and this reading
/// released what the probe saw, so the question goes back to the kernel rather
/// than being quoted from a holder that no longer exists. Nothing else bounds
/// the wait, and the refusal as collected stands.
///
/// Both answers are taken as callables because the departure this distinguishes
/// occupies only the moment between collecting a refusal and reading the record
/// against it. No caller can hold that window open, so the choice is made
/// decidable here instead of being observable only by reproducing the race.
#[cfg(unix)]
pub(super) fn resolve_refusal<T>(
    state: GuardianState,
    collected: T,
    wait: impl FnOnce() -> io::Result<T>,
    reask: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    match state {
        GuardianState::Pending => wait(),
        GuardianState::Departed => reask(),
        GuardianState::Unbounded => Ok(collected),
    }
}

/// What one optional owner record says about waiting for the lock it describes.
#[cfg(unix)]
pub(super) fn guardian_state(owner: Option<&LockOwnerEvidence>) -> GuardianState {
    owner.map_or(GuardianState::Unbounded, LockOwnerEvidence::guardian_state)
}

pub(super) fn owner_record(
    protected: Option<(ProtectedGroup, u32)>,
    owner_released: bool,
    context: Option<&str>,
) -> io::Result<LockOwnerEvidence> {
    Ok(LockOwnerEvidence {
        pid: process::id(),
        cwd: std::env::current_dir()?.display().to_string(),
        owner_released,
        protected_group: protected.map(|(group, _)| group),
        protected_guardian: protected.map(|(_, guardian)| guardian),
        context: context.map(ToOwned::to_owned),
    })
}

pub(super) fn write_owner(
    file: &fs::File,
    protected: Option<(ProtectedGroup, u32)>,
    owner_released: bool,
    context: Option<&str>,
) -> io::Result<()> {
    write_owner_with(file, protected, owner_released, context, || Ok(()))
}

/// Rewrite the record, offering the last instant its predecessor is still there.
///
/// `before_landing` runs immediately before the one write that lands the new
/// record, and after every step this function takes that a reader could see, so
/// what it finds there is what a reader arriving there would find. It is handed
/// no descriptor and is expected to open its own, because a reader inside this
/// window is a separate process, and a clone of this one would share the cursor
/// the write is about to use.
///
/// The window is the reason for the seam. It occupies only the last instant
/// before the replacement lands, which no caller can hold open, so the question
/// is made decidable here rather than left observable only by reproducing the
/// race -- the same reason resolve_refusal takes its two answers as callables.
pub(super) fn write_owner_with(
    file: &fs::File,
    protected: Option<(ProtectedGroup, u32)>,
    owner_released: bool,
    context: Option<&str>,
    before_landing: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let record = serde_json::to_vec(&owner_record(protected, owner_released, context)?)
        .map_err(io::Error::other)?;
    let spanned = file.metadata()?.len().min(OWNER_BYTES_LIMIT);
    let bytes = replacement_record(record, spanned);
    let mut writer = file;
    writer.seek(SeekFrom::Start(0))?;
    before_landing()?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    file.sync_data()
}

pub(super) fn clear_owner(file: &fs::File) -> io::Result<()> {
    file.set_len(0)?;
    file.sync_data()
}

pub(super) fn read_owner(file: &fs::File) -> io::Result<Option<LockOwnerEvidence>> {
    let length = file.metadata()?.len();
    if length == 0 {
        return Ok(None);
    }
    if length > OWNER_BYTES_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "owner record is neither empty nor a whole record: {length} bytes exceeds the \
                 {OWNER_BYTES_LIMIT}-byte limit"
            ),
        ));
    }
    let mut reader = file;
    reader.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(length as usize);
    reader.take(OWNER_BYTES_LIMIT).read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).map(Some).map_err(|source| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("owner record is neither empty nor a whole record: {source}"),
        )
    })
}
