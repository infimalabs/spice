use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use wire::manifest::{SYMBOL_RECIPE_VERSION, SymbolRecipe};
use wire::{Digest, Domain};

use super::{AppOrigin, ReleaseEnvironment, prebuilt, stage_target, windows_cross, workspace_root};

const RECIPE_PATH: &str = "symbols/recipe.json";
const DATA_MODE: u32 = 0o644;
const SHA256_HEX_LENGTH: usize = 64;

/// Rebuild one detached platform debug artifact from its authenticated recipe.
///
/// The released executable is never rebuilt or republished. Its exact digest is
/// admitted first; the fresh build must then reproduce the recipe's native
/// build identity before its debug sidecar can leave the private scratch root.
pub fn rebuild_release_symbols(
    recipe_path: &Path,
    released_binary: &Path,
    destination: &Path,
    mut environment: ReleaseEnvironment,
    ambient: &sill::Environment,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> Result<(), String> {
    let recipe_bytes = read_regular(recipe_path)?;
    let recipe: SymbolRecipe = wire::decode(&recipe_bytes).map_err(|error| error.to_string())?;
    admit_recipe(&recipe)?;
    admit_local_inputs(&recipe, released_binary)?;
    // Cargo hashes the final unit's link arguments, which name the canvas
    // object under the target directory, so a Darwin rebuild links only from
    // the canonical stage's own target directory when the caller names it.
    let canonical_target = recipe.target == wire::manifest::ReleaseTarget::Aarch64AppleDarwin
        && environment.cargo_target_dir.is_some();
    let (work, stage) = configure_scratch(destination, &mut environment, canonical_target)?;
    let build = match recipe.target {
        wire::manifest::ReleaseTarget::X86_64UnknownLinuxMusl => {
            crate::linux_sealed_stage(&environment, true, output, error_output)
                .map_err(|error| error.to_string())
        }
        wire::manifest::ReleaseTarget::X86_64PcWindowsMsvc => {
            crate::windows_cross_symbols(&environment, ambient, &stage, output, error_output)
                .map_err(|error| error.to_string())
        }
        wire::manifest::ReleaseTarget::Aarch64AppleDarwin => {
            let app = if environment.release_canvas.is_some() {
                AppOrigin::Provider
            } else {
                AppOrigin::Compile
            };
            stage_target(
                recipe.target.as_str(),
                environment,
                None,
                Some(&recipe.proof_container),
                app,
                true,
                output,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        }
    };
    let result = build.and_then(|()| match recipe.target {
        wire::manifest::ReleaseTarget::X86_64PcWindowsMsvc => {
            retain_windows_symbols(&recipe, destination, &stage, output)
        }
        wire::manifest::ReleaseTarget::X86_64UnknownLinuxMusl
        | wire::manifest::ReleaseTarget::Aarch64AppleDarwin => {
            retain_symbol_artifact(&recipe, destination, &stage, output)
        }
    });
    let cleanup = fs::remove_dir_all(&work)
        .map_err(|error| format!("cannot remove symbol scratch {}: {error}", work.display()));
    result.and(cleanup)
}

fn admit_local_inputs(recipe: &SymbolRecipe, released_binary: &Path) -> Result<(), String> {
    let workspace = workspace_root().map_err(|error| error.to_string())?;
    let commit = git_head(&workspace)?;
    if commit != recipe.source_commit {
        return Err(format!(
            "symbol recipe names source {}, not checked-out {commit}",
            recipe.source_commit
        ));
    }
    let lock = read_regular(&workspace.join("Cargo.lock"))?;
    let lock = Digest::compute(Domain::DistArtifact, &lock);
    if lock != recipe.cargo_lock {
        return Err("checked-out Cargo.lock differs from the symbol recipe".to_owned());
    }
    let released = read_regular(released_binary)?;
    let released = Digest::compute(Domain::DistArtifact, &released);
    if released != recipe.released_binary {
        return Err("released executable digest differs from the symbol recipe".to_owned());
    }
    Ok(())
}

fn configure_scratch(
    destination: &Path,
    environment: &mut ReleaseEnvironment,
    canonical_target: bool,
) -> Result<(PathBuf, PathBuf), String> {
    let work = work_root(destination)?;
    fs::create_dir(&work).map_err(|error| format!("cannot create {}: {error}", work.display()))?;
    let stage = work.join("stage");
    let temporary = work.join("tmp");
    fs::create_dir(&temporary)
        .map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;
    environment.stage_root = Some(stage.clone().into_os_string());
    if !canonical_target {
        environment.cargo_target_dir = Some(work.join("target").into_os_string());
    }
    environment.temporary = Some(temporary.into_os_string());
    Ok((work, stage))
}

fn retain_symbol_artifact(
    recipe: &SymbolRecipe,
    destination: &Path,
    stage: &Path,
    output: &mut impl Write,
) -> Result<(), String> {
    let payload_path = stage
        .join("targets")
        .join(recipe.target.as_str())
        .join("payload.json");
    let payload_bytes = read_regular(&payload_path)?;
    let payload: wire::manifest::TargetPayload =
        wire::decode(&payload_bytes).map_err(|error| error.to_string())?;
    if payload.target != recipe.target {
        return Err(format!(
            "rebuilt payload target {} differs from recipe {}",
            payload.target, recipe.target
        ));
    }
    let generated = read_inventory_document::<SymbolRecipe>(stage, &payload, RECIPE_PATH)?;
    admit_rebuilt_recipe(recipe, &generated)?;
    let bytes = read_debug_artifact(recipe, stage, &payload)?;
    publish_debug_artifact(recipe, destination, &bytes, output)
}

/// Admit a Windows rebuild and retain its PDB.
///
/// The rebuild is the canonical cross build, which never executes the PE, so
/// it lays out the candidate, its observed toolchain, and the PDB its link
/// wrote rather than a staged payload. The candidate must be the recipe's
/// built executable byte for byte; its content digest is also the Windows
/// build identity.
fn retain_windows_symbols(
    recipe: &SymbolRecipe,
    destination: &Path,
    stage: &Path,
    output: &mut impl Write,
) -> Result<(), String> {
    let root = stage.join(windows_cross::ROOT);
    let built = Digest::compute(
        Domain::DistArtifact,
        &read_regular(&root.join(prebuilt::CANDIDATE))?,
    );
    if built != recipe.built_binary {
        return Err("rebuilt executable digest differs from the released build".to_owned());
    }
    if built.to_string() != recipe.build_id {
        return Err(format!(
            "rebuilt platform identity {built} differs from released {}",
            recipe.build_id
        ));
    }
    let toolchain: prebuilt::Toolchain =
        serde_json::from_slice(&read_regular(&root.join(prebuilt::TOOLCHAIN))?)
            .map_err(|error| format!("rebuilt toolchain record is malformed: {error}"))?;
    if toolchain.rust != recipe.toolchain {
        return Err(format!(
            "rebuilt toolchain {} differs from the release recipe {}",
            toolchain.rust, recipe.toolchain
        ));
    }
    let bytes = read_regular(&root.join(windows_cross::CANDIDATE_PDB))?;
    if bytes.is_empty() {
        return Err("rebuilt Windows candidate linked an empty PDB".to_owned());
    }
    publish_debug_artifact(recipe, destination, &bytes, output)
}

fn publish_debug_artifact(
    recipe: &SymbolRecipe,
    destination: &Path,
    bytes: &[u8],
    output: &mut impl Write,
) -> Result<(), String> {
    write_debug_artifact(destination, bytes)?;
    writeln!(
        output,
        "rebuilt {} symbols {} bytes build-id {}",
        recipe.target,
        bytes.len(),
        recipe.build_id
    )
    .map_err(|error| error.to_string())
}

fn admit_rebuilt_recipe(expected: &SymbolRecipe, generated: &SymbolRecipe) -> Result<(), String> {
    if generated.built_binary != expected.built_binary {
        return Err("rebuilt executable digest differs from the released build".to_owned());
    }
    if generated.build_id != expected.build_id {
        return Err(format!(
            "rebuilt platform identity {} differs from released {}",
            generated.build_id, expected.build_id
        ));
    }
    if generated.target != expected.target
        || generated.source_commit != expected.source_commit
        || generated.source != expected.source
        || generated.source_archive != expected.source_archive
        || generated.cargo_lock != expected.cargo_lock
        || generated.vendor_authority != expected.vendor_authority
        || generated.proof_container != expected.proof_container
        || generated.toolchain != expected.toolchain
        || generated.flags != expected.flags
    {
        return Err("rebuilt symbol authorities differ from the release recipe".to_owned());
    }
    Ok(())
}

fn read_debug_artifact(
    recipe: &SymbolRecipe,
    stage: &Path,
    payload: &wire::manifest::TargetPayload,
) -> Result<Vec<u8>, String> {
    let debug_path = match recipe.target {
        wire::manifest::ReleaseTarget::Aarch64AppleDarwin => "symbols/spice.symbols",
        wire::manifest::ReleaseTarget::X86_64UnknownLinuxMusl => "symbols/spice.debug",
        wire::manifest::ReleaseTarget::X86_64PcWindowsMsvc => {
            return Err("a Windows rebuild stages no payload".to_owned());
        }
    };
    let entry = payload
        .inventory
        .iter()
        .find(|entry| entry.path == debug_path)
        .ok_or_else(|| format!("rebuilt {} stage produced no {debug_path}", recipe.target))?;
    let object = stage.join(format!("objects/{}-{:04o}", entry.digest, entry.mode));
    let bytes = read_regular(&object)?;
    if bytes.len() as u64 != entry.bytes
        || Digest::compute(Domain::DistArtifact, &bytes) != entry.digest
    {
        return Err("rebuilt debug object differs from its authenticated inventory".to_owned());
    }
    Ok(bytes)
}

fn write_debug_artifact(destination: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    fs::write(destination, bytes)
        .map_err(|error| format!("cannot write {}: {error}", destination.display()))
}

fn admit_recipe(recipe: &SymbolRecipe) -> Result<(), String> {
    if recipe.version != SYMBOL_RECIPE_VERSION {
        return Err(format!(
            "symbol recipe schema is {}, expected {SYMBOL_RECIPE_VERSION}",
            recipe.version
        ));
    }
    if !wire::manifest::git_commit_is_canonical(&recipe.source_commit) {
        return Err("symbol recipe source commit is not canonical".to_owned());
    }
    if recipe.build_id.is_empty() || recipe.toolchain.is_empty() || recipe.flags.is_empty() {
        return Err("symbol recipe omits its build identity, toolchain, or flags".to_owned());
    }
    let identity = recipe.proof_container.strip_prefix("sha256:").or_else(|| {
        recipe
            .proof_container
            .split_once("@sha256:")
            .map(|(_, digest)| digest)
    });
    if !matches!(identity, Some(digest) if digest.len() == SHA256_HEX_LENGTH && digest.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
    {
        return Err("symbol recipe proof image is not digest-pinned".to_owned());
    }
    Ok(())
}

fn read_inventory_document<T: serde::de::DeserializeOwned>(
    stage: &Path,
    payload: &wire::manifest::TargetPayload,
    path: &str,
) -> Result<T, String> {
    let entry = payload
        .inventory
        .iter()
        .find(|entry| entry.path == path && entry.mode == DATA_MODE)
        .ok_or_else(|| format!("rebuilt payload carries no {path}"))?;
    let object = stage.join(format!("objects/{}-{:04o}", entry.digest, entry.mode));
    let bytes = read_regular(&object)?;
    if bytes.len() as u64 != entry.bytes
        || Digest::compute(Domain::DistArtifact, &bytes) != entry.digest
    {
        return Err(format!("rebuilt {path} differs from its inventory"));
    }
    wire::decode(&bytes).map_err(|error| error.to_string())
}

fn read_regular(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!("{} is not one regular file", path.display()));
    }
    fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn git_head(root: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("cannot launch git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git rev-parse HEAD refused: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let commit = String::from_utf8(output.stdout)
        .map_err(|_| "git rev-parse HEAD was not UTF-8".to_owned())?;
    let commit = commit.trim().to_owned();
    if !wire::manifest::git_commit_is_canonical(&commit) {
        return Err("git rev-parse HEAD returned no canonical commit".to_owned());
    }
    Ok(commit)
}

fn work_root(destination: &Path) -> Result<PathBuf, String> {
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "symbol destination has no UTF-8 filename".to_owned())?;
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let work = parent.join(format!(".{name}.spice-symbol-work"));
    match fs::symlink_metadata(&work) {
        Ok(_) => Err(format!("symbol scratch already exists: {}", work.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(work),
        Err(error) => Err(format!("cannot inspect {}: {error}", work.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wire::manifest::{ReleaseTarget, SYMBOL_RECIPE_VERSION};

    fn recipe() -> SymbolRecipe {
        SymbolRecipe {
            version: SYMBOL_RECIPE_VERSION,
            target: ReleaseTarget::X86_64UnknownLinuxMusl,
            source_commit: "a".repeat(40),
            source: Digest::compute(Domain::DistSourceCommit, b"source"),
            source_archive: Digest::compute(Domain::DistSourceArchive, b"archive"),
            cargo_lock: Digest::compute(Domain::DistArtifact, b"lock"),
            vendor_authority: Digest::compute(Domain::DistPins, b"pins"),
            proof_container: format!(
                "registry.example/spice-proof@sha256:{}",
                "b".repeat(SHA256_HEX_LENGTH)
            ),
            toolchain: "rustc 1.97.1".to_owned(),
            flags: vec!["--locked".to_owned()],
            released_binary: Digest::compute(Domain::DistArtifact, b"binary"),
            built_binary: Digest::compute(Domain::DistArtifact, b"binary"),
            build_id: "0123456789abcdef".to_owned(),
        }
    }

    /// A signed release publishes the signed executable, while a rebuild can
    /// only reproduce the executable as it was built before signing.
    #[test]
    fn rebuilt_recipe_of_a_signed_release_admits_its_unsigned_build() {
        let mut expected = recipe();
        expected.target = ReleaseTarget::Aarch64AppleDarwin;
        let mut rebuilt = expected.clone();
        expected.released_binary = Digest::compute(Domain::DistArtifact, b"signed binary");

        admit_rebuilt_recipe(&expected, &rebuilt).expect("an exact unsigned rebuild admits");

        rebuilt.built_binary = Digest::compute(Domain::DistArtifact, b"different binary");
        rebuilt.released_binary = rebuilt.built_binary.clone();
        assert!(admit_rebuilt_recipe(&expected, &rebuilt).is_err());
    }

    #[test]
    fn recipe_admission_refuses_unpinned_or_incomplete_rebuild_authority() {
        admit_recipe(&recipe()).expect("complete recipe admits");

        let mut image_identity = recipe();
        image_identity.proof_container = format!("sha256:{}", "b".repeat(SHA256_HEX_LENGTH));
        admit_recipe(&image_identity).expect("exact OCI image identity admits");

        let mut unpinned = recipe();
        unpinned.proof_container = "registry.example/spice-proof:latest".to_owned();
        assert!(admit_recipe(&unpinned).is_err());

        let mut missing_identity = recipe();
        missing_identity.build_id.clear();
        assert!(admit_recipe(&missing_identity).is_err());

        let mut stale = recipe();
        stale.version += 1;
        assert!(admit_recipe(&stale).is_err());
    }

    /// A Windows rebuild yields no staged payload, so the candidate bytes and
    /// observed toolchain alone bind the retained PDB to the release.
    #[test]
    fn windows_pdb_is_retained_only_for_the_released_candidate() {
        let work =
            std::env::temp_dir().join(format!("spice-windows-symbols-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        let root = work.join("stage").join(windows_cross::ROOT);
        fs::create_dir_all(root.join("candidate")).unwrap();
        fs::write(root.join(prebuilt::CANDIDATE), b"binary").unwrap();
        fs::write(root.join(windows_cross::CANDIDATE_PDB), b"pdb").unwrap();
        let toolchain = |rust: &str| {
            serde_json::to_vec(&prebuilt::Toolchain {
                rust: rust.to_owned(),
                wasm_target: "wasm32-unknown-unknown".to_owned(),
                trunk: "trunk 0.22.1".to_owned(),
            })
            .unwrap()
        };
        fs::write(root.join(prebuilt::TOOLCHAIN), toolchain("rustc 1.97.1")).unwrap();
        let mut expected = recipe();
        expected.target = ReleaseTarget::X86_64PcWindowsMsvc;
        expected.build_id = expected.built_binary.to_string();
        let stage = work.join("stage");
        let destination = work.join("spice.pdb");
        let mut output = Vec::new();

        let admitted = retain_windows_symbols(&expected, &destination, &stage, &mut output);
        let retained = fs::read(&destination);
        fs::remove_file(&destination).unwrap();
        let mut other_identity = expected.clone();
        other_identity.build_id = "0123456789abcdef".to_owned();
        let wrong_identity =
            retain_windows_symbols(&other_identity, &destination, &stage, &mut output);
        fs::write(root.join(prebuilt::TOOLCHAIN), toolchain("rustc 1.98.0")).unwrap();
        let wrong_toolchain = retain_windows_symbols(&expected, &destination, &stage, &mut output);
        fs::write(root.join(prebuilt::TOOLCHAIN), toolchain("rustc 1.97.1")).unwrap();
        fs::write(root.join(prebuilt::CANDIDATE), b"other binary").unwrap();
        let wrong_candidate = retain_windows_symbols(&expected, &destination, &stage, &mut output);
        let leaked = destination.exists();
        fs::remove_dir_all(&work).unwrap();

        admitted.expect("the released candidate admits its PDB");
        assert_eq!(retained.unwrap(), b"pdb");
        assert!(wrong_identity.is_err());
        assert!(wrong_toolchain.is_err());
        assert!(wrong_candidate.is_err());
        assert!(!leaked, "a refused rebuild retains no PDB");
    }
}
