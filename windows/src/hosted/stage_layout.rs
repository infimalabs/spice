use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

pub(super) const RESULT_PATH: &str = "result.json";
pub(super) const STAGE_DIRECTORY: &str = "stage";
pub(super) const STAGE_LOG_PATH: &str = "stage.log";
pub(super) const SMOKE_LOG_PATH: &str = "smoke.log";
pub(super) const SMOKE_DIRECTORY: &str = "smoke";
pub(super) const RUNNER_PATH: &str = "runner/windows-hosted.exe";
/// The attested inventory of every other file a native workflow submitted to
/// provider attestation from this root.
pub(super) const SUBJECTS_PATH: &str = "subjects.json";

const LINUX_TARGET: &str = "x86_64-unknown-linux-musl";
const MACOS_TARGET: &str = "aarch64-apple-darwin";
const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";

const LINUX_RECORDS: &[RequiredRecord] = &[
    RequiredRecord::exact("result", RESULT_PATH),
    RequiredRecord::exact("stage log", STAGE_LOG_PATH),
    RequiredRecord::exact("smoke record", SMOKE_LOG_PATH),
    RequiredRecord::exact(
        "payload",
        "stage/targets/x86_64-unknown-linux-musl/payload.json",
    ),
    RequiredRecord::nested("object", "stage/objects/", ""),
    RequiredRecord::nested("proof", "stage/proofs/", "/receipt.json"),
];
const MACOS_RECORDS: &[RequiredRecord] = &[
    RequiredRecord::exact("result", RESULT_PATH),
    RequiredRecord::exact("payload", "stage/targets/aarch64-apple-darwin/payload.json"),
    RequiredRecord::nested("object", "stage/objects/", ""),
    RequiredRecord::nested("proof", "stage/proofs/", "/receipt.json"),
];
const WINDOWS_RECORDS: &[RequiredRecord] = &[
    RequiredRecord::exact("result", RESULT_PATH),
    RequiredRecord::exact("runner", RUNNER_PATH),
    RequiredRecord::exact(
        "payload",
        "stage/targets/x86_64-pc-windows-msvc/payload.json",
    ),
    RequiredRecord::nested("object", "stage/objects/", ""),
    RequiredRecord::nested("proof", "stage/proofs/", "/receipt.json"),
];

/// The one relative layout published by every native hosted stage.
pub(super) struct StageLayout {
    root: PathBuf,
}

impl StageLayout {
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn result(&self) -> PathBuf {
        self.root.join(RESULT_PATH)
    }

    pub(super) fn stage(&self) -> PathBuf {
        self.root.join(STAGE_DIRECTORY)
    }

    pub(super) fn stage_log(&self) -> PathBuf {
        self.root.join(STAGE_LOG_PATH)
    }

    pub(super) fn smoke_log(&self) -> PathBuf {
        self.root.join(SMOKE_LOG_PATH)
    }

    pub(super) fn smoke(&self) -> PathBuf {
        self.root.join(SMOKE_DIRECTORY)
    }

    pub(super) fn runner(&self) -> PathBuf {
        self.root.join(RUNNER_PATH)
    }

    pub(super) fn subjects(&self) -> PathBuf {
        self.root.join(SUBJECTS_PATH)
    }
}

#[derive(Clone, Copy)]
struct RequiredRecord {
    name: &'static str,
    prefix: &'static str,
    suffix: &'static str,
}

impl RequiredRecord {
    const fn exact(name: &'static str, path: &'static str) -> Self {
        Self {
            name,
            prefix: path,
            suffix: path,
        }
    }

    const fn nested(name: &'static str, prefix: &'static str, suffix: &'static str) -> Self {
        Self {
            name,
            prefix,
            suffix,
        }
    }

    fn matches(self, path: &str) -> bool {
        if self.prefix == self.suffix {
            path == self.prefix
        } else {
            path.starts_with(self.prefix)
                && path.len() > self.prefix.len() + self.suffix.len()
                && path.ends_with(self.suffix)
        }
    }

    fn representative(self) -> PathBuf {
        if self.prefix == self.suffix {
            PathBuf::from(self.prefix)
        } else {
            PathBuf::from(format!("{}representative{}", self.prefix, self.suffix))
        }
    }
}

/// The repository-owned required-record schema shared by native producers and clients.
#[derive(Clone, Copy)]
pub struct NativeStageContract {
    target: &'static str,
    records: &'static [RequiredRecord],
}

impl NativeStageContract {
    const ALL: [Self; 3] = [
        Self {
            target: LINUX_TARGET,
            records: LINUX_RECORDS,
        },
        Self {
            target: MACOS_TARGET,
            records: MACOS_RECORDS,
        },
        Self {
            target: WINDOWS_TARGET,
            records: WINDOWS_RECORDS,
        },
    ];

    /// Resolve the sole contract for `target`.
    pub fn for_target(target: &str) -> Result<Self, StageLayoutError> {
        Self::ALL
            .into_iter()
            .find(|contract| contract.target == target)
            .ok_or_else(|| StageLayoutError::UnknownTarget(target.to_owned()))
    }

    pub(super) fn require(self, paths: &BTreeSet<PathBuf>) -> Result<(), StageLayoutError> {
        for record in self.records {
            if !paths
                .iter()
                .filter_map(|path| portable_path(path).ok())
                .any(|path| record.matches(&path))
            {
                return Err(StageLayoutError::MissingRecord {
                    target: self.target,
                    record: record.name,
                    expected: record.representative(),
                });
            }
        }
        Ok(())
    }

