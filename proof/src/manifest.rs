//! Verification, composition, and manifest-last release publication.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use wire::manifest::{
    ProofEnvironment, ProofStanding, RELEASE_MANIFEST_VERSION, ReleaseManifest,
    ReleaseManifestTarget, ReleaseTarget, TargetPayload,
};
use wire::{Digest, Domain};

use crate::evidence::admit_receipt;
use crate::{Battery, EvidenceError, PayloadError, admit, verify_stored};

pub(crate) const DATA_MODE: u32 = 0o644;
const MANIFEST_POINTER: &str = "release/manifest.json";
const TARGETS: [ReleaseTarget; 3] = [
    ReleaseTarget::Aarch64AppleDarwin,
    ReleaseTarget::X86_64UnknownLinuxMusl,
    ReleaseTarget::X86_64PcWindowsMsvc,
];

/// One exact payload and detached receipt submitted for release.
#[derive(Clone, Copy, Debug)]
pub struct ManifestInput<'a> {
    payload: &'a [u8],
    receipt: &'a [u8],
}

impl<'a> ManifestInput<'a> {
    #[must_use]
    pub const fn new(payload: &'a [u8], receipt: &'a [u8]) -> Self {
        Self { payload, receipt }
    }
}

/// A complete canonical release and the exact bytes published for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedManifest {
    pub manifest: ReleaseManifest,
    pub bytes: Vec<u8>,
    pub digest: Digest,
    pub pointer: PathBuf,
}

/// Admit the current fixed manifest pointer and every object it names.
///
/// The caller supplies the executable identities observed at the binary rim.
/// Every target payload must carry the complete app generation, and the payload
/// matching `native_target` must carry the invoking executable itself.
///
/// # Errors
///
/// Refuses missing, non-regular, symlinked, noncanonical, stale, partial, or
/// mixed-target evidence. Every content-addressed object is re-read and its
/// digest and typed bindings are checked before a manifest is returned.
pub fn admit_manifest(
    root: &Path,
    app: &Digest,
    native_target: ReleaseTarget,
    native: &Digest,
) -> Result<PublishedManifest, ManifestAdmissionError> {
    admit_manifest_with(root, app, Some((native_target, native)))
}

/// Admit the complete proof manifest without selecting one invoking binary.
///
/// Proof recognition runs in the resident rather than either target binary.
/// It therefore validates both payloads, receipts, the current app, and the
/// complete shared release closure, while the later target-native release
/// command retains the executable-identity check performed by
/// [`admit_manifest`].
pub fn admit_proof_manifest(
    root: &Path,
    app: &Digest,
) -> Result<PublishedManifest, ManifestAdmissionError> {
    admit_manifest_with(root, app, None)
}

fn admit_manifest_with(
    root: &Path,
    app: &Digest,
    native: Option<(ReleaseTarget, &Digest)>,
) -> Result<PublishedManifest, ManifestAdmissionError> {
    let root = admit_stage_root(root)?;
    let published = admit_pointer(&root)?;
    let battery = Battery::current();
    let stage = StageIdentity {
        root: &root,
        manifest: &published.manifest,
        app,
        native,
        battery: battery.digest(),
    };
    let mut first_payload = None;
    for (index, (entry, expected_target)) in
        published.manifest.targets.iter().zip(TARGETS).enumerate()
    {
        let payload = admit_target(&stage, entry, expected_target, index)?;
        admit_shared_payload(first_payload.as_ref(), &payload)?;
        first_payload.get_or_insert(payload);
    }
    Ok(published)
}

/// The identities one release stage binds every target entry to: where its
/// objects live, the manifest they belong to, the app generation and battery
/// they must name, and the invoking native executable when one is selected.
#[derive(Clone, Copy)]
struct StageIdentity<'a> {
    root: &'a Path,
    manifest: &'a ReleaseManifest,
    app: &'a Digest,
    native: Option<(ReleaseTarget, &'a Digest)>,
    battery: &'a Digest,
}

fn admit_stage_root(root: &Path) -> Result<PathBuf, ManifestAdmissionError> {
    let stage_metadata =
        fs::symlink_metadata(root).map_err(|source| ManifestAdmissionError::Input {
            label: "release stage",
            path: root.to_path_buf(),
            source,
        })?;
    if stage_metadata.file_type().is_symlink() {
        return Err(ManifestAdmissionError::StageSymlink(root.to_path_buf()));
    }
    if !stage_metadata.is_dir() {
        return Err(ManifestAdmissionError::StageNotDirectory(
            root.to_path_buf(),
        ));
    }
    fs::canonicalize(root).map_err(|source| ManifestAdmissionError::Input {
        label: "release stage",
        path: root.to_path_buf(),
        source,
    })
}

