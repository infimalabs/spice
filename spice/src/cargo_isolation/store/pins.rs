//! Process-lifetime pins for manual Cargo readers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sill::{Acquisition, Availability, LockFile, LockName, Possession};

use super::*;

const PREFIX: &str = "reader-";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinRecord {
    schema: u64,
    generation: String,
}

/// Kernel-held evidence that one manual Cargo child may read a generation.
#[derive(Debug)]
pub(in crate::cargo_isolation) struct GenerationPin {
    possession: Option<Possession>,
    path: PathBuf,
}

impl GenerationPin {
    pub(super) fn acquire(layout: &Layout, generation: &str) -> Result<Self, IsolationError> {
        require_generation_name(generation)?;
        let name = format!("{PREFIX}{}", uuid::Uuid::new_v4().simple());
        let lock_name = LockName::new(name.clone()).map_err(|error| {
            IsolationError::new("cargo-isolation::pin-name-invalid", error.to_string())
        })?;
        let path = layout.pins.join(format!("{name}.lock"));
        let lock = LockFile::new(lock_name, &path);
        let possession = match lock
            .try_acquire()
            .map_err(|error| IsolationError::io("cargo-isolation::pin-unavailable", &path, error))?
        {
            Acquisition::Acquired(possession) => possession,
            Acquisition::Contended => {
                return Err(IsolationError::new(
                    "cargo-isolation::pin-collision",
                    format!("fresh reader pin is already held: {path:?}"),
                ));
            }
        };
        if let Err(error) = set_mode(&path, 0o600).and_then(|()| {
            let record = serde_json::to_vec(&PinRecord {
                schema: SCHEMA,
                generation: generation.to_owned(),
            })
            .map_err(|error| {
                IsolationError::new("cargo-isolation::pin-invalid", error.to_string())
            })?;
            possession.rewrite_record(&record).map_err(|error| {
                IsolationError::io("cargo-isolation::pin-unavailable", &path, error)
            })
        }) {
            drop(possession);
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok(Self {
            possession: Some(possession),
            path,
        })
    }

    pub(in crate::cargo_isolation) fn retain_for_child(
        &mut self,
        command: &mut Command,
    ) -> Result<(), IsolationError> {
        let possession = self.possession.as_mut().ok_or_else(|| {
            IsolationError::new(
                "cargo-isolation::pin-unavailable",
                "manual Cargo reader pin was already released",
            )
        })?;
        possession.retain_through_child(command);
        Ok(())
    }
}

impl Drop for GenerationPin {
    fn drop(&mut self) {
        drop(self.possession.take());
        let lock = LockFile::new(
            LockName::new("released-reader-pin").expect("static lock name is valid"),
            &self.path,
        );
        if matches!(lock.availability(), Ok(Availability::Available)) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(super) fn inspect(layout: &Layout, issues: &mut Vec<String>) -> Result<(), IsolationError> {
    for entry in read_directory(&layout.pins)? {
        let path = entry.path();
        match observe(layout, &path) {
            Ok(PinState::Active(_)) => {}
            Ok(PinState::Abandoned) => {
                issues.push(format!("abandoned reader pin retained: {path:?}"));
            }
            Err(error) => issues.push(format!("uncertain reader pin retained: {path:?}: {error}")),
        }
    }
    Ok(())
}

pub(super) fn retain_active(
    layout: &Layout,
    referenced: &mut BTreeSet<String>,
    uncertain: &mut Vec<String>,
    removed: &mut usize,
) -> Result<(), IsolationError> {
    let mut uncertain_liveness = false;
    for entry in read_directory(&layout.pins)? {
        let path = entry.path();
        match observe(layout, &path) {
            Ok(PinState::Active(generation)) => {
                referenced.insert(generation);
            }
            Ok(PinState::Abandoned) => match fs::remove_file(&path) {
                Ok(()) => *removed += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    uncertain_liveness = true;
                    uncertain.push(format!("retained abandoned reader pin {path:?}: {error}"));
                }
            },
            Err(error) => {
                uncertain_liveness = true;
                uncertain.push(format!("retained uncertain reader pin {path:?}: {error}"));
            }
        }
    }
    if uncertain_liveness {
        retain_every_generation(layout, referenced)?;
    }
    Ok(())
}

enum PinState {
    Active(String),
    Abandoned,
}

fn observe(layout: &Layout, path: &Path) -> Result<PinState, IsolationError> {
    require_pin_name(path)?;
    require_mode(path, 0o600)?;
    let record: PinRecord = read_json(path)?;
    if record.schema != SCHEMA {
        return Err(IsolationError::new(
            "cargo-isolation::pin-schema-mismatch",
            format!(
                "reader pin carries schema {} instead of {SCHEMA}",
                record.schema
            ),
        ));
    }
    require_generation_name(&record.generation)?;
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .expect("require_pin_name admitted UTF-8");
    let lock = LockFile::new(
        LockName::new(name).expect("require_pin_name admitted one component"),
        path,
    );
    match lock
        .availability()
        .map_err(|error| IsolationError::io("cargo-isolation::pin-unavailable", path, error))?
    {
        Availability::Held => {
            validate_generation(layout, &record.generation)?;
            Ok(PinState::Active(record.generation))
        }
        Availability::Available => Ok(PinState::Abandoned),
    }
}

fn require_pin_name(path: &Path) -> Result<(), IsolationError> {
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return Err(IsolationError::new(
            "cargo-isolation::pin-name-invalid",
            format!("reader pin name is not UTF-8: {path:?}"),
        ));
    };
    let Some(identity) = name
        .strip_prefix(PREFIX)
        .and_then(|name| name.strip_suffix(".lock"))
    else {
        return Err(IsolationError::new(
            "cargo-isolation::pin-name-invalid",
            format!("unrecognized reader pin name: {name:?}"),
        ));
    };
    uuid::Uuid::parse_str(identity).map_err(|error| {
        IsolationError::new("cargo-isolation::pin-name-invalid", error.to_string())
    })?;
    Ok(())
}

fn require_generation_name(generation: &str) -> Result<(), IsolationError> {
    if generation.len() == 64
        && generation
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Ok(());
    }
    Err(IsolationError::new(
        "cargo-isolation::pin-generation-invalid",
        format!("reader pin carries an invalid generation: {generation:?}"),
    ))
}

fn retain_every_generation(
    layout: &Layout,
    referenced: &mut BTreeSet<String>,
) -> Result<(), IsolationError> {
    for generation in read_directory(&layout.generations)? {
        if generation.file_type().is_ok_and(|kind| kind.is_dir())
            && let Some(name) = generation.file_name().to_str()
            && !name.starts_with(TEMPORARY_PREFIX)
        {
            referenced.insert(name.to_owned());
        }
    }
    Ok(())
}
