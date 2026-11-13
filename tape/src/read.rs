//! Stable loci and the reads that resolve them.
//!
//! A locus is a position in captured bytes, not a position in storage, so it
//! survives the member it names being archived. One member's bytes resolve
//! from the first storage present: its plaintext, then its seekable archive.
//! Plaintext is authoritative wherever it survives: sealing removes it only
//! after an archive that reproduces it and the index naming it are durable, so
//! plaintext beside an index entry is a seal's unfinished cleanup, never a
//! stale copy. Only a missing file counts as absent; any other failure to
//! open, decode, or verify storage is returned rather than skipped, so
//! corruption never reads as a different storage or as an unissued member.
//!
//! Resolution consults the index first because sealing publishes the archive
//! before the index that names it: an index entry therefore proves its member
//! is readable, while the absence of one only means the member has not been
//! sealed *yet*.
//!
//! That ordering leaves exactly one race for a reader that takes no lock. A
//! reader can observe an index without member N, and by the time it opens N's
//! plaintext the sealer has published N and removed it. The plaintext is then
//! missing without the member being unknown. Re-reading the index tells the
//! two cases apart: an index that changed means a seal landed underneath and
//! the read retries against the sealed member, while an unchanged index means
//! the member was never issued.
//!
//! Bounded reads charge the plaintext bytes they decode before decoding them:
//! the range itself from plaintext and every frame the range touches from an
//! archive.

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::archive::Archive;
use crate::{Error, Extent, Operation, Refusal, Tape};

/// Times a reader re-resolves while a sealer publishes underneath it.
///
/// One retry is enough for the race this guards, because a member is sealed
/// once and never returns to plaintext. The remaining attempts exist so that a
/// reader racing several sealers in a row still terminates with an answer
/// rather than a refusal.
const ATTEMPTS: usize = 4;

/// A stable position in one tape.
///
/// The member is assigned when the first byte of the range is appended and
/// never changes. Nothing here records whether the member is plaintext or
/// sealed, because a caller holding a locus is not entitled to care.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Locus {
    /// Member holding these bytes.
    pub member: u64,
    /// Offset of the first byte within the member's plaintext.
    pub offset: u64,
    /// Bytes this locus covers.
    pub length: u64,
}

impl Locus {
    /// A locus over one member's byte range.
    #[must_use]
    pub const fn new(member: u64, offset: u64, length: u64) -> Self {
        Self {
            member,
            offset,
            length,
        }
    }

    /// The position just past this locus, unless it overflows.
    const fn end(self) -> Option<u64> {
        self.offset.checked_add(self.length)
    }
}

impl fmt::Display for Locus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "member {} offset {} length {}",
            self.member, self.offset, self.length
        )
    }
}

/// Plaintext a read decoded, ending where its range ends.
///
/// Decoding starts where the storage allows rather than where the range
/// does, so `offset` is at or before the range's own start: the range itself
/// for plaintext and its first frame for an archive. A reader walking backwards
/// uses the extra bytes instead of decoding them again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Window {
    /// Member offset of the first byte in `bytes`.
    pub offset: u64,
    /// Plaintext from `offset` through the end of the requested range.
    pub bytes: Vec<u8>,
}

/// The storage one member currently resolves to.
enum Stored {
    Plain { file: File, length: u64 },
    Archive(Archive),
}

impl Tape {
    /// Reproduce the exact bytes one locus covers.
    ///
    /// The same locus answers identically whether its member is still
    /// accepting appends or has been sealed.
    ///
    /// # Errors
    ///
    /// Refuses a locus naming a member this tape never issued, and a locus
    /// reaching past the end of its member. Returns an I/O failure when a
    /// member exists but cannot be read, decoded, or verified.
    pub fn read(&self, locus: Locus) -> Result<Vec<u8>, Error> {
        for _ in 0..ATTEMPTS {
            let index = self.read_index()?;

            if let Some(member) = index.find(locus.member) {
                validate(locus, member.length)?;
                let stored = self
                    .stored(locus.member, Some(member.length))?
                    .ok_or_else(|| vanished(locus.member))?;
                return self
                    .window(stored, locus, |_| Ok::<(), Error>(()))
                    .map(|window| exact(locus, window));
            }

            if locus.member > index.next_member() {
                return Err(unknown(locus));
            }

            if let Some(bytes) = self.read_active(locus)? {
                return Ok(bytes);
            }

            // The plaintext is gone. Either a seal landed between the two
            // observations, or this member was never issued at all.
            if self.read_index()? == index {
                return Err(unknown(locus));
            }
        }

        Err(vanished(locus.member))
    }

