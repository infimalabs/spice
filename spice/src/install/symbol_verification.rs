//! Read-only verification of the installed macOS executable against its dSYM.
//!
//! The install receipt records what the installer authenticated at publication.
//! This surface re-reads the installed files and proves the executable bytes,
//! both Mach-O `LC_UUID`s, every receipt path, and the Git provenance still
//! agree with that record, without acquiring mutation authority.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use super::{
    Asset, InstallError, InstallStatus, Operation, Repository, SymbolProvenance, read_record,
    symbols, verify_progress,
};

/// Which retained Mach-O image a UUID was read from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolImage {
    Executable,
    Dwarf,
}

impl fmt::Display for SymbolImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Executable => "executable",
            Self::Dwarf => "dSYM DWARF",
        })
    }
}

/// Installed executable and dSYM identity the receipt vouches for, reported
/// only after every recorded value agreed with the files on disk.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SymbolVerification {
    receipt: wire::Digest,
    executable: String,
    executable_sha256: String,
    uuid: String,
    commit: String,
    dirty: bool,
    info: String,
    dwarf: String,
    /// `__TEXT` load address of the executable, kept for sample translation.
    #[serde(skip)]
    text_address: Option<u64>,
}

impl SymbolVerification {
    /// Linked `__TEXT` segment address, absent when the image declares none.
    #[must_use]
    pub(super) const fn text_address(&self) -> Option<u64> {
        self.text_address
    }

    /// Git-common-relative path of the verified executable.
    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// Lowercase hex SHA-256 recomputed from the installed executable bytes.
    #[must_use]
    pub fn executable_sha256(&self) -> &str {
        &self.executable_sha256
    }

    /// `LC_UUID` shared by the installed executable and its retained DWARF.
    #[must_use]
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// Full Git commit the executable was linked from.
    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }

    /// Whether tracked files differed from that commit when linked.
    #[must_use]
    pub const fn dirty(&self) -> bool {
        self.dirty
    }

    /// Git-common-relative path of the retained DWARF image.
    #[must_use]
    pub fn dwarf(&self) -> &str {
        &self.dwarf
    }

    /// Absolute path of the retained DWARF image.
    #[must_use]
    pub fn dwarf_path(&self, repository: &Repository) -> PathBuf {
        repository.common_dir().join(&self.dwarf)
    }

    /// Absolute path of the verified executable.
    #[must_use]
    pub fn executable_path(&self, repository: &Repository) -> PathBuf {
        repository.common_dir().join(&self.executable)
    }
}

/// Why the installed symbols could not be verified.
#[derive(Debug)]
pub enum SymbolVerificationError {
    Install(InstallError),
    /// The receipt is mid-transition, so no complete pair is installed.
    Incomplete(InstallStatus),
    /// The installed build published no macOS symbols.
    NoSymbols,
    /// The receipt records symbol provenance but no operation for the asset.
    AssetMissing(Asset),
    /// The installed executable bytes hash to another value than recorded.
    ExecutableSha256 {
        expected: String,
        observed: String,
    },
    /// A Mach-O image carries another `LC_UUID` than the receipt records.
    Uuid {
        image: SymbolImage,
        path: String,
        recorded: String,
        observed: Option<String>,
    },
}

impl fmt::Display for SymbolVerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Install(error) => fmt::Display::fmt(error, formatter),
            Self::Incomplete(status) => {
                write!(formatter, "install receipt is {status:?}, not Complete")
            }
            Self::NoSymbols => formatter.write_str("installed build published no macOS symbols"),
            Self::AssetMissing(asset) => {
                write!(
                    formatter,
                    "install receipt publishes no {asset:?} operation"
                )
            }
            Self::ExecutableSha256 { expected, observed } => write!(
                formatter,
                "installed executable SHA-256 is {observed}; receipt recorded {expected}"
            ),
            Self::Uuid {
                image,
                path,
                recorded,
                observed,
            } => write!(
                formatter,
                "installed {image} at {path} carries LC_UUID {}; receipt recorded {recorded}",
                observed.as_deref().unwrap_or("absent"),
            ),
        }
    }
}

impl Error for SymbolVerificationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Install(error) => Some(error),
            _ => None,
        }
    }
}

impl From<InstallError> for SymbolVerificationError {
    fn from(error: InstallError) -> Self {
        Self::Install(error)
    }
}

/// Verify the installed executable, its retained dSYM, and their receipt.
///
/// Both `LC_UUID`s are compared before any drift check so that a replaced
/// image reports the exact UUID it carries rather than only a digest drift.
/// Nothing is written and no mutation authority is acquired.
pub fn verify_symbols(
    repository: &Repository,
) -> Result<SymbolVerification, SymbolVerificationError> {
    let loaded = read_record(repository)?.ok_or(InstallError::RecordMissing)?;
    let record = loaded.record;
    if record.status != InstallStatus::Complete {
        return Err(SymbolVerificationError::Incomplete(record.status));
    }
    let provenance = record
        .plan
        .symbols
        .as_ref()
        .ok_or(SymbolVerificationError::NoSymbols)?;
    provenance.validate()?;
    let binary = operation(&record.plan.operations, Asset::Binary)?;
    let info = operation(&record.plan.operations, Asset::SymbolsInfo)?;
    let dwarf = operation(&record.plan.operations, Asset::SymbolsDwarf)?;
    if info.path != symbols::SYMBOLS_INFO_PATH || dwarf.path != symbols::SYMBOLS_DWARF_PATH {
        return Err(InstallError::InvalidRecord(
            "install symbol operations publish outside the retained bundle".to_owned(),
        )
        .into());
    }
    let executable_bytes = read(repository, binary)?;
    let dwarf_bytes = read(repository, dwarf)?;
    let observed = verify_images(
        provenance,
        (&binary.path, &executable_bytes),
        (&dwarf.path, &dwarf_bytes),
    )?;
    verify_progress(repository, &record)?;
    Ok(SymbolVerification {
        receipt: record.plan_digest.clone(),
        executable: binary.path.clone(),
        executable_sha256: observed,
        uuid: provenance.uuid().to_owned(),
        commit: provenance.commit().to_owned(),
        dirty: provenance.dirty(),
        info: info.path.clone(),
        dwarf: dwarf.path.clone(),
        text_address: symbols::text_segment_address(&executable_bytes),
    })
}

