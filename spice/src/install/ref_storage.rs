//! The Git reference storage an install can serve.
//!
//! The gate reads references through gix, which has only the files backend:
//! a reftable repository opens but cannot instantiate `HEAD`, so every gated
//! commit there would be refused. Install names that before applying anything.

use std::io;
use std::path::Path;

use super::InstallError;
use crate::repository::Repository;

/// The `extensions.refStorage` value Git writes for its default backend.
const FILES: &str = "files";

/// Refuse the repository unless its Git-common configuration leaves references
/// in the files backend.
pub(super) fn verify_files_storage(repository: &Repository) -> Result<(), InstallError> {
    verify_common_config(repository.common_dir())
}

/// [`verify_files_storage`] over the Git-common directory holding `config`.
fn verify_common_config(common: &Path) -> Result<(), InstallError> {
    let path = common.join("config");
    let config = gix_config::File::from_path_no_includes(path.clone(), gix_config::Source::Local)
        .map_err(|error| match error {
        gix_config::file::init::from_paths::Error::Io { source, path } => {
            InstallError::io("read Git configuration", path, source)
        }
        gix_config::file::init::from_paths::Error::Init(error) => {
            InstallError::io("parse Git configuration", path, io::Error::other(error))
        }
    })?;
    match config.string("extensions.refStorage") {
        None => Ok(()),
        Some(format) if format.eq_ignore_ascii_case(FILES.as_bytes()) => Ok(()),
        Some(format) => Err(InstallError::ReferenceStorage {
            common: common.to_path_buf(),
            format: format.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn common(label: &str, config: &str) -> std::path::PathBuf {
        let common = std::env::temp_dir().join(format!(
            "spice-ref-storage-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&common).expect("create common dir");
        fs::write(common.join("config"), config).expect("write config");
        common
    }

    #[test]
    fn files_storage_by_default_or_by_name_is_admitted() {
        let unnamed = common("default", "[core]\n\trepositoryformatversion = 0\n");
        verify_common_config(&unnamed).expect("absent refStorage is the files backend");
        let named = common(
            "files",
            "[core]\n\trepositoryformatversion = 1\n[extensions]\n\trefStorage = Files\n",
        );
        verify_common_config(&named).expect("files is the one backend the gate reads");
        fs::remove_dir_all(unnamed).ok();
        fs::remove_dir_all(named).ok();
    }

    #[test]
    fn reftable_storage_is_refused_with_its_format() {
        let common = common(
            "reftable",
            "[core]\n\trepositoryformatversion = 1\n[extensions]\n\trefstorage = reftable\n",
        );
        let error = verify_common_config(&common).expect_err("reftable is refused");
        assert!(matches!(
            &error,
            InstallError::ReferenceStorage { common: refused, format }
                if refused == &common && format == "reftable"
        ));
        fs::remove_dir_all(common).ok();
    }
}
