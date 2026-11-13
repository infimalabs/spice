//! Resolution of live model spellings against one running occupant.

use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use wire::CanonicalUuid;

use super::{
    Driver, Kick, RETUNE_TIMEOUT, RuntimePaths, call, door_root, kick_path, reply_name, send_to,
};

/// One model spelling resolved against the exact running occupant's startup
/// catalog.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub(in crate::resident) enum ModelResolution {
    Resolved { provider: String, model: String },
    Refused { detail: String },
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct ModelResolutionReply {
    pub(super) result: ModelResolution,
}

impl<W: Write + Send + 'static> Driver<W> {
    /// Resolve one operator spelling against the model catalog OMP reported at
    /// startup, answering through the caller's one-shot callback door.
    pub(super) fn resolve_model(&self, spelling: &str, reply: &Path) {
        let result = self
            .registration
            .models
            .get()
            .ok_or_else(|| "omp has not listed its served models".to_owned())
            .and_then(|served| {
                vendor::omp::resolve_model(spelling, served).map_err(|error| error.to_string())
            });
        let result = match result {
            Ok(model) => ModelResolution::Resolved {
                provider: model.provider.clone(),
                model: model.id.clone(),
            },
            Err(detail) => ModelResolution::Refused { detail },
        };
        let _sent = send_to(reply, &ModelResolutionReply { result });
    }
}

/// Resolve a model spelling against the exact running occupant's retained
/// served-model catalog without changing either durable or runtime state.
///
/// # Errors
///
/// Returns an I/O error when no matching supervisor can receive the request
/// or its callback cannot be read.
pub(in crate::resident) fn resolve_model(
    paths: &RuntimePaths,
    lane: CanonicalUuid,
    occupant: CanonicalUuid,
    spelling: String,
) -> io::Result<ModelResolution> {
    let request = super::super::super::new_uuid().to_string();
    let reply = door_root(paths).join(reply_name(&paths.common, 'm', &request));
    let answer: ModelResolutionReply =
        call(&kick_path(paths, lane), reply, RETUNE_TIMEOUT, |reply| {
            Kick::ResolveModel {
                occupant,
                spelling,
                reply,
            }
        })?;
    Ok(answer.result)
}
