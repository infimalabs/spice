use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use wire::manifest::{SYMBOL_RECIPE_VERSION, SymbolRecipe};
use wire::{Digest, Domain};

use super::{AppOrigin, ReleaseEnvironment, stage_target, workspace_root};

const RECIPE_PATH: &str = "symbols/recipe.json";
const DATA_MODE: u32 = 0o644;

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
    output: &mut impl Write,
) -> Result<(), String> {
    let recipe_bytes = read_regular(recipe_path)?;
    let recipe: SymbolRecipe = wire::decode(&recipe_bytes).map_err(|error| error.to_string())?;
    admit_recipe(&recipe)?;
    admit_local_inputs(&recipe, released_binary)?;
    let (work, stage) = configure_scratch(destination, &mut environment)?;
    let result = rebuild_symbol_artifact(&recipe, destination, &stage, environment, output);
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
) -> Result<(PathBuf, PathBuf), String> {
    let work = work_root(destination)?;
    fs::create_dir(&work).map_err(|error| format!("cannot create {}: {error}", work.display()))?;
    let stage = work.join("stage");
    let target = work.join("target");
    let temporary = work.join("tmp");
    fs::create_dir(&temporary)
        .map_err(|error| format!("cannot create {}: {error}", temporary.display()))?;
    environment.stage_root = Some(stage.clone().into_os_string());
    environment.cargo_target_dir = Some(target.into_os_string());
    environment.temporary = Some(temporary.into_os_string());
    Ok((work, stage))
}

fn rebuild_symbol_artifact(
    recipe: &SymbolRecipe,
    destination: &Path,
    stage: &Path,
    environment: ReleaseEnvironment,
    output: &mut impl Write,
) -> Result<(), String> {
    let mut records = Vec::new();
    let build = stage_target(
        recipe.target.as_str(),
        environment,
        None,
        Some(&recipe.proof_container),
        AppOrigin::Compile,
        true,
        &mut records,
    )
    .map_err(|error| error.to_string())?;
    let generated =
        read_inventory_document::<SymbolRecipe>(stage, &build.staged.payload, RECIPE_PATH)?;
    admit_rebuilt_recipe(recipe, &generated)?;
    let bytes = read_debug_artifact(recipe, stage, &build.staged.payload)?;
    write_debug_artifact(destination, &bytes)?;
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
    if generated.released_binary != expected.released_binary {
        return Err("rebuilt executable digest differs from the released binary".to_owned());
    }
    if generated.build_id != expected.build_id {
        return Err(format!(
            "rebuilt platform identity {} differs from released {}",
            generated.build_id, expected.build_id
        ));
    }
    if generated.source_commit != expected.source_commit
        || generated.cargo_lock != expected.cargo_lock
        || generated.vendor_authority != expected.vendor_authority
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
        wire::manifest::ReleaseTarget::X86_64PcWindowsMsvc => "symbols/spice.pdb",
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
    if !recipe.proof_container.contains("@sha256:") {
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
            proof_container: format!("registry.example/spice-proof@sha256:{}", "b".repeat(64)),
            toolchain: "rustc 1.97.1".to_owned(),
            flags: vec!["--locked".to_owned()],
            released_binary: Digest::compute(Domain::DistArtifact, b"binary"),
            build_id: "0123456789abcdef".to_owned(),
        }
    }

    #[test]
    fn rebuilt_recipe_refuses_binary_drift_with_the_same_build_identity() {
        let expected = recipe();
        let mut rebuilt = expected.clone();
        rebuilt.released_binary = Digest::compute(Domain::DistArtifact, b"different binary");

        assert!(admit_rebuilt_recipe(&expected, &rebuilt).is_err());
        admit_rebuilt_recipe(&expected, &expected).expect("an exact rebuild admits");
    }

    #[test]
    fn recipe_admission_refuses_unpinned_or_incomplete_rebuild_authority() {
        admit_recipe(&recipe()).expect("complete recipe admits");

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
}
