//! Canonical package admission and deterministic Git tree materialization.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use wire::manifest::{
    InventoryEntry, ProductVersion, RELEASE_MANIFEST_VERSION, RELEASE_PACKAGE_VERSION,
    ReleaseManifest, ReleasePackage, ReleasePackageTarget, ReleaseTarget,
};
use wire::{Digest, Domain};

mod tree;

const MAX_RELEASE_MEMBERS: usize = 32;
const MAX_RELEASE_MEMBER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_RELEASE_BYTES: u64 = 1024 * 1024 * 1024;
const _: () =
    assert!(MAX_RELEASE_MEMBERS > 0 && MAX_RELEASE_MEMBER_BYTES > 0 && MAX_RELEASE_BYTES > 0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Preview {
    pub(super) identity: Digest,
    pub(super) git_tree: String,
}

#[derive(Debug)]
pub(super) enum PackageFailure {
    Moved(String),
    Environment(String),
}

impl fmt::Display for PackageFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Moved(detail) | Self::Environment(detail) => detail.fmt(formatter),
        }
    }
}

struct LoadedTarget {
    target: ReleaseTarget,
    executable: proof::PackageMember,
    payload: proof::PackageMember,
    proof: proof::PackageMember,
    debug: Option<proof::PackageMember>,
}

struct Loaded {
    notes: proof::PackageMember,
    manifest: proof::PackageMember,
    targets: Vec<LoadedTarget>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MemberShape {
    mode: u32,
    length: u64,
    digest: Digest,
    limit: u64,
}

#[derive(Debug)]
struct PendingFile {
    label: String,
    file: File,
    shape: MemberShape,
}

#[derive(Debug)]
struct ReleaseBudget {
    members: BTreeMap<String, MemberShape>,
    bytes: u64,
}

#[derive(Debug)]
struct ReleasePreflight {
    manifest: ReleaseManifest,
    manifest_member: proof::PackageMember,
    package_bytes: Vec<u8>,
    pending: BTreeMap<String, PendingFile>,
    budget: ReleaseBudget,
}

#[derive(Clone, Debug)]
pub(super) struct ReleaseMember {
    member: proof::PackageMember,
    limit: u64,
}

impl ReleaseMember {
    pub(super) fn digest(&self) -> &Digest {
        self.member.digest()
    }

    pub(super) fn copy_to(&self, writer: &mut dyn io::Write) -> Result<u64, PackageFailure> {
        let mut budget = CaptureBudget::default();
        budget.admit(self.member.path(), self.member.length(), self.limit)?;
        self.member
            .copy_to(writer)
            .map_err(|error| PackageFailure::Moved(error.to_string()))
    }
}

#[derive(Debug)]
pub(super) struct ReleaseIndex {
    preview: Preview,
    tree: proof::PackageTree,
    budget: ReleaseBudget,
}

impl ReleaseIndex {
    pub(super) fn admit(
        root: &Path,
        version: &ProductVersion,
        notes_identity: Digest,
        notes: &[u8],
        manifest_identity: &Digest,
    ) -> Result<Self, PackageFailure> {
        validate_notes(&notes_identity, notes).map_err(PackageFailure::Moved)?;
        let preflight = ReleasePreflight::capture(
            root,
            version,
            notes_identity.clone(),
            notes,
            manifest_identity,
        )?;
        let ReleasePreflight {
            manifest: document,
            manifest_member,
            package_bytes,
            pending,
            budget,
        } = preflight;
        let notes = inline_member("release/notes.md", 0o644, notes)?;
        let admitted = admit_pending(pending)?;
        let loaded = load(document, notes, manifest_member, admitted)?;
        let tree = proof::build_package(package_input(&loaded, version, notes_identity))
            .map_err(|error| PackageFailure::Moved(error.to_string()))?;
        if tree.package().bytes() != package_bytes {
            return Err(PackageFailure::Moved(
                "derived release package changed after staged body admission".to_owned(),
            ));
        }
        budget.verify(tree.members())?;
        let preview = Preview {
            identity: tree.package().identity().clone(),
            git_tree: tree::identity(tree.members(), &budget).map_err(PackageFailure::Moved)?,
        };
        Ok(Self {
            preview,
            tree,
            budget,
        })
    }

