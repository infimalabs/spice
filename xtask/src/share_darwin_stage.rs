use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use atomic::{RootedFiles, RootedPublish as _};
use dist::DarwinSigning;
use wire::manifest::{MacSymbolArtifact, ReleaseTarget};
use wire::{Digest, Domain};

use crate::{ShareDarwinRequest, share_darwin};

const PAYLOAD_PATH: &str = "targets/aarch64-apple-darwin/payload.json";
const PAYLOAD_LIMIT: u64 = 1024 * 1024;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Sign and notarize an already-built Darwin stage without exposing credentials
/// to any build program, then publish a fresh stage bound to the final bytes.
///
/// # Errors
///
/// Refuses a malformed input stage, an occupied output root, signing or
/// notarization failure, or an output that does not contain exactly the
/// inventoried payload and objects.
pub fn sign_staged_darwin(
    input: &Path,
    output: &Path,
    signing: DarwinSigning,
    log: &mut impl Write,
) -> Result<(), StagedDarwinError> {
    require_fresh_output(output)?;
    let unsigned = read_stage(input)?;
    let temporary = TemporaryExecutable::create(input, &unsigned.native)?;
    share_darwin(&ShareDarwinRequest::new(temporary.path(), signing), log)?;
    let signed = read_regular(temporary.path())?;
    finalize_stage(unsigned, signed, output, log)
}

/// Rebind an admitted unsigned Darwin stage to an independently signed native
/// executable without loading signing credentials.
///
/// # Errors
///
/// Refuses a malformed input stage, nonregular signed input, moved canvas,
/// occupied output root, or any finalized stage that differs from its payload.
pub fn finalize_staged_darwin(
    input: &Path,
    signed_native: &Path,
    output: &Path,
    log: &mut impl Write,
) -> Result<(), StagedDarwinError> {
    require_fresh_output(output)?;
    let unsigned = read_stage(input)?;
    let signed = read_regular(signed_native)?;
    finalize_stage(unsigned, signed, output, log)
}

fn finalize_stage(
    unsigned: ReadStage,
    signed: Vec<u8>,
    output: &Path,
    log: &mut impl Write,
) -> Result<(), StagedDarwinError> {
    require_canvas(&unsigned.payload, &signed)?;
    let finalized = dist::restage_signed_darwin(
        &unsigned.payload_bytes,
        &unsigned.native,
        &unsigned.symbols,
        signed,
    )?;
    let mut publisher = RootedFiles::new(output);
    for artifact in &finalized.artifacts {
        publisher.publish(&artifact.object_path(), artifact.mode, &artifact.bytes)?;
    }
    publisher.publish(&finalized.payload_path, 0o644, &finalized.payload_bytes)?;
    let verified = read_stage(output)?;
    require_exact_output(output, &verified)?;
    writeln!(
        log,
        "finalized signed Darwin payload {} at {}",
        finalized.payload_digest,
        output.join(PAYLOAD_PATH).display()
    )?;
    Ok(())
}

/// Admit one finalized Darwin stage without running build or signing code.
///
/// # Errors
///
/// Refuses a noncanonical payload, missing or extra objects, stale digests,
/// stale dSYM binding, or a native executable whose linked canvas moved.
pub fn verify_staged_darwin(stage: &Path) -> Result<(), StagedDarwinError> {
    let verified = read_stage(stage)?;
    require_exact_output(stage, &verified)
}

struct ReadStage {
    payload: wire::manifest::TargetPayload,
    payload_bytes: Vec<u8>,
    native: Vec<u8>,
    symbols: Vec<u8>,
    paths: BTreeSet<PathBuf>,
}

