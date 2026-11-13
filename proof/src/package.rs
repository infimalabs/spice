//! Canonical external package index and closed package tree.

use std::fmt;
use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde::de::DeserializeOwned;
use wire::manifest::{
    ProductVersion, ProofStanding, RELEASE_MANIFEST_VERSION, RELEASE_PACKAGE_VERSION,
    ReleaseManifest, ReleasePackage, ReleasePackageTarget, ReleaseTarget,
};
use wire::{Digest, Domain};

const PACKAGE_NAME: &str = "spice";
const DATA_MODE: u32 = 0o644;
const EXECUTABLE_MODE: u32 = 0o755;
const MANIFEST_PATH: &str = "release/manifest.json";
const NOTES_PATH: &str = "release/notes.md";
const PACKAGE_PATH: &str = "release/package.json";
pub const PACKAGE_DOCUMENT_LIMIT: u64 = 8 * 1024 * 1024;
const TARGETS: [ReleaseTarget; 3] = [
    ReleaseTarget::Aarch64AppleDarwin,
    ReleaseTarget::X86_64UnknownLinuxMusl,
    ReleaseTarget::X86_64PcWindowsMsvc,
];

/// Exact bytes supplied for one supported target package entry.
#[derive(Clone, Debug)]
pub struct PackageTargetInput {
    target: ReleaseTarget,
    executable: PackageMember,
    payload: PackageMember,
    proof: PackageMember,
    debug: Option<PackageMember>,
}

impl PackageTargetInput {
    #[must_use]
    pub const fn new(
        target: ReleaseTarget,
        executable: PackageMember,
        payload: PackageMember,
        proof: PackageMember,
    ) -> Self {
        Self {
            target,
            executable,
            payload,
            proof,
            debug: None,
        }
    }

    #[must_use]
    pub fn with_debug(mut self, symbols: Option<PackageMember>) -> Self {
        self.debug = symbols;
        self
    }
}

/// Complete already-sealed input required to form one external package tree.
#[derive(Clone, Debug)]
pub struct PackageInput<'a> {
    version: &'a ProductVersion,
    notes_identity: Digest,
    notes: PackageMember,
    manifest: PackageMember,
    targets: Vec<PackageTargetInput>,
}

impl<'a> PackageInput<'a> {
    #[must_use]
    pub fn new(
        version: &'a ProductVersion,
        notes_identity: Digest,
        notes: PackageMember,
        manifest: PackageMember,
        targets: Vec<PackageTargetInput>,
    ) -> Self {
        Self {
            version,
            notes_identity,
            notes,
            manifest,
            targets,
        }
    }
}

/// One exact regular file in the deterministic package tree.
#[derive(Clone, Debug)]
pub struct PackageMember {
    path: String,
    mode: u32,
    length: u64,
    digest: Digest,
    body: MemberBody,
}