    /// Read exactly one range of a previously admitted extent, charging the
    /// plaintext it decodes before decoding it.
    ///
    /// The extent is an observation, not caller authority: its member and
    /// range must contain the locus. An active member that sealed after the
    /// observation is read from its immutable replacement. `charge` is called
    /// once, before any decoding, with the bytes this read decodes; its
    /// failure is returned and nothing is decoded.
    ///
    /// # Errors
    ///
    /// Refuses a mismatched or out-of-range locus, returns `charge`'s failure,
    /// and returns an I/O failure when the storage cannot be read, decoded, or
    /// verified.
    pub fn read_extent<E: From<Error>>(
        &self,
        locus: Locus,
        extent: Extent,
        charge: impl FnOnce(u64) -> Result<(), E>,
    ) -> Result<Vec<u8>, E> {
        self.read_window(locus, extent, charge)
            .map(|window| exact(locus, window))
    }

    /// Read one range of a previously admitted extent together with every
    /// earlier byte its storage decoded on the way, charging that work first.
    ///
    /// The returned window ends at the range's end and starts at or before
    /// its beginning; see [`Window`]. Charging is [`Self::read_extent`]'s.
    ///
    /// # Errors
    ///
    /// Returns [`Self::read_extent`]'s failures.
    pub fn read_window<E: From<Error>>(
        &self,
        locus: Locus,
        extent: Extent,
        charge: impl FnOnce(u64) -> Result<(), E>,
    ) -> Result<Window, E> {
        let stored = self.extent_storage(locus, extent)?;
        self.window(stored, locus, charge)
    }