    pub(super) const fn preview(&self) -> &Preview {
        &self.preview
    }

    pub(super) fn materialize(&self, root: &Path) -> Result<Preview, PackageFailure> {
        let git_tree = tree::write(root, self.tree.members(), &self.budget)
            .map_err(PackageFailure::Environment)?;
        if git_tree != self.preview.git_tree {
            return Err(PackageFailure::Moved(
                "materialized Git tree changed from its admitted preview".to_owned(),
            ));
        }
        Ok(self.preview.clone())
    }

    pub(super) fn target_executable(
        &self,
        target: ReleaseTarget,
    ) -> Result<ReleaseMember, PackageFailure> {
        let path = target_package_path(target, target.executable_path());
        let member = self
            .tree
            .members()
            .iter()
            .find(|member| member.path() == path)
            .cloned()
            .ok_or_else(|| {
                PackageFailure::Moved(format!("release manifest has no {target} target"))
            })?;
        let limit = self
            .budget
            .members
            .get(&path)
            .expect("admitted tree member has a budget shape")
            .limit;
        Ok(ReleaseMember { member, limit })
    }
}

fn validate_notes(identity: &Digest, notes: &[u8]) -> Result<(), String> {
    if identity == &Digest::compute(Domain::DistReleaseNotes, notes) {
        Ok(())
    } else {
        Err("release note bytes disagree with their identity".to_owned())
    }
}

fn package_input<'a>(
    loaded: &'a Loaded,
    version: &'a ProductVersion,
    notes_identity: Digest,
) -> proof::PackageInput<'a> {
    let targets = loaded
        .targets
        .iter()
        .map(|target| {
            proof::PackageTargetInput::new(
                target.target,
                target.executable.clone(),
                target.payload.clone(),
                target.proof.clone(),
            )
            .with_debug(target.debug.clone())
        })
        .collect();
    proof::PackageInput::new(
        version,
        notes_identity,
        loaded.notes.clone(),
        loaded.manifest.clone(),
        targets,
    )
}

fn load(
    document: ReleaseManifest,
    notes: proof::PackageMember,
    manifest_member: proof::PackageMember,
    mut files: BTreeMap<String, proof::PackageMember>,
) -> Result<Loaded, PackageFailure> {
    let mut targets = Vec::with_capacity(document.targets.len());
    for target in document.targets {
        let payload_path = format!("targets/{}/payload.json", target.target);
        let payload = take_member(&mut files, &payload_path)?;
        let payload_bytes = payload
            .read_bounded(proof::PACKAGE_DOCUMENT_LIMIT)
            .map_err(|error| PackageFailure::Moved(error.to_string()))?;
        let admitted = proof::admit(&payload_bytes)
            .map_err(|error| PackageFailure::Moved(error.to_string()))?;
        if admitted.digest() != &target.payload
            || admitted.value().target != target.target
            || admitted.value().inventory != target.inventory
        {
            return Err(PackageFailure::Moved(format!(
                "staged {} payload disagrees with the release manifest",
                target.target
            )));
        }
        let executable = take_member(
            &mut files,
            &target_package_path(target.target, target.target.executable_path()),
        )?;
        let proof = take_member(&mut files, &format!("targets/{}/proof.json", target.target))?;
        let debug = target
            .target
            .debug_path()
            .map(|path| take_member(&mut files, &target_package_path(target.target, path)))
            .transpose()?;
        targets.push(LoadedTarget {
            target: target.target,
            executable,
            payload,
            proof,
            debug,
        });
    }
    if let Some(path) = files.keys().next() {
        return Err(PackageFailure::Moved(format!(
            "release preflight retained unexpected member {path:?}"
        )));
    }
    Ok(Loaded {
        notes,
        manifest: manifest_member,
        targets,
    })
}

