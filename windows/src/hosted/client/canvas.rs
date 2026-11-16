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

use super::{WindowsHostedError, command, linux, successful_text};
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

#[derive(Clone, Deserialize)]
struct ProviderRun {
    path: String,
    status: String,
    conclusion: Option<String>,
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
    let mut page: ArtifactPage =
        serde_json::from_str(&listed).map_err(WindowsHostedError::Decode)?;
    let run_id = carrier(&mut page.artifacts, |run_id| {
        provider_run(root, repository, run_id)
    })?
    .ok_or_else(|| {
        WindowsHostedError::Canvas(format!(
            "no completed-success {} run carries unexpired provider artifact {name}; stage it hosted again",
            linux::WORKFLOW_PATH
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

/// The newest unexpired artifact owned by a completed-success Linux staging
/// run. A failed or unattested run is never a canvas provenance boundary.
fn carrier<E>(
    artifacts: &mut [ProviderArtifact],
    mut provider_run: impl FnMut(u64) -> Result<ProviderRun, E>,
) -> Result<Option<u64>, E> {
    artifacts.sort_unstable_by_key(|artifact| std::cmp::Reverse(artifact.id));
    for artifact in artifacts.iter().filter(|artifact| !artifact.expired) {
        let Some(owner) = &artifact.workflow_run else {
            continue;
        };
        let run_id = owner.id;
        let run = provider_run(run_id)?;
        if run.path == linux::WORKFLOW_PATH
            && run.status == "completed"
            && run.conclusion.as_deref() == Some("success")
        {
            return Ok(Some(run_id));
        }
    }
    Ok(None)
}

fn provider_run(
    root: &Path,
    repository: &str,
    run_id: u64,
) -> Result<ProviderRun, WindowsHostedError> {
    let observed = successful_text(
        "canvas provider run lookup",
        command(
            root,
            "gh",
            [
                OsString::from("api"),
                OsString::from(format!("repos/{repository}/actions/runs/{run_id}")),
            ],
        )?,
    )?;
    serde_json::from_str(&observed).map_err(WindowsHostedError::Decode)
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
    use super::{
        ArtifactRun, CanvasPin, LinuxCanvas, ProviderArtifact, ProviderRun, carrier, linux,
    };
    use wire::{Digest, Domain};

    fn artifact(id: u64, expired: bool, run: Option<u64>) -> ProviderArtifact {
        ProviderArtifact {
            id,
            expired,
            workflow_run: run.map(|id| ArtifactRun { id }),
        }
    }

    #[test]
    fn the_newest_successful_linux_run_carries_the_canvas() {
        let runs = [
            (
                70,
                ProviderRun {
                    path: linux::WORKFLOW_PATH.to_owned(),
                    status: "completed".to_owned(),
                    conclusion: Some("success".to_owned()),
                },
            ),
            (
                80,
                ProviderRun {
                    path: linux::WORKFLOW_PATH.to_owned(),
                    status: "completed".to_owned(),
                    conclusion: Some("failure".to_owned()),
                },
            ),
            (
                90,
                ProviderRun {
                    path: ".github/workflows/windows.yml".to_owned(),
                    status: "completed".to_owned(),
                    conclusion: Some("success".to_owned()),
                },
            ),
        ];
        let lookup = |id| {
            Ok::<_, ()>(
                runs.iter()
                    .find(|(run_id, _)| *run_id == id)
                    .expect("provider run exists")
                    .1
                    .to_owned(),
            )
        };

        assert_eq!(carrier(&mut [], lookup), Ok(None));
        let mut expired = [artifact(9, true, Some(70))];
        assert_eq!(carrier(&mut expired, lookup), Ok(None));
        let mut unowned = [artifact(9, false, None)];
        assert_eq!(carrier(&mut unowned, lookup), Ok(None));
        let mut candidates = [
            artifact(7, false, Some(70)),
            artifact(9, false, Some(90)),
            artifact(8, false, Some(80)),
        ];
        assert_eq!(carrier(&mut candidates, lookup), Ok(Some(70)));
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
