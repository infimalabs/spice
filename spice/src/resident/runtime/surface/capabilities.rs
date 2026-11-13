use super::*;

/// The generation's capabilities: `owner` authenticates the bootstrap
/// principal, each admitted principal's issued capability its own principal,
/// and each seated lane's capability that lane, deciding authority by the
/// journal's current principal role heads.
pub(super) fn build(
    bus: &Bus,
    owner: CanonicalUuid,
    residency: &ResidencyHead,
) -> CommandResult<serve::axum::Capabilities> {
    let invalid = |error: serve::axum::CapabilityError| {
        environment_fault("serve::capability-invalid", &error.to_string())
    };
    let roles = crate::resident::principal_surface::roles(bus)?;
    let principal = roles.bootstrap().ok_or_else(|| {
        environment_fault(
            "principal::bootstrap-unborn",
            "the bootstrap principal is born before the surface serves",
        )
    })?;
    let capabilities = serve::axum::Capabilities::owned_by(owner, principal).map_err(invalid)?;
    capabilities.publish(roles).map_err(invalid)?;
    for lane in &residency.lanes {
        for capability in &lane.capabilities {
            capabilities
                .insert(*capability, Actor::Lane(lane.lane))
                .map_err(invalid)?;
        }
    }
    Ok(capabilities)
}
