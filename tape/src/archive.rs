//! Seekable Zstandard storage for sealed members.
//!
//! An archive is the Zstandard seekable format (upstream's
//! `contrib/seekable_format`): independent Zstandard
//! frames followed by a skippable seek table naming each frame's compressed
//! and decompressed size. Every frame carries Zstandard's own content
//! checksum, so any Zstandard decoder verifies it, and upstream's seekable
//! implementation writes and parses the table, so any seekable-format reader
//! can seek in a tape archive.
//!
//! The table's optional per-frame checksum column is left off. Upstream's
//! ranged decoder checks that column only when its output happens to end with
//! the frame's last block, so it cannot be relied on; a frame's own checksum is
//! checked by every whole-frame decode. Reads therefore decode whole frames,
//! each from exactly the compressed bytes the table names.
//!
//! Every frame but the last holds exactly [`FRAME_BYTES`] of plaintext. A table
//! that disagrees, or names a frame larger than the format can need, is
//! refused rather than trusted.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

use zstd_safe::seekable::{AdvancedSeekable, FrameLog, Seekable};
use zstd_safe::{CCtx, CParameter, DCtx, OutBuffer};

/// Plaintext bytes in every frame but an archive's last.
pub(crate) const FRAME_BYTES: u32 = 256 * 1024;

/// Zstandard compression level every archive is written at.
const LEVEL: i32 = 3;

/// Plaintext bytes compared at once while verifying an archive.
const COMPARE_BYTES: usize = 64 * 1024;

/// Bit of a Zstandard frame header descriptor declaring a content checksum.
const CONTENT_CHECKSUM: u8 = 0x04;

/// Compress everything `source` yields into one archive written to `sink`.
///
/// Memory is one plaintext frame, one compressed frame, and the compressor's
/// state; the seek table it accumulates holds eight bytes per frame.
///
/// # Errors
///
/// Returns the first failure reading `source`, compressing, or writing `sink`.
pub(crate) fn write(source: &mut impl Read, sink: &mut impl Write) -> io::Result<u64> {
    let mut context = CCtx::try_create()
        .ok_or_else(|| io::Error::other("cannot allocate a Zstandard compressor"))?;
    context
        .set_parameter(CParameter::CompressionLevel(LEVEL))
        .and_then(|_| context.set_parameter(CParameter::ChecksumFlag(true)))
        .map_err(zstd_error)?;
    let mut log = FrameLog::try_create(false)
        .ok_or_else(|| io::Error::other("cannot allocate a seek table"))?;
    let mut plaintext = vec![0; FRAME_BYTES as usize];
    let mut frame = Vec::with_capacity(zstd_safe::compress_bound(FRAME_BYTES as usize));
    let mut written = 0u64;
    loop {
        let read = fill(source, &mut plaintext)?;
        if read == 0 {
            break;
        }
        frame.clear();
        let compressed = context
            .compress2(&mut frame, &plaintext[..read])
            .map_err(zstd_error)?;
        sink.write_all(&frame)?;
        written += compressed as u64;
        log.log_frame(
            u32::try_from(compressed).map_err(|_| invalid("a compressed frame exceeds u32"))?,
            u32::try_from(read).map_err(|_| invalid("a plaintext frame exceeds u32"))?,
            None,
        )
        .map_err(zstd_error)?;
    }
    loop {
        let mut out = OutBuffer::around(&mut frame);
        let remaining = log.write_seek_table(&mut out).map_err(zstd_error)?;
        let table = out.as_slice();
        sink.write_all(table)?;
        written += table.len() as u64;
        if remaining == 0 {
            return Ok(written);
        }
    }
}

/// One opened archive and the frame it last decoded.
pub(crate) struct Archive {
    seekable: AdvancedSeekable<'static, File>,
    file: File,
    frames: u32,
    length: u64,
    context: DCtx<'static>,
    compressed: Vec<u8>,
    frame: Vec<u8>,
}

impl Archive {
    /// Load and validate an archive's seek table.
    ///
    /// # Errors
    ///
    /// Returns an I/O failure when the table cannot be read, or an
    /// [`io::ErrorKind::InvalidData`] failure when it names no frame or a
    /// frame layout this writer never produces.
    pub(crate) fn open(file: File) -> io::Result<Self> {
        let reader = file.try_clone()?;
        let seekable = Seekable::try_create()
            .ok_or_else(|| io::Error::other("cannot allocate a seekable Zstandard decoder"))?
            .init_advanced(Box::new(reader))
            .map_err(zstd_error)?;
        let frames = seekable.num_frames();
        if frames == 0 {
            return Err(invalid("the archive seek table names no frame"));
        }
        let bound = zstd_safe::compress_bound(FRAME_BYTES as usize);
        let mut length = 0u64;
        for index in 0..frames {
            let size = seekable
                .frame_decompressed_size(index)
                .map_err(zstd_error)?;
            let last = index + 1 == frames;
            if size == 0 || size > FRAME_BYTES as usize || (!last && size != FRAME_BYTES as usize) {
                return Err(invalid(&format!(
                    "archive frame {index} holds {size} byte(s), not the {FRAME_BYTES}-byte frame layout"
                )));
            }
            if seekable.frame_compressed_size(index).map_err(zstd_error)? > bound {
                return Err(invalid(&format!(
                    "archive frame {index} is larger than any {FRAME_BYTES}-byte frame compresses to"
                )));
            }
            length += size as u64;
        }
        Ok(Self {
            seekable,
            file,
            frames,
            length,
            context: DCtx::try_create()
                .ok_or_else(|| io::Error::other("cannot allocate a Zstandard decoder"))?,
            compressed: Vec::with_capacity(bound),
            frame: Vec::with_capacity(FRAME_BYTES as usize),
        })
    }

