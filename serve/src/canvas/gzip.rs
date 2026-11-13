//! Spice's rack convention: every member of a Spice rack is one gzip stream
//! stored under its logical path plus exactly one `.gz`.
//!
//! The stored bytes answer a browser unchanged under `Content-Encoding: gzip`,
//! and a generic ZIP tool extracts a file whose name says how it is encoded.
//! Spice writes every stream with a fixed header at flate2's best level, so
//! the same files always yield the same rack. Every path taken or returned
//! here is the logical one.

use std::io::Write as _;

use flate2::{Compression, Crc, Decompress, FlushDecompress, Status};

/// The suffix every stored name carries beyond its logical path.
const SUFFIX: &str = ".gz";
/// gzip magic, deflate, no flags, mtime 0, no extra flags, unknown OS.
const HEADER: [u8; 10] = [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
const MAGIC: [u8; 2] = [0x1f, 0x8b];
const DEFLATE: u8 = 8;
const FIXED_HEADER_BYTES: usize = 10;
/// The gzip trailer: CRC-32 and size of the uncompressed bytes.
const TRAILER_BYTES: usize = 8;
const HEADER_CRC: u8 = 1 << 1;
const EXTRA: u8 = 1 << 2;
const NAME: u8 = 1 << 3;
const COMMENT: u8 = 1 << 4;
const RESERVED: u8 = 0xe0;

/// One Spice rack member: a borrowed gzip stream under its logical path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stream<'a> {
    path: &'a str,
    gzip: &'a [u8],
    deflate: &'a [u8],
    crc: u32,
    size: u32,
}

impl<'a> Stream<'a> {
    /// Read one rack member as a Spice stream.
    ///
    /// # Errors
    ///
    /// Refuses a stored name without the `.gz` suffix and stored bytes that
    /// are not one gzip stream.
    pub fn parse(member: rack::Member<'a>) -> Result<Self, rack::Error> {
        let path = logical_path(member.path())?;
        let gzip = member.bytes();
        let (deflate, crc, size) =
            parts(gzip).ok_or_else(|| refused(path, "stored bytes are not one gzip stream"))?;
        Ok(Self {
            path,
            gzip,
            deflate,
            crc,
            size,
        })
    }

    /// The member's logical path within the rack.
    #[must_use]
    pub const fn path(self) -> &'a str {
        self.path
    }

    /// The exact stored gzip stream.
    #[must_use]
    pub const fn gzip(self) -> &'a [u8] {
        self.gzip
    }

    /// Uncompressed size the stream's trailer declares.
    #[must_use]
    pub const fn size(self) -> u32 {
        self.size
    }

    /// Inflate this stream, requiring exactly one deflate stream of its
    /// declared size and CRC.
    ///
    /// # Errors
    ///
    /// Refuses, naming the member, bytes that are not exactly that stream.
    pub fn inflate(self) -> Result<Vec<u8>, rack::Error> {
        let refused = || refused(self.path, "is not exactly one gzip stream");
        // Inflation stops at the declared size, so a stream that would grow
        // past it ends short of `StreamEnd` and is refused.
        let mut inflated = Vec::with_capacity(self.size as usize);
        let mut inflater = Decompress::new(false);
        let status = inflater
            .decompress_vec(self.deflate, &mut inflated, FlushDecompress::Finish)
            .map_err(|_| refused())?;
        let mut crc = Crc::new();
        crc.update(&inflated);
        if status != Status::StreamEnd
            || inflater.total_in() != self.deflate.len() as u64
            || inflated.len() != self.size as usize
            || crc.sum() != self.crc
        {
            return Err(refused());
        }
        Ok(inflated)
    }
}

/// The stream stored for logical `path`.
///
/// # Errors
///
/// Refuses a stored member that is not one gzip stream.
pub fn get<'a>(rack: &rack::Rack<'a>, path: &str) -> Result<Option<Stream<'a>>, rack::Error> {
    rack.get(&format!("{path}{SUFFIX}"))
        .map(Stream::parse)
        .transpose()
}

/// The streams beneath `directory`, keyed by their logical path within it.
/// The empty directory names every member.
pub fn beneath<'r, 'a: 'r>(
    rack: &'r rack::Rack<'a>,
    directory: &str,
) -> impl Iterator<Item = Result<(&'a str, Stream<'a>), rack::Error>> + 'r {
    rack.beneath(directory).map(|(file, member)| {
        let stream = Stream::parse(member)?;
        Ok((logical_path(file)?, stream))
    })
}

/// Write the deterministic Spice rack of `files`, each a logical path and its
/// original bytes.
///
/// # Errors
///
/// Refuses what [`rack::write`] refuses.
pub fn write<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Result<Vec<u8>, rack::Error> {
    let stored = files
        .into_iter()
        .map(|(path, bytes)| (stored_name(path), compress(bytes)))
        .collect::<Vec<_>>();
    rack::write(
        stored
            .iter()
            .map(|(name, gzip)| (name.as_str(), gzip.as_slice())),
    )
}

/// The stored name of logical `path`.
#[must_use]
pub fn stored_name(path: &str) -> String {
    format!("{path}{SUFFIX}")
}

/// One deterministic gzip stream of `bytes`: the fixed header, raw deflate at
/// the best level, and trailer.
#[must_use]
pub fn compress(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::from(HEADER), Compression::best());
    encoder
        .write_all(bytes)
        .expect("deflating into memory cannot fail");
    let mut stream = encoder.finish().expect("deflating into memory cannot fail");
    let mut crc = Crc::new();
    crc.update(bytes);
    stream.extend_from_slice(&crc.sum().to_le_bytes());
    stream.extend_from_slice(&crc.amount().to_le_bytes());
    stream
}

/// The logical path of one stored name: the name less its `.gz` suffix.
fn logical_path(name: &str) -> Result<&str, rack::Error> {
    name.strip_suffix(SUFFIX)
        .ok_or_else(|| refused(name, "stored name lacks the .gz suffix"))
}

fn refused(path: &str, reason: &'static str) -> rack::Error {
    rack::Error::Member {
        path: path.to_owned(),
        reason,
    }
}

/// The deflate body, CRC, and declared size of one gzip stream.
fn parts(gzip: &[u8]) -> Option<(&[u8], u32, u32)> {
    let header = gzip.get(..FIXED_HEADER_BYTES)?;
    let flags = header[3];
    if header[..2] != MAGIC || header[2] != DEFLATE || flags & RESERVED != 0 {
        return None;
    }
    let mut rest = &gzip[FIXED_HEADER_BYTES..];
    if flags & EXTRA != 0 {
        let length = usize::from(u16::from_le_bytes(rest.get(..2)?.try_into().ok()?));
        rest = rest.get(2 + length..)?;
    }
    for field in [NAME, COMMENT] {
        if flags & field != 0 {
            rest = &rest[rest.iter().position(|byte| *byte == 0)? + 1..];
        }
    }
    if flags & HEADER_CRC != 0 {
        rest = rest.get(2..)?;
    }
    let (deflate, trailer) = rest.split_at(rest.len().checked_sub(TRAILER_BYTES)?);
    let crc = u32::from_le_bytes(trailer[..4].try_into().ok()?);
    let size = u32::from_le_bytes(trailer[4..].try_into().ok()?);
    Some((deflate, crc, size))
}

#[cfg(test)]
mod tests;
