//! Exact file export for complete canonical documents and evidence.
//!
//! A bounded read never streams a whole document to the terminal; the caller
//! names a file instead. The bytes are published atomically beside the
//! destination, so a reader sees the previous file or the complete new one,
//! and the receipt carries the length and digest a later reader verifies
//! against. `-` is refused rather than read as standard output: that spelling
//! would turn the explicit export back into unbounded terminal output.

use std::io::{self, BufWriter, Read, Write};
use std::path::Path;

use wire::output::Export;
use wire::{Digest, Domain, Refusal, RefusalDiagnostic};

/// Bytes a producer's writes collect before reaching the staged file.
const WRITE_BUFFER_BYTES: usize = 1 << 20;

/// Why a produced export did not publish. The destination is unchanged.
#[derive(Debug)]
pub enum ExportFailure<E> {
    /// The destination was refused, or writing, syncing, or publishing to it
    /// failed, including a write the producer saw fail.
    Refused(Refusal),
    /// The producer stopped for its own reason.
    Produce(E),
}

/// Write `bytes` to `path` atomically and return the receipt.
///
/// # Errors
///
/// Refuses `-`, a destination with no file name or no existing parent, and
/// any write, sync, or rename failure; the destination is unchanged unless
/// only the final directory sync failed.
pub fn export(path: &Path, bytes: &[u8]) -> Result<Export, Refusal> {
    export_with(path, |writer| writer.write_all(bytes))
        .map_err(|failure| write_refusal(path, failure))
}

/// Stream exactly `expected_bytes` to `path` atomically and return its receipt.
///
/// # Errors
///
/// Refuses the same destinations and publication failures as [`export`], and
/// refuses a reader that ends early or continues past the expected length.
pub fn export_reader(
    path: &Path,
    reader: impl Read,
    expected_bytes: u64,
) -> Result<Export, Refusal> {
    export_with(path, |writer| {
        let copied = io::copy(&mut reader.take(expected_bytes.saturating_add(1)), writer)?;
        if copied < expected_bytes {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("export reader ended at {copied} bytes; expected {expected_bytes}"),
            ));
        }
        if copied > expected_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("export reader exceeds its expected {expected_bytes}-byte length"),
            ));
        }
        Ok(())
    })
    .map_err(|failure| write_refusal(path, failure))
}

/// Publish whatever `produce` writes to `path` atomically, hashing it as it
/// passes, and return the receipt.
///
/// The producer writes through a fixed buffer into the staged file, so a
/// document of any size never has to exist in memory at once.
///
/// # Errors
///
/// Refuses the same destinations and publication failures as [`export`]; a
/// write the producer saw fail is reported as that refusal. Otherwise the
/// producer's own failure is returned as [`ExportFailure::Produce`].
pub fn export_with<E>(
    path: &Path,
    produce: impl FnOnce(&mut dyn Write) -> Result<(), E>,
) -> Result<Export, ExportFailure<E>> {
    if path == Path::new("-") {
        return Err(ExportFailure::Refused(refusal(
            "output::export-stdout",
            "-".to_owned(),
            "name a file; a complete export never streams to standard output",
        )));
    }
    let destination = atomic::Destination::new(path).map_err(|error| {
        ExportFailure::Refused(refusal(
            "output::export-destination",
            path.display().to_string(),
            &error.to_string(),
        ))
    })?;
    let mut produced = None;
    let mut receipt = None;
    let published = destination.publish(|staged| {
        let mut file = Tracked {
            inner: BufWriter::with_capacity(WRITE_BUFFER_BYTES, staged),
            failure: None,
        };
        let hashed = Digest::write_through(Domain::CliExport, &mut file, |writer| {
            produce(writer).map_err(|error| {
                produced = Some(error);
                io::Error::other("the export producer stopped")
            })
        })
        .map(|(_, bytes, digest)| (bytes, digest));
        if let Some(failure) = file.failure.take() {
            // The producer only saw its write fail; the file is the cause.
            produced = None;
            return Err(failure);
        }
        let (bytes, digest) = hashed?;
        file.inner.flush()?;
        receipt = Some((bytes, digest));
        Ok(())
    });
    match (published, produced) {
        (_, Some(error)) => return Err(ExportFailure::Produce(error)),
        (Err(error), None) => {
            return Err(ExportFailure::Refused(refusal(
                "output::export-write",
                destination.path().display().to_string(),
                &error.to_string(),
            )));
        }
        (Ok(()), None) => {}
    }
    let (bytes, digest) = receipt.expect("successful publication measured its input");
    Ok(Export {
        path: destination.path().display().to_string(),
        bytes,
        digest,
    })
}