    /// Plaintext bytes the archive reproduces.
    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    /// Plaintext offset where the frame holding `offset` begins.
    pub(crate) fn frame_start(offset: u64) -> u64 {
        offset - offset % u64::from(FRAME_BYTES)
    }

    /// Plaintext bytes decoded to reproduce `[offset, end)`: every frame the
    /// range touches, in full. An empty range touches none.
    pub(crate) fn decoded_work(&self, offset: u64, end: u64) -> u64 {
        if offset == end {
            return 0;
        }
        let frame = u64::from(FRAME_BYTES);
        let last_end = ((end - 1) / frame + 1) * frame;
        last_end.min(self.length) - Self::frame_start(offset)
    }

    /// Decode `[start, end)` where `start` begins a frame.
    pub(crate) fn decode_range(&mut self, start: u64, end: u64) -> io::Result<Vec<u8>> {
        let capacity = usize::try_from(end - start)
            .map_err(|_| invalid("the decoded range exceeds this target's memory"))?;
        let mut bytes = Vec::with_capacity(capacity);
        let mut index = self.frame_index(start);
        while bytes.len() < capacity {
            let decoded = self.decode(index)?;
            let wanted = decoded.len().min(capacity - bytes.len());
            bytes.extend_from_slice(&decoded[..wanted]);
            index += 1;
        }
        Ok(bytes)
    }

    /// A reader yielding plaintext from `offset` to the end of the archive.
    pub(crate) fn reader_from(mut self, offset: u64) -> io::Result<Frames> {
        if offset >= self.length {
            return Ok(Frames {
                next: self.frames,
                at: 0,
                held: 0,
                archive: self,
            });
        }
        let index = self.frame_index(offset);
        let held = self.decode(index)?.len();
        Ok(Frames {
            next: index + 1,
            at: usize::try_from(offset - Self::frame_start(offset))
                .expect("a frame offset fits usize"),
            held,
            archive: self,
        })
    }

    fn frame_index(&self, offset: u64) -> u32 {
        u32::try_from(offset / u64::from(FRAME_BYTES)).expect("a validated frame index fits u32")
    }

    /// Decode one whole frame from exactly the bytes the table names,
    /// verifying its content checksum and size.
    fn decode(&mut self, index: u32) -> io::Result<&[u8]> {
        let expected = self
            .seekable
            .frame_decompressed_size(index)
            .map_err(zstd_error)?;
        let offset = self
            .seekable
            .frame_compressed_offset(index)
            .map_err(|_| invalid(&format!("archive frame {index} is not in its seek table")))?;
        let size = self
            .seekable
            .frame_compressed_size(index)
            .map_err(zstd_error)?;
        self.compressed.resize(size, 0);
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut self.compressed)?;
        let checksummed = self
            .compressed
            .get(4)
            .is_some_and(|descriptor| descriptor & CONTENT_CHECKSUM != 0);
        if !checksummed
            || zstd_safe::find_frame_compressed_size(&self.compressed).map_err(zstd_error)? != size
        {
            return Err(invalid(&format!(
                "archive frame {index} is not one checksummed Zstandard frame"
            )));
        }
        self.frame.clear();
        let decoded = self
            .context
            .decompress(&mut self.frame, &self.compressed)
            .map_err(zstd_error)?;
        if decoded != expected {
            return Err(invalid(&format!(
                "archive frame {index} decoded {decoded} of its {expected} byte(s)"
            )));
        }
        Ok(&self.frame[..decoded])
    }
}

/// Plaintext streamed one verified frame at a time.
pub(crate) struct Frames {
    archive: Archive,
    next: u32,
    at: usize,
    held: usize,
}

impl Read for Frames {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.at == self.held {
            if self.next == self.archive.frames || buffer.is_empty() {
                return Ok(0);
            }
            self.held = self.archive.decode(self.next)?.len();
            self.next += 1;
            self.at = 0;
        }
        let count = buffer.len().min(self.held - self.at);
        buffer[..count].copy_from_slice(&self.archive.frame[self.at..self.at + count]);
        self.at += count;
        Ok(count)
    }
}

/// Whether two readers yield exactly the same bytes to their ends.
///
/// # Errors
///
/// Returns the first failure either reader reports.
pub(crate) fn identical(left: &mut impl Read, right: &mut impl Read) -> io::Result<bool> {
    let mut ours = vec![0; COMPARE_BYTES];
    let mut theirs = vec![0; COMPARE_BYTES];
    loop {
        let read = fill(left, &mut ours)?;
        if fill(right, &mut theirs[..read.max(1)])? != read || ours[..read] != theirs[..read] {
            return Ok(false);
        }
        if read == 0 {
            return Ok(true);
        }
    }
}

/// Read until `buffer` is full or the reader ends, reporting bytes read.
fn fill(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn invalid(detail: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail.to_owned())
}

/// Name a Zstandard failure.
///
/// The seekable table reader reports a failed read or seek of the archive
/// file as one opaque code, so every failure here is data the caller cannot
/// trust.
fn zstd_error(code: usize) -> io::Error {
    invalid(&format!("zstd: {}", zstd_safe::get_error_name(code)))
}