#[derive(Clone, Debug)]
enum MemberBody {
    Inline(Arc<[u8]>),
    File {
        file: Arc<Mutex<File>>,
        identity: MemberFileIdentity,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MemberFileIdentity {
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl PackageMember {
    /// Construct one observed package-tree member.
    ///
    /// The role is derived from the path so callers re-reading a staged tree
    /// cannot choose a digest domain independently from the package grammar.
    #[must_use]
    fn inline(path: impl Into<String>, mode: u32, bytes: impl Into<Vec<u8>>) -> Self {
        let path = path.into();
        let bytes = bytes.into();
        let digest = Digest::compute(member_domain(&path), &bytes);
        Self {
            path,
            mode,
            length: bytes.len() as u64,
            digest,
            body: MemberBody::Inline(bytes.into()),
        }
    }

    /// Admit one complete inline document under an explicit positive ceiling.
    ///
    /// # Errors
    ///
    /// Refuses zero or a body larger than `limit` before retaining it.
    pub fn admit_inline(
        path: impl Into<String>,
        mode: u32,
        bytes: &[u8],
        limit: u64,
    ) -> Result<Self, PackageError> {
        if limit == 0 || limit > PACKAGE_DOCUMENT_LIMIT || bytes.len() as u64 > limit {
            return Err(member_error(
                &path.into(),
                &format!(
                    "length={} requested={limit} maximum={PACKAGE_DOCUMENT_LIMIT}",
                    bytes.len()
                ),
            ));
        }
        Ok(Self::inline(path, mode, bytes))
    }

    /// Admit one already-confined regular file without retaining its body.
    ///
    /// # Errors
    ///
    /// Refuses a zero ceiling, non-regular input, overflow, read failure, or
    /// movement while the exact domain digest is streamed.
    pub fn admit_file(
        path: impl Into<String>,
        mode: u32,
        file: File,
        limit: u64,
    ) -> Result<Self, PackageError> {
        let path = path.into();
        let identity = MemberFileIdentity::capture(&file, &path)?;
        let (length, digest) = digest_file(&file, &path, limit)?;
        if length != identity.length || MemberFileIdentity::capture(&file, &path)? != identity {
            return Err(member_error(
                &path,
                "file moved while its digest was admitted",
            ));
        }
        Ok(Self {
            path,
            mode,
            length,
            digest,
            body: MemberBody::File {
                file: Arc::new(Mutex::new(file)),
                identity,
            },
        })
    }

    /// Capture one bounded document from an already-open regular file.
    ///
    /// The source is read and domain-hashed exactly once. The returned bytes
    /// share the member's retained inline body, so callers can decode the
    /// document without reopening or rehashing its staged source.
    ///
    /// # Errors
    ///
    /// Refuses a zero or non-document ceiling, non-regular input, overflow,
    /// read failure, or movement during the single capture pass.
    pub fn admit_document_file(
        path: impl Into<String>,
        mode: u32,
        mut file: File,
        limit: u64,
    ) -> Result<(Self, Arc<[u8]>), PackageError> {
        let path = path.into();
        let metadata = file
            .metadata()
            .map_err(|error| document_source_error(&path, error))?;
        if !metadata.is_file() {
            return Err(member_error(&path, "source is not a regular file"));
        }
        let identity = MemberFileIdentity::from_metadata(&metadata);
        if limit == 0 || limit > PACKAGE_DOCUMENT_LIMIT || identity.length > limit {
            return Err(member_error(
                &path,
                &format!(
                    "length={} requested={limit} maximum={PACKAGE_DOCUMENT_LIMIT}",
                    identity.length
                ),
            ));
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|error| document_source_error(&path, error))?;
        let mut bytes = Vec::with_capacity(
            usize::try_from(identity.length).expect("bounded package document fits usize"),
        );
        let (length, digest) =
            Digest::copy_reader(member_domain(&path), &mut file, &mut bytes, limit).map_err(
                |error| {
                    if error.kind() == std::io::ErrorKind::InvalidData {
                        member_error(&path, "file moved beyond its admitted document ceiling")
                    } else {
                        document_source_error(&path, error)
                    }
                },
            )?;
        let observed = file
            .metadata()
            .map_err(|error| document_source_error(&path, error))?;
        if length != identity.length || MemberFileIdentity::from_metadata(&observed) != identity {
            return Err(member_error(
                &path,
                "file moved while its document was captured",
            ));
        }
        let bytes: Arc<[u8]> = bytes.into();
        Ok((
            Self {
                path,
                mode,
                length,
                digest,
                body: MemberBody::Inline(bytes.clone()),
            },
            bytes,
        ))
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub const fn mode(&self) -> u32 {
        self.mode
    }

    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }

    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }

    /// Read a canonical document only after its complete size is admitted.
    ///
    /// # Errors
    ///
    /// Refuses zero/insufficient bounds, read failures, or file movement.
    pub fn read_bounded(&self, limit: u64) -> Result<Vec<u8>, PackageError> {
        if limit == 0 || self.length > limit {
            return Err(member_error(
                &self.path,
                &format!("length={} maximum={limit}", self.length),
            ));
        }
        match &self.body {
            MemberBody::Inline(bytes) => Ok(bytes.to_vec()),
            MemberBody::File { file, identity } => {
                let mut file = file
                    .lock()
                    .expect("package member file lock is not poisoned");
                require_file_identity(&file, &self.path, identity)?;
                file.seek(SeekFrom::Start(0))
                    .map_err(|error| member_error(&self.path, &error.to_string()))?;
                let mut bytes = Vec::with_capacity(
                    usize::try_from(self.length).expect("bounded package member fits usize"),
                );
                Read::by_ref(&mut *file)
                    .take(limit.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .map_err(|error| member_error(&self.path, &error.to_string()))?;
                require_file_identity(&file, &self.path, identity)?;
                if bytes.len() as u64 != self.length
                    || Digest::compute(member_domain(&self.path), &bytes) != self.digest
                {
                    return Err(member_error(&self.path, "file moved while it was read"));
                }
                Ok(bytes)
            }
        }
    }

    /// Copy one verified member body without retaining another full body.
    ///
    /// # Errors
    ///
    /// Refuses destination failures or file movement before or after the copy.
    pub fn copy_to(&self, writer: &mut dyn Write) -> Result<u64, PackageError> {
        match &self.body {
            MemberBody::Inline(bytes) => {
                writer
                    .write_all(bytes)
                    .map_err(|error| member_error(&self.path, &error.to_string()))?;
            }
            MemberBody::File { file, identity } => {
                let mut guard = file
                    .lock()
                    .expect("package member file lock is not poisoned");
                require_file_identity(&guard, &self.path, identity)?;
                guard
                    .seek(SeekFrom::Start(0))
                    .map_err(|error| member_error(&self.path, &error.to_string()))?;
                let (copied, digest) = Digest::copy_reader(
                    member_domain(&self.path),
                    &mut *guard,
                    writer,
                    self.length,
                )
                .map_err(|error| member_error(&self.path, &error.to_string()))?;
                require_file_identity(&guard, &self.path, identity)?;
                if copied != self.length || digest != self.digest {
                    return Err(member_error(
                        &self.path,
                        "copied bytes differ from the admitted member",
                    ));
                }
            }
        }
        Ok(self.length)
    }

    fn revalidate(&self) -> Result<(), PackageError> {
        self.copy_to(&mut std::io::sink()).map(|_| ())
    }
}

impl PartialEq for PackageMember {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.mode == other.mode
            && self.length == other.length
            && self.digest == other.digest
    }
}

impl Eq for PackageMember {}

impl MemberFileIdentity {
    fn capture(file: &File, path: &str) -> Result<Self, PackageError> {
        let metadata = file
            .metadata()
            .map_err(|error| member_error(path, &error.to_string()))?;
        if !metadata.is_file() {
            return Err(member_error(path, "source is not a regular file"));
        }
        Ok(Self::from_metadata(&metadata))
    }

    fn from_metadata(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;

        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

fn digest_file(file: &File, path: &str, limit: u64) -> Result<(u64, Digest), PackageError> {
    let mut file = file
        .try_clone()
        .map_err(|error| member_error(path, &error.to_string()))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| member_error(path, &error.to_string()))?;
    Digest::compute_reader(member_domain(path), file, limit)
        .map_err(|error| member_error(path, &error.to_string()))
}

fn require_file_identity(
    file: &File,
    path: &str,
    identity: &MemberFileIdentity,
) -> Result<(), PackageError> {
    if MemberFileIdentity::capture(file, path)? == *identity {
        Ok(())
    } else {
        Err(member_error(path, "file identity moved after admission"))
    }
}

fn member_error(path: &str, detail: &str) -> PackageError {
    PackageError::Member {
        path: path.to_owned(),
        detail: detail.to_owned(),
    }
}

fn document_source_error(path: &str, error: std::io::Error) -> PackageError {
    PackageError::Source {
        path: path.to_owned(),
        detail: error.to_string(),
    }
}

/// One canonical package index and its exact domain identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedPackage {
    document: ReleasePackage,
    bytes: Vec<u8>,
    identity: Digest,
}

impl AdmittedPackage {
    #[must_use]
    pub const fn document(&self) -> &ReleasePackage {
        &self.document
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn identity(&self) -> &Digest {
        &self.identity
    }
}

/// Complete deterministic regular-file closure for one release package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageTree {
    package: AdmittedPackage,
    members: Vec<PackageMember>,
}

impl PackageTree {
    #[must_use]
    pub const fn package(&self) -> &AdmittedPackage {
        &self.package
    }

    #[must_use]
    pub fn members(&self) -> &[PackageMember] {
        &self.members
    }
}

/// Build and admit one exact external package tree.
///
/// # Errors
///
/// Refuses noncanonical or stale evidence, target order or binding drift, a
/// non-clean proof, or executable bytes that differ from the sealed payload.
pub fn build_package(input: PackageInput<'_>) -> Result<PackageTree, PackageError> {
    assemble_package(input)
}

/// Build and admit only the canonical external package index.
///
/// This verifies the same manifest, payload, proof, executable, target-order,
/// and notes bindings as [`build_package`] without cloning the (potentially
/// large) executable members into a package tree. Release preview uses this
/// consequence-free form because it needs the exact package identity, while
/// the later rebuild/upload operation owns materializing the tree.
///
/// # Errors
///
/// Returns the same evidence and binding refusals as [`build_package`].
pub fn build_package_index(input: PackageInput<'_>) -> Result<AdmittedPackage, PackageError> {
    assemble_package_index(&input)
}

/// Admit the canonical index without requiring the publication's complete tree.
///
/// # Errors
///
/// Refuses unsupported schemas, noncanonical documents, and target/path drift.
pub fn admit_package_index(bytes: &[u8]) -> Result<ReleasePackage, PackageError> {
    let document = canonical(bytes, "release package")?;
    admit_package_document(&document)?;
    Ok(document)
}

/// Admit one install target against the canonical package index.
///
/// Other targets and release publication documents are not install inputs.
///
/// # Errors
///
/// Refuses invalid index shape, member identity or evidence binding drift.
pub fn admit_package_target(
    document: &ReleasePackage,
    input: &PackageTargetInput,
) -> Result<(), PackageError> {
    admit_package_document(document)?;
    let index = TARGETS
        .iter()
        .position(|target| *target == input.target)
        .expect("every release target has a canonical package entry");
    let selected = &document.targets[index];
    let admitted = admit_target_members(input, &document.source, index)?;
    for (matches, field) in [
        (
            selected.executable == input.executable.digest,
            "package executable",
        ),
        (
            selected.payload == *admitted.payload.digest(),
            "package payload",
        ),
        (selected.proof == *admitted.proof.digest(), "package proof"),
    ] {
        if !matches {
            return Err(PackageError::Binding { index, field });
        }
    }
    Ok(())
}

fn assemble_package(input: PackageInput<'_>) -> Result<PackageTree, PackageError> {
    let package = assemble_package_index(&input)?;
    let mut members = vec![
        input.manifest.clone(),
        input.notes.clone(),
        member(
            PACKAGE_PATH,
            DATA_MODE,
            package.bytes(),
            Domain::DistReleasePackage,
        ),
    ];
    for target in &input.targets {
        members.extend([
            target.executable.clone(),
            target.payload.clone(),
            target.proof.clone(),
        ]);
        if let Some(symbols) = &target.debug {
            members.push(symbols.clone());
        }
    }
    members.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(PackageTree { package, members })
}

fn assemble_package_index(input: &PackageInput<'_>) -> Result<AdmittedPackage, PackageError> {
    require_member(&input.manifest, MANIFEST_PATH, DATA_MODE)?;
    require_member(&input.notes, NOTES_PATH, DATA_MODE)?;
    input.notes.revalidate()?;
    let manifest_bytes = input.manifest.read_bounded(PACKAGE_DOCUMENT_LIMIT)?;
    let manifest: ReleaseManifest = canonical(&manifest_bytes, "release manifest")?;
    let manifest_identity = input.manifest.digest.clone();
    admit_manifest_shape(&manifest)?;
    if input.targets.len() != TARGETS.len() {
        return Err(PackageError::TargetCount(input.targets.len()));
    }
    let mut package_targets = Vec::with_capacity(TARGETS.len());
    let mut first_app: Option<(Digest, Digest)> = None;
    for (index, ((target, expected), sealed)) in input
        .targets
        .iter()
        .zip(TARGETS)
        .zip(&manifest.targets)
        .enumerate()
    {
        if target.target != expected || sealed.target != expected {
            return Err(PackageError::TargetOrder {
                index,
                expected,
                input: target.target,
                manifest: sealed.target,
            });
        }
        let admitted = admit_target(target, sealed, &manifest.source, index)?;
        let payload = admitted.payload.value();
        let app = (&payload.app, &payload.canvas);
        if let Some(first) = &first_app {
            if (&first.0, &first.1) != app {
                return Err(PackageError::Binding {
                    index,
                    field: "shared app generation",
                });
            }
        } else {
            first_app = Some((payload.app.clone(), payload.canvas.clone()));
        }
        package_targets.push(ReleasePackageTarget {
            target: expected,
            executable_path: executable_path(expected),
            executable: target.executable.digest.clone(),
            payload: admitted.payload.digest().clone(),
            proof: admitted.proof.digest().clone(),
            debug_path: debug_path(expected),
            signature_path: proof_path(expected),
        });
    }
    let document = ReleasePackage {
        schema: RELEASE_PACKAGE_VERSION,
        package: PACKAGE_NAME.to_owned(),
        product_version: input.version.clone(),
        source: manifest.source.clone(),
        manifest: manifest_identity,
        notes: input.notes_identity.clone(),
        notes_content: input.notes.digest.clone(),
        targets: package_targets,
    };
    let bytes = wire::encode(&document).map_err(|error| PackageError::Encode {
        label: "release package",
        detail: error.to_string(),
    })?;
    Ok(AdmittedPackage {
        identity: Digest::compute(Domain::DistReleasePackage, &bytes),
        document,
        bytes,
    })
}

#[derive(Debug)]
struct TargetEvidence {
    payload: crate::AdmittedPayload,
    proof: crate::evidence::AdmittedReceipt,
}

fn admit_target(
    input: &PackageTargetInput,
    sealed: &wire::manifest::ReleaseManifestTarget,
    source: &Digest,
    index: usize,
) -> Result<TargetEvidence, PackageError> {
    let admitted = admit_target_members(input, source, index)?;
    let payload = admitted.payload.value();
    let proof = admitted.proof.value();
    let bindings = [
        (
            sealed.payload == *admitted.payload.digest(),
            "manifest payload",
        ),
        (
            sealed.payload_bytes == input.payload.length,
            "manifest payload length",
        ),
        (sealed.inventory == payload.inventory, "manifest inventory"),
        (sealed.symbols == payload.symbols, "manifest symbols"),
        (sealed.receipt == *admitted.proof.digest(), "manifest proof"),
        (
            sealed.receipt_bytes == input.proof.length,
            "manifest proof length",
        ),
        (proof.battery == sealed.battery, "proof battery"),
        (proof.replay == sealed.replay, "proof replay"),
        (proof.environment == sealed.environment, "proof environment"),
        (sealed.standing == ProofStanding::Clean, "manifest standing"),
    ];
    if let Some((_, field)) = bindings.into_iter().find(|(matches, _)| !matches) {
        return Err(PackageError::Binding { index, field });
    }
    Ok(admitted)
}

fn admit_target_members(
    input: &PackageTargetInput,
    source: &Digest,
    index: usize,
) -> Result<TargetEvidence, PackageError> {
    require_member(
        &input.executable,
        &executable_path(input.target),
        EXECUTABLE_MODE,
    )?;
    input.executable.revalidate()?;
    require_member(&input.payload, &payload_path(input.target), DATA_MODE)?;
    require_member(&input.proof, &proof_path(input.target), DATA_MODE)?;
    let payload_bytes = input.payload.read_bounded(PACKAGE_DOCUMENT_LIMIT)?;
    let proof_bytes = input.proof.read_bounded(PACKAGE_DOCUMENT_LIMIT)?;
    let admitted_payload = crate::payload::admit(&payload_bytes).map_err(PackageError::Payload)?;
    let payload = admitted_payload.value();
    let admitted_proof =
        crate::evidence::admit_receipt(&proof_bytes).map_err(PackageError::Proof)?;
    let proof = admitted_proof.value();
    let debug_inventory = input
        .target
        .debug_path()
        .and_then(|path| payload.inventory.iter().find(|entry| entry.path == path));
    let debug_binding = match (input.target.debug_path(), &input.debug, debug_inventory) {
        (Some(_), Some(member), Some(entry)) => {
            require_member(
                member,
                &debug_path(input.target).expect("debug path exists"),
                DATA_MODE,
            )?;
            member.revalidate()?;
            entry.mode == DATA_MODE
                && entry.bytes == member.length
                && entry.digest == member.digest
                && match (&payload.symbols, input.target) {
                    (Some(metadata), ReleaseTarget::Aarch64AppleDarwin) => {
                        metadata.bytes == member.length
                            && metadata.digest == member.digest
                            && metadata.path
                                == ReleaseTarget::Aarch64AppleDarwin
                                    .debug_path()
                                    .expect("Darwin debug path exists")
                    }
                    (None, ReleaseTarget::X86_64PcWindowsMsvc) => true,
                    _ => false,
                }
        }
        (None, None, None) => payload.symbols.is_none(),
        _ => false,
    };
    let executable_inventory = payload
        .inventory
        .iter()
        .find(|entry| entry.path == input.target.executable_path())
        .ok_or(PackageError::Binding {
            index,
            field: "executable inventory",
        })?;
    let bindings = [
        (payload.target == input.target, "payload target"),
        (payload.source == *source, "payload source"),
        (
            payload.native == input.executable.digest,
            "payload executable",
        ),
        (
            executable_inventory.mode == EXECUTABLE_MODE,
            "executable mode",
        ),
        (
            executable_inventory.digest == input.executable.digest,
            "inventory executable",
        ),
        (debug_binding, "package debug artifact"),
        (proof.target == input.target, "proof target"),
        (proof.payload == *admitted_payload.digest(), "proof payload"),
        (proof.standing == ProofStanding::Clean, "proof standing"),
    ];
    if let Some((_, field)) = bindings.into_iter().find(|(matches, _)| !matches) {
        return Err(PackageError::Binding { index, field });
    }
    Ok(TargetEvidence {
        payload: admitted_payload,
        proof: admitted_proof,
    })
}

fn admit_manifest_shape(manifest: &ReleaseManifest) -> Result<(), PackageError> {
    if manifest.version != RELEASE_MANIFEST_VERSION {
        return Err(PackageError::ManifestVersion(manifest.version));
    }
    if manifest.targets.len() != TARGETS.len() {
        return Err(PackageError::TargetCount(manifest.targets.len()));
    }
    Ok(())
}

fn admit_package_document(document: &ReleasePackage) -> Result<(), PackageError> {
    if document.schema != RELEASE_PACKAGE_VERSION {
        return Err(PackageError::PackageVersion(document.schema));
    }
    if document.package != PACKAGE_NAME {
        return Err(PackageError::PackageName(document.package.clone()));
    }
    if document.targets.len() != TARGETS.len() {
        return Err(PackageError::TargetCount(document.targets.len()));
    }
    for (index, (entry, expected)) in document.targets.iter().zip(TARGETS).enumerate() {
        if entry.target != expected
            || entry.executable_path != executable_path(expected)
            || entry.debug_path != debug_path(expected)
            || entry.signature_path != proof_path(expected)
        {
            return Err(PackageError::PackageTarget { index });
        }
    }
    Ok(())
}

fn canonical<T>(bytes: &[u8], label: &'static str) -> Result<T, PackageError>
where
    T: DeserializeOwned + Serialize,
{
    let value = wire::decode(bytes).map_err(|error| PackageError::Decode {
        label,
        detail: error.to_string(),
    })?;
    let canonical = wire::encode(&value).map_err(|error| PackageError::Encode {
        label,
        detail: error.to_string(),
    })?;
    if canonical == bytes {
        Ok(value)
    } else {
        Err(PackageError::NonCanonical(label))
    }
}

fn member(path: &str, mode: u32, bytes: &[u8], domain: Domain) -> PackageMember {
    let member = PackageMember::inline(path, mode, bytes);
    debug_assert_eq!(member.digest, Digest::compute(domain, bytes));
    member
}

fn require_member(member: &PackageMember, path: &str, mode: u32) -> Result<(), PackageError> {
    if member.path == path && member.mode == mode {
        Ok(())
    } else {
        Err(PackageError::Closure)
    }
}

fn member_domain(path: &str) -> Domain {
    match path {
        MANIFEST_PATH => Domain::DistReleaseManifest,
        NOTES_PATH => Domain::DistReleaseNotesContent,
        PACKAGE_PATH => Domain::DistReleasePackage,
        path if path.ends_with("/payload.json") => Domain::DistTargetPayload,
        path if path.ends_with("/proof.json") => Domain::ProofReceipt,
        _ => Domain::DistArtifact,
    }
}

fn executable_path(target: ReleaseTarget) -> String {
    format!("targets/{target}/{}", target.executable_path())
}

fn payload_path(target: ReleaseTarget) -> String {
    format!("targets/{target}/payload.json")
}

fn proof_path(target: ReleaseTarget) -> String {
    format!("targets/{target}/proof.json")
}

fn debug_path(target: ReleaseTarget) -> Option<String> {
    target
        .debug_path()
        .map(|path| format!("targets/{target}/{path}"))
}

/// Why an external release package was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum PackageError {
    Member {
        path: String,
        detail: String,
    },
    Source {
        path: String,
        detail: String,
    },
    Decode {
        label: &'static str,
        detail: String,
    },
    Encode {
        label: &'static str,
        detail: String,
    },
    NonCanonical(&'static str),
    Payload(crate::PayloadError),
    Proof(crate::EvidenceError),
    ManifestVersion(u16),
    PackageVersion(u16),
    PackageName(String),
    TargetCount(usize),
    TargetOrder {
        index: usize,
        expected: ReleaseTarget,
        input: ReleaseTarget,
        manifest: ReleaseTarget,
    },
    Binding {
        index: usize,
        field: &'static str,
    },
    PackageTarget {
        index: usize,
    },
    Closure,
}

impl fmt::Display for PackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Member { path, detail } => {
                write!(formatter, "release package member {path} refuses: {detail}")
            }
            Self::Source { path, detail } => {
                write!(
                    formatter,
                    "cannot read release package member {path}: {detail}"
                )
            }
            Self::Decode { label, detail } => {
                write!(formatter, "{label} does not decode: {detail}")
            }
            Self::Encode { label, detail } => {
                write!(formatter, "{label} does not encode: {detail}")
            }
            Self::NonCanonical(label) => {
                write!(formatter, "{label} is not canonical current wire JSON")
            }
            Self::Payload(error) => write!(formatter, "target payload refuses: {error}"),
            Self::Proof(error) => write!(formatter, "proof receipt refuses: {error}"),
            Self::ManifestVersion(version) => {
                write!(
                    formatter,
                    "release manifest schema {version} is unsupported"
                )
            }
            Self::PackageVersion(version) => {
                write!(formatter, "release package schema {version} is unsupported")
            }
            Self::PackageName(name) => {
                write!(formatter, "release package name is unsupported: {name}")
            }
            Self::TargetCount(count) => {
                write!(
                    formatter,
                    "release package has {count} targets, expected {}",
                    TARGETS.len()
                )
            }
            Self::TargetOrder {
                index,
                expected,
                input,
                manifest,
            } => write!(
                formatter,
                "target {index} order moved: expected {expected}, input {input}, manifest {manifest}"
            ),
            Self::Binding { index, field } => {
                write!(formatter, "release target {index} binding moved at {field}")
            }
            Self::PackageTarget { index } => write!(
                formatter,
                "release package target {index} has noncanonical target artifact paths"
            ),
            Self::Closure => {
                formatter.write_str("release package tree is not the exact canonical closure")
            }
        }
    }
}

impl std::error::Error for PackageError {}

#[cfg(test)]
mod tests;
