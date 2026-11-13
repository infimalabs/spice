//! Product and build identity captured before the app surface binds.

use super::*;

pub(super) fn app_version_from_journal(
    app: &serve::canvas::CanvasAssets,
    journal: &journal::Journal,
) -> CommandResult<wire::Version> {
    let schema = journal
        .schema_version()
        .map_err(|error| store_fault("journal::schema-unreadable", &error.to_string()))?;
    app_version(app, schema)
}

pub(crate) fn app_version(
    app: &serve::canvas::CanvasAssets,
    journal: u32,
) -> CommandResult<wire::Version> {
    let running = spice::version::running().map_err(Outcome::Fault)?;
    let generation = app.generation().map_err(|error| {
        environment_fault("serve::app-generation-unavailable", &error.to_string())
    })?;
    Ok(wire::Version {
        product: running.product.clone(),
        binary: running.binary.clone(),
        wire: wire::WireVersion::CURRENT,
        journal,
        app_protocol: wire::AppProtocolVersion::CURRENT,
        app: generation,
    })
}
