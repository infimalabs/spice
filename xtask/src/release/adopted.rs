//! The canvas a Darwin or Windows release stage adopts from the staged Linux
//! payload, so all three executables link the one app generation the Linux
//! proof image compiled.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use wire::Digest;
use wire::manifest::ReleaseTarget;

use super::TargetBuildError;

/// The target whose sealed build compiles the release app.
const CANVAS_TARGET: ReleaseTarget = ReleaseTarget::X86_64UnknownLinuxMusl;

/// The rack the staged Linux executable links, and the app generation its
/// payload records.
pub(super) struct LinuxCanvas {
    pub(super) generation: Digest,
    pub(super) bytes: Vec<u8>,
}

/// Read the canvas of the Linux payload staged under `stage_root` from the
/// source archive `archive`.
///
/// # Errors
///
/// Refuses a missing or unadmitted Linux payload, one recording another
/// archive, an unreadable executable object, and an executable whose linked
/// rack differs from the payload's recorded canvas.
pub(super) fn linux_canvas(
    stage_root: &Path,
    archive: &Digest,
) -> Result<LinuxCanvas, TargetBuildError> {
    let path = stage_root
        .join("targets")
        .join(CANVAS_TARGET.as_str())
        .join("payload.json");
    let payload_bytes = fs::read(&path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            refused(format!(
                "no {CANVAS_TARGET} payload is staged at {}; stage {CANVAS_TARGET} from this \
                 archive first",
                path.display()
            ))
        } else {
            TargetBuildError::Io { path, source }
        }
    })?;
    let admitted = proof::admit(&payload_bytes)
        .map_err(|error| refused(format!("staged {CANVAS_TARGET} payload refused: {error}")))?;
    let payload = admitted.value();
    if payload.target != CANVAS_TARGET {
        return Err(refused(format!(
            "the {CANVAS_TARGET} payload pointer names target {}",
            payload.target
        )));
    }
    if payload.archive != *archive {
        return Err(refused(format!(
            "the staged {CANVAS_TARGET} payload was built from archive {}, this stage builds \
             archive {archive}",
            payload.archive
        )));
    }
    let entry = payload
        .inventory
        .iter()
        .find(|entry| entry.path == CANVAS_TARGET.executable_path())
        .ok_or_else(|| {
            refused(format!(
                "the staged {CANVAS_TARGET} payload carries no executable"
            ))
        })?;
    let object = stage_root.join(PathBuf::from(format!(
        "objects/{}-{:04o}",
        entry.digest, entry.mode
    )));
    let executable = fs::read(&object).map_err(|source| TargetBuildError::Io {
        path: object.clone(),
        source,
    })?;
    let linked = ::rack::link::extract(&executable).map_err(|error| {
        refused(format!(
            "{} links no readable canvas: {error}",
            object.display()
        ))
    })?;
    let canvas = dist::Canvas::new(payload.app.clone(), linked);
    if *canvas.archive() != payload.canvas {
        return Err(refused(format!(
            "the staged {CANVAS_TARGET} executable links canvas {}, its payload records {}",
            canvas.archive(),
            payload.canvas
        )));
    }
    Ok(LinuxCanvas {
        generation: payload.app.clone(),
        bytes: linked.to_vec(),
    })
}

fn refused(reason: String) -> TargetBuildError {
    TargetBuildError::AdoptedCanvas(reason)
}