fn operation(
    operations: &[Operation],
    asset: Asset,
) -> Result<&Operation, SymbolVerificationError> {
    operations
        .iter()
        .find(|operation| operation.asset == asset)
        .ok_or(SymbolVerificationError::AssetMissing(asset))
}

fn read(repository: &Repository, operation: &Operation) -> Result<Vec<u8>, InstallError> {
    let path = operation.destination(repository);
    fs::read(&path).map_err(|error| InstallError::io("read installed symbol image", path, error))
}

/// Prove both retained images declare the recorded `LC_UUID`, then that the
/// executable still hashes to the recorded digest, which this returns.
fn verify_images(
    provenance: &SymbolProvenance,
    executable: (&str, &[u8]),
    dwarf: (&str, &[u8]),
) -> Result<String, SymbolVerificationError> {
    expect_uuid(SymbolImage::Executable, executable, provenance)?;
    expect_uuid(SymbolImage::Dwarf, dwarf, provenance)?;
    let observed = format!("{:x}", Sha256::digest(executable.1));
    if observed != provenance.executable_sha256() {
        return Err(SymbolVerificationError::ExecutableSha256 {
            expected: provenance.executable_sha256().to_owned(),
            observed,
        });
    }
    Ok(observed)
}

fn expect_uuid(
    image: SymbolImage,
    (path, bytes): (&str, &[u8]),
    provenance: &SymbolProvenance,
) -> Result<(), SymbolVerificationError> {
    let observed = symbols::executable_uuid(bytes);
    if observed.as_deref() == Some(provenance.uuid()) {
        return Ok(());
    }
    Err(SymbolVerificationError::Uuid {
        image,
        path: path.to_owned(),
        recorded: provenance.uuid().to_owned(),
        observed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXECUTABLE: &str = ".spice-install/bin/spice";

    fn provenance(uuid: &str, executable: &[u8]) -> SymbolProvenance {
        serde_json::from_value(serde_json::json!({
            "executable_sha256": format!("{:x}", Sha256::digest(executable)),
            "uuid": uuid,
            "commit": "0".repeat(40),
            "dirty": false,
            "path": symbols::SYMBOLS_PATH,
        }))
        .expect("fixture provenance decodes")
    }

    #[test]
    fn images_verify_only_when_both_uuids_and_the_executable_digest_agree() {
        let recorded = [0xE3; 16];
        let executable = symbols::macho_with_uuid(recorded);
        let uuid = symbols::executable_uuid(&executable).expect("fixture declares a UUID");
        let provenance = provenance(&uuid, &executable);
        let dwarf_path = symbols::SYMBOLS_DWARF_PATH;
        let agreeing_dwarf = symbols::macho_with_uuid(recorded);

        assert_eq!(
            verify_images(
                &provenance,
                (EXECUTABLE, &executable),
                (dwarf_path, &agreeing_dwarf)
            )
            .expect("the agreeing pair verifies"),
            provenance.executable_sha256()
        );

        let foreign = symbols::macho_with_uuid([0x59; 16]);
        let foreign_uuid = symbols::executable_uuid(&foreign);
        let Err(SymbolVerificationError::Uuid {
            image: SymbolImage::Dwarf,
            path,
            recorded: named,
            observed,
        }) = verify_images(
            &provenance,
            (EXECUTABLE, &executable),
            (dwarf_path, &foreign),
        )
        else {
            panic!("a replaced dSYM names its own UUID");
        };
        assert_eq!(
            (path.as_str(), named.as_str(), observed),
            (dwarf_path, uuid.as_str(), foreign_uuid.clone())
        );

        assert!(matches!(
            verify_images(
                &provenance,
                (EXECUTABLE, b"not mach-o"),
                (dwarf_path, &foreign)
            ),
            Err(SymbolVerificationError::Uuid {
                image: SymbolImage::Executable,
                observed: None,
                ..
            })
        ));

        let mut rebuilt = executable.clone();
        rebuilt.push(0);
        let Err(SymbolVerificationError::ExecutableSha256 { expected, observed }) = verify_images(
            &provenance,
            (EXECUTABLE, &rebuilt),
            (dwarf_path, &agreeing_dwarf),
        ) else {
            panic!("same-UUID executable bytes that changed report their digest");
        };
        assert_eq!(expected, provenance.executable_sha256());
        assert_eq!(observed, format!("{:x}", Sha256::digest(&rebuilt)));
    }
}
