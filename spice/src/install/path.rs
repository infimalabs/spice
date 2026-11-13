use std::path::{Component, Path};

use super::InstallError;

pub(super) fn relative_parents(path: &str) -> Result<Vec<String>, InstallError> {
    let path = validate_relative(path)?;
    let mut parents = path.ancestors().skip(1).collect::<Vec<_>>();
    parents.retain(|parent| !parent.as_os_str().is_empty());
    parents.reverse();
    parents.into_iter().map(relative_path).collect()
}

pub(super) fn validate_relative(path: &str) -> Result<&Path, InstallError> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(InstallError::InvalidRecord(format!(
            "install path is not a strict relative path: {path:?}"
        )));
    }
    Ok(path)
}

pub(super) fn relative_path(path: &Path) -> Result<String, InstallError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| InstallError::InvalidRecord(format!("install path is not UTF-8: {path:?}")))
}