fn admit_pointer(root: &Path) -> Result<PublishedManifest, ManifestAdmissionError> {
    let pointer = PathBuf::from(MANIFEST_POINTER);
    let bytes = read_regular(root, &pointer, "release manifest pointer")?;
    let manifest: ReleaseManifest = wire::decode(&bytes)
        .map_err(|source| ManifestAdmissionError::ManifestDecode(source.to_string()))?;
    let canonical = wire::encode(&manifest)
        .map_err(|source| ManifestAdmissionError::ManifestEncode(source.to_string()))?;
    if canonical != bytes {
        return Err(ManifestAdmissionError::ManifestNonCanonical);
    }
    if manifest.version != RELEASE_MANIFEST_VERSION {
        return Err(ManifestAdmissionError::ManifestVersion(manifest.version));
    }
    if manifest.targets.len() != TARGETS.len() {
        return Err(ManifestAdmissionError::EntryCount(manifest.targets.len()));
    }

    let digest = Digest::compute(Domain::DistReleaseManifest, &bytes);
    let object = object_path("manifest", &digest);
    if read_regular(root, &object, "release manifest object")? != bytes {
        return Err(ManifestAdmissionError::ManifestObject);
    }
    Ok(PublishedManifest {
        manifest,
        bytes,
        digest,
        pointer,
    })
}

fn admit_target(
    stage: &StageIdentity<'_>,
    entry: &ReleaseManifestTarget,
    expected_target: ReleaseTarget,
    index: usize,
) -> Result<TargetPayload, ManifestAdmissionError> {
    if entry.target != expected_target {
        return Err(ManifestAdmissionError::TargetOrder {
            index,
            expected: expected_target,
            observed: entry.target,
        });
    }
    let payload = admit_target_payload(stage, entry, index)?;
    admit_target_receipt(stage.root, entry, index, &payload, stage.battery)?;
    Ok(payload)
}

fn admit_target_payload(
    stage: &StageIdentity<'_>,
    entry: &ReleaseManifestTarget,
    index: usize,
) -> Result<TargetPayload, ManifestAdmissionError> {
    let StageIdentity {
        root,
        manifest,
        app,
        native,
        ..
    } = *stage;
    let bytes = read_regular(
        root,
        &object_path("payload", &entry.payload),
        "target payload object",
    )?;
    require_binding(
        bytes.len() as u64 == entry.payload_bytes,
        index,
        "payload length",
    )?;
    let payload =
        admit(&bytes).map_err(|source| ManifestAdmissionError::Payload { index, source })?;
    let value = payload.value();
    require_binding(payload.digest() == &entry.payload, index, "payload digest")?;
    require_binding(
        value.inventory == entry.inventory,
        index,
        "payload inventory",
    )?;
    require_binding(
        value.symbols == entry.symbols,
        index,
        "payload symbol metadata",
    )?;
    require_binding(value.target == entry.target, index, "payload target")?;
    require_binding(value.source == manifest.source, index, "payload source")?;
    require_binding(
        value.archive == manifest.archive,
        index,
        "payload source archive",
    )?;
    require_binding(value.app == *app, index, "app generation identity")?;
    if let Some((native_target, native)) = native {
        require_binding(
            value.target != native_target || value.native == *native,
            index,
            "native executable identity",
        )?;
    }
    Ok(value.clone())
}

fn admit_target_receipt(
    root: &Path,
    entry: &ReleaseManifestTarget,
    index: usize,
    payload: &TargetPayload,
    battery: &Digest,
) -> Result<(), ManifestAdmissionError> {
    let bytes = read_regular(
        root,
        &object_path("receipt", &entry.receipt),
        "detached proof receipt object",
    )?;
    require_binding(
        bytes.len() as u64 == entry.receipt_bytes,
        index,
        "receipt length",
    )?;
    let admitted = admit_receipt(&bytes)
        .map_err(|source| ManifestAdmissionError::Receipt { index, source })?;
    require_binding(admitted.digest() == &entry.receipt, index, "receipt digest")?;
    let receipt = admitted.value();
    for (matches, field) in [
        (receipt.target == entry.target, "receipt target"),
        (receipt.payload == entry.payload, "receipt payload"),
        (receipt.battery == entry.battery, "receipt battery"),
        (receipt.replay == entry.replay, "receipt replay"),
        (
            receipt.environment == entry.environment,
            "receipt environment",
        ),
        (receipt.standing == entry.standing, "receipt standing"),
        (entry.battery == *battery, "current proof battery"),
        (
            entry.standing == ProofStanding::Clean,
            "clean proof standing",
        ),
    ] {
        require_binding(matches, index, field)?;
    }
    require_binding(
        target_environment_matches(entry, payload),
        index,
        "target proof environment",
    )
}

