use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use sha2::{Digest as _, Sha256};
use wire::ReleaseNotes;
use wire::manifest::{
    InventoryEntry, ProductVersion, ReleaseManifest, SYMBOL_RECIPE_VERSION, SymbolRecipe,
    TargetPayload,
};
use zip::write::SimpleFileOptions;

use super::PublishError;

pub(super) const ASSET_NAME: &str = "spice-release-evidence.zip";
const DATA_MODE: u32 = 0o644;
const MAX_PACKAGE_BYTES: u64 = 1024 * 1024;
const MAX_MEMBER_BYTES: u64 = 512 * 1024 * 1024;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

pub(super) struct CapturedPackage {
    pub(super) bytes: u64,
    pub(super) sha256: String,
}

/// Admit the complete sealed stage once, then publish its small deterministic
/// evidence projection without duplicating native executables or symbols.
pub(super) fn capture(
    stage_root: &Path,
    assets_root: &Path,
    version: &ProductVersion,
    notes: &ReleaseNotes,
) -> Result<CapturedPackage, PublishError> {
    let tree = package_tree(stage_root, version, notes)?;
    let recipes = recipe_members(stage_root)?;
    let path = assets_root.join(ASSET_NAME);
    let mut members = evidence_members(tree.members())?;
    members.extend(recipes.iter());
    publish_archive(&path, &members)?;
    let (bytes, sha256) = file_sha256(&path)?;
    if bytes > MAX_PACKAGE_BYTES {
        return Err(PublishError::Refused(format!(
            "release evidence archive exceeds {MAX_PACKAGE_BYTES} bytes"
        )));
    }
    Ok(CapturedPackage { bytes, sha256 })
}

fn package_tree(
    stage: &Path,
    version: &ProductVersion,
    notes: &ReleaseNotes,
) -> Result<proof::PackageTree, PublishError> {
    let (manifest_member, manifest_bytes) =
        document_member(stage, "release/manifest.json", "release/manifest.json")?;
    let manifest: ReleaseManifest = wire::decode(&manifest_bytes).map_err(PublishError::Decode)?;
    let mut targets = Vec::with_capacity(manifest.targets.len());
    for sealed in &manifest.targets {
        let target = sealed.target;
        let payload_path = format!("targets/{target}/payload.json");
        let (payload_member, payload_bytes) = document_member(stage, &payload_path, &payload_path)?;
        let payload: TargetPayload = wire::decode(&payload_bytes).map_err(PublishError::Decode)?;
        let executable = artifact_member(
            stage,
            target,
            inventory(&payload, target.executable_path())?,
        )?;
        let proof_path = format!("targets/{target}/proof.json");
        let proof = file_member(
            stage,
            &format!("objects/receipt-{}.json", sealed.receipt),
            &proof_path,
            DATA_MODE,
            proof::PACKAGE_DOCUMENT_LIMIT,
        )?;
        targets.push(proof::PackageTargetInput::new(
            target,
            executable,
            payload_member,
            proof,
        ));
    }
    let notes_member = proof::PackageMember::admit_inline(
        "release/notes.md",
        DATA_MODE,
        notes.document.as_bytes(),
        proof::PACKAGE_DOCUMENT_LIMIT,
    )?;
    proof::build_package(proof::PackageInput::new(
        version,
        notes.identity.clone(),
        notes_member,
        manifest_member,
        targets,
    ))
    .map_err(PublishError::Package)
}

fn inventory<'a>(
    payload: &'a TargetPayload,
    path: &str,
) -> Result<&'a InventoryEntry, PublishError> {
    payload
        .inventory
        .iter()
        .find(|entry| entry.path == path)
        .ok_or_else(|| {
            PublishError::Refused(format!(
                "staged {} payload has no {path} artifact",
                payload.target
            ))
        })
}

fn artifact_member(
    stage: &Path,
    target: wire::manifest::ReleaseTarget,
    entry: &InventoryEntry,
) -> Result<proof::PackageMember, PublishError> {
    file_member(
        stage,
        &format!("objects/{}-{:04o}", entry.digest, entry.mode),
        &format!("targets/{target}/{}", entry.path),
        entry.mode,
        MAX_MEMBER_BYTES,
    )
}