fn take_member(
    files: &mut BTreeMap<String, proof::PackageMember>,
    path: &str,
) -> Result<proof::PackageMember, PackageFailure> {
    files
        .remove(path)
        .ok_or_else(|| PackageFailure::Moved(format!("release preflight has no {path:?}")))
}

fn admit_pending(
    pending: BTreeMap<String, PendingFile>,
) -> Result<BTreeMap<String, proof::PackageMember>, PackageFailure> {
    pending
        .into_iter()
        .map(|(path, pending)| {
            let member = proof::PackageMember::admit_file(
                &path,
                pending.shape.mode,
                pending.file,
                pending.shape.limit,
            )
            .map_err(|error| PackageFailure::Moved(error.to_string()))?;
            if member.length() != pending.shape.length || member.digest() != &pending.shape.digest {
                return Err(PackageFailure::Moved(format!(
                    "{} object disagrees with its payload inventory",
                    pending.label
                )));
            }
            Ok((path, member))
        })
        .collect()
}

fn inline_member(
    path: &str,
    mode: u32,
    bytes: &[u8],
) -> Result<proof::PackageMember, PackageFailure> {
    proof::PackageMember::admit_inline(path, mode, bytes, proof::PACKAGE_DOCUMENT_LIMIT)
        .map_err(|error| PackageFailure::Moved(error.to_string()))
}

impl ReleasePreflight {
    fn capture(
        root: &Path,
        version: &ProductVersion,
        notes_identity: Digest,
        notes: &[u8],
        manifest_identity: &Digest,
    ) -> Result<Self, PackageFailure> {
        let stage = root.join("dist/stage");
        let (manifest_member, manifest) = capture_manifest(&stage, manifest_identity)?;
        let document: ReleaseManifest =
            wire::decode(&manifest).map_err(|error| PackageFailure::Moved(error.to_string()))?;
        let canonical =
            wire::encode(&document).map_err(|error| PackageFailure::Moved(error.to_string()))?;
        if canonical.as_slice() != manifest.as_ref() {
            return Err(PackageFailure::Moved(
                "release manifest is not canonical".to_owned(),
            ));
        }
        let package_bytes = package_bytes(
            version,
            notes_identity,
            notes,
            manifest_member.digest(),
            &document,
        )?;
        let mut pending = BTreeMap::new();
        let mut shapes = BTreeMap::new();
        let mut total = CaptureBudget::default();
        add_inline(
            &mut shapes,
            &mut total,
            "release/notes.md",
            0o644,
            notes,
            Domain::DistReleaseNotesContent,
        )?;
        add_member(
            &mut shapes,
            &mut total,
            &manifest_member,
            proof::PACKAGE_DOCUMENT_LIMIT,
        )?;
        add_targets(
            &stage,
            &document.targets,
            &mut pending,
            &mut shapes,
            &mut total,
        )?;
        add_inline(
            &mut shapes,
            &mut total,
            "release/package.json",
            0o644,
            &package_bytes,
            Domain::DistReleasePackage,
        )?;
        Ok(Self {
            manifest: document,
            manifest_member,
            package_bytes,
            pending,
            budget: ReleaseBudget {
                members: shapes,
                bytes: total.bytes,
            },
        })
    }
}

