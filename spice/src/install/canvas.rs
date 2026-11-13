//! The canvas generation an installed executable carries.
//!
//! The executable is the only canvas carrier: installation publishes no app
//! file, and a receipt names the generation its executable links.

use std::fs;
use std::path::Path;

use wire::{Digest, Domain};

use super::InstallError;

/// The app generation linked into one selected executable's bytes.
///
/// Admission reads the linked archive once and inflates only the shell that
/// names its generation. A packaged executable must also be the one its
/// sealed payload binds, carrying that payload's archive and generation.
pub(super) fn generation(binary: &Path, bytes: &[u8]) -> Result<Digest, InstallError> {
    let refused = |reason: String| InstallError::CanvasRefused {
        path: binary.to_path_buf(),
        reason,
    };
    let archive = rack::link::extract(bytes).map_err(|error| refused(error.to_string()))?;
    let generation = rack::Rack::admit(archive)
        .and_then(|rack| serve::canvas::generation(&rack))
        .map_err(|error| refused(error.to_string()))?;
    let Some((_, target_root)) = package_layout(binary) else {
        return Ok(generation);
    };
    let payload_path = target_root.join("payload.json");
    let payload_bytes = fs::read(&payload_path)
        .map_err(|error| InstallError::io("read appliance payload", &payload_path, error))?;
    let admitted = proof::admit(&payload_bytes)
        .map_err(|error| InstallError::InvalidRecord(format!("appliance payload: {error}")))?;
    let payload = admitted.value();
    let target = target_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(InstallError::BinaryContextChanged)?;
    if payload.target.as_str() != target
        || payload.native != Digest::compute(Domain::DistArtifact, bytes)
        || payload.canvas != Digest::compute(Domain::DistArtifact, archive)
        || payload.app != generation
    {
        return Err(InstallError::BinaryContextChanged);
    }
    Ok(generation)
}

/// The release package root and target directory of a packaged executable.
pub(super) fn package_layout(binary: &Path) -> Option<(&Path, &Path)> {
    let bin = binary
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "bin"))?;
    let target = bin.parent()?;
    let targets = target
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "targets"))?;
    let root = targets.parent()?;
    root.join("release/package.json")
        .is_file()
        .then_some((root, target))
}

/// A minimal executable whose linked rack's app shell names `generation`.
#[cfg(test)]
pub(super) fn linked(generation: &Digest) -> Vec<u8> {
    let shell = format!("<main data-app-generation=\"{generation}\"></main>");
    let path = format!(
        "{}/{}",
        serve::canvas::APP_DIRECTORY,
        wire::manifest::APP_HTML
    );
    let archive = serve::canvas::gzip::write([(path.as_str(), shell.as_bytes())])
        .expect("fixture rack writes");
    rack::link::object(&archive, rack::link::Target::LinuxX86_64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_executable_names_the_generation_its_linked_archive_carries() {
        let expected = Digest::compute(Domain::DistArtifact, b"linked canvas");
        let binary = Path::new("spice");
        assert_eq!(
            generation(binary, &linked(&expected)).expect("linked archive admits"),
            expected
        );
        assert!(matches!(
            generation(binary, b"no linked archive"),
            Err(InstallError::CanvasRefused { .. })
        ));
    }
}
