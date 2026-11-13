use std::fs;
use std::io;
use std::path::Path;

use crate::repository::Repository;

use super::handle::{AdmittedPath, retirement_temporary_name};
use super::path::relative_path;
use super::{
    Asset, FileImage, Generation, InstallError, ObservedFile, Operation, ParentObservation,
    ReceiptGeneration, Retirement, fixed_asset_bytes, read_binary,
};

pub(super) fn retire_path(
    repository: &Repository,
    retirement: &Retirement,
    index: usize,
) -> Result<Option<Generation>, InstallError> {
    let path = AdmittedPath::open(
        repository.common_dir(),
        &retirement.destination,
        &retirement.parent,
        false,
        0,
    )?;
    let current = path.observe()?;
    if !retirement.discards_payload() {
        if let Some(current) = current {
            if !retirement.expects_in_place(Some(&current)) {
                return Err(super::retirement_drift(retirement, Some(current)));
            }
            path.remove(current.generation)?;
        }
        return path.parent_generation();
    }
    let temporary = retirement_temporary_name(index);
    let staged = path.observe_named(&temporary)?;
    match (current, staged) {
        (Some(current), None) if retirement.expects_in_place(Some(&current)) => {
            path.retire(current.generation, &temporary)?;
        }
        (None, Some(staged)) if retirement.expects_in_place(Some(&staged)) => {
            path.resume_retirement(staged.generation, &temporary)?;
        }
        (None, None) => {}
        (Some(_), Some(_)) => {
            return Err(temporary_collision(repository, retirement, &temporary));
        }
        (Some(current), None) => {
            return Err(super::retirement_drift(retirement, Some(current)));
        }
        (None, Some(staged)) => {
            return Err(super::retirement_drift(retirement, Some(staged)));
        }
    }
    path.parent_generation()
}

pub(super) fn prepare_retirement(
    repository: &Repository,
    retirement: &Retirement,
    index: usize,
) -> Result<Option<Generation>, InstallError> {
    debug_assert!(retirement.discards_payload());
    let path = AdmittedPath::open(
        repository.common_dir(),
        &retirement.destination,
        &retirement.parent,
        false,
        0,
    )?;
    let current = path.observe()?;
    if !retirement.expects_in_place(current.as_ref()) {
        return Err(super::retirement_drift(retirement, current));
    }
    let temporary = retirement_temporary_name(index);
    if path.observe_named(&temporary)?.is_some() {
        return Err(temporary_collision(repository, retirement, &temporary));
    }
    path.parent_generation()
}

/// Remove the directories a discarded retirement's binding names as missing,
/// innermost first and each only while it is empty.
///
/// The binding anchors above those directories. An earlier attempt may have
/// withdrawn an inner part of the chain before stopping, so withdrawal
/// resumes from the deepest prefix that still exists; a chain already
/// withdrawn leaves nothing to open and nothing to do.
pub(super) fn withdraw_parent(
    repository: &Repository,
    retirement: &Retirement,
) -> Result<(), InstallError> {
    debug_assert!(retirement.discards_payload());
    let file = Path::new(&retirement.destination)
        .file_name()
        .ok_or_else(|| InstallError::PathShape(retirement.destination(repository)))?;
    for missing in Path::new(&retirement.parent.missing)
        .ancestors()
        .filter(|missing| !missing.as_os_str().is_empty())
    {
        let parent = ParentObservation {
            missing: relative_path(missing)?,
            ..retirement.parent.clone()
        };
        let destination = relative_path(&Path::new(&parent.ancestor).join(missing).join(file))?;
        let mut path = AdmittedPath::open(
            repository.common_dir(),
            &destination,
            &parent,
            false,
            missing.components().count(),
        )?;
        if path.is_ready() {
            return path.cleanup_introduced();
        }
    }
    Ok(())
}

fn temporary_collision(
    repository: &Repository,
    retirement: &Retirement,
    temporary: &std::ffi::OsStr,
) -> InstallError {
    let mut path = retirement.destination(repository);
    path.set_file_name(temporary);
    InstallError::ParentChanged(path)
}

pub(super) fn restore_retirement(
    repository: &Repository,
    retirement: &Retirement,
    previous: Option<&FileImage>,
) -> Result<Option<Generation>, InstallError> {
    let mut path = AdmittedPath::open(
        repository.common_dir(),
        &retirement.destination,
        &retirement.parent,
        false,
        retirement.introduced_parents.len(),
    )?;
    let current = path.observe()?;
    if current.is_some() {
        return Err(super::retirement_drift(retirement, current));
    }
    let restored = previous
        .map(|previous| path.publish(previous, None).map(Some))
        .unwrap_or(Ok(None))?;
    path.cleanup_introduced()?;
    Ok(restored)
}

