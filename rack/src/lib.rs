//! A rack: one deterministic ZIP whose members are stored with method STORE
//! and read in place.
//!
//! A member is its exact bytes under its exact name; the crate interprets
//! neither. [`Rack::admit`] delegates ZIP indexing and header consistency to
//! `zip`, then requires every member to be stored, unencrypted, and named by
//! a normalized relative path, so each lookup borrows the archive bytes and
//! neither copies nor decompresses. ZIP codec features are disabled.
//!
//! The default build only reads. Optional features add the rest:
//! `write` builds an archive from entries, `link` emits the object that links
//! an archive into an executable and extracts it from a built image, and
//! `linked` reads the archive linked into the running executable.

use std::fmt;
use std::io::Cursor;

#[cfg(feature = "link")]
pub mod link;
#[cfg(feature = "linked")]
mod linked;
#[cfg(feature = "write")]
mod write;

#[cfg(feature = "linked")]
pub use linked::linked;
#[cfg(feature = "write")]
pub use write::write;

// ZIP exposes central-header positions and raw names. Borrow the name at
// that position rather than retaining its temporary metadata allocation.
const ZIP_CENTRAL_HEADER_BYTES: u64 = 46;

/// One admitted rack whose members borrow the archive bytes.
#[derive(Clone, Debug)]
pub struct Rack<'a> {
    /// Every member, in path byte order.
    members: Box<[Member<'a>]>,
}

/// One stored member of an admitted rack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Member<'a> {
    path: &'a str,
    bytes: &'a [u8],
}

impl<'a> Member<'a> {
    /// The member's path within the rack.
    #[must_use]
    pub const fn path(self) -> &'a str {
        self.path
    }

    /// The member's exact stored bytes.
    #[must_use]
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }
}

impl<'a> Rack<'a> {
    /// Admit one rack and index its members.
    ///
    /// # Errors
    ///
    /// Refuses a ZIP whose records are truncated, inconsistent, or span
    /// disks; a member that is not stored, is encrypted, or whose path is not
    /// a normalized relative path; and a repeated path.
    pub fn admit(bytes: &'a [u8]) -> Result<Self, Error> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
        archive.validate_header_consistency()?;
        let mut members = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            members.push(indexed_member(bytes, &mut archive, index)?);
        }
        members.sort_unstable_by_key(|member| member.path);
        Ok(Self {
            members: members.into_boxed_slice(),
        })
    }

    /// The member stored at `path`.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<Member<'a>> {
        self.members
            .binary_search_by(|member| member.path.cmp(path))
            .ok()
            .map(|index| self.members[index])
    }

    /// Every member, in path byte order.
    #[must_use]
    pub fn members(&self) -> &[Member<'a>] {
        &self.members
    }

    /// The members beneath `directory`, keyed by their path within it, in
    /// order. The empty directory names every member.
    pub fn beneath(&self, directory: &str) -> impl Iterator<Item = (&'a str, Member<'a>)> + '_ {
        let prefix = if directory.is_empty() {
            String::new()
        } else {
            format!("{directory}/")
        };
        let start = self
            .members
            .partition_point(|member| member.path < prefix.as_str());
        self.members[start..].iter().map_while(move |member| {
            member
                .path
                .strip_prefix(prefix.as_str())
                .map(|file| (file, *member))
        })
    }
}

/// Why a rack cannot be admitted or written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// The end record or central directory disagrees with the archive bytes.
    Structure(&'static str),
    /// One member is malformed.
    Member { path: String, reason: &'static str },
    /// The upstream ZIP reader or writer refused the archive.
    Zip(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Structure(reason) => write!(formatter, "rack refused: {reason}"),
            Self::Zip(reason) => write!(formatter, "rack ZIP refused: {reason}"),
            Self::Member { path, reason } => {
                write!(formatter, "rack member {path:?} refused: {reason}")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<zip::result::ZipError> for Error {
    fn from(error: zip::result::ZipError) -> Self {
        Self::Zip(error.to_string())
    }
}

fn member_error(path: &str, reason: &'static str) -> Error {
    Error::Member {
        path: path.to_owned(),
        reason,
    }
}

/// Borrow one member from the ranges the ZIP reader has admitted.
fn indexed_member<'a>(
    bytes: &'a [u8],
    archive: &mut zip::ZipArchive<Cursor<&'a [u8]>>,
    index: usize,
) -> Result<Member<'a>, Error> {
    let file = archive.by_index_raw(index)?;
    let name_start = file
        .central_header_start()
        .checked_add(ZIP_CENTRAL_HEADER_BYTES)
        .ok_or(Error::Structure("ZIP member name offset overflows"))?;
    let name = archive_slice(bytes, name_start, file.name_raw().len() as u64)?;
    let path = std::str::from_utf8(name)
        .map_err(|_| member_error(&String::from_utf8_lossy(name), "path is not UTF-8"))?;
    check_path(path)?;
    if file.compression() != zip::CompressionMethod::Stored {
        return Err(member_error(path, "method is not STORE"));
    }
    if file.encrypted() {
        return Err(member_error(path, "stored bytes are encrypted"));
    }
    if file.size() != file.compressed_size() {
        return Err(member_error(path, "stored sizes disagree"));
    }
    let start = file
        .data_start()
        .ok_or_else(|| member_error(path, "stored bytes have no offset"))?;
    let bytes = archive_slice(bytes, start, file.compressed_size())?;
    Ok(Member { path, bytes })
}

fn archive_slice(bytes: &[u8], start: u64, length: u64) -> Result<&[u8], Error> {
    let refused = || Error::Structure("ZIP member range is outside the archive");
    let end = start.checked_add(length).ok_or_else(refused)?;
    let start = usize::try_from(start).map_err(|_| refused())?;
    let end = usize::try_from(end).map_err(|_| refused())?;
    bytes.get(start..end).ok_or_else(refused)
}

/// A normalized relative path: slash-separated, with no empty, dot, dot-dot,
/// backslash, or NUL segment.
fn check_path(path: &str) -> Result<(), Error> {
    if path.contains(['\\', '\0'])
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(member_error(path, "path is not a normalized relative path"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
