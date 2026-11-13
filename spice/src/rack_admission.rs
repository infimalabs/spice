//! The journal fact admitting an installed executable's linked rack.
//!
//! A `rack::admitted` fact names a rack archive digest and its app generation.
//! Its sequence code is the generation short code a rack URL carries, so the
//! resident appends one per distinct archive at startup and the rack resource
//! reader resolves the code of the archive it links.

use journal::{FactReadBudget, GuardError, JournalRead};
use wire::body::RackAdmitted;
use wire::{Digest, FactKind, Seq, SequenceCode};

/// The sequence of the newest fact admitting `archive`, if one was appended.
///
/// Facts of the kind are read newest first; there is one per archive ever
/// installed, so the scan is a handful of rows.
///
/// # Errors
///
/// Fails when the journal's `rack::admitted` facts cannot be read.
pub fn admitted(read: &JournalRead<'_>, archive: &Digest) -> Result<Option<Seq>, GuardError> {
    let mut before = None;
    loop {
        let page =
            read.recent_fact_page(FactKind::RackAdmitted, before, FactReadBudget::RETAINED)?;
        let found = page.rows.iter().find(|fact| {
            fact.fact
                .read::<RackAdmitted>()
                .is_some_and(|body| body.archive == *archive)
        });
        if let Some(found) = found {
            return Ok(Some(found.seq));
        }
        match page.continuation {
            Some(next) => before = Some(next),
            None => return Ok(None),
        }
    }
}

/// The generation short code of the fact admitting `archive`, if one exists.
///
/// # Errors
///
/// Fails when the journal's `rack::admitted` facts cannot be read.
pub fn code(read: &JournalRead<'_>, archive: &Digest) -> Result<Option<SequenceCode>, GuardError> {
    admitted(read, archive).map(|seq| seq.map(|seq| SequenceCode::new(seq.get())))
}
