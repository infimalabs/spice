//! Publish optional, source-owned renderer files into the ordinary app generation.

use std::fs;
use std::path::Path;

use wire::manifest::renderer::{self, File, Member};
use wire::{Digest, Domain};

use super::{AppBuildError, read_regular};

pub(super) const DIRECTORY: &str = "app/renderer";

pub(super) fn stage(root: &Path, stage: &Path) -> Result<(), AppBuildError> {
    let directory = root.join(DIRECTORY);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(AppBuildError::Io {
                path: directory,
                source,
            });
        }
    };
    let mut members = Vec::new();
    let mut total = 0_u64;
    for entry in entries {
        let entry = entry.map_err(|source| AppBuildError::Io {
            path: directory.clone(),
            source,
        })?;
        let name = entry.file_name();
        let file = name
            .to_str()
            .and_then(File::parse_filename)
            .ok_or_else(|| {
                AppBuildError::GeneratedStage(
                    "renderer source has an unsupported file name".to_owned(),
                )
            })?;
        let metadata = fs::symlink_metadata(entry.path()).map_err(|source| AppBuildError::Io {
            path: entry.path(),
            source,
        })?;
        total = total.checked_add(metadata.len()).ok_or_else(bound_error)?;
        if members.len() == renderer::MAX_FILES || total > renderer::MAX_BYTES {
            return Err(bound_error());
        }
        let bytes = read_regular(&entry.path())?;
        let digest = Digest::compute(Domain::InstallArtifact, &bytes);
        let path = stage.join(file.to_string());
        fs::write(&path, bytes).map_err(|source| AppBuildError::Io { path, source })?;
        members.push(Member { file, digest });
    }
    if members.is_empty() {
        return Ok(());
    }
    members.sort_by_key(|member| member.file);
    let declaration = renderer::encode(&members)
        .map_err(|error| AppBuildError::GeneratedStage(error.to_owned()))?;
    let path = stage.join(wire::manifest::APP_HTML);
    let html = String::from_utf8(read_regular(&path)?)
        .map_err(|_| AppBuildError::GeneratedStage("app shell is not UTF-8".to_owned()))?;
    const MAIN: &str = "<main id=\"app\"";
    if html.matches(MAIN).count() != 1 || html.contains(renderer::ATTRIBUTE) {
        return Err(AppBuildError::GeneratedStage(
            "app shell has no unique renderer declaration site".to_owned(),
        ));
    }
    let html = html.replacen(
        MAIN,
        &format!("{MAIN} {}=\"{declaration}\"", renderer::ATTRIBUTE),
        1,
    );
    fs::write(&path, html).map_err(|source| AppBuildError::Io { path, source })
}

fn bound_error() -> AppBuildError {
    AppBuildError::GeneratedStage("renderer sources exceed their file or byte bound".to_owned())
}