fn read_stage(root: &Path) -> Result<ReadStage, StagedDarwinError> {
    let payload_path = root.join(PAYLOAD_PATH);
    let payload_bytes = read_bounded(&payload_path, PAYLOAD_LIMIT)?;
    let admitted = proof::admit(&payload_bytes)?;
    let payload = admitted.value().clone();
    if payload.target != ReleaseTarget::Aarch64AppleDarwin {
        return Err(StagedDarwinError::Target(payload.target));
    }
    if payload.inventory.len() != 2 {
        return Err(StagedDarwinError::Inventory(
            "Darwin stage must contain exactly its executable and dSYM artifact".to_owned(),
        ));
    }
    let native_entry = payload
        .inventory
        .iter()
        .find(|entry| entry.path == payload.target.executable_path())
        .ok_or_else(|| StagedDarwinError::Inventory("missing Darwin executable".to_owned()))?;
    let symbol_entry = payload
        .inventory
        .iter()
        .find(|entry| entry.path == "symbols/spice.symbols")
        .ok_or_else(|| StagedDarwinError::Inventory("missing Darwin dSYM artifact".to_owned()))?;
    let native_path = object_path(root, &native_entry.digest, native_entry.mode);
    let symbol_path = object_path(root, &symbol_entry.digest, symbol_entry.mode);
    let native = read_exact(&native_path, native_entry.bytes)?;
    let symbols = read_exact(&symbol_path, symbol_entry.bytes)?;
    require_digest(&native_path, &native, &native_entry.digest)?;
    require_digest(&symbol_path, &symbols, &symbol_entry.digest)?;
    if payload.native != native_entry.digest {
        return Err(StagedDarwinError::Inventory(
            "payload native digest differs from its executable inventory".to_owned(),
        ));
    }
    let document = MacSymbolArtifact::decode(&symbols)
        .map_err(|error| StagedDarwinError::Inventory(error.to_string()))?;
    let metadata = payload
        .symbols
        .as_ref()
        .ok_or_else(|| StagedDarwinError::Inventory("missing Darwin symbol metadata".to_owned()))?;
    if document.executable != payload.native
        || metadata.executable != payload.native
        || metadata.digest != symbol_entry.digest
        || metadata.bytes != symbol_entry.bytes
        || metadata.mode != symbol_entry.mode
        || metadata.path != symbol_entry.path
        || metadata.uuid != document.uuid
    {
        return Err(StagedDarwinError::Inventory(
            "Darwin dSYM artifact is not bound to the staged executable".to_owned(),
        ));
    }
    require_canvas(&payload, &native)?;
    Ok(ReadStage {
        payload,
        payload_bytes,
        native,
        symbols,
        paths: BTreeSet::from([
            PathBuf::from(PAYLOAD_PATH),
            native_path
                .strip_prefix(root)
                .expect("derived object is rooted")
                .to_path_buf(),
            symbol_path
                .strip_prefix(root)
                .expect("derived object is rooted")
                .to_path_buf(),
        ]),
    })
}

fn require_canvas(
    payload: &wire::manifest::TargetPayload,
    executable: &[u8],
) -> Result<(), StagedDarwinError> {
    let linked = rack::link::extract(executable)
        .map_err(|error| StagedDarwinError::Canvas(error.to_string()))?;
    let observed = Digest::compute(Domain::DistArtifact, linked);
    if observed == payload.canvas {
        Ok(())
    } else {
        Err(StagedDarwinError::Canvas(format!(
            "expected {}, observed {observed}",
            payload.canvas
        )))
    }
}

fn require_exact_output(root: &Path, stage: &ReadStage) -> Result<(), StagedDarwinError> {
    let observed = regular_files(root)?;
    if observed == stage.paths {
        Ok(())
    } else {
        Err(StagedDarwinError::UnexpectedFiles { observed })
    }
}

fn regular_files(root: &Path) -> Result<BTreeSet<PathBuf>, StagedDarwinError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|source| StagedDarwinError::Io {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| StagedDarwinError::Io {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| StagedDarwinError::Io {
                path: path.clone(),
                source,
            })?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() {
                files.insert(
                    path.strip_prefix(root)
                        .expect("walked path is rooted")
                        .to_path_buf(),
                );
            } else {
                return Err(StagedDarwinError::NonRegular(path));
            }
        }
    }
    Ok(files)
}

fn require_fresh_output(output: &Path) -> Result<(), StagedDarwinError> {
    match fs::metadata(output) {
        Ok(metadata) if !metadata.is_dir() => {
            Err(StagedDarwinError::OutputOccupied(output.to_path_buf()))
        }
        Ok(_) => {
            let mut entries = fs::read_dir(output).map_err(|source| StagedDarwinError::Io {
                path: output.to_path_buf(),
                source,
            })?;
            if entries
                .next()
                .transpose()
                .map_err(|source| StagedDarwinError::Io {
                    path: output.to_path_buf(),
                    source,
                })?
                .is_some()
            {
                Err(StagedDarwinError::OutputOccupied(output.to_path_buf()))
            } else {
                Ok(())
            }
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StagedDarwinError::Io {
            path: output.to_path_buf(),
            source,
        }),
    }
}