fn capture_manifest(
    stage: &Path,
    expected: &Digest,
) -> Result<(proof::PackageMember, std::sync::Arc<[u8]>), PackageFailure> {
    const PATH: &str = "release/manifest.json";
    let source = stage.join(PATH);
    let file = open_manifest(&source)?;
    let metadata = file
        .metadata()
        .map_err(|error| manifest_io_failure(error, "inspect"))?;
    let identity = ManifestIdentity::from_metadata(&metadata);
    let named =
        fs::symlink_metadata(&source).map_err(|error| manifest_io_failure(error, "inspect"))?;
    if !metadata.is_file()
        || !named.file_type().is_file()
        || ManifestIdentity::from_metadata(&named) != identity
    {
        return Err(PackageFailure::Moved(format!(
            "staged release manifest {} is not one stable regular file",
            source.display()
        )));
    }
    if metadata.len() > proof::PACKAGE_DOCUMENT_LIMIT {
        return Err(PackageFailure::Moved(format!(
            "staged release manifest {} is {} bytes; maximum is {}",
            source.display(),
            metadata.len(),
            proof::PACKAGE_DOCUMENT_LIMIT
        )));
    }
    let held = file
        .try_clone()
        .map_err(|error| manifest_io_failure(error, "retain"))?;
    let (member, bytes) =
        proof::PackageMember::admit_document_file(PATH, 0o644, file, proof::PACKAGE_DOCUMENT_LIMIT)
            .map_err(|error| match error {
                proof::PackageError::Source { .. } => {
                    PackageFailure::Environment(error.to_string())
                }
                _ => PackageFailure::Moved(error.to_string()),
            })?;
    let reopened = open_manifest(&source)?;
    if member.length() != identity.length
        || ManifestIdentity::capture(&held)? != identity
        || ManifestIdentity::capture(&reopened)? != identity
    {
        return Err(PackageFailure::Moved(
            "staged release manifest changed while it was captured".to_owned(),
        ));
    }
    if member.digest() != expected {
        return Err(PackageFailure::Moved(
            "staged release manifest object disagrees with its payload inventory".to_owned(),
        ));
    }
    Ok((member, bytes))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManifestIdentity {
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl ManifestIdentity {
    fn capture(file: &File) -> Result<Self, PackageFailure> {
        file.metadata()
            .map(|metadata| Self::from_metadata(&metadata))
            .map_err(|error| manifest_io_failure(error, "inspect"))
    }

    fn from_metadata(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

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

fn open_manifest(path: &Path) -> Result<File, PackageFailure> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options
        .open(path)
        .map_err(|error| manifest_io_failure(error, "read"))
}

fn manifest_io_failure(error: io::Error, operation: &str) -> PackageFailure {
    let detail = format!("cannot {operation} staged release manifest: {error}");
    if error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ELOOP) {
        PackageFailure::Moved(detail)
    } else {
        PackageFailure::Environment(detail)
    }
}

fn add_targets(
    stage: &Path,
    targets: &[wire::manifest::ReleaseManifestTarget],
    pending: &mut BTreeMap<String, PendingFile>,
    shapes: &mut BTreeMap<String, MemberShape>,
    total: &mut CaptureBudget,
) -> Result<(), PackageFailure> {
    for target in targets {
        add_pending(
            pending,
            shapes,
            total,
            PendingSpec {
                source: stage.join(format!("objects/payload-{}.json", target.payload)),
                path: format!("targets/{}/payload.json", target.target),
                label: format!("staged {} payload", target.target),
                mode: 0o644,
                length: target.payload_bytes,
                digest: target.payload.clone(),
                limit: proof::PACKAGE_DOCUMENT_LIMIT,
            },
        )?;
        let executable_path = target.target.executable_path();
        add_artifact(
            stage,
            pending,
            shapes,
            total,
            inventory_entry(&target.inventory, executable_path)?,
            &target_package_path(target.target, executable_path),
            &format!("staged {} executable", target.target),
        )?;
        match (target.target.debug_path(), &target.symbols) {
            (Some(path), symbols) => {
                let entry = inventory_entry(&target.inventory, path)?;
                if let Some(symbols) = symbols {
                    if (entry.mode, entry.bytes, &entry.digest)
                        != (symbols.mode, symbols.bytes, &symbols.digest)
                    {
                        return Err(PackageFailure::Moved(format!(
                            "staged {} symbol metadata disagrees with its inventory",
                            target.target
                        )));
                    }
                } else if target.target == ReleaseTarget::Aarch64AppleDarwin {
                    return Err(PackageFailure::Moved(
                        "staged macOS target has no symbol metadata".to_owned(),
                    ));
                }
                add_artifact(
                    stage,
                    pending,
                    shapes,
                    total,
                    entry,
                    &target_package_path(target.target, path),
                    &format!("staged {} debug artifact", target.target),
                )?;
            }
            (None, None) => {}
            (None, Some(_)) => {
                return Err(PackageFailure::Moved(format!(
                    "staged {} target unexpectedly carries symbol metadata",
                    target.target
                )));
            }
        }
        add_pending(
            pending,
            shapes,
            total,
            PendingSpec {
                source: stage.join(format!("objects/receipt-{}.json", target.receipt)),
                path: format!("targets/{}/proof.json", target.target),
                label: format!("staged {} proof", target.target),
                mode: 0o644,
                length: target.receipt_bytes,
                digest: target.receipt.clone(),
                limit: proof::PACKAGE_DOCUMENT_LIMIT,
            },
        )?;
    }
    Ok(())
}

struct PendingSpec {
    source: PathBuf,
    path: String,
    label: String,
    mode: u32,
    length: u64,
    digest: Digest,
    limit: u64,
}

fn add_pending(
    pending: &mut BTreeMap<String, PendingFile>,
    shapes: &mut BTreeMap<String, MemberShape>,
    total: &mut CaptureBudget,
    spec: PendingSpec,
) -> Result<(), PackageFailure> {
    let file = File::open(&spec.source).map_err(|error| {
        let detail = format!("cannot read {}: {error}", spec.label);
        if error.kind() == std::io::ErrorKind::NotFound {
            PackageFailure::Moved(detail)
        } else {
            PackageFailure::Environment(detail)
        }
    })?;
    let metadata = file.metadata().map_err(|error| {
        PackageFailure::Environment(format!("cannot inspect {}: {error}", spec.label))
    })?;
    if !metadata.is_file() {
        return Err(PackageFailure::Moved(format!(
            "{} is not a regular file",
            spec.label
        )));
    }
    if metadata.len() != spec.length {
        return Err(PackageFailure::Moved(format!(
            "{} is {} bytes; manifest declares {}",
            spec.label,
            metadata.len(),
            spec.length
        )));
    }
    total.admit(&spec.path, spec.length, spec.limit)?;
    let shape = MemberShape {
        mode: spec.mode,
        length: spec.length,
        digest: spec.digest,
        limit: spec.limit,
    };
    insert_shape(shapes, &spec.path, shape.clone())?;
    if pending
        .insert(
            spec.path.clone(),
            PendingFile {
                label: spec.label,
                file,
                shape,
            },
        )
        .is_some()
    {
        return Err(PackageFailure::Moved(format!(
            "release member {:?} occurs twice",
            spec.path
        )));
    }
    Ok(())
}

fn add_artifact(
    stage: &Path,
    pending: &mut BTreeMap<String, PendingFile>,
    shapes: &mut BTreeMap<String, MemberShape>,
    total: &mut CaptureBudget,
    entry: &InventoryEntry,
    path: &str,
    label: &str,
) -> Result<(), PackageFailure> {
    add_pending(
        pending,
        shapes,
        total,
        PendingSpec {
            source: stage.join(format!("objects/{}-{:04o}", entry.digest, entry.mode)),
            path: path.to_owned(),
            label: label.to_owned(),
            mode: entry.mode,
            length: entry.bytes,
            digest: entry.digest.clone(),
            limit: MAX_RELEASE_MEMBER_BYTES,
        },
    )
}

fn add_inline(
    shapes: &mut BTreeMap<String, MemberShape>,
    total: &mut CaptureBudget,
    path: &str,
    mode: u32,
    bytes: &[u8],
    domain: Domain,
) -> Result<(), PackageFailure> {
    let length = bytes.len() as u64;
    total.admit(path, length, proof::PACKAGE_DOCUMENT_LIMIT)?;
    insert_shape(
        shapes,
        path,
        MemberShape {
            mode,
            length,
            digest: Digest::compute(domain, bytes),
            limit: proof::PACKAGE_DOCUMENT_LIMIT,
        },
    )
}

fn add_member(
    shapes: &mut BTreeMap<String, MemberShape>,
    total: &mut CaptureBudget,
    member: &proof::PackageMember,
    limit: u64,
) -> Result<(), PackageFailure> {
    total.admit(member.path(), member.length(), limit)?;
    insert_shape(
        shapes,
        member.path(),
        MemberShape {
            mode: member.mode(),
            length: member.length(),
            digest: member.digest().clone(),
            limit,
        },
    )
}

fn insert_shape(
    shapes: &mut BTreeMap<String, MemberShape>,
    path: &str,
    shape: MemberShape,
) -> Result<(), PackageFailure> {
    if shapes.insert(path.to_owned(), shape).is_some() {
        Err(PackageFailure::Moved(format!(
            "release member {path:?} occurs twice"
        )))
    } else {
        Ok(())
    }
}

fn inventory_entry<'a>(
    inventory: &'a [InventoryEntry],
    path: &str,
) -> Result<&'a InventoryEntry, PackageFailure> {
    let mut matches = inventory.iter().filter(|entry| entry.path == path);
    let entry = matches
        .next()
        .ok_or_else(|| PackageFailure::Moved(format!("staged target payload has no {path}")))?;
    if matches.next().is_some() {
        return Err(PackageFailure::Moved(format!(
            "staged target payload repeats {path}"
        )));
    }
    Ok(entry)
}

