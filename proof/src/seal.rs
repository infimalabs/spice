//! One proof operation from the exact staged payload to detached evidence.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use wire::manifest::ProofReceipt;
use wire::{Digest, ProofEvidence};

use crate::config::Config;
use crate::container::Limits;
use crate::evidence::{EvidencePublishError, admit_receipt, publish};
use crate::layout::Layout;
use crate::proving::{ProofError, Proved, Staged};
use crate::replay::ReceiptError;

/// Prove the one payload selected by `layout` and atomically publish evidence.
///
/// The shared replay core admits the payload before environment setup and
/// executes the complete current battery once. Only a clean replay whose new
/// receipt verifies as one payload, target, environment, battery, replay, and
/// receipt identity chain reaches the exact detached-receipt destination.
///
/// # Errors
///
/// Returns the shared core's typed failure, or a receipt creation or
/// publication failure. Infrastructure failure and non-clean replay both
/// return before the receipt directory or destination is changed.
pub fn seal(
    layout: &Layout,
    config: &Config,
    workspace: impl AsRef<Path>,
    limits: Limits,
) -> Result<ProofEvidence, SealError> {
    let proved = Staged::admit(layout)?.replay(config, workspace, limits)?;
    publish_receipt(&proved, publish)
}

/// Create the receipt `proved` derives, verify it, and publish it last.
fn publish_receipt<P>(proved: &Proved, publisher: P) -> Result<ProofEvidence, SealError>
where
    P: FnOnce(&ProofReceipt, &Path) -> Result<Digest, EvidencePublishError>,
{
    let payload = proved.payload();
    let receipt = proved
        .replay()
        .receipt(payload.value(), payload.digest().clone())
        .map_err(SealError::Receipt)?;
    let receipt_bytes =
        wire::encode(&receipt).map_err(|source| SealError::EncodeReceipt(source.to_string()))?;
    let admitted = admit_receipt(&receipt_bytes).map_err(ProofError::Evidence)?;
    let verified = proved.verify(&admitted)?;

    let destination = proved.receipt().path();
    let parent = destination
        .parent()
        .expect("a layout-derived receipt path always has a parent");
    fs::create_dir_all(parent).map_err(|source| SealError::PrepareDestination {
        path: parent.to_path_buf(),
        source,
    })?;
    let published = publisher(&receipt, destination).map_err(SealError::Publish)?;
    if published != *verified.receipt_digest() {
        return Err(SealError::PublishedReceipt {
            verified: verified.receipt_digest().clone(),
            published,
        });
    }

    Ok(verified.into())
}

/// Why one proof operation did not publish clean detached evidence.
#[derive(Debug)]
pub enum SealError {
    Proof(ProofError),
    Receipt(ReceiptError),
    EncodeReceipt(String),
    PrepareDestination {
        path: std::path::PathBuf,
        source: io::Error,
    },
    Publish(EvidencePublishError),
    PublishedReceipt {
        verified: Digest,
        published: Digest,
    },
}

impl From<ProofError> for SealError {
    fn from(error: ProofError) -> Self {
        Self::Proof(error)
    }
}

impl fmt::Display for SealError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Proof(source) => source.fmt(formatter),
            Self::Receipt(source) => source.fmt(formatter),
            Self::EncodeReceipt(source) => {
                write!(formatter, "proof receipt encoding failed: {source}")
            }
            Self::PrepareDestination { path, source } => write!(
                formatter,
                "proof receipt directory could not be prepared at {}: {source}",
                path.display()
            ),
            Self::Publish(source) => source.fmt(formatter),
            Self::PublishedReceipt {
                verified,
                published,
            } => write!(
                formatter,
                "published proof receipt identity {published} does not match verified identity {verified}"
            ),
        }
    }
}