pub(super) fn publish_operation(
    repository: &Repository,
    operation: &Operation,
    image: &FileImage,
) -> Result<(Generation, Generation), InstallError> {
    let path = AdmittedPath::open(
        operation.location.root(repository),
        &operation.destination,
        operation.publication_parent(),
        true,
        operation.publication_introduced(),
    )?;
    let current = path.observe()?;
    if !operation.expects_in_place(current.as_ref()) {
        return Err(InstallError::Drift {
            path: operation.path.clone(),
            observed: current.map(|observed| observed.image.generated().digest),
        });
    }
    let generation = path.publish(image, current.map(|observed| observed.generation))?;
    let parent = path
        .parent_generation()?
        .ok_or_else(|| InstallError::ParentChanged(operation.destination(repository)))?;
    Ok((generation, parent))
}

pub(super) fn materialize_generated(
    operation: &Operation,
    spice_binary: Option<&Path>,
    symbols: Option<&super::symbols::Source>,
    common_git: &Path,
) -> Result<FileImage, InstallError> {
    let asset = operation.asset.known().ok_or_else(|| {
        InstallError::InvalidRecord(
            "install plan publishes an asset this build does not know".to_owned(),
        )
    })?;
    let bytes = match fixed_asset_bytes(asset, common_git)? {
        Some(bytes) => bytes,
        None => match asset {
            Asset::Binary => read_binary(spice_binary.ok_or(InstallError::BinaryContextChanged)?)?,
            Asset::SymbolsInfo | Asset::SymbolsDwarf => symbols
                .and_then(|source| source.bytes(asset))
                .ok_or(InstallError::BinaryContextChanged)?
                .to_vec(),
            _ => return Err(InstallError::BinaryContextChanged),
        },
    };
    let image = FileImage::new(operation.generated.mode, bytes);
    if image.generated() != operation.generated {
        return Err(InstallError::BinaryContextChanged);
    }
    Ok(image)
}

pub(super) fn matches_generated(observed: Option<&ObservedFile>, operation: &Operation) -> bool {
    observed.is_some_and(|image| image.generated() == operation.generated)
}

pub(super) fn restore_operation(
    repository: &Repository,
    operation: &Operation,
    expected: Generation,
    previous: Option<&FileImage>,
) -> Result<Option<Generation>, InstallError> {
    let mut path = AdmittedPath::open(
        operation.location.root(repository),
        &operation.destination,
        &operation.parent,
        false,
        operation.introduced_parents.len(),
    )?;
    let restored = match previous {
        Some(previous) => path.publish(previous, Some(expected)).map(Some),
        None => {
            path.remove(expected)?;
            Ok(None)
        }
    }?;
    path.cleanup_introduced()?;
    Ok(restored)
}

pub(super) fn cleanup_operation(
    repository: &Repository,
    operation: &Operation,
) -> Result<(), InstallError> {
    let mut path = AdmittedPath::open(
        operation.location.root(repository),
        &operation.destination,
        &operation.parent,
        false,
        operation.introduced_parents.len(),
    )?;
    path.cleanup_introduced()
}

pub(super) fn observe_bound(
    root: &Path,
    destination: &str,
    parent: &ParentObservation,
) -> Result<(Option<ObservedFile>, Option<Generation>), InstallError> {
    let path = AdmittedPath::open(root, destination, parent, false, 0)?;
    Ok((path.observe()?, path.parent_generation()?))
}

pub(super) fn publish_bound(
    root: &Path,
    destination: &str,
    parent: &ParentObservation,
    introduced: usize,
    image: &FileImage,
    expected: Option<ReceiptGeneration>,
) -> Result<ReceiptGeneration, InstallError> {
    let path = AdmittedPath::open(root, destination, parent, true, introduced)?;
    path.publish_receipt(image, expected)
}

/// Whether reversal can restore an observed occupant's `mode` here.
///
/// Windows expresses only owner-only modes as a protected owner DACL, so an
/// occupant readable by others could be captured but never put back.
pub(super) const fn restorable_mode(mode: u32) -> bool {
    cfg!(unix) || mode & 0o077 == 0
}

pub(super) fn observe(path: &Path) -> Result<Option<ObservedFile>, InstallError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(InstallError::io("inspect file", path, source)),
    };
    if !metadata.file_type().is_file() {
        return Err(InstallError::PathShape(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bytes = fs::read(path).map_err(|source| InstallError::io("read file", path, source))?;
        Ok(Some(ObservedFile {
            image: FileImage::new(metadata.permissions().mode() & 0o777, bytes),
            generation: Generation::observe(&metadata),
        }))
    }
    #[cfg(not(unix))]
    {
        use std::io::Read;
        let mut file =
            fs::File::open(path).map_err(|source| InstallError::io("open file", path, source))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| InstallError::io("read file", path, source))?;
        let mode = sill::authority::file_mode(&file)
            .map_err(|source| InstallError::io("inspect file mode", path, source))?;
        Ok(Some(ObservedFile {
            image: FileImage::new(mode & 0o777, bytes),
            generation: Generation::of_file(&file)
                .map_err(|source| InstallError::io("inspect file", path, source))?,
        }))
    }
}
