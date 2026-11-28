//! Authenticated macOS dSYM retention beside the installed executable.
//!
//! A package payload may bind one `MacSymbolArtifact` to the exact executable
//! it ships. When the package carries that artifact, the installer
//! republishes the bundle beside the executable and records its provenance in
//! the same receipt, so the pair publishes, refreshes, rolls back, and
//! unapplies as one generation. Published release evidence omits symbols, so
//! a package without the artifact installs the executable alone.

use std::fs;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use wire::manifest::{
    MacSymbolArtifact, MacSymbolMetadata, ReleaseTarget, git_commit_is_canonical,
    macho_uuid_is_canonical,
};
use wire::{Digest, Domain};

use super::{
    Asset, DATA_MODE, GeneratedImage, InstallError, InstallPlan, InstalledOperations, Location,
    PlanState, Repository, push_operation,
};

/// Canonical retained dSYM bundle beside the installed executable.
pub(super) const SYMBOLS_PATH: &str = ".spice-install/bin/spice.dSYM";
pub(super) const SYMBOLS_INFO_PATH: &str = ".spice-install/bin/spice.dSYM/Contents/Info.plist";
pub(super) const SYMBOLS_DWARF_PATH: &str =
    ".spice-install/bin/spice.dSYM/Contents/Resources/DWARF/spice";
/// Symbol assets in their sole publication order, after the executable.
pub(super) const ASSETS: [Asset; 2] = [Asset::SymbolsInfo, Asset::SymbolsDwarf];

const MH_MAGIC_64: u32 = 0xFEED_FACF;
const MH_HEADER_64_LENGTH: usize = 32;
const LC_UUID: u32 = 0x1B;
const LC_UUID_LENGTH: usize = 24;
const LC_SEGMENT_64: u32 = 0x19;
const LC_SEGMENT_64_LENGTH: usize = 72;
const TEXT_SEGMENT: &[u8] = b"__TEXT\0";
const SHA256_HEX_LENGTH: usize = 64;

/// Provenance of the executable and dSYM one install record retains.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolProvenance {
    executable_sha256: String,
    uuid: String,
    commit: String,
    dirty: bool,
    path: String,
}

impl SymbolProvenance {
    /// Lowercase hex SHA-256 of the installed executable bytes.
    #[must_use]
    pub fn executable_sha256(&self) -> &str {
        &self.executable_sha256
    }