fn document_member(
    stage: &Path,
    source: &str,
    package: &str,
) -> Result<(proof::PackageMember, std::sync::Arc<[u8]>), PublishError> {
    let path = stage.join(source);
    let file = File::open(&path).map_err(|source| PublishError::Io {
        path: path.clone(),
        source,
    })?;
    proof::PackageMember::admit_document_file(
        package,
        DATA_MODE,
        file,
        proof::PACKAGE_DOCUMENT_LIMIT,
    )
    .map_err(PublishError::Package)
}

fn file_member(
    stage: &Path,
    source: &str,
    package: &str,
    mode: u32,
    limit: u64,
) -> Result<proof::PackageMember, PublishError> {
    let path = stage.join(source);
    let file = File::open(&path).map_err(|source| PublishError::Io {
        path: path.clone(),
        source,
    })?;
    proof::PackageMember::admit_file(package, mode, file, limit).map_err(PublishError::Package)
}

fn evidence_members(
    members: &[proof::PackageMember],
) -> Result<Vec<&proof::PackageMember>, PublishError> {
    let selected = members
        .iter()
        .filter(|member| {
            let path = member.path();
            path == "release/package.json"
                || path
                    .strip_prefix("targets/")
                    .is_some_and(|rest| rest.matches('/').count() == 1)
                    && (path.ends_with("/payload.json") || path.ends_with("/proof.json"))
        })
        .collect::<Vec<_>>();
    if selected.len() != 7 {
        return Err(PublishError::Refused(format!(
            "sealed release evidence has {} members, expected 7",
            selected.len()
        )));
    }
    Ok(selected)
}

fn recipe_members(stage: &Path) -> Result<Vec<proof::PackageMember>, PublishError> {
    let (_, manifest_bytes) =
        document_member(stage, "release/manifest.json", "release/manifest.json")?;
    let manifest: ReleaseManifest = wire::decode(&manifest_bytes).map_err(PublishError::Decode)?;
    let mut members = Vec::with_capacity(manifest.targets.len());
    for sealed in manifest.targets {
        let target = sealed.target;
        let payload_path = format!("targets/{target}/payload.json");
        let (_, payload_bytes) = document_member(stage, &payload_path, &payload_path)?;
        let payload: TargetPayload = wire::decode(&payload_bytes).map_err(PublishError::Decode)?;
        let entry = inventory(&payload, "symbols/recipe.json")?;
        let member = artifact_member(stage, target, entry)?;
        let bytes = member
            .read_bounded(proof::PACKAGE_DOCUMENT_LIMIT)
            .map_err(PublishError::Package)?;
        let recipe: SymbolRecipe = wire::decode(&bytes).map_err(PublishError::Decode)?;
        let valid = recipe.version == SYMBOL_RECIPE_VERSION
            && recipe.target == target
            && recipe.source == payload.source
            && recipe.source_archive == payload.archive
            && recipe.vendor_authority == payload.pins_digest
            && recipe.proof_container == payload.pins.proof_container
            && recipe.toolchain == payload.pins.rust
            && recipe.released_binary == payload.native
            && wire::manifest::git_commit_is_canonical(&recipe.source_commit)
            && !recipe.build_id.is_empty();
        if !valid {
            return Err(PublishError::Refused(format!(
                "staged {target} symbol recipe does not bind its target payload"
            )));
        }
        members.push(member);
    }
    if members.len() != 3 {
        return Err(PublishError::Refused(format!(
            "sealed release carries {} symbol recipes, expected 3",
            members.len()
        )));
    }
    Ok(members)
}

fn publish_archive(path: &Path, members: &[&proof::PackageMember]) -> Result<(), PublishError> {
    let package_bytes = members
        .iter()
        .try_fold(0_u64, |total, member| total.checked_add(member.length()));
    if package_bytes.is_none_or(|bytes| bytes > MAX_PACKAGE_BYTES) {
        return Err(PublishError::Refused(format!(
            "release package members exceed {MAX_PACKAGE_BYTES} bytes"
        )));
    }
    let destination = atomic::Destination::new(path).map_err(|source| PublishError::Atomic {
        path: path.to_path_buf(),
        source,
    })?;
    let mut staged = destination.stage().map_err(|source| PublishError::Atomic {
        path: path.to_path_buf(),
        source,
    })?;
    {
        let mut archive = zip::ZipWriter::new(&mut staged);
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .last_modified_time(zip::DateTime::default());
        let mut ordered = members.to_vec();
        ordered.sort_unstable_by_key(|member| member.path());
        for member in ordered {
            archive.start_file(member.path(), options.unix_permissions(member.mode()))?;
            member.copy_to(&mut archive)?;
        }
        archive.finish()?;
    }
    staged.commit().map_err(|source| PublishError::Atomic {
        path: path.to_path_buf(),
        source,
    })
}