    /// Stream an observed range through `buffer` without holding it.
    ///
    /// `visit` receives consecutive chunks of at most `buffer.len()` bytes
    /// covering exactly the range. Plaintext is read in place. An archive
    /// decodes from the frame holding the range's start, one frame at a time.
    /// Memory holds only the current frame and the archive's seek index.
    ///
    /// # Errors
    ///
    /// Refuses a mismatched or out-of-range locus, returns an I/O failure when
    /// the storage cannot be read or ends short of the range, and returns the
    /// first failure `visit` reports.
    ///
    /// # Panics
    ///
    /// Panics when `buffer` is empty, which could never make progress.
    pub fn stream_extent<E: From<Error>>(
        &self,
        locus: Locus,
        extent: Extent,
        buffer: &mut [u8],
        mut visit: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        assert!(!buffer.is_empty(), "a streamed read needs buffer space");
        let stored = self.extent_storage(locus, extent)?;
        let (operation, path, mut reader) = self.open_range(stored, locus)?;
        let mut remaining = locus.length;
        while remaining != 0 {
            let want = buffer
                .len()
                .min(usize::try_from(remaining).unwrap_or(usize::MAX));
            let read = match reader.read(&mut buffer[..want]) {
                Ok(0) => {
                    let short = io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("range ended {remaining} bytes short"),
                    );
                    return Err(Error::io(operation, path, short).into());
                }
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(Error::io(operation, path, error).into()),
            };
            visit(&buffer[..read])?;
            remaining -= read as u64;
        }
        Ok(())
    }

    /// Resolve the storage an observed extent's range reads from.
    fn extent_storage(&self, locus: Locus, extent: Extent) -> Result<Stored, Error> {
        if locus.member != extent.member {
            return Err(unknown(locus));
        }
        validate(locus, extent.length)?;
        self.stored(locus.member, extent.sealed.then_some(extent.length))?
            .ok_or_else(|| vanished(locus.member))
    }

    /// Resolve one member's storage in preference order, or `None` when no
    /// storage for it exists.
    ///
    /// `indexed` is the length the index names for a sealed member; plaintext
    /// and archives must agree with it exactly.
    fn stored(&self, member: u64, indexed: Option<u64>) -> Result<Option<Stored>, Error> {
        let path = self.plain_path(member);
        if let Some(file) = open_present(&path, Operation::ReadPlaintext)? {
            let length = file
                .metadata()
                .map_err(|error| Error::io(Operation::ReadPlaintext, &path, error))?
                .len();
            if let Some(indexed) = indexed.filter(|indexed| *indexed != length) {
                return Err(Error::Refused(Refusal::ResidueMismatch {
                    member,
                    indexed,
                    observed: length,
                }));
            }
            return Ok(Some(Stored::Plain { file, length }));
        }

        let path = self.archive_path(member);
        if let Some(file) = open_present(&path, Operation::ReadArchive)? {
            let archive = Archive::open(file)
                .map_err(|error| Error::io(Operation::ReadArchive, &path, error))?;
            if let Some(indexed) = indexed.filter(|indexed| *indexed != archive.length()) {
                let disagreement = io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "the archive reproduces {} byte(s) where the index names {indexed}",
                        archive.length()
                    ),
                );
                return Err(Error::io(Operation::ReadArchive, path, disagreement));
            }
            return Ok(Some(Stored::Archive(archive)));
        }

        Ok(None)
    }

    /// Decode a range from resolved storage, charging the work first.
    fn window<E: From<Error>>(
        &self,
        stored: Stored,
        locus: Locus,
        charge: impl FnOnce(u64) -> Result<(), E>,
    ) -> Result<Window, E> {
        let end = locus.end().ok_or_else(|| out_of_range(locus, u64::MAX))?;
        match stored {
            Stored::Plain { mut file, length } => {
                validate(locus, length)?;
                charge(locus.length)?;
                let path = self.plain_path(locus.member);
                let mut bytes = vec![0; span(locus, length)?];
                file.seek(SeekFrom::Start(locus.offset))
                    .and_then(|_| file.read_exact(&mut bytes))
                    .map_err(|error| Error::io(Operation::ReadPlaintext, path, error))?;
                Ok(Window {
                    offset: locus.offset,
                    bytes,
                })
            }
            Stored::Archive(mut archive) => {
                validate(locus, archive.length())?;
                charge(archive.decoded_work(locus.offset, end))?;
                let offset = if locus.length == 0 {
                    locus.offset
                } else {
                    Archive::frame_start(locus.offset)
                };
                let bytes = archive.decode_range(offset, end).map_err(|error| {
                    Error::io(
                        Operation::ReadArchive,
                        self.archive_path(locus.member),
                        error,
                    )
                })?;
                Ok(Window { offset, bytes })
            }
        }
    }

    /// Open a reader positioned at the first byte of a range.
    fn open_range(
        &self,
        stored: Stored,
        locus: Locus,
    ) -> Result<(Operation, PathBuf, Box<dyn Read>), Error> {
        match stored {
            Stored::Plain { mut file, length } => {
                let path = self.plain_path(locus.member);
                validate(locus, length)?;
                file.seek(SeekFrom::Start(locus.offset))
                    .map_err(|error| Error::io(Operation::ReadPlaintext, &path, error))?;
                Ok((Operation::ReadPlaintext, path, Box::new(file)))
            }
            Stored::Archive(archive) => {
                let path = self.archive_path(locus.member);
                validate(locus, archive.length())?;
                let reader = archive
                    .reader_from(locus.offset)
                    .map_err(|error| Error::io(Operation::ReadArchive, &path, error))?;
                Ok((Operation::ReadArchive, path, Box::new(reader)))
            }
        }
    }

    /// Read a range from the member still accepting appends.
    ///
    /// Reports `None` rather than an error when the plaintext is absent, so
    /// the caller can decide whether that means sealed or never issued.
    fn read_active(&self, locus: Locus) -> Result<Option<Vec<u8>>, Error> {
        let path = self.plain_path(locus.member);
        let Some(file) = open_present(&path, Operation::ReadPlaintext)? else {
            return Ok(None);
        };
        let length = file
            .metadata()
            .map_err(|error| Error::io(Operation::ReadPlaintext, &path, error))?
            .len();
        self.window(Stored::Plain { file, length }, locus, |_| {
            Ok::<(), Error>(())
        })
        .map(|window| Some(window.bytes))
    }
}

/// Open one storage file, reporting `None` only when it does not exist.
fn open_present(path: &std::path::Path, operation: Operation) -> Result<Option<File>, Error> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::io(operation, path, error)),
    }
}

/// The exact range out of a window that ends where the range ends.
fn exact(locus: Locus, mut window: Window) -> Vec<u8> {
    let skip = usize::try_from(locus.offset - window.offset)
        .expect("a decoded window's prefix fits in memory");
    window.bytes.drain(..skip);
    window.bytes
}

/// Refuse a locus whose member this tape never issued.
fn unknown(locus: Locus) -> Error {
    Error::Refused(Refusal::UnknownMember {
        member: locus.member,
    })
}

/// Refuse a member that no storage holds.
fn vanished(member: u64) -> Error {
    Error::Refused(Refusal::MemberVanished { member })
}

fn out_of_range(locus: Locus, length: u64) -> Error {
    Error::Refused(Refusal::OutOfRange { locus, length })
}

/// Refuse a locus that reaches past the bytes its member holds.
fn validate(locus: Locus, length: u64) -> Result<(), Error> {
    match locus.end() {
        Some(end) if end <= length => Ok(()),
        _ => Err(out_of_range(locus, length)),
    }
}

/// This locus's length as a buffer size, refusing one this target cannot hold.
fn span(locus: Locus, length: u64) -> Result<usize, Error> {
    usize::try_from(locus.length).map_err(|_| out_of_range(locus, length))
}