    /// Mach-O `LC_UUID` shared by the executable and its dSYM.
    #[must_use]
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// Full Git commit the executable was linked from.
    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }

    /// Whether tracked files differed from that commit when linked.
    #[must_use]
    pub const fn dirty(&self) -> bool {
        self.dirty
    }

    /// Git-common-relative path of the retained dSYM bundle.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    pub(super) fn validate(&self) -> Result<(), InstallError> {
        let sha256_canonical = self.executable_sha256.len() == SHA256_HEX_LENGTH
            && self
                .executable_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !sha256_canonical
            || !macho_uuid_is_canonical(&self.uuid)
            || !git_commit_is_canonical(&self.commit)
            || self.path != SYMBOLS_PATH
        {
            return Err(InstallError::InvalidRecord(
                "install symbol provenance is not canonical".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Symbol bundle authenticated against the selected executable.
pub(super) struct Source {
    provenance: SymbolProvenance,
    info: Vec<u8>,
    dwarf: Vec<u8>,
}

impl Source {
    /// Resolve the symbols bound to `binary`, if any.
    ///
    /// A packaged executable resolves through its sealed payload (see
    /// [`Self::resolve_package`]). A worktree build carries no payload, so it
    /// instead looks beside the executable for the sibling dSYM a
    /// symbol-generating build writes (see [`Self::resolve_worktree`]). A
    /// build selected from neither place publishes no symbols, and that
    /// absence is not an error.
    pub(super) fn resolve(
        repository: &Repository,
        binary: &Path,
    ) -> Result<Option<Self>, InstallError> {
        match super::canvas::package_layout(binary) {
            Some((_, target_root)) => Self::resolve_package(binary, target_root),
            None => Self::resolve_worktree(binary, || worktree_git_state(repository)),
        }
    }

    /// Resolve the symbol artifact a sealed package payload binds to `binary`.
    ///
    /// The payload names symbol metadata for the Darwin target and forbids it
    /// for Linux and Windows. Release evidence omits the artifact itself, so
    /// an absent artifact publishes no symbols; a present one must match the
    /// payload and bind the exact executable, UUID, and commit, or the
    /// install refuses.
    fn resolve_package(binary: &Path, target_root: &Path) -> Result<Option<Self>, InstallError> {
        let payload_path = target_root.join("payload.json");
        let payload_bytes = fs::read(&payload_path)
            .map_err(|error| InstallError::io("read appliance payload", &payload_path, error))?;
        let admitted = proof::admit(&payload_bytes)
            .map_err(|error| InstallError::InvalidRecord(format!("appliance payload: {error}")))?;
        let payload = admitted.value();
        let metadata = match (payload.target, payload.symbols.as_ref()) {
            (ReleaseTarget::Aarch64AppleDarwin, Some(metadata)) => metadata,
            (ReleaseTarget::X86_64UnknownLinuxMusl | ReleaseTarget::X86_64PcWindowsMsvc, None) => {
                return Ok(None);
            }
            (ReleaseTarget::Aarch64AppleDarwin, None) => {
                return Err(refused(&payload_path, "names no macOS symbol artifact"));
            }
            (ReleaseTarget::X86_64UnknownLinuxMusl, Some(_)) => {
                return Err(refused(&payload_path, "names a symbol artifact for Linux"));
            }
            (ReleaseTarget::X86_64PcWindowsMsvc, Some(_)) => {
                return Err(refused(
                    &payload_path,
                    "names a symbol artifact for Windows",
                ));
            }
        };
        let binary_bytes = super::read_binary(binary)?;
        let native = Digest::compute(Domain::DistArtifact, &binary_bytes);
        if native != payload.native || metadata.executable != native {
            return Err(refused(binary, "is not the executable the payload binds"));
        }
        let artifact_path = target_root.join(&metadata.path);
        let Some(artifact) = read_artifact(&artifact_path, metadata)? else {
            return Ok(None);
        };
        if artifact.executable != native {
            return Err(refused(
                &artifact_path,
                "binds another executable than the payload",
            ));
        }
        let uuid = executable_uuid(&binary_bytes)
            .ok_or_else(|| refused(binary, "carries no thin 64-bit Mach-O LC_UUID"))?;
        if artifact.uuid != metadata.uuid || uuid != artifact.uuid {
            return Err(refused(&artifact_path, "UUID differs from the executable"));
        }
        if executable_uuid(&artifact.dwarf).as_deref() != Some(uuid.as_str()) {
            return Err(refused(
                &artifact_path,
                "DWARF UUID differs from the executable",
            ));
        }
        if !artifact.commit_is_canonical()
            || Digest::compute(Domain::DistSourceCommit, artifact.commit.as_bytes())
                != payload.source
        {
            return Err(refused(
                &artifact_path,
                "names another commit than the payload",
            ));
        }
        Ok(Some(Self {
            provenance: SymbolProvenance {
                executable_sha256: format!("{:x}", Sha256::digest(&binary_bytes)),
                uuid,
                commit: artifact.commit,
                dirty: artifact.dirty,
                path: SYMBOLS_PATH.to_owned(),
            },
            info: artifact.info,
            dwarf: artifact.dwarf,
        }))
    }

    /// Resolve a dSYM generated for `binary` directly in a worktree build.
    ///
    /// A worktree build carries no sealed payload, so this trusts nothing but
    /// the files themselves: the dSYM must sit at the fixed sibling path a
    /// symbol-generating build writes (`<binary>.dSYM`), and its DWARF image
    /// must declare the exact `LC_UUID` the executable does. Anything else --
    /// an absent, partial, stale, or foreign dSYM -- publishes no symbols
    /// rather than refusing the install; only a build that explicitly asked
    /// for symbols ever writes one at that path, and an ordinary worktree
    /// install never does. `git_state` reads the linked commit and whether
    /// tracked files differ from it only once the bundle matches.
    fn resolve_worktree(
        binary: &Path,
        git_state: impl FnOnce() -> Option<(String, bool)>,
    ) -> Result<Option<Self>, InstallError> {
        let Some(name) = binary.file_name() else {
            return Ok(None);
        };
        let bundle = worktree_dsym_path(binary);
        let dwarf_path = bundle.join("Contents/Resources/DWARF").join(name);
        let info_path = bundle.join("Contents/Info.plist");
        if !dwarf_path.is_file() || !info_path.is_file() {
            return Ok(None);
        }
        let binary_bytes = super::read_binary(binary)?;
        let Some(uuid) = executable_uuid(&binary_bytes) else {
            return Ok(None);
        };
        let Ok(dwarf) = fs::read(&dwarf_path) else {
            return Ok(None);
        };
        if executable_uuid(&dwarf).as_deref() != Some(uuid.as_str()) {
            return Ok(None);
        }
        let Ok(info) = fs::read(&info_path) else {
            return Ok(None);
        };
        let Some((commit, dirty)) = git_state() else {
            return Ok(None);
        };
        Ok(Some(Self {
            provenance: SymbolProvenance {
                executable_sha256: format!("{:x}", Sha256::digest(&binary_bytes)),
                uuid,
                commit,
                dirty,
                path: SYMBOLS_PATH.to_owned(),
            },
            info,
            dwarf,
        }))
    }

    pub(super) const fn provenance(&self) -> &SymbolProvenance {
        &self.provenance
    }

    pub(super) fn bytes(&self, asset: Asset) -> Option<&[u8]> {
        match asset {
            Asset::SymbolsInfo => Some(&self.info),
            Asset::SymbolsDwarf => Some(&self.dwarf),
            _ => None,
        }
    }

    pub(super) fn differs_from(&self, plan: &InstallPlan) -> bool {
        plan.symbols.as_ref() != Some(&self.provenance)
            || ASSETS.into_iter().any(|asset| {
                let expected = self
                    .bytes(asset)
                    .map(|bytes| GeneratedImage::new(DATA_MODE, bytes));
                plan.operations
                    .iter()
                    .find(|operation| operation.asset == asset)
                    .map(|operation| &operation.generated)
                    != expected.as_ref()
            })
    }
}

/// Append the two symbol operations after the executable operation.
pub(super) fn append_operations(
    repository: &Repository,
    installed: &InstalledOperations<'_>,
    source: &Source,
    (directories, operations): PlanState<'_>,
) -> Result<(), InstallError> {
    for asset in ASSETS {
        let path = path(asset).expect("symbol assets publish fixed paths");
        let bytes = source.bytes(asset).expect("symbol assets carry bytes");
        push_operation(
            repository,
            installed,
            (&mut *directories, &mut *operations),
            asset,
            (Location::CommonGit, path.to_owned()),
            None,
            GeneratedImage::new(DATA_MODE, bytes),
        )?;
    }
    Ok(())
}

/// The retained bundle path a symbol asset publishes.
pub(super) fn path(asset: Asset) -> Option<&'static str> {
    match asset {
        Asset::SymbolsInfo => Some(SYMBOLS_INFO_PATH),
        Asset::SymbolsDwarf => Some(SYMBOLS_DWARF_PATH),
        _ => None,
    }
}

fn refused(path: &Path, reason: &'static str) -> InstallError {
    InstallError::SymbolsRefused {
        path: path.to_path_buf(),
        reason,
    }
}

/// Sibling dSYM path a worktree symbol build writes beside its executable.
fn worktree_dsym_path(binary: &Path) -> PathBuf {
    let mut path = binary.as_os_str().to_owned();
    path.push(".dSYM");
    PathBuf::from(path)
}

/// Read the repository's current commit and whether tracked files differ
/// from it, or `None` when either read is unavailable or inconclusive.
///
/// A worktree symbol bundle is generated and consumed in the same command
/// invocation, so this is expected to succeed whenever [`Source::resolve`]
/// finds one; any failure here is treated the same as no dSYM at all rather
/// than an install refusal.
fn worktree_git_state(repository: &Repository) -> Option<(String, bool)> {
    let root = repository.worktree_root();
    let revision = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output()
        .ok()?;
    if !revision.status.success() {
        return None;
    }
    let commit = String::from_utf8(revision.stdout).ok()?.trim().to_owned();
    if !git_commit_is_canonical(&commit) {
        return None;
    }
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--quiet", "HEAD", "--"])
        .status()
        .ok()?;
    match status.code() {
        Some(0) => Some((commit, false)),
        Some(1) => Some((commit, true)),
        _ => None,
    }
}

/// Whether one opened file stayed unchanged and bound to its path across the
/// whole read.
fn settled_in_place(
    opened: sill::authority::FileIdentity,
    held: sill::authority::FileIdentity,
    named: sill::authority::FileIdentity,
) -> bool {
    opened == held && held == named
}

/// Read the artifact through one handle and prove it never moved beneath it.
///
/// An absent artifact answers `None`; any other unreadable, foreign, or
/// moving artifact refuses.
fn read_artifact(
    path: &Path,
    metadata: &MacSymbolMetadata,
) -> Result<Option<MacSymbolArtifact>, InstallError> {
    let mut file = match sill::authority::open_unfollowed(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(InstallError::io(
                "open appliance symbol artifact",
                path,
                error,
            ));
        }
    };
    let opened_metadata = file
        .metadata()
        .map_err(|error| InstallError::io("inspect appliance symbol artifact", path, error))?;
    if !opened_metadata.is_file() {
        return Err(refused(path, "is not a regular file"));
    }
    let opened = sill::authority::file_identity(&file)
        .map_err(|error| InstallError::io("inspect appliance symbol artifact", path, error))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| InstallError::io("read appliance symbol artifact", path, error))?;
    let held = sill::authority::file_identity(&file)
        .map_err(|error| InstallError::io("reinspect appliance symbol artifact", path, error))?;
    let named = sill::authority::path_identity(path)
        .map_err(|error| InstallError::io("reinspect appliance symbol path", path, error))?;
    if !settled_in_place(opened, held, named) {
        return Err(refused(path, "moved while it was read"));
    }
    if !has_data_mode(&opened_metadata)
        || metadata.mode != DATA_MODE
        || bytes.len() as u64 != metadata.bytes
        || Digest::compute(Domain::DistArtifact, &bytes) != metadata.digest
    {
        return Err(refused(path, "differs from the authenticated payload"));
    }

    let artifact = MacSymbolArtifact::decode(&bytes)
        .map_err(|error| InstallError::InvalidRecord(format!("symbol artifact: {error}")))?;
    if !artifact.uuid_is_canonical() || artifact.info.is_empty() || artifact.dwarf.is_empty() {
        return Err(refused(path, "is not a canonical current symbol artifact"));
    }
    Ok(Some(artifact))
}
#[cfg(unix)]
fn has_data_mode(metadata: &fs::Metadata) -> bool {
    metadata.mode() & 0o777 == DATA_MODE
}

