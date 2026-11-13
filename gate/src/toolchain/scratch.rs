//! Exclusive scratch roots for the fixtures that plant real trees on disk.
//!
//! Every fixture in this family needs a directory of its own in the shared
//! temporary directory. Naming one from the process id and clearing whatever
//! answers to that name looks like reuse of a dead leftover, but eleven lanes
//! share one temporary directory and process ids coincide across them, so the
//! directory being cleared can be a live peer's fixture. These roots are taken
//! by creating them instead.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, io};

/// How many names one fixture tries before giving up on an exclusive root.
const SCRATCH_ATTEMPTS: usize = 100;

/// One scratch root under `label`, on a name nothing else already holds.
///
/// `create_dir` refuses a directory that exists rather than opening it, so
/// retrying under a fresh name is what makes the root exclusive: a run only
/// ever owns a name its own `create_dir` succeeded on. The name carries the
/// clock rather than the process id because ids are reused and the clock is
/// not, which is what keeps two lanes off the same name in the first place.
pub(crate) fn exclusive_scratch_root(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..SCRATCH_ATTEMPTS {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock reads after the epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "{label}-{stamp}-{}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::create_dir(&root) {
            Ok(()) => return root,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("exclusive scratch root is created: {error}"),
        }
    }
    panic!("no exclusive scratch root was available in {SCRATCH_ATTEMPTS} attempts");
}