fn package_bytes(
    version: &ProductVersion,
    notes_identity: Digest,
    notes: &[u8],
    manifest_identity: &Digest,
    manifest: &ReleaseManifest,
) -> Result<Vec<u8>, PackageFailure> {
    const TARGETS: [ReleaseTarget; 3] = [
        ReleaseTarget::Aarch64AppleDarwin,
        ReleaseTarget::X86_64UnknownLinuxMusl,
        ReleaseTarget::X86_64PcWindowsMsvc,
    ];
    if manifest.version != RELEASE_MANIFEST_VERSION || manifest.targets.len() != TARGETS.len() {
        return Err(PackageFailure::Moved(
            "release manifest has no current complete target inventory".to_owned(),
        ));
    }
    let targets = manifest
        .targets
        .iter()
        .zip(TARGETS)
        .enumerate()
        .map(|(index, (target, expected))| {
            if target.target != expected {
                return Err(PackageFailure::Moved(format!(
                    "release manifest target {index} is {}, expected {expected}",
                    target.target
                )));
            }
            let executable = inventory_entry(&target.inventory, target.target.executable_path())?;
            if executable.mode != 0o755 {
                return Err(PackageFailure::Moved(format!(
                    "staged {} executable has mode {:o}",
                    target.target, executable.mode
                )));
            }
            let debug_path = target.target.debug_path().map(|path| {
                inventory_entry(&target.inventory, path).map(|entry| {
                    if entry.mode != 0o644 {
                        return Err(PackageFailure::Moved(format!(
                            "staged {} debug artifact has mode {:o}",
                            target.target, entry.mode
                        )));
                    }
                    Ok(target_package_path(target.target, path))
                })
            });
            let debug_path = debug_path.transpose()?.transpose()?;
            Ok(ReleasePackageTarget {
                target: target.target,
                executable_path: target_package_path(
                    target.target,
                    target.target.executable_path(),
                ),
                executable: executable.digest.clone(),
                payload: target.payload.clone(),
                proof: target.receipt.clone(),
                debug_path,
                signature_path: format!("targets/{}/proof.json", target.target),
            })
        })
        .collect::<Result<Vec<_>, PackageFailure>>()?;
    wire::encode(&ReleasePackage {
        schema: RELEASE_PACKAGE_VERSION,
        package: "spice".to_owned(),
        product_version: version.clone(),
        source: manifest.source.clone(),
        manifest: manifest_identity.clone(),
        notes: notes_identity,
        notes_content: Digest::compute(Domain::DistReleaseNotesContent, notes),
        targets,
    })
    .map_err(|error| PackageFailure::Moved(error.to_string()))
}

