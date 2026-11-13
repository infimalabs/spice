//! Fixed-memory validation, bounded windows, and streams for one artifact payload.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use wire::{Digest, Domain, TaskStamp};

use crate::model::{Artifact, ObjectMetadata};

use super::{
    Error, MAX_ARTIFACT_BYTES, METADATA, Store, decode_canonical, object_dir, open_regular_file,
    payload_path, read_cited, read_manifest, validate_manifest, validate_object,
    validate_payload_identity,
};

/// One opened payload whose citation, object metadata, length, digest, and
/// text encoding were proved before any bytes leave the store.
#[derive(Debug)]
pub struct ArtifactReader {
    artifact: Artifact,
    file: File,
    path: PathBuf,
}

impl ArtifactReader {
    /// The proved citation for this payload.
    #[must_use]
    pub const fn artifact(&self) -> &Artifact {
        &self.artifact
    }

    /// Fill `bytes` from `offset`, returning the bytes read.
    pub fn read_window(&mut self, offset: u64, bytes: &mut [u8]) -> Result<usize, Error> {
        let start = offset.min(self.artifact.bytes);
        let available =
            usize::try_from(self.artifact.bytes - start).expect("artifact byte bound fits usize");
        let read = bytes.len().min(available);
        self.file
            .seek(SeekFrom::Start(start))
            .and_then(|_| self.file.read_exact(&mut bytes[..read]))
            .map_err(|source| Error::io("read artifact payload window", &self.path, source))?;
        Ok(read)
    }

    /// Read at most `length` payload bytes from `offset`.
    pub fn window(&mut self, offset: u64, length: usize) -> Result<Vec<u8>, Error> {
        let available = usize::try_from(self.artifact.bytes.saturating_sub(offset))
            .expect("artifact byte bound fits usize");
        let mut bytes = vec![0; length.min(available)];
        let read = self.read_window(offset, &mut bytes)?;
        debug_assert_eq!(read, bytes.len());
        Ok(bytes)
    }

    /// Return the payload stream to its first byte.
    pub fn rewind(&mut self) -> Result<(), Error> {
        self.file
            .seek(SeekFrom::Start(0))
            .map(|_| ())
            .map_err(|source| Error::io("rewind artifact payload", &self.path, source))
    }
}

impl Read for ArtifactReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.file.read(bytes)
    }
}

impl Store {
    /// Open one cited payload after proving only that artifact's object and
    /// content. The returned reader supports bounded windows and streaming
    /// extraction without retaining the complete payload in memory.
    pub fn open_payload(
        &self,
        task: &TaskStamp,
        artifact_id: &str,
    ) -> Result<ArtifactReader, Error> {
        let _possession = self.acquire()?;
        let task_dir = self.task_dir(task);
        let manifest = read_manifest(&task_dir, task)?;
        validate_manifest(&task_dir, task, &manifest, false)?;
        let artifact = manifest
            .artifacts
            .into_iter()
            .find(|artifact| artifact.id == artifact_id)
            .ok_or_else(|| {
                Error::refused(
                    "artifact identity",
                    format!("task {task} has no artifact {artifact_id:?}"),
                )
            })?;
        let object = object_dir(&task_dir, &artifact.digest);
        let metadata_path = object.join(METADATA);
        let metadata_bytes = read_cited(&metadata_path, &artifact.id, "object metadata")?;
        let metadata = decode_canonical::<ObjectMetadata>(
            &metadata_bytes,
            &metadata_path.display().to_string(),
        )?;
        validate_object(&metadata_path.display().to_string(), &metadata, &artifact)?;

        let path = payload_path(&task_dir, &artifact.digest);
        let mut file = open_cited(&path, &artifact.id, "payload")?;
        let observed = if artifact.media_type.is_text() {
            Digest::compute_reader(
                Domain::TaskArtifactPayload,
                Utf8Reader::new(&mut file),
                MAX_ARTIFACT_BYTES as u64,
            )
        } else {
            Digest::compute_reader(
                Domain::TaskArtifactPayload,
                &mut file,
                MAX_ARTIFACT_BYTES as u64,
            )
        }
        .map_err(|source| {
            if source.kind() == io::ErrorKind::InvalidData {
                Error::corrupt(path.display().to_string(), source.to_string())
            } else {
                Error::io("read cited artifact payload", &path, source)
            }
        })?;
        validate_payload_identity(&path.display().to_string(), observed, &artifact)?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| Error::io("rewind cited artifact payload", &path, source))?;
        Ok(ArtifactReader {
            artifact,
            file,
            path,
        })
    }
}

fn open_cited(path: &Path, artifact_id: &str, member: &str) -> Result<File, Error> {
    match open_regular_file(path) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => Err(Error::corrupt(
            path.display().to_string(),
            format!("artifact {artifact_id} {member} is missing"),
        )),
        Err(source) => Err(Error::io("open cited artifact member", path, source)),
        Ok(file) => {
            let metadata = file
                .metadata()
                .map_err(|source| Error::io("inspect cited artifact member", path, source))?;
            if metadata.file_type().is_file() {
                Ok(file)
            } else {
                Err(Error::corrupt(
                    path.display().to_string(),
                    format!("artifact {artifact_id} {member} is not a regular file"),
                ))
            }
        }
    }
}

/// A validating adapter that rejects malformed UTF-8 without retaining the
/// complete text payload. At most one incomplete scalar prefix crosses reads.
struct Utf8Reader<R> {
    inner: R,
    carry: [u8; 3],
    carried: usize,
}

impl<R> Utf8Reader<R> {
    const fn new(inner: R) -> Self {
        Self {
            inner,
            carry: [0; 3],
            carried: 0,
        }
    }
}

impl<R: Read> Read for Utf8Reader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(bytes)?;
        if read == 0 {
            return if self.carried == 0 {
                Ok(0)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "text artifact ends inside a UTF-8 scalar",
                ))
            };
        }

        let mut start = 0;
        if self.carried != 0 {
            let width = utf8_scalar_width(self.carry[0])
                .expect("only an incomplete UTF-8 scalar is carried");
            let needed = width - self.carried;
            if read < needed {
                self.carry[self.carried..self.carried + read].copy_from_slice(&bytes[..read]);
                self.carried += read;
                return Ok(read);
            }
            let mut scalar = [0_u8; 4];
            scalar[..self.carried].copy_from_slice(&self.carry[..self.carried]);
            scalar[self.carried..width].copy_from_slice(&bytes[..needed]);
            std::str::from_utf8(&scalar[..width]).map_err(invalid_utf8)?;
            self.carried = 0;
            start = needed;
        }

        match std::str::from_utf8(&bytes[start..read]) {
            Ok(_) => Ok(read),
            Err(error) if error.error_len().is_some() => Err(invalid_utf8(error)),
            Err(error) => {
                let valid = start + error.valid_up_to();
                let tail = &bytes[valid..read];
                self.carry[..tail.len()].copy_from_slice(tail);
                self.carried = tail.len();
                Ok(read)
            }
        }
    }
}

fn utf8_scalar_width(lead: u8) -> Option<usize> {
    match lead {
        0x00..=0x7f => Some(1),
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

fn invalid_utf8(error: std::str::Utf8Error) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("text artifact is not UTF-8: {error}"),
    )
}