#[cfg(windows)]
fn has_data_mode(metadata: &fs::Metadata) -> bool {
    !metadata.permissions().readonly()
}

/// Load commands of a thin little-endian 64-bit Mach-O image, in order.
///
/// Each entry pairs the command kind with its complete bytes, including the
/// eight-byte command header. Any header or command that does not fit the
/// image rejects the whole image rather than the one command.
pub(super) fn load_commands(bytes: &[u8]) -> Option<Vec<(u32, &[u8])>> {
    let word = |offset: usize| {
        bytes
            .get(offset..offset + 4)
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
    };
    if word(0)? != MH_MAGIC_64 {
        return None;
    }
    let commands = word(16)? as usize;
    let commands_length = word(20)? as usize;
    let end = MH_HEADER_64_LENGTH.checked_add(commands_length)?;
    if end > bytes.len() {
        return None;
    }
    let mut offset = MH_HEADER_64_LENGTH;
    let mut loaded = Vec::new();
    for _ in 0..commands {
        let command = word(offset)?;
        let length = word(offset + 4)? as usize;
        if length < 8 || offset.checked_add(length)? > end {
            return None;
        }
        loaded.push((command, &bytes[offset..offset + length]));
        offset += length;
    }
    Some(loaded)
}

/// `LC_UUID` of a thin little-endian 64-bit Mach-O image, canonically spelled.
pub(super) fn executable_uuid(bytes: &[u8]) -> Option<String> {
    let mut uuid = None;
    for (command, body) in load_commands(bytes)? {
        if command == LC_UUID {
            if body.len() != LC_UUID_LENGTH || uuid.is_some() {
                return None;
            }
            uuid = Some(format_uuid(&body[8..LC_UUID_LENGTH]));
        }
    }
    uuid
}

