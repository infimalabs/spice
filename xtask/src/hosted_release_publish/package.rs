use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use sha2::{Digest as _, Sha256};
use wire::ReleaseNotes;
use wire::manifest::{
    InventoryEntry, ProductVersion, ReleaseManifest, ReleaseTarget, TargetPayload,
};
use zip::write::SimpleFileOptions;

use super::PublishError;

const TARGETS: [ReleaseTarget; 3] = [
    ReleaseTarget::Aarch64AppleDarwin,
    ReleaseTarget::X86_64UnknownLinuxMusl,
    ReleaseTarget::X86_64PcWindowsMsvc,
];
const DATA_MODE: u32 = 0o644;
const MAX_PACKAGE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_MEMBER_BYTES: u64 = 512 * 1024 * 1024;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

pub(super) struct CapturedPackage {
    pub(super) name: String,
    pub(super) bytes: u64,
    pub(super) sha256: String,
}

/// Admit the complete sealed stage once, then publish one deterministic
/// package projection per target without retaining another copy of its large
/// native members.
pub(super) fn capture(
    worktree: &Path,
    assets_root: &Path,
    version: &ProductVersion,
    notes: &ReleaseNotes,
) -> Result<Vec<CapturedPackage>, PublishError> {
    let tree = package_tree(&worktree.join("dist/stage"), version, notes)?;
    TARGETS
        .into_iter()
        .map(|target| {
            let name = asset_name(target);
            let path = assets_root.join(&name);
            let members = target_members(tree.members(), target)?;
            publish_archive(&path, &members)?;
            let (bytes, sha256) = file_sha256(&path)?;
            Ok(CapturedPackage {
                name,
                bytes,
                sha256,
            })
        })
        .collect()
}

pub(super) fn asset_name(target: ReleaseTarget) -> String {
    format!("spice-{target}-package.zip")
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
        let debug = target
            .debug_path()
            .map(|path| {
                inventory(&payload, path).and_then(|entry| artifact_member(stage, target, entry))
            })
            .transpose()?;
        targets.push(
            proof::PackageTargetInput::new(target, executable, payload_member, proof)
                .with_debug(debug),
        );
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

fn target_members(
    members: &[proof::PackageMember],
    target: ReleaseTarget,
) -> Result<Vec<&proof::PackageMember>, PublishError> {
    let prefix = format!("targets/{target}/");
    let selected = members
        .iter()
        .filter(|member| {
            member.path() == "release/package.json" || member.path().starts_with(&prefix)
        })
        .collect::<Vec<_>>();
    let expected = if target.debug_path().is_some() { 5 } else { 4 };
    if selected.len() != expected {
        return Err(PublishError::Refused(format!(
            "sealed {target} package has {} members, expected {expected}",
            selected.len()
        )));
    }
    Ok(selected)
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
    fn package_archive_is_deterministic_and_preserves_members_and_modes() {
        let scratch = Scratch::new();
        let executable = proof::PackageMember::admit_inline(
            "targets/aarch64-apple-darwin/bin/spice",
            0o755,
            b"native",
            proof::PACKAGE_DOCUMENT_LIMIT,
        )
        .expect("executable member");
        let payload = proof::PackageMember::admit_inline(
            "targets/aarch64-apple-darwin/payload.json",
            0o644,
            b"payload",
            proof::PACKAGE_DOCUMENT_LIMIT,
        )
        .expect("payload member");
        let proof = proof::PackageMember::admit_inline(
            "targets/aarch64-apple-darwin/proof.json",
            0o644,
            b"proof",
            proof::PACKAGE_DOCUMENT_LIMIT,
        )
        .expect("proof member");
        let symbols = proof::PackageMember::admit_inline(
            "targets/aarch64-apple-darwin/symbols/spice.symbols",
            0o644,
            b"symbols",
            proof::PACKAGE_DOCUMENT_LIMIT,
        )
        .expect("symbol member");
        let package = proof::PackageMember::admit_inline(
            "release/package.json",
            0o644,
            b"{}",
            proof::PACKAGE_DOCUMENT_LIMIT,
        )
        .expect("package member");
        let unrelated = proof::PackageMember::admit_inline(
            "targets/x86_64-unknown-linux-musl/bin/spice",
            0o755,
            b"linux",
            proof::PACKAGE_DOCUMENT_LIMIT,
        )
        .expect("unrelated member");
        let all = vec![unrelated, executable, package, proof, payload, symbols];
        let first = scratch.0.join("first.zip");
        let second = scratch.0.join("second.zip");
        let members =
            target_members(&all, ReleaseTarget::Aarch64AppleDarwin).expect("target members");
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
        assert_eq!(archive.len(), 5);
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
        let second_member = archive.by_index(1).expect("second member");
        assert_eq!(
            second_member.name().expect("second member name").as_ref(),
            "targets/aarch64-apple-darwin/bin/spice"
        );
        assert_eq!(second_member.unix_mode(), Some(0o100755));
        drop(second_member);
        assert!(
            archive
                .by_name("targets/x86_64-unknown-linux-musl/bin/spice")
                .is_err()
        );
        let extracted = scratch.0.join("extracted");
        archive.extract(&extracted).expect("archive extracts");
        assert_eq!(
            fs::read(extracted.join("release/package.json")).expect("package extracts"),
            b"{}"
        );
        assert_eq!(
            fs::read(extracted.join("targets/aarch64-apple-darwin/bin/spice"))
                .expect("executable extracts"),
            b"native"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(extracted.join("targets/aarch64-apple-darwin/bin/spice"))
                    .expect("executable metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
    }
}
