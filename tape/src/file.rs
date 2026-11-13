//! One standalone file archived beside its plaintext.
//!
//! The seekable archive a sealed member uses, for a file that belongs to no
//! tape: it is staged beside its destination, reopened and proven to
//! reproduce every plaintext byte, and only then published. The plaintext is
//! never touched; removing it once the archive is published is the caller's
//! decision.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use crate::archive::{self, Archive, Frames};

/// Archive `plaintext` as `destination`, returning the plaintext length.
///
/// # Errors
///
/// Refuses an empty plaintext, which has no frame to archive, and a plaintext
/// that changed while it was archived; returns any failure to read, stage,
/// verify, or publish. A failure publishes nothing and removes the staged
/// file.
pub fn archive_file(plaintext: &Path, destination: &Path) -> io::Result<u64> {
    let directory = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "archive has no directory"))?;
    let staged = directory.join(crate::temporary_name());
    let published = stage(plaintext, &staged).and_then(|length| {
        atomic::Destination::new(destination)
            .and_then(|target| target.replace_from(&staged))
            .map_err(io::Error::other)?;
        Ok(length)
    });
    if published.is_err() {
        let _ = fs::remove_file(&staged);
    }
    published
}

/// Write a synchronized archive of `plaintext` to `staged` and prove that the
/// reopened archive reproduces the reopened plaintext to its end.
fn stage(plaintext: &Path, staged: &Path) -> io::Result<u64> {
    let mut source = File::open(plaintext)?;
    let length = source.metadata()?.len();
    if length == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an empty file has no archive",
        ));
    }
    {
        let mut file = crate::create_private(staged)?;
        archive::write(&mut Read::by_ref(&mut source).take(length), &mut file)?;
        file.sync_all()?;
    }
    let archive = Archive::open(File::open(staged)?)?;
    let archived = archive.length();
    let mut frames = archive.reader_from(0)?;
    if archived != length || !archive::identical(&mut frames, &mut File::open(plaintext)?)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the archive does not reproduce its plaintext",
        ));
    }
    Ok(length)
}

/// A published archive whose seek table has been validated.
pub struct ArchivedFile(Archive);

impl ArchivedFile {
    /// Open the archive at `path`.
    ///
    /// # Errors
    ///
    /// Returns the failure to open the file or an
    /// [`io::ErrorKind::InvalidData`] failure for a seek table this writer
    /// never produces.
    pub fn open(path: &Path) -> io::Result<Self> {
        File::open(path).and_then(Archive::open).map(Self)
    }

    /// Plaintext bytes the archive reproduces.
    #[must_use]
    pub fn length(&self) -> u64 {
        self.0.length()
    }

    /// The plaintext from `offset` to its end, each frame verified as it is
    /// decoded.
    ///
    /// # Errors
    ///
    /// Returns the failure to read or verify the first frame.
    pub fn read_from(self, offset: u64) -> io::Result<ArchivedReader> {
        self.0.reader_from(offset).map(ArchivedReader)
    }
}

/// Plaintext streamed from an [`ArchivedFile`].
pub struct ArchivedReader(Frames);

impl Read for ArchivedReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Scratch;

    #[test]
    fn an_archived_file_reproduces_its_plaintext_from_any_offset_and_leaves_it_in_place() {
        let scratch = Scratch::new("file-archive");
        let plain = scratch.path().join("log");
        let bytes: Vec<u8> = (0..600_000u32).map(|n| (n % 251) as u8).collect();
        fs::write(&plain, &bytes).expect("plaintext");
        let archived = scratch.path().join("log.zst");

        assert_eq!(
            archive_file(&plain, &archived).expect("archives"),
            bytes.len() as u64
        );
        assert_eq!(fs::read(&plain).expect("plaintext stays"), bytes);
        let file = ArchivedFile::open(&archived).expect("archive opens");
        assert_eq!(file.length(), bytes.len() as u64);
        let mut tail = Vec::new();
        file.read_from(300_000)
            .expect("reads")
            .read_to_end(&mut tail)
            .expect("decodes");
        assert_eq!(tail, bytes[300_000..]);
        let entries = fs::read_dir(scratch.path()).expect("lists").count();
        assert_eq!(entries, 2, "no staged file remains");
        scratch.remove();
    }
}
