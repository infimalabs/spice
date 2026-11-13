//! Mutation-free re-derivation of one existing detached proof.

use std::error::Error;
use std::fmt;
use std::path::Path;

use wire::ProofEvidence;

use crate::config::Config;
use crate::container::Limits;
use crate::evidence::{EvidenceError, admit_receipt};
use crate::layout::{InputError, Layout};
use crate::proving::{ProofError, Staged};

/// Re-execute the current battery and verify one existing detached receipt.
///
/// Payload and receipt bytes are admitted before environment setup. The shared
/// replay core then executes the current battery once in the exact
/// target-matched environment. Success re-derives the shared evidence DTO
/// without publishing or changing any repository or appliance state.
///
/// # Errors
///
/// Returns the stored receipt's read or admission failure, or the shared
/// core's typed failure.
pub fn rederive(
    layout: &Layout,
    config: &Config,
    workspace: impl AsRef<Path>,
    limits: Limits,
) -> Result<ProofEvidence, RederiveError> {
    let staged = Staged::admit(layout)?;
    let receipt_bytes = staged
        .receipt()
        .read()
        .map_err(RederiveError::ReadReceipt)?;
    let receipt = admit_receipt(&receipt_bytes).map_err(RederiveError::AdmitReceipt)?;
    let proved = staged.replay(config, workspace, limits)?;
    Ok(proved.verify(&receipt)?.into())
}

/// Why existing detached proof evidence could not be re-derived.
#[derive(Debug)]
pub enum RederiveError {
    Proof(ProofError),
    ReadReceipt(InputError),
    AdmitReceipt(EvidenceError),
}

impl From<ProofError> for RederiveError {
    fn from(error: ProofError) -> Self {
        Self::Proof(error)
    }
}

impl fmt::Display for RederiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Proof(source) => source.fmt(formatter),
            Self::ReadReceipt(source) => source.fmt(formatter),
            Self::AdmitReceipt(source) => source.fmt(formatter),
        }
    }
}

impl Error for RederiveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Proof(source) => Some(source),
            Self::ReadReceipt(source) => Some(source),
            Self::AdmitReceipt(source) => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use sill::Deadlines;
    use wire::manifest::ReleaseTarget;

    use super::RederiveError;
    use crate::config::Config;
    use crate::container::Limits;
    use crate::evidence::EvidenceError;
    use crate::layout::{Input, InputError};
    use crate::proving::fixture::Fixture;

    #[test]
    fn absent_and_noncanonical_receipts_refuse_before_environment_setup() {
        let fixture = Fixture::new(ReleaseTarget::X86_64UnknownLinuxMusl);
        let layout = fixture.layout();
        let config = Config::fixture(None, None);
        let before = snapshot(&fixture.selected);

        let missing = super::rederive(&layout, &config, &fixture.worktree, limits())
            .expect_err("missing receipt refuses");
        assert!(matches!(
            missing,
            RederiveError::ReadReceipt(InputError::Missing {
                input: Input::Receipt,
                ..
            })
        ));
        assert_eq!(snapshot(&fixture.selected), before);

        let receipt = fixture.staged().receipt().path().to_path_buf();
        fs::create_dir_all(receipt.parent().expect("receipt parent"))
            .expect("create receipt parent");
        let mut noncanonical = wire::encode(&fixture.receipt(&fixture.clean_replay()))
            .expect("fixture receipt encodes");
        noncanonical.push(b'\n');
        fs::write(&receipt, noncanonical).expect("write noncanonical receipt");
        let with_noncanonical_receipt = snapshot(&fixture.selected);

        let refused = super::rederive(&layout, &config, &fixture.worktree, limits())
            .expect_err("noncanonical receipt refuses");
        assert!(matches!(
            refused,
            RederiveError::AdmitReceipt(EvidenceError::NonCanonicalReceipt)
        ));
        assert_eq!(snapshot(&fixture.selected), with_noncanonical_receipt);
        assert_ne!(before, with_noncanonical_receipt);
    }

    fn limits() -> Limits {
        Limits::new(
            Deadlines::new(
                Duration::from_secs(30),
                Duration::from_secs(1),
                Duration::from_secs(1),
            )
            .expect("fixture deadlines"),
            Duration::from_millis(10),
            4096,
        )
        .expect("fixture limits")
    }

    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(path) = pending.pop() {
            let mut entries = fs::read_dir(&path)
                .expect("snapshot directory")
                .map(|entry| entry.expect("snapshot entry").path())
                .collect::<Vec<_>>();
            entries.sort();
            for entry in entries.into_iter().rev() {
                if entry.is_dir() {
                    pending.push(entry);
                } else {
                    let relative = entry
                        .strip_prefix(root)
                        .expect("snapshot path belongs to root")
                        .to_path_buf();
                    files.push((relative, fs::read(entry).expect("snapshot file")));
                }
            }
        }
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }
}