fn file_sha256(path: &Path) -> Result<(u64, String), PublishError> {
    let mut file = File::open(path).map_err(|source| PublishError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let bytes = file
        .metadata()
        .map_err(|source| PublishError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    let mut digest = Sha256::new();
    let mut observed = 0_u64;
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer).map_err(|source| PublishError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        observed += read as u64;
        digest.update(&buffer[..read]);
    }
    if observed != bytes {
        return Err(PublishError::Refused(
            "release package moved while its checksum was captured".to_owned(),
        ));
    }
    Ok((bytes, format!("{:x}", digest.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "spice-release-package-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("scratch directory");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn evidence_archive_is_deterministic_small_and_closed() {
        let scratch = Scratch::new();
        let mut all = Vec::new();
        for target in [
            "aarch64-apple-darwin",
            "x86_64-unknown-linux-musl",
            "x86_64-pc-windows-msvc",
        ] {
            for name in ["payload.json", "proof.json"] {
                all.push(
                    proof::PackageMember::admit_inline(
                        format!("targets/{target}/{name}"),
                        0o644,
                        name.as_bytes(),
                        proof::PACKAGE_DOCUMENT_LIMIT,
                    )
                    .expect("evidence member"),
                );
            }
        }
        all.push(
            proof::PackageMember::admit_inline(
                "release/package.json",
                0o644,
                b"{}",
                proof::PACKAGE_DOCUMENT_LIMIT,
            )
            .expect("package member"),
        );
        all.push(
            proof::PackageMember::admit_inline(
                "targets/aarch64-apple-darwin/bin/spice",
                0o755,
                b"native",
                proof::PACKAGE_DOCUMENT_LIMIT,
            )
            .expect("executable member"),
        );
        all.push(
            proof::PackageMember::admit_inline(
                "targets/aarch64-apple-darwin/symbols/spice.symbols",
                0o644,
                b"symbols",
                proof::PACKAGE_DOCUMENT_LIMIT,
            )
            .expect("symbol member"),
        );
        let recipes = [
            "aarch64-apple-darwin",
            "x86_64-unknown-linux-musl",
            "x86_64-pc-windows-msvc",
        ]
        .map(|target| {
            proof::PackageMember::admit_inline(
                format!("targets/{target}/symbols/recipe.json"),
                0o644,
                br#"{"version":1}"#,
                proof::PACKAGE_DOCUMENT_LIMIT,
            )
            .expect("recipe member")
        });
        let first = scratch.0.join("first.zip");
        let second = scratch.0.join("second.zip");
        let mut members = evidence_members(&all).expect("evidence members");
        members.extend(recipes.iter());
        publish_archive(&first, &members).expect("first archive");
        let mut reversed = members;
        reversed.reverse();
        publish_archive(&second, &reversed).expect("second archive");
        assert_eq!(
            fs::read(&first).expect("first bytes"),
            fs::read(&second).expect("second bytes")
        );

        let file = File::open(first).expect("archive opens");
        let mut archive = zip::ZipArchive::new(file).expect("archive reads");
        assert_eq!(archive.len(), 10);
        let mut first_member = archive.by_index(0).expect("first member");
        assert_eq!(
            first_member.name().expect("first member name").as_ref(),
            "release/package.json"
        );
        assert_eq!(first_member.unix_mode(), Some(0o100644));
        let mut bytes = Vec::new();
        first_member.read_to_end(&mut bytes).expect("member reads");
        assert_eq!(bytes, b"{}");
        drop(first_member);
        assert!(
            archive
                .by_name("targets/aarch64-apple-darwin/bin/spice")
                .is_err()
        );
        assert!(
            archive
                .by_name("targets/aarch64-apple-darwin/symbols/spice.symbols")
                .is_err()
        );
        assert!(
            archive
                .by_name("targets/x86_64-unknown-linux-musl/symbols/recipe.json")
                .is_ok()
        );
        let extracted = scratch.0.join("extracted");
        archive.extract(&extracted).expect("archive extracts");
        assert_eq!(
            fs::read(extracted.join("release/package.json")).expect("package extracts"),
            b"{}"
        );
        assert_eq!(
            fs::read(extracted.join("targets/aarch64-apple-darwin/payload.json"))
                .expect("payload extracts"),
            b"payload.json"
        );
    }
}