    fn admit_roots(
        self,
        uploaded_root: &Path,
        attested_root: &Path,
        paths: &BTreeSet<PathBuf>,
    ) -> Result<(), StageLayoutError> {
        self.require(paths)?;
        for path in paths {
            let uploaded = uploaded_root.join(path);
            if !uploaded.starts_with(attested_root) {
                return Err(StageLayoutError::UncoveredRecord {
                    target: self.target,
                    record: path.clone(),
                    attested_root: attested_root.to_path_buf(),
                });
            }
        }
        Ok(())
    }
}

/// Write the exact regular-file inventory a native producer packages and
/// submits to provider attestation.
pub fn declare_native_stage(target: &str, root: &Path) -> Result<(), StageLayoutError> {
    let contract = NativeStageContract::for_target(target)?;
    let mut paths = BTreeSet::new();
    collect_regular_files(root, root, &mut paths)?;
    paths.remove(Path::new(SUBJECTS_PATH));
    contract.admit_roots(root, root, &paths)?;
    let portable = paths
        .iter()
        .map(|path| portable_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    let bytes = serde_json::to_vec(&portable).map_err(StageLayoutError::Encode)?;
    let destination = root.join(SUBJECTS_PATH);
    fs::write(&destination, bytes).map_err(|source| StageLayoutError::Io {
        path: destination,
        source,
    })
}

fn collect_regular_files(
    root: &Path,
    directory: &Path,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<(), StageLayoutError> {
    for entry in fs::read_dir(directory).map_err(|source| StageLayoutError::Io {
        path: directory.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| StageLayoutError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| StageLayoutError::Io {
            path: path.clone(),
            source,
        })?;
        if file_type.is_dir() {
            collect_regular_files(root, &path, paths)?;
        } else if file_type.is_file() {
            paths.insert(
                path.strip_prefix(root)
                    .expect("walked stage path remains beneath its root")
                    .to_path_buf(),
            );
        } else {
            return Err(StageLayoutError::NonRegular(path));
        }
    }
    Ok(())
}

fn portable_path(path: &Path) -> Result<String, StageLayoutError> {
    path.components()
        .map(|component| match component {
            Component::Normal(part) => part
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| StageLayoutError::NonUnicode(path.to_path_buf())),
            _ => Err(StageLayoutError::NonCanonical(path.to_path_buf())),
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("/"))
}

/// Why a native stage package cannot satisfy its production admission
/// contract.
#[derive(Debug)]
pub enum StageLayoutError {
    UnknownTarget(String),
    MissingRecord {
        target: &'static str,
        record: &'static str,
        expected: PathBuf,
    },
    UncoveredRecord {
        target: &'static str,
        record: PathBuf,
        attested_root: PathBuf,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Encode(serde_json::Error),
    NonRegular(PathBuf),
    NonUnicode(PathBuf),
    NonCanonical(PathBuf),
}

impl fmt::Display for StageLayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTarget(target) => write!(formatter, "unknown native target {target}"),
            Self::MissingRecord {
                target,
                record,
                expected,
            } => write!(
                formatter,
                "{target} stage layout omits required {record} record {}",
                expected.display()
            ),
            Self::UncoveredRecord {
                target,
                record,
                attested_root,
            } => write!(
                formatter,
                "{target} stage record {} is not covered by provenance root {}",
                record.display(),
                attested_root.display()
            ),
            Self::Io { path, source } => {
                write!(formatter, "cannot inspect {}: {source}", path.display())
            }
            Self::Encode(error) => write!(formatter, "cannot encode stage subjects: {error}"),
            Self::NonRegular(path) => {
                write!(
                    formatter,
                    "native stage contains non-regular path {}",
                    path.display()
                )
            }
            Self::NonUnicode(path) => {
                write!(
                    formatter,
                    "native stage path is not Unicode: {}",
                    path.display()
                )
            }
            Self::NonCanonical(path) => {
                write!(
                    formatter,
                    "native stage path is not relative: {}",
                    path.display()
                )
            }
        }
    }
}

impl Error for StageLayoutError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Encode(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use super::{LINUX_TARGET, MACOS_TARGET, NativeStageContract, RESULT_PATH, WINDOWS_TARGET};

    fn observed_paths(target: &str) -> BTreeSet<PathBuf> {
        let paths: &[&str] = match target {
            LINUX_TARGET => &[
                RESULT_PATH,
                "stage.log",
                "smoke.log",
                "stage/targets/x86_64-unknown-linux-musl/payload.json",
                "stage/objects/object",
                "stage/proofs/proof/receipt.json",
            ],
            MACOS_TARGET => &[
                RESULT_PATH,
                "stage/targets/aarch64-apple-darwin/payload.json",
                "stage/objects/object",
                "stage/proofs/proof/receipt.json",
            ],
            WINDOWS_TARGET => &[
                RESULT_PATH,
                "runner/windows-hosted.exe",
                "stage/targets/x86_64-pc-windows-msvc/payload.json",
                "stage/objects/object",
                "stage/proofs/proof/receipt.json",
            ],
            _ => panic!("unexpected production target {target}"),
        };
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn independently_supplied_inventories_exercise_each_target_contract() {
        for contract in NativeStageContract::ALL {
            let records = observed_paths(contract.target);
            contract.require(&records).unwrap();
            let root_error = contract
                .admit_roots(Path::new("result"), Path::new("result/stage"), &records)
                .unwrap_err()
                .to_string();
            assert!(root_error.contains(contract.target));
            assert!(root_error.contains(RESULT_PATH));

            let mut incomplete = records;
            incomplete.remove(Path::new(RESULT_PATH));
            let record_error = contract.require(&incomplete).unwrap_err().to_string();
            assert!(record_error.contains(contract.target));
            assert!(record_error.contains(RESULT_PATH));
        }
    }
}
