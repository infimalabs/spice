//! Provider-side proof of one exact staged release target.

use std::error::Error;
use std::fmt;
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use wire::ProofEvidence;
use wire::manifest::ReleaseTarget;

const MEMBER_RUNTIME: Duration = Duration::from_secs(60 * 60);
const TERMINATE_HORIZON: Duration = Duration::from_secs(10);
const KILL_HORIZON: Duration = Duration::from_secs(10);
const TERMINATION_GRACE: Duration = Duration::from_secs(5);
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// Prove one provider-staged payload and retain its detached receipt beside the
/// staged files that the provider returns.
///
/// # Errors
///
/// Refuses unavailable repository policy, invalid process limits, a failed
/// proof replay, or output failure.
pub fn prove_staged_target(
    target: ReleaseTarget,
    output: &mut impl Write,
) -> Result<ProofEvidence, StageProofError> {
    let repository = spice::repository::Repository::discover(Path::new("."))
        .map_err(|error| StageProofError::Refused(error.to_string()))?;
    let config = spice::config::load_repository(&repository, &[])
        .map_err(|error| StageProofError::Refused(error.to_string()))?;
    let deadlines = sill::Deadlines::new(MEMBER_RUNTIME, TERMINATE_HORIZON, KILL_HORIZON)
        .map_err(|error| StageProofError::Refused(error.to_string()))?;
    let limits = proof::Limits::new(deadlines, TERMINATION_GRACE, OUTPUT_LIMIT)
        .map_err(|error| StageProofError::Refused(error.to_string()))?;
    let proof_root = repository.worktree_root().join("dist/stage/proofs");
    let layout = proof::Layout::new(repository.worktree_root(), proof_root, target);
    let evidence = proof::seal(
        &layout,
        config.root().proof(),
        repository.worktree_root(),
        limits,
    )
    .map_err(StageProofError::Seal)?;
    writeln!(
        output,
        "proved staged {target} payload={} receipt={}",
        evidence.payload, evidence.receipt
    )
    .map_err(StageProofError::Output)?;
    Ok(evidence)
}

/// Why provider-side proof did not publish one detached receipt.
#[derive(Debug)]
pub enum StageProofError {
    Refused(String),
    Seal(proof::SealError),
    Output(std::io::Error),
}

impl fmt::Display for StageProofError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(detail) => detail.fmt(formatter),
            Self::Seal(error) => error.fmt(formatter),
            Self::Output(error) => error.fmt(formatter),
        }
    }
}

impl Error for StageProofError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Seal(error) => Some(error),
            Self::Output(error) => Some(error),
            Self::Refused(_) => None,
        }
    }
}
