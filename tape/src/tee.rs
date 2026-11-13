//! Committing bytes before anything interprets them.
//!
//! The append and its directory are synchronized before the locus is returned:
//! bytes committed under a name that is not yet committed are bytes a crash
//! can leave unreachable. A caller that holds a locus therefore holds a promise
//! the filesystem has already made, which is what lets every decoder downstream
//! be allowed to fail: whatever it could not understand is still on disk, byte
//! for byte, at a position that outlives the process that captured it.

use std::io::Write;

use crate::{Error, Locus, Operation, Tape, open_member};

impl Tape {
    /// Append exact bytes and return the locus naming them.
    ///
    /// The bytes and their directory entry are synchronized before this
    /// returns. Nothing inspects, normalizes, or re-frames the bytes: a tee that
    /// understood its input would be a decoder, and a decoder that failed would
    /// take the capture down with it.
    ///
    /// # Errors
    ///
    /// Refuses when another writer holds the tape. Returns an I/O failure when
    /// the member cannot be opened, extended, or synchronized, or when the
    /// directory holding the member cannot be synchronized. No locus is
    /// returned in any of those cases.
    pub fn tee(&self, bytes: &[u8]) -> Result<Locus, Error> {
        let _possession = self.possess()?;
        // Residue of an interrupted seal is left for the next seal: removing it
        // means verifying an archive, which a capture path cannot afford, and
        // residue never receives appends because its member is indexed.
        let member = self.read_index()?.next_member();
        let path = self.plain_path(member);
        let mut file =
            open_member(&path).map_err(|error| Error::io(Operation::Append, &path, error))?;

        // Under the writer lock the end of the member is the offset this
        // append will occupy, so the locus is known before the write.
        let offset = file
            .metadata()
            .map_err(|error| Error::io(Operation::Append, &path, error))?
            .len();

        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| Error::io(Operation::Append, path, error))?;

        // A previous attempt may have created this member and then failed its
        // directory sync. Synchronize on every successful append so a retry
        // cannot mistake that unresolved name for a durable existing member.
        self.sync_directory()?;

        Ok(Locus::new(member, offset, bytes.len() as u64))
    }
}