fn target_package_path(target: ReleaseTarget, path: &str) -> String {
    format!("targets/{target}/{path}")
}

#[derive(Default)]
struct CaptureBudget {
    members: usize,
    bytes: u64,
}

impl CaptureBudget {
    fn admit(&mut self, path: &str, bytes: u64, role_limit: u64) -> Result<(), PackageFailure> {
        if role_limit == 0 || role_limit > MAX_RELEASE_MEMBER_BYTES {
            return Err(PackageFailure::Moved(format!(
                "release member {path:?} has invalid {role_limit}-byte role ceiling"
            )));
        }
        if self.members >= MAX_RELEASE_MEMBERS {
            return Err(PackageFailure::Moved(format!(
                "release index exceeds {MAX_RELEASE_MEMBERS} members"
            )));
        }
        if bytes > role_limit || bytes > MAX_RELEASE_MEMBER_BYTES {
            return Err(PackageFailure::Moved(format!(
                "release member {path:?} is {bytes} bytes; maximum is {}",
                role_limit.min(MAX_RELEASE_MEMBER_BYTES)
            )));
        }
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|total| *total <= MAX_RELEASE_BYTES)
            .ok_or_else(|| {
                PackageFailure::Moved(format!(
                    "release index exceeds {MAX_RELEASE_BYTES} aggregate bytes"
                ))
            })?;
        self.members += 1;
        Ok(())
    }
}

