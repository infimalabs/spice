//! Wall milliseconds accepted gate runs report per arm, retained so the sweep
//! can rank slow arms without timing anything itself.
//!
//! `spice gate prove` records every arm it proved; a reused receipt proves
//! nothing and records nothing. Each arm keeps its newest runs, oldest first,
//! and ranks by their median so one cold cache neither files nor hides a child.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Runs retained per arm.
pub const ARM_RUNS_RETAINED: usize = 8;

const ARM_LEDGER_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArmLedger {
    version: u32,
    arms: Vec<ArmTiming>,
}

/// The newest retained runs of one arm.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmTiming {
    pub arm: String,
    /// Wall milliseconds, oldest first, at most [`ARM_RUNS_RETAINED`].
    pub millis: Vec<u64>,
}

impl ArmTiming {
    /// Runs observed.
    #[must_use]
    pub const fn runs(&self) -> usize {
        self.millis.len()
    }

    /// Whole wall seconds at the upper median of the retained runs.
    #[must_use]
    pub fn seconds(&self) -> u64 {
        let mut sorted = self.millis.clone();
        sorted.sort_unstable();
        sorted
            .get(sorted.len() / 2)
            .map_or(0, |millis| millis.div_ceil(1000))
    }
}

/// Every recorded arm in arm order; an absent ledger records none.
///
/// # Errors
///
/// Returns the read failure, or `InvalidData` for bytes that are not one
/// current arm ledger.
pub fn read_arms(path: &Path) -> io::Result<Vec<ArmTiming>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let ledger: ArmLedger =
        wire::decode(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if ledger.version != ARM_LEDGER_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "gate arm ledger version {} is not {ARM_LEDGER_VERSION}",
                ledger.version
            ),
        ));
    }
    Ok(ledger.arms)
}
fn create_real_directory(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!(
                "gate arm ledger parent is not a real directory: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn ensure_parent(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "gate arm ledger has no parent directory",
        )
    })?;
    let authority = parent.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "gate arm ledger parent has no authority directory",
        )
    })?;
    create_real_directory(authority)?;
    create_real_directory(parent)
}

/// Append one run per observed arm under `lock`, keeping each arm's newest
/// [`ARM_RUNS_RETAINED`] runs, in one atomic publication.
///
/// # Errors
///
/// Returns the lock, read, encoding, or publication failure.
pub fn record_arms(
    path: &Path,
    lock: &Path,
    observed: impl IntoIterator<Item = (String, u64)>,
) -> io::Result<()> {
    ensure_parent(path)?;
    let name = sill::LockName::new("gate-arms").expect("a fixed lock name is one normal component");
    let _possession = sill::LockFile::new(name, lock).acquire()?;
    let mut arms: BTreeMap<String, Vec<u64>> = read_arms(path)?
        .into_iter()
        .map(|timing| (timing.arm, timing.millis))
        .collect();
    for (arm, millis) in observed {
        let runs = arms.entry(arm).or_default();
        runs.push(millis);
        if runs.len() > ARM_RUNS_RETAINED {
            runs.drain(..runs.len() - ARM_RUNS_RETAINED);
        }
    }
    let bytes = wire::encode(&ArmLedger {
        version: ARM_LEDGER_VERSION,
        arms: arms
            .into_iter()
            .map(|(arm, millis)| ArmTiming { arm, millis })
            .collect(),
    })
    .map_err(io::Error::other)?;
    atomic::Destination::new(path)
        .map_err(io::Error::other)?
        .publish(|staged| staged.write_all(&bytes))
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_arm_ranks_by_its_upper_median_run_in_whole_seconds() {
        let timing = ArmTiming {
            arm: "cargo:test:app::lib:app".to_owned(),
            millis: vec![123_400, 900, 61_001],
        };
        assert_eq!(timing.seconds(), 62, "one cold run neither hides nor files");
        assert_eq!(timing.runs(), 3);
        let even = ArmTiming {
            arm: String::new(),
            millis: vec![1_000, 4_000],
        };
        assert_eq!(even.seconds(), 4, "an even count takes the upper median");
        assert_eq!(
            ArmTiming {
                arm: String::new(),
                millis: Vec::new()
            }
            .seconds(),
            0
        );
    }

    #[test]
    fn a_record_creates_its_canonical_parent_and_keeps_newest_runs() {
        let root = std::env::temp_dir().join(format!("spice-gate-arms-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("runtime/locks")).expect("lock parent exists");
        let path = root.join("authority/gate/arms.json");
        let lock = root.join("runtime/locks/gate-arms");
        assert_eq!(read_arms(&path).expect("an absent ledger reads"), []);

        for run in 1..=u64::try_from(ARM_RUNS_RETAINED + 2).expect("small") {
            record_arms(
                &path,
                &lock,
                [("slow".to_owned(), run), ("quick".to_owned(), 1)],
            )
            .expect("a record publishes");
        }
        let arms = read_arms(&path).expect("the ledger reads back");
        assert_eq!(arms.len(), 2);
        assert_eq!(
            arms[1].millis,
            (3..=u64::try_from(ARM_RUNS_RETAINED + 2).expect("small")).collect::<Vec<_>>(),
            "the oldest runs fall off the front"
        );
        assert_eq!(arms[0].arm, "quick");
        fs::remove_dir_all(root).expect("fixture removes");
    }

    #[cfg(unix)]
    #[test]
    fn a_record_refuses_a_linked_parent_before_publication() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("spice-gate-arms-linked-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let outside = root.join("outside");
        fs::create_dir_all(root.join("authority")).expect("authority parent exists");
        fs::create_dir_all(root.join("runtime/locks")).expect("lock parent exists");
        fs::create_dir_all(&outside).expect("outside directory exists");
        symlink(&outside, root.join("authority/gate")).expect("linked parent plants");
        let path = root.join("authority/gate/arms.json");
        let error = record_arms(
            &path,
            &root.join("runtime/locks/gate-arms"),
            [("merge-integrity".to_owned(), 1)],
        )
        .expect_err("a linked parent refuses");
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
        assert!(!outside.join("arms.json").exists());
        fs::remove_dir_all(root).expect("fixture removes");
    }
    #[cfg(unix)]
    #[test]
    fn a_record_refuses_a_linked_authority_before_creating_its_parent() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "spice-gate-arms-linked-authority-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let outside = root.join("outside");
        fs::create_dir_all(root.join("runtime/locks")).expect("lock parent exists");
        fs::create_dir_all(&outside).expect("outside directory exists");
        symlink(&outside, root.join("authority")).expect("linked authority plants");

        let error = record_arms(
            &root.join("authority/gate/arms.json"),
            &root.join("runtime/locks/gate-arms"),
            [("merge-integrity".to_owned(), 1)],
        )
        .expect_err("a linked authority refuses");
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
        assert!(!outside.join("gate").exists());
        fs::remove_dir_all(root).expect("fixture removes");
    }
}