fn target_environment_matches(entry: &ReleaseManifestTarget, payload: &TargetPayload) -> bool {
    match (&entry.environment, entry.target) {
        (ProofEnvironment::Native { .. }, ReleaseTarget::Aarch64AppleDarwin)
        | (ProofEnvironment::HostedWindows { .. }, ReleaseTarget::X86_64PcWindowsMsvc) => true,
        (ProofEnvironment::Oci { image }, ReleaseTarget::X86_64UnknownLinuxMusl) => {
            image.as_str() == payload.pins.proof_container
        }
        _ => false,
    }
}

fn admit_shared_payload(
    first: Option<&TargetPayload>,
    observed: &TargetPayload,
) -> Result<(), ManifestAdmissionError> {
    let Some(first) = first else {
        return Ok(());
    };
    for (matches, field) in [
        (observed.app == first.app, "app identity"),
        (observed.canvas == first.canvas, "canvas archive"),
        (observed.pins == first.pins, "toolchain and container pins"),
    ] {
        if !matches {
            return Err(ManifestAdmissionError::Mixed { field });
        }
    }
    Ok(())
}

fn require_binding(
    matches: bool,
    index: usize,
    field: &'static str,
) -> Result<(), ManifestAdmissionError> {
    if matches {
        Ok(())
    } else {
        Err(ManifestAdmissionError::Binding { index, field })
    }
}

