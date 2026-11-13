use super::*;

pub(super) fn selected_package_input_paths(
    inventory: &Inventory,
    canonical_root: &Path,
    roots: &BTreeSet<PathBuf>,
    selected_paths: &[String],
    changed_sources: &[String],
) -> Result<BTreeSet<String>, WorkspaceTestError> {
    let relative_roots = roots
        .iter()
        .map(|path| {
            path.strip_prefix(canonical_root)
                .map(Path::to_path_buf)
                .map_err(|_| {
                    WorkspaceTestError::new(format!(
                        "Cargo package root {} escapes selected root {}",
                        path.display(),
                        canonical_root.display(),
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut inputs = BTreeSet::new();
    for path in selected_paths {
        let candidate = Path::new(path.as_str());
        if relative_roots
            .iter()
            .any(|root| candidate.starts_with(root))
            || workspace_cargo_authority(candidate)
        {
            inputs.insert(path.clone());
        }
    }
    inputs.extend(changed_sources.iter().cloned());
    for package in inventory
        .packages
        .iter()
        .filter(|package| roots.contains(&package.root))
    {
        for target in package
            .declared_targets
            .iter()
            .chain(&package.removed_targets)
        {
            let target_source = target.source.strip_prefix(canonical_root).map_err(|_| {
                WorkspaceTestError::new(format!(
                    "Cargo target source {} escapes selected root {}",
                    target.source.display(),
                    canonical_root.display(),
                ))
            })?;
            let target_source = path_text(target_source)?;
            inputs.insert(target_source);
            for declared in &target.declared_path_sources {
                let relative = declared.path.strip_prefix(canonical_root).map_err(|_| {
                    WorkspaceTestError::new(format!(
                        "declared Cargo input {} escapes selected root {}",
                        declared.path.display(),
                        canonical_root.display(),
                    ))
                })?;
                let relative = path_text(relative)?;
                inputs.insert(relative);
            }
        }
    }
    Ok(inputs)
}

fn workspace_cargo_authority(path: &Path) -> bool {
    matches!(
        path.to_str(),
        Some("Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml")
    ) || path.starts_with(".cargo")
}
