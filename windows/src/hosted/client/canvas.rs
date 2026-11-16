//! The staged Linux canvas a hosted Darwin or Windows release stage links.
//!
//! The Linux release stage alone compiles the app. Its hosted run uploads a
//! `canvas-DIGEST` artifact holding the Linux payload and executable, kept
//! until its retention expires it; a later stage binds that run and digest
//! into its request, and its runner links exactly those bytes.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;

use serde::Deserialize;
use wire::Digest;
use wire::manifest::{ReleaseTarget, TargetPayload};

use super::{WindowsHostedError, command, successful_text};
use crate::hosted::{CANVAS_ARTIFACT_PREFIX, CanvasPin};

const LINUX_TARGET: ReleaseTarget = ReleaseTarget::X86_64UnknownLinuxMusl;
const ARTIFACT_PAGE: u32 = 100;

/// The staged Linux payload's app generation, and the provider run and
/// digest of the canvas that carries it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LinuxCanvas {
    pub(super) pin: CanvasPin,
    pub(super) app: Digest,
}

#[derive(Deserialize)]
struct ArtifactPage {
    artifacts: Vec<ProviderArtifact>,
}

#[derive(Deserialize)]
struct ProviderArtifact {
    id: u64,
    expired: bool,
    workflow_run: Option<ArtifactRun>,
}

#[derive(Deserialize)]
struct ArtifactRun {
    id: u64,
}

/// The canvas of the Linux payload staged below `stage` from
/// `source_archive`, and the provider run still carrying it.
///
/// # Errors
///
/// Refuses a missing Linux payload, one staged from another archive, and a
/// canvas no unexpired provider artifact carries; returns provider failures.
pub(super) fn locate(
    root: &Path,
    repository: &str,
    stage: &Path,
    source_archive: &str,
) -> Result<LinuxCanvas, WindowsHostedError> {
    let path = stage
        .join("targets")
        .join(LINUX_TARGET.as_str())
        .join("payload.json");
    let bytes = fs::read(&path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            WindowsHostedError::Canvas(format!(
                "no staged {LINUX_TARGET} payload; stage it from this archive first"
            ))
        } else {
            WindowsHostedError::Io {
                path: path.clone(),
                source,
            }
        }
    })?;
    let payload: TargetPayload = wire::decode(&bytes).map_err(WindowsHostedError::Decode)?;
    if payload.target != LINUX_TARGET || payload.archive.as_str() != source_archive {
        return Err(WindowsHostedError::Canvas(format!(
            "the staged {LINUX_TARGET} payload records archive {}, not {source_archive}; stage it from this archive first",
            payload.archive
        )));
    }
    let name = format!("{CANVAS_ARTIFACT_PREFIX}{}", payload.canvas);
    let listed = successful_text(
        "canvas artifact lookup",
        command(
            root,
            "gh",
            [
                OsString::from("api"),
                OsString::from(format!(
                    "repos/{repository}/actions/artifacts?name={name}&per_page={ARTIFACT_PAGE}"
                )),
            ],
        )?,
    )?;
    let page: ArtifactPage = serde_json::from_str(&listed).map_err(WindowsHostedError::Decode)?;
    let run_id = carrier(&page.artifacts).ok_or_else(|| {
        WindowsHostedError::Canvas(format!(
            "no unexpired provider artifact {name} carries the staged {LINUX_TARGET} canvas; stage it hosted again"
        ))
    })?;
    Ok(LinuxCanvas {
        pin: CanvasPin {
            run_id,
            digest: payload.canvas,
        },
        app: payload.app,
    })
}

/// The run owning the newest unexpired artifact; every copy holds the same
/// digest-named bytes, which the runner verifies.
fn carrier(artifacts: &[ProviderArtifact]) -> Option<u64> {
    artifacts
        .iter()
        .filter(|artifact| !artifact.expired)
        .filter_map(|artifact| Some((artifact.id, artifact.workflow_run.as_ref()?.id)))
        .max()
        .map(|(_, run_id)| run_id)
}

/// Require a returned target payload to link exactly the adopted canvas.
pub(super) fn admit_payload(
    payload: &TargetPayload,
    canvas: &LinuxCanvas,
) -> Result<(), WindowsHostedError> {
    if canvas.linked_by(&payload.app, &payload.canvas) {
        Ok(())
    } else {
        Err(WindowsHostedError::Canvas(format!(
            "the hosted {} payload links app {} and canvas {}, not the staged Linux app {} and canvas {}",
            payload.target, payload.app, payload.canvas, canvas.app, canvas.pin.digest
        )))
    }
}

impl LinuxCanvas {
    /// Whether a payload recording `app` and `digest` links this canvas.
    fn linked_by(&self, app: &Digest, digest: &Digest) -> bool {
        *app == self.app && *digest == self.pin.digest
    }
}

#[cfg(test)]
mod tests {
    use super::{ArtifactRun, CanvasPin, LinuxCanvas, ProviderArtifact, carrier};
    use wire::{Digest, Domain};

    fn artifact(id: u64, expired: bool, run: Option<u64>) -> ProviderArtifact {
        ProviderArtifact {
            id,
            expired,
            workflow_run: run.map(|id| ArtifactRun { id }),
        }
    }

    #[test]
    fn the_newest_unexpired_artifact_names_the_carrying_run() {
        assert_eq!(carrier(&[]), None);
        assert_eq!(carrier(&[artifact(9, true, Some(90))]), None);
        assert_eq!(carrier(&[artifact(9, false, None)]), None);
        assert_eq!(
            carrier(&[
                artifact(7, false, Some(70)),
                artifact(9, true, Some(90)),
                artifact(8, false, Some(80)),
            ]),
            Some(80)
        );
    }

    #[test]
    fn a_returned_payload_links_only_the_adopted_generation_and_canvas() {
        let digest = |bytes: &[u8]| Digest::compute(Domain::DistArtifact, bytes);
        let canvas = LinuxCanvas {
            pin: CanvasPin {
                run_id: 7,
                digest: digest(b"canvas"),
            },
            app: digest(b"app"),
        };
        assert!(canvas.linked_by(&digest(b"app"), &digest(b"canvas")));
        assert!(!canvas.linked_by(&digest(b"other app"), &digest(b"canvas")));
        assert!(!canvas.linked_by(&digest(b"app"), &digest(b"other canvas")));
    }
}