fn read_regular(
    root: &Path,
    relative: &Path,
    label: &'static str,
) -> Result<Vec<u8>, ManifestAdmissionError> {
    if !atomic::is_rooted_path(relative) {
        return Err(ManifestAdmissionError::OutsideRoot {
            label,
            path: relative.to_path_buf(),
        });
    }
    let mut component_path = root.to_path_buf();
    for component in relative
        .parent()
        .expect("fixed object paths have parents")
        .components()
    {
        component_path.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&component_path).map_err(|source| {
            ManifestAdmissionError::Input {
                label,
                path: component_path.clone(),
                source,
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ManifestAdmissionError::Symlink {
                label,
                path: component_path,
            });
        }
        if !metadata.is_dir() {
            return Err(ManifestAdmissionError::NotDirectory {
                label,
                path: component_path,
            });
        }
    }
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path).map_err(|source| ManifestAdmissionError::Input {
        label,
        path: path.clone(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(ManifestAdmissionError::Symlink { label, path });
    }
    if !metadata.is_file() {
        return Err(ManifestAdmissionError::NotRegular { label, path });
    }
    let parent = fs::canonicalize(path.parent().expect("fixed object paths have parents"))
        .map_err(|source| ManifestAdmissionError::Input {
            label,
            path: path.clone(),
            source,
        })?;
    if !parent.starts_with(root) {
        return Err(ManifestAdmissionError::OutsideRoot { label, path });
    }
    fs::read(&path).map_err(|source| ManifestAdmissionError::Input {
        label,
        path,
        source,
    })
}

/// Verify both proofs, compose one manifest, and publish its pointer last.
///
/// # Errors
///
/// Refuses any shape other than all supported targets in declaration order,
/// any payload or proof defect, or source disagreement before publication.
/// Immutable payload, receipt, and manifest objects are all attempted before
/// the sole mutable pointer.
pub fn publish_manifest<'a, I, P>(
    inputs: I,
    publisher: &mut P,
) -> Result<PublishedManifest, ManifestError>
where
    I: IntoIterator<Item = ManifestInput<'a>>,
    P: atomic::RootedPublish,
{
    let inputs = inputs.into_iter().collect::<Vec<_>>();
    if inputs.len() != TARGETS.len() {
        return Err(ManifestError::EntryCount(inputs.len()));
    }

    let mut source = None;
    let mut first_payload: Option<TargetPayload> = None;
    let mut seen = BTreeSet::new();
    let mut targets = Vec::with_capacity(TARGETS.len());
    let mut objects = Vec::with_capacity(TARGETS.len() * 2 + 1);
    for (index, (input, expected)) in inputs.into_iter().zip(TARGETS).enumerate() {
        let admitted =
            admit(input.payload).map_err(|source| ManifestError::Payload { index, source })?;
        if !seen.insert(admitted.value().target) {
            return Err(ManifestError::DuplicateTarget(admitted.value().target));
        }
        if admitted.value().target != expected {
            return Err(ManifestError::TargetOrder {
                index,
                expected,
                observed: admitted.value().target,
            });
        }
        match &source {
            None => source = Some(admitted.value().source.clone()),
            Some(first) if first != &admitted.value().source => {
                return Err(ManifestError::SourceMismatch {
                    first: first.clone(),
                    observed: admitted.value().source.clone(),
                });
            }
            Some(_) => {}
        }
        if let Some(first) = &first_payload {
            for (matches, field) in [
                (admitted.value().app == first.app, "app identity"),
                (admitted.value().archive == first.archive, "source archive"),
                (admitted.value().canvas == first.canvas, "canvas archive"),
                (
                    admitted.value().pins == first.pins,
                    "toolchain and container pins",
                ),
            ] {
                if !matches {
                    return Err(ManifestError::Mixed { field });
                }
            }
        } else {
            first_payload = Some(admitted.value().clone());
        }

        let verified = verify_stored(&admitted, input.receipt)
            .map_err(|source| ManifestError::Evidence { index, source })?;
        let payload_digest = verified.payload_digest().clone();
        let receipt_digest = verified.receipt_digest().clone();
        targets.push(ReleaseManifestTarget {
            target: verified.target(),
            payload: payload_digest.clone(),
            payload_bytes: input.payload.len() as u64,
            inventory: admitted.value().inventory.clone(),
            symbols: admitted.value().symbols.clone(),
            receipt: receipt_digest.clone(),
            receipt_bytes: input.receipt.len() as u64,
            battery: verified.battery_digest().clone(),
            replay: verified.replay_digest().clone(),
            environment: verified.environment().clone(),
            standing: ProofStanding::Clean,
        });
        objects.push((object_path("payload", &payload_digest), input.payload));
        objects.push((object_path("receipt", &receipt_digest), input.receipt));
    }

    let first_payload = first_payload.expect("three admitted entries establish a source");
    let manifest = ReleaseManifest {
        version: RELEASE_MANIFEST_VERSION,
        source: source.expect("three admitted entries establish a source"),
        archive: first_payload.archive,
        targets,
    };
    let bytes =
        wire::encode(&manifest).map_err(|source| ManifestError::Encode(source.to_string()))?;
    let digest = Digest::compute(Domain::DistReleaseManifest, &bytes);
    objects.push((object_path("manifest", &digest), bytes.as_slice()));
    objects.sort_by(|left, right| left.0.cmp(&right.0));
    for (path, object) in objects {
        publisher
            .publish(&path, DATA_MODE, object)
            .map_err(|source| ManifestError::Publish { path, source })?;
    }

    let pointer = PathBuf::from(MANIFEST_POINTER);
    publisher
        .publish(&pointer, DATA_MODE, &bytes)
        .map_err(|source| ManifestError::Publish {
            path: pointer.clone(),
            source,
        })?;
    Ok(PublishedManifest {
        manifest,
        bytes,
        digest,
        pointer,
    })
}

fn object_path(kind: &str, digest: &Digest) -> PathBuf {
    PathBuf::from(format!("objects/{kind}-{digest}.json"))
}

#[derive(Debug)]
pub enum ManifestError {
    EntryCount(usize),
    Payload {
        index: usize,
        source: PayloadError,
    },
    TargetOrder {
        index: usize,
        expected: ReleaseTarget,
        observed: ReleaseTarget,
    },
    DuplicateTarget(ReleaseTarget),
    SourceMismatch {
        first: Digest,
        observed: Digest,
    },
    Mixed {
        field: &'static str,
    },
    Evidence {
        index: usize,
        source: EvidenceError,
    },
    Encode(String),
    Publish {
        path: PathBuf,
        source: atomic::Error,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EntryCount(count) => write!(
                formatter,
                "release manifest requires exactly three target proofs, found {count}"
            ),
            Self::Payload { index, source } => {
                write!(
                    formatter,
                    "release manifest payload {index} is refused: {source}"
                )
            }
            Self::TargetOrder {
                index,
                expected,
                observed,
            } => write!(
                formatter,
                "release manifest target {index} is {observed}, expected {expected}"
            ),
            Self::DuplicateTarget(target) => {
                write!(formatter, "release manifest repeats target {target}")
            }
            Self::SourceMismatch { first, observed } => write!(
                formatter,
                "release manifest source {observed} disagrees with first source {first}"
            ),
            Self::Mixed { field } => {
                write!(formatter, "release manifest mixes target {field}")
            }
            Self::Evidence { index, source } => {
                write!(
                    formatter,
                    "release manifest proof {index} is refused: {source}"
                )
            }
            Self::Encode(source) => write!(formatter, "release manifest does not encode: {source}"),
            Self::Publish { path, source } => write!(
                formatter,
                "release manifest publication failed at {}: {source}",
                path.display()
            ),
        }
    }
}