/// Preferred address of the `__TEXT` segment, from which every sampled
/// runtime address slides by one image-wide offset.
pub(super) fn text_segment_address(bytes: &[u8]) -> Option<u64> {
    load_commands(bytes)?
        .into_iter()
        .find_map(|(command, body)| {
            if command != LC_SEGMENT_64 || body.len() < LC_SEGMENT_64_LENGTH {
                return None;
            }
            let name = &body[8..24];
            let address = body[24..32].try_into().map(u64::from_le_bytes).ok()?;
            name.starts_with(TEXT_SEGMENT).then_some(address)
        })
}

fn format_uuid(raw: &[u8]) -> String {
    let hex = raw
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Minimal thin 64-bit Mach-O bytes carrying exactly one `LC_UUID`.
#[cfg(test)]
pub(super) fn macho_with_uuid(uuid: [u8; 16]) -> Vec<u8> {
    let mut image = Vec::new();
    image.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
    image.extend_from_slice(&[0; 12]);
    image.extend_from_slice(&1_u32.to_le_bytes());
    image.extend_from_slice(&(LC_UUID_LENGTH as u32).to_le_bytes());
    image.extend_from_slice(&[0; 8]);
    image.extend_from_slice(&LC_UUID.to_le_bytes());
    image.extend_from_slice(&(LC_UUID_LENGTH as u32).to_le_bytes());
    image.extend_from_slice(&uuid);
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One scratch directory, removed again when the test that made it ends.
    struct Scratch(std::path::PathBuf);

    impl std::ops::Deref for Scratch {
        type Target = std::path::Path;

        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl AsRef<std::path::Path> for Scratch {
        fn as_ref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Scratch {
        let root = std::env::temp_dir().join(format!(
            "spice-install-symbols-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&root).expect("symbol scratch creates");
        Scratch(root)
    }

    fn repository(label: &str) -> (Scratch, Repository) {
        let root = scratch(label);
        crate::test_repository::initialize(&root);
        let repository = Repository::discover(&root).expect("scratch repository discovers");
        (root, repository)
    }

    #[test]
    fn resolve_worktree_publishes_a_matching_sibling_dsym_and_ignores_a_mismatched_one() {
        let root = scratch("worktree");
        let binary = root.join("spice");
        let uuid = [0xAA; 16];
        fs::write(&binary, macho_with_uuid(uuid)).unwrap();
        let commit = "a".repeat(40);
        let git_state = |dirty: bool| {
            let commit = commit.clone();
            move || Some((commit, dirty))
        };

        // No sibling dSYM yet: an ordinary worktree build publishes nothing.
        assert!(
            Source::resolve_worktree(&binary, git_state(false))
                .unwrap()
                .is_none()
        );

        // A matching sibling dSYM resolves, carrying the worktree's commit.
        let dwarf_directory = worktree_dsym_path(&binary).join("Contents/Resources/DWARF");
        fs::create_dir_all(&dwarf_directory).unwrap();
        fs::write(dwarf_directory.join("spice"), macho_with_uuid(uuid)).unwrap();
        fs::write(
            worktree_dsym_path(&binary).join("Contents/Info.plist"),
            b"<plist/>",
        )
        .unwrap();
        let source = Source::resolve_worktree(&binary, git_state(true))
            .unwrap()
            .expect("a matching sibling dSYM resolves");
        assert_eq!(source.provenance().commit(), commit);
        assert!(source.provenance().dirty());
        assert_eq!(source.provenance().uuid(), format_uuid(&uuid));
        assert_eq!(source.provenance().path(), SYMBOLS_PATH);
        assert_eq!(
            source.bytes(Asset::SymbolsDwarf),
            Some(macho_with_uuid(uuid)).as_deref()
        );

        // Unreadable worktree provenance publishes nothing, not a refusal.
        assert!(
            Source::resolve_worktree(&binary, || None)
                .unwrap()
                .is_none()
        );

        // A dSYM whose DWARF UUID disagrees publishes nothing, not a refusal.
        fs::write(dwarf_directory.join("spice"), macho_with_uuid([0xBB; 16])).unwrap();
        assert!(
            Source::resolve_worktree(&binary, git_state(false))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_package_without_its_symbol_artifact_previews_and_applies_the_executable_alone() {
        let (root, repository) = repository("package");
        let package = root.join("package");
        fs::create_dir_all(package.join("release")).unwrap();
        fs::write(package.join("release/package.json"), b"{}").unwrap();
        let target_root = package.join("targets/aarch64-apple-darwin");
        fs::create_dir_all(target_root.join("bin")).unwrap();
        let binary = target_root.join("bin/spice");
        let app = Digest::compute(Domain::DistArtifact, b"app generation");
        let executable = super::super::canvas::linked(&app);
        fs::write(&binary, &executable).unwrap();
        let symbols = b"symbol fixture";
        fs::write(
            target_root.join("payload.json"),
            darwin_payload(&executable, symbols, &app),
        )
        .unwrap();
        assert_eq!(
            super::super::canvas::package_layout(&binary).map(|(_, target)| target),
            Some(target_root.as_path()),
        );

        // A present artifact the payload does not bind refuses before install
        // state exists.
        let artifact = target_root.join("symbols/spice.symbols");
        fs::create_dir(artifact.parent().unwrap()).unwrap();
        fs::write(&artifact, b"foreign symbol").unwrap();
        assert!(matches!(
            super::super::preview(&repository, Some(&binary)),
            Err(InstallError::SymbolsRefused { .. })
        ));
        assert!(!repository.common_dir().join(".spice-install").exists());
        fs::remove_file(&artifact).unwrap();

        // Release evidence omits the artifact the payload names. The real
        // preview and digest-bound apply publish only the authenticated
        // executable.
        let preview =
            super::super::preview(&repository, Some(&binary)).expect("package install previews");
        assert!(preview.plan().symbols().is_none());
        assert!(preview.plan().operations().iter().all(|operation| {
            operation.path() != SYMBOLS_INFO_PATH && operation.path() != SYMBOLS_DWARF_PATH
        }));
        let expected = preview.digest().clone();
        let receipt = super::super::apply(&repository, Some(&binary), &expected)
            .expect("digest-bound package install applies");
        assert_eq!(receipt.digest(), &expected);
        assert!(receipt.plan().symbols().is_none());
        assert!(receipt.plan().operations().iter().all(|operation| {
            operation.path() != SYMBOLS_INFO_PATH && operation.path() != SYMBOLS_DWARF_PATH
        }));
        assert_eq!(
            fs::read(repository.common_dir().join(super::super::BINARY_PATH)).unwrap(),
            executable,
        );
        assert!(!repository.common_dir().join(SYMBOLS_PATH).exists());
    }

    fn darwin_payload(executable: &[u8], symbols: &[u8], app: &Digest) -> Vec<u8> {
        use wire::manifest::{BuildPins, InventoryEntry, TARGET_PAYLOAD_VERSION, TargetPayload};

        let native = Digest::compute(Domain::DistArtifact, executable);
        let canvas = rack::link::extract(executable).expect("fixture canvas extracts");
        let metadata = MacSymbolMetadata {
            path: "symbols/spice.symbols".to_owned(),
            mode: DATA_MODE,
            bytes: symbols.len() as u64,
            digest: Digest::compute(Domain::DistArtifact, symbols),
            uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
            executable: native.clone(),
        };
        let recipe = b"symbol recipe fixture";
        let inventory = vec![
            InventoryEntry {
                path: "bin/spice".to_owned(),
                mode: 0o755,
                bytes: executable.len() as u64,
                digest: native.clone(),
            },
            InventoryEntry {
                path: "symbols/recipe.json".to_owned(),
                mode: DATA_MODE,
                bytes: recipe.len() as u64,
                digest: Digest::compute(Domain::DistArtifact, recipe),
            },
            InventoryEntry {
                path: metadata.path.clone(),
                mode: metadata.mode,
                bytes: metadata.bytes,
                digest: metadata.digest.clone(),
            },
        ];
        let pins = BuildPins {
            rust: "rustc 1.97.1".to_owned(),
            wasm_target: "wasm32-unknown-unknown".to_owned(),
            trunk: "trunk 0.21.0".to_owned(),
            proof_container: format!("spice/proof@sha256:{}", "1".repeat(64)),
        };
        wire::encode(&TargetPayload {
            version: TARGET_PAYLOAD_VERSION,
            target: ReleaseTarget::Aarch64AppleDarwin,
            source: Digest::compute(Domain::DistSourceCommit, b"source"),
            archive: Digest::compute(Domain::DistSourceArchive, b"source archive"),
            app: app.clone(),
            canvas: Digest::compute(Domain::DistArtifact, canvas),
            native,
            symbols: Some(metadata),
            pins_digest: Digest::compute(
                Domain::DistPins,
                &wire::encode(&pins).expect("pins encode"),
            ),
            inventory_digest: Digest::compute(
                Domain::DistInventory,
                &wire::encode(&inventory).expect("inventory encodes"),
            ),
            pins,
            inventory,
        })
        .expect("payload encodes")
    }

    #[test]
    fn a_read_settles_only_on_one_unchanged_named_file_identity() {
        let root = scratch("settled");
        let first = root.join("first");
        let second = root.join("second");
        fs::write(&first, b"artifact").unwrap();
        fs::write(&second, b"artifact").unwrap();
        let opened_file = fs::File::open(&first).unwrap();
        let opened = sill::authority::file_identity(&opened_file).unwrap();
        let named = sill::authority::path_identity(&first).unwrap();
        assert!(settled_in_place(opened, opened, named));

        // Another object of the same length can never impersonate the opened
        // file, regardless of matching timestamps or permissions.
        let replacement = sill::authority::path_identity(&second).unwrap();
        assert_ne!(opened.object(), replacement.object());
        assert!(!settled_in_place(opened, opened, replacement));

        // The same object changing while held also refuses.
        fs::write(&first, b"artifact grew").unwrap();
        let changed = sill::authority::file_identity(&opened_file).unwrap();
        assert!(!settled_in_place(opened, changed, changed));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn executable_uuid_reads_exactly_one_thin_64_bit_lc_uuid() {
        let raw: [u8; 16] = [
            0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0x9A, 0xBC, 0xDE, 0xF0, 0x12, 0x34,
            0x56, 0x78,
        ];
        let mut image = Vec::new();
        image.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
        image.extend_from_slice(&[0; 12]);
        image.extend_from_slice(&2_u32.to_le_bytes());
        image.extend_from_slice(&(16 + LC_UUID_LENGTH as u32).to_le_bytes());
        image.extend_from_slice(&[0; 8]);
        // A non-UUID command precedes the UUID command.
        image.extend_from_slice(&0x32_u32.to_le_bytes());
        image.extend_from_slice(&16_u32.to_le_bytes());
        image.extend_from_slice(&[0; 8]);
        image.extend_from_slice(&LC_UUID.to_le_bytes());
        image.extend_from_slice(&(LC_UUID_LENGTH as u32).to_le_bytes());
        image.extend_from_slice(&raw);
        assert_eq!(
            executable_uuid(&image).as_deref(),
            Some("ABCDEF01-2345-6789-9ABC-DEF012345678")
        );
        let mut truncated = image.clone();
        truncated.truncate(image.len() - 1);
        assert_eq!(executable_uuid(&truncated), None);
        let mut fat = image.clone();
        fat[..4].copy_from_slice(&0xCAFE_BABE_u32.to_le_bytes());
        assert_eq!(executable_uuid(&fat), None);
        let mut doubled = image.clone();
        doubled[16..20].copy_from_slice(&3_u32.to_le_bytes());
        doubled[20..24].copy_from_slice(&(16 + 2 * LC_UUID_LENGTH as u32).to_le_bytes());
        doubled.extend_from_slice(&LC_UUID.to_le_bytes());
        doubled.extend_from_slice(&(LC_UUID_LENGTH as u32).to_le_bytes());
        doubled.extend_from_slice(&raw);
        assert_eq!(executable_uuid(&doubled), None);
        assert_eq!(executable_uuid(b"#!/bin/sh\n"), None);
    }

    #[test]
    fn text_segment_address_reads_only_the_text_segment_command() {
        let segment = |name: &[u8], address: u64| {
            let mut command = Vec::new();
            command.extend_from_slice(&LC_SEGMENT_64.to_le_bytes());
            command.extend_from_slice(&(LC_SEGMENT_64_LENGTH as u32).to_le_bytes());
            let mut segname = [0_u8; 16];
            segname[..name.len()].copy_from_slice(name);
            command.extend_from_slice(&segname);
            command.extend_from_slice(&address.to_le_bytes());
            command.extend_from_slice(&[0; LC_SEGMENT_64_LENGTH - 32]);
            command
        };
        let mut image = Vec::new();
        image.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
        image.extend_from_slice(&[0; 12]);
        image.extend_from_slice(&2_u32.to_le_bytes());
        image.extend_from_slice(&(2 * LC_SEGMENT_64_LENGTH as u32).to_le_bytes());
        image.extend_from_slice(&[0; 8]);
        image.extend_from_slice(&segment(b"__PAGEZERO", 0));
        image.extend_from_slice(&segment(b"__TEXT", 0x1_0000_0000));
        assert_eq!(text_segment_address(&image), Some(0x1_0000_0000));
        assert_eq!(executable_uuid(&image), None);
        let mut without_text = image.clone();
        without_text[16..20].copy_from_slice(&1_u32.to_le_bytes());
        without_text[20..24].copy_from_slice(&(LC_SEGMENT_64_LENGTH as u32).to_le_bytes());
        without_text.truncate(MH_HEADER_64_LENGTH + LC_SEGMENT_64_LENGTH);
        assert_eq!(text_segment_address(&without_text), None);
        let mut short = image.clone();
        short[MH_HEADER_64_LENGTH + LC_SEGMENT_64_LENGTH + 4..][..4]
            .copy_from_slice(&8_u32.to_le_bytes());
        assert_eq!(text_segment_address(&short), None);
    }
}
