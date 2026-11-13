//! Bounded reclamation for stranded install publications.
//!
//! Every install publication stages its image under a canonical
//! `.spice-install-<pid>-<counter>.tmp` name in the destination's parent. A
//! crash can strand one; on Windows, an image a process is still running also
//! leaves under such a name when it is replaced or retired. Maintenance
//! reclaims a bounded set of these whose minting process has exited and whose
//! content has stayed quiet for a grace period.

use std::path::PathBuf;
use std::time::Duration;

use crate::repository::Repository;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

pub use platform::{inspect_temporary_records, reclaim_temporary_records};

const PREFIX: &[u8] = b".spice-install-";
const SUFFIX: &[u8] = b".tmp";
const ENTRY_LIMIT: usize = 4096;
const CANDIDATE_LIMIT: usize = 32;
const STALE_GRACE: Duration = Duration::from_secs(60 * 60);
fn record_directory(repository: &Repository) -> PathBuf {
    repository
        .state_root()
        .join(super::RECORD_PATH)
        .parent()
        .expect("the install receipt lives in a directory")
        .to_path_buf()
}

/// Read-only evidence about stranded install publication files.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InstallTempInspection {
    runtime_present: bool,
    examined_entries: usize,
    omitted_entries: usize,
    omitted_candidates: usize,
    reclaimable_count: usize,
    reclaimable_bytes: u64,
    unsafe_candidates: Vec<String>,
}

impl InstallTempInspection {
    /// Whether the repository-common state root exists.
    #[must_use]
    pub const fn runtime_present(&self) -> bool {
        self.runtime_present
    }

    /// Number of entries examined during the bounded pass.
    #[must_use]
    pub const fn examined_entries(&self) -> usize {
        self.examined_entries
    }

    /// Lower bound on entries omitted after the entry cap was reached.
    #[must_use]
    pub const fn omitted_entries(&self) -> usize {
        self.omitted_entries
    }

    /// Number of canonical candidates omitted after the action cap.
    #[must_use]
    pub const fn omitted_candidates(&self) -> usize {
        self.omitted_candidates
    }

    /// Number of stale install-publication files one invocation can reclaim.
    #[must_use]
    pub const fn reclaimable_count(&self) -> usize {
        self.reclaimable_count
    }

    /// Total bytes named by the bounded reclaimable candidate set.
    #[must_use]
    pub const fn reclaimable_bytes(&self) -> u64 {
        self.reclaimable_bytes
    }

    /// Candidates retained because their shape or ownership is unsafe.
    #[must_use]
    pub fn unsafe_candidates(&self) -> &[String] {
        &self.unsafe_candidates
    }

    /// Whether either bounded scan cap withheld state from this result.
    #[must_use]
    pub const fn incomplete(&self) -> bool {
        self.omitted_entries != 0 || self.omitted_candidates != 0
    }
}

/// The nonzero minting process identifier a canonical publication name
/// carries, or `None` for any other name.
fn candidate_pid(bytes: &[u8]) -> Option<u64> {
    let body = bytes.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let separator = body.iter().position(|byte| *byte == b'-')?;
    let (pid, counter) = (&body[..separator], &body[separator + 1..]);
    if counter.contains(&b'-') {
        return None;
    }
    canonical_decimal(counter, true)?;
    canonical_decimal(pid, false)
}

fn canonical_decimal(bytes: &[u8], zero_allowed: bool) -> Option<u64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    if bytes[0] == b'0' {
        return (zero_allowed && bytes.len() == 1).then_some(0);
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn concern(report: &mut InstallTempInspection, name: &str, reason: &str) {
    report.unsafe_candidates.push(format!("{name}: {reason}"));
}

#[cfg(test)]
mod tests {
    use super::candidate_pid;

    #[test]
    fn canonical_names_close_over_pid_and_counter_grammar() {
        for counter in ["0", "1", "18446744073709551615"] {
            let name = format!(".spice-install-7-{counter}.tmp");
            assert_eq!(candidate_pid(name.as_bytes()), Some(7), "{name}");
        }
        for name in [
            ".spice-install-0-0.tmp",
            ".spice-install-01-0.tmp",
            ".spice-install-1-00.tmp",
            ".spice-install-1--1.tmp",
            ".spice-install-1-+1.tmp",
            ".spice-install-1- 1.tmp",
            ".spice-install-1-18446744073709551616.tmp",
            ".spice-install-1-0.tmp.extra",
            "foreign",
        ] {
            assert_eq!(candidate_pid(name.as_bytes()), None, "{name}");
        }
    }

    #[test]
    fn minted_temporary_names_are_reclaimable_candidates() {
        let name = super::super::handle::temporary_name();
        assert_eq!(
            candidate_pid(name.as_encoded_bytes()),
            Some(u64::from(std::process::id()))
        );
    }
}