impl Error for SealError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Proof(source) => Some(source),
            Self::Receipt(source) => Some(source),
            Self::PrepareDestination { source, .. } => Some(source),
            Self::Publish(source) => Some(source),
            Self::EncodeReceipt(_) | Self::PublishedReceipt { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::io::Write as _;
    use std::path::PathBuf;

    use wire::manifest::ReleaseTarget;
    use wire::{Digest, Domain};

    use super::{SealError, publish_receipt};
    use crate::evidence::{EvidenceError, EvidencePublishError, publish};
    use crate::proving::ProofError;
    use crate::proving::fixture::Fixture;
    use crate::replay::ReplayVerdict;

    const OLD_RECEIPT: &[u8] = b"previous receipt";

    #[test]
    fn clean_proof_republishes_identical_receipt_bytes_and_identity() {
        let fixture = Fixture::new(ReleaseTarget::X86_64UnknownLinuxMusl);
        let decoys = plant_decoys(&fixture);

        let first = publish_receipt(&fixture.proved_by(fixture.clean_replay()), publish)
            .expect("clean replay publishes");
        let proved = fixture.proved_by(fixture.clean_replay());
        let first_bytes = fs::read(proved.receipt().path()).expect("first receipt bytes");
        let second = publish_receipt(&proved, publish).expect("identical replay republishes");
        let second_bytes = fs::read(proved.receipt().path()).expect("second receipt bytes");

        assert_eq!(first, second);
        assert_eq!(first_bytes, second_bytes);
        assert_eq!(
            first.receipt,
            Digest::compute(Domain::ProofReceipt, &first_bytes)
        );
        assert_eq!(
            fs::read(fixture.layout().payload_path()).expect("payload remains readable"),
            fixture.payload_bytes
        );
        for (path, expected) in decoys {
            assert_eq!(fs::read(path).expect("read decoy"), expected);
        }
    }

    #[test]
    fn unverified_evidence_never_reaches_the_publisher() {
        let fixture = Fixture::new(ReleaseTarget::X86_64UnknownLinuxMusl);
        let wrong_battery = Digest::compute(Domain::ProofBattery, b"another battery");
        let proved = fixture.proved_by(ReplayVerdict::fixture_clean(
            fixture.target,
            fixture.environment(),
            wrong_battery,
        ));
        let receipt = old_receipt(&proved);
        let published = Cell::new(false);

        let error = publish_receipt(&proved, |_, _| {
            published.set(true);
            unreachable!("unverified evidence cannot publish")
        })
        .expect_err("wrong battery refuses");

        assert!(matches!(
            error,
            SealError::Proof(ProofError::Evidence(EvidenceError::BatteryDigest))
        ));
        assert!(!published.get());
        assert_eq!(fs::read(receipt).expect("old receipt"), OLD_RECEIPT);
    }

    #[test]
    fn interrupted_staging_never_replaces_the_existing_receipt() {
        let fixture = Fixture::new(ReleaseTarget::X86_64UnknownLinuxMusl);
        let proved = fixture.proved_by(fixture.clean_replay());
        let receipt = old_receipt(&proved);

        let error = publish_receipt(&proved, |_, destination| {
            let publication =
                atomic::Destination::new(destination).expect("fixture receipt parent is present");
            let mut staged = publication.stage().expect("stage interrupted write");
            staged.write_all(b"partial receipt").expect("write partial");
            drop(staged);
            Err(EvidencePublishError::Publish(atomic::Error::Refused(
                atomic::Refusal::DestinationHasNoFileName {
                    destination: fixture.root.clone(),
                },
            )))
        })
        .expect_err("interrupted publisher refuses");

        assert!(matches!(error, SealError::Publish(_)));
        assert_eq!(fs::read(&receipt).expect("old receipt"), OLD_RECEIPT);
        assert_eq!(
            fs::read_dir(receipt.parent().expect("receipt parent"))
                .expect("read receipt parent")
                .count(),
            1,
            "interrupted staging leaves no temporary file"
        );
    }

    /// Plant a previous receipt at `proved`'s destination and return its path.
    fn old_receipt(proved: &crate::proving::Proved) -> PathBuf {
        let path = proved.receipt().path().to_path_buf();
        fs::create_dir_all(path.parent().expect("receipt parent exists"))
            .expect("create receipt parent");
        fs::write(&path, OLD_RECEIPT).expect("write old receipt");
        path
    }

    fn plant_decoys(fixture: &Fixture) -> Vec<(PathBuf, Vec<u8>)> {
        ["artifact", "journal", "fold", "resident", "cache"]
            .into_iter()
            .map(|name| {
                let path = fixture.root.join(name);
                let bytes = format!("untouched {name}").into_bytes();
                fs::write(&path, &bytes).expect("write decoy");
                (path, bytes)
            })
            .collect()
    }
}