fn object_path(root: &Path, digest: &Digest, mode: u32) -> PathBuf {
    root.join(format!("objects/{digest}-{mode:04o}"))
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, StagedDarwinError> {
    let metadata = fs::metadata(path).map_err(|source| StagedDarwinError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(StagedDarwinError::NonRegular(path.to_path_buf()));
    }
    if metadata.len() > limit {
        return Err(StagedDarwinError::Size {
            path: path.to_path_buf(),
            expected: limit,
            observed: metadata.len(),
        });
    }
    read_regular(path)
}

fn read_exact(path: &Path, expected: u64) -> Result<Vec<u8>, StagedDarwinError> {
    let metadata = fs::metadata(path).map_err(|source| StagedDarwinError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(StagedDarwinError::NonRegular(path.to_path_buf()));
    }
    if metadata.len() != expected {
        return Err(StagedDarwinError::Size {
            path: path.to_path_buf(),
            expected,
            observed: metadata.len(),
        });
    }
    read_regular(path)
}

fn read_regular(path: &Path) -> Result<Vec<u8>, StagedDarwinError> {
    fs::read(path).map_err(|source| StagedDarwinError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn require_digest(path: &Path, bytes: &[u8], expected: &Digest) -> Result<(), StagedDarwinError> {
    let observed = Digest::compute(Domain::DistArtifact, bytes);
    if &observed == expected {
        Ok(())
    } else {
        Err(StagedDarwinError::Digest {
            path: path.to_path_buf(),
            expected: expected.clone(),
            observed,
        })
    }
}

struct TemporaryExecutable(PathBuf);

impl TemporaryExecutable {
    fn create(stage: &Path, bytes: &[u8]) -> Result<Self, StagedDarwinError> {
        let base = stage.parent().unwrap_or(stage);
        for _ in 0..100 {
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!(".darwin-sign-{}-{sequence}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => {
                    let executable = path.join("spice");
                    fs::write(&executable, bytes).map_err(|source| StagedDarwinError::Io {
                        path: executable.clone(),
                        source,
                    })?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;
                        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
                            .map_err(|source| StagedDarwinError::Io {
                                path: executable.clone(),
                                source,
                            })?;
                    }
                    return Ok(Self(executable));
                }
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => {
                    return Err(StagedDarwinError::Io { path, source });
                }
            }
        }
        Err(StagedDarwinError::TemporaryExhausted(base.to_path_buf()))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryExecutable {
    fn drop(&mut self) {
        if let Some(parent) = self.0.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }
}

#[derive(Debug)]
pub enum StagedDarwinError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    NonRegular(PathBuf),
    Size {
        path: PathBuf,
        expected: u64,
        observed: u64,
    },
    Digest {
        path: PathBuf,
        expected: Digest,
        observed: Digest,
    },
    Target(ReleaseTarget),
    Inventory(String),
    Canvas(String),
    OutputOccupied(PathBuf),
    UnexpectedFiles {
        observed: BTreeSet<PathBuf>,
    },
    TemporaryExhausted(PathBuf),
    Payload(proof::PayloadError),
    Stage(dist::StageError),
    Publish(atomic::Error),
    Share(crate::ShareDarwinError),
    Output(io::Error),
}

impl fmt::Display for StagedDarwinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::NonRegular(path) => write!(
                formatter,
                "stage member is not a regular file: {}",
                path.display()
            ),
            Self::Size {
                path,
                expected,
                observed,
            } => write!(
                formatter,
                "stage member {} has {observed} bytes, expected {expected}",
                path.display()
            ),
            Self::Digest {
                path,
                expected,
                observed,
            } => write!(
                formatter,
                "stage member {} has digest {observed}, expected {expected}",
                path.display()
            ),
            Self::Target(target) => write!(
                formatter,
                "staged signing requires aarch64-apple-darwin, not {target}"
            ),
            Self::Inventory(reason) => {
                write!(formatter, "Darwin stage inventory is invalid: {reason}")
            }
            Self::Canvas(reason) => {
                write!(formatter, "Darwin executable canvas is invalid: {reason}")
            }
            Self::OutputOccupied(path) => write!(
                formatter,
                "signed Darwin stage output is not empty: {}",
                path.display()
            ),
            Self::UnexpectedFiles { observed } => write!(
                formatter,
                "signed Darwin stage contains files outside its exact payload and inventory: {observed:?}"
            ),
            Self::TemporaryExhausted(path) => write!(
                formatter,
                "cannot allocate a temporary signing executable beneath {}",
                path.display()
            ),
            Self::Payload(source) => source.fmt(formatter),
            Self::Stage(source) => source.fmt(formatter),
            Self::Publish(source) => source.fmt(formatter),
            Self::Share(source) => source.fmt(formatter),
            Self::Output(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for StagedDarwinError {}

impl From<proof::PayloadError> for StagedDarwinError {
    fn from(source: proof::PayloadError) -> Self {
        Self::Payload(source)
    }
}

impl From<dist::StageError> for StagedDarwinError {
    fn from(source: dist::StageError) -> Self {
        Self::Stage(source)
    }
}

impl From<atomic::Error> for StagedDarwinError {
    fn from(source: atomic::Error) -> Self {
        Self::Publish(source)
    }
}

impl From<crate::ShareDarwinError> for StagedDarwinError {
    fn from(source: crate::ShareDarwinError) -> Self {
        Self::Share(source)
    }
}

impl From<io::Error> for StagedDarwinError {
    fn from(source: io::Error) -> Self {
        Self::Output(source)
    }
}