impl Error for ManifestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Payload { source, .. } => Some(source),
            Self::Evidence { source, .. } => Some(source),
            Self::Publish { source, .. } => Some(source),
            Self::EntryCount(_)
            | Self::TargetOrder { .. }
            | Self::DuplicateTarget(_)
            | Self::SourceMismatch { .. }
            | Self::Mixed { .. }
            | Self::Encode(_) => None,
        }
    }
}

/// Why a sealed release pointer or one of its immutable objects was refused.
#[derive(Debug)]
pub enum ManifestAdmissionError {
    Input {
        label: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Symlink {
        label: &'static str,
        path: PathBuf,
    },
    NotRegular {
        label: &'static str,
        path: PathBuf,
    },
    NotDirectory {
        label: &'static str,
        path: PathBuf,
    },
    OutsideRoot {
        label: &'static str,
        path: PathBuf,
    },
    StageSymlink(PathBuf),
    StageNotDirectory(PathBuf),
    ManifestDecode(String),
    ManifestEncode(String),
    ManifestNonCanonical,
    ManifestVersion(u16),
    ManifestObject,
    EntryCount(usize),
    TargetOrder {
        index: usize,
        expected: ReleaseTarget,
        observed: ReleaseTarget,
    },
    Payload {
        index: usize,
        source: PayloadError,
    },
    Receipt {
        index: usize,
        source: EvidenceError,
    },
    Binding {
        index: usize,
        field: &'static str,
    },
    Mixed {
        field: &'static str,
    },
}

impl fmt::Display for ManifestAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input {
                label,
                path,
                source,
            } => write!(
                formatter,
                "{label} {} is unreadable: {source}",
                path.display()
            ),
            Self::Symlink { label, path } => {
                write!(formatter, "{label} {} is a symlink", path.display())
            }
            Self::NotRegular { label, path } => {
                write!(
                    formatter,
                    "{label} {} is not a regular file",
                    path.display()
                )
            }
            Self::NotDirectory { label, path } => {
                write!(
                    formatter,
                    "{label} parent {} is not a directory",
                    path.display()
                )
            }
            Self::OutsideRoot { label, path } => {
                write!(
                    formatter,
                    "{label} {} escapes the release stage",
                    path.display()
                )
            }
            Self::StageSymlink(path) => {
                write!(formatter, "release stage {} is a symlink", path.display())
            }
            Self::StageNotDirectory(path) => write!(
                formatter,
                "release stage {} is not a directory",
                path.display()
            ),
            Self::ManifestDecode(source) => {
                write!(formatter, "release manifest does not decode: {source}")
            }
            Self::ManifestEncode(source) => {
                write!(formatter, "release manifest does not encode: {source}")
            }
            Self::ManifestNonCanonical => {
                formatter.write_str("release manifest pointer is not canonical")
            }
            Self::ManifestVersion(version) => {
                write!(
                    formatter,
                    "release manifest version {version} is not current"
                )
            }
            Self::ManifestObject => {
                formatter.write_str("release manifest object disagrees with the current pointer")
            }
            Self::EntryCount(count) => write!(
                formatter,
                "release manifest requires exactly three targets, found {count}"
            ),
            Self::TargetOrder {
                index,
                expected,
                observed,
            } => write!(
                formatter,
                "release manifest target {index} is {observed}, expected {expected}"
            ),
            Self::Payload { index, source } => {
                write!(formatter, "release payload {index} is refused: {source}")
            }
            Self::Receipt { index, source } => {
                write!(formatter, "release receipt {index} is refused: {source}")
            }
            Self::Binding { index, field } => {
                write!(
                    formatter,
                    "release target {index} has a stale or mismatched {field}"
                )
            }
            Self::Mixed { field } => write!(formatter, "release targets mix {field}"),
        }
    }
}

impl Error for ManifestAdmissionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Input { source, .. } => Some(source),
            Self::Payload { source, .. } => Some(source),
            Self::Receipt { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