impl ReleaseBudget {
    fn stream(&self) -> StreamBudget<'_> {
        StreamBudget {
            expected: self,
            observed: CaptureBudget::default(),
            seen: BTreeSet::new(),
        }
    }

    fn verify(&self, members: &[proof::PackageMember]) -> Result<(), PackageFailure> {
        let mut stream = self.stream();
        for member in members {
            stream.admit(member).map_err(PackageFailure::Moved)?;
        }
        stream.finish().map_err(PackageFailure::Moved)
    }
}

struct StreamBudget<'a> {
    expected: &'a ReleaseBudget,
    observed: CaptureBudget,
    seen: BTreeSet<String>,
}

impl StreamBudget<'_> {
    fn admit(&mut self, member: &proof::PackageMember) -> Result<(), String> {
        let shape =
            self.expected.members.get(member.path()).ok_or_else(|| {
                format!("release stream has unexpected member {:?}", member.path())
            })?;
        let observed = MemberShape {
            mode: member.mode(),
            length: member.length(),
            digest: member.digest().clone(),
            limit: shape.limit,
        };
        if &observed != shape {
            return Err(format!(
                "release stream member {:?} moved from its admitted shape",
                member.path()
            ));
        }
        if !self.seen.insert(member.path().to_owned()) {
            return Err(format!(
                "release stream member {:?} occurs twice",
                member.path()
            ));
        }
        self.observed
            .admit(member.path(), member.length(), shape.limit)
            .map_err(|error| error.to_string())
    }

    fn finish(self) -> Result<(), String> {
        if self.seen.len() != self.expected.members.len()
            || self.observed.members != self.expected.members.len()
            || self.observed.bytes != self.expected.bytes
        {
            return Err(
                "release stream did not consume its complete admitted inventory".to_owned(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