/// Remembers the first failure the staged file reported, so a producer that
/// wraps or swallows it cannot turn a write failure into its own.
struct Tracked<W> {
    inner: W,
    failure: Option<io::Error>,
}

impl<W: Write> Write for Tracked<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.inner
            .write(bytes)
            .inspect_err(|error| self.note(error))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().inspect_err(|error| self.note(error))
    }
}

impl<W> Tracked<W> {
    fn note(&mut self, error: &io::Error) {
        if self.failure.is_none() {
            self.failure = Some(io::Error::new(error.kind(), error.to_string()));
        }
    }
}

/// An I/O producer's failure is a failed write to the export.
fn write_refusal(path: &Path, failure: ExportFailure<io::Error>) -> Refusal {
    match failure {
        ExportFailure::Refused(refusal) => refusal,
        ExportFailure::Produce(error) => refusal(
            "output::export-write",
            path.display().to_string(),
            &error.to_string(),
        ),
    }
}

fn refusal(reason: &str, observed: String, detail: &str) -> Refusal {
    Refusal::bare(
        RefusalDiagnostic::new(
            reason,
            Some("output-export".to_owned()),
            Some("one complete document written to a named file".to_owned()),
            Some(observed),
        )
        .with_detail(detail),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn an_export_replaces_the_file_whole_and_its_receipt_verifies_the_bytes() {
        let root = std::env::temp_dir().join(format!("spice-export-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("fixture directory");
        let path = root.join("ledger.md");
        std::fs::write(&path, "a longer previous document").expect("fixture file");

        let document = "# Ledger\nnaïve 🦀\n".as_bytes();
        let receipt = export(&path, document).expect("export publishes");
        let written = std::fs::read(&receipt.path).expect("exported file reads");

        assert_eq!(written, document, "no residue of the previous file");
        assert_eq!(receipt.bytes, document.len() as u64);
        assert_eq!(receipt.digest, Digest::compute(Domain::CliExport, &written));
        assert!(
            export(Path::new("-"), document).is_err(),
            "a complete export never streams to standard output"
        );
        std::fs::remove_dir_all(&root).expect("fixture directory removes");
    }

    #[test]
    fn a_streaming_export_requires_the_exact_declared_length() {
        let root = std::env::temp_dir().join(format!("spice-stream-export-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("fixture directory");
        let path = root.join("payload.bin");
        std::fs::write(&path, b"previous").expect("fixture file");

        let payload = b"\0streamed payload\xff";
        let receipt = export_reader(&path, Cursor::new(payload), payload.len() as u64)
            .expect("exact stream publishes");
        assert_eq!(std::fs::read(&receipt.path).expect("read export"), payload);

        for expected in [payload.len() as u64 - 1, payload.len() as u64 + 1] {
            assert!(
                export_reader(&path, Cursor::new(payload), expected).is_err(),
                "length mismatch refuses"
            );
            assert_eq!(
                std::fs::read(&path).expect("read preserved export"),
                payload,
                "a refused stream leaves the destination unchanged"
            );
        }
        std::fs::remove_dir_all(&root).expect("fixture directory removes");
    }
}
