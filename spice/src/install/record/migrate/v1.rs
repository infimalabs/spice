use super::*;

pub(super) const FROM: u16 = 1;

/// Move the public schema-one receipt binding from state-root-relative to the
/// same absolute path under the Git-common root. All other retained authority
/// stays in the shared model and receives only the new schema identity.
pub(super) fn cross(
    repository: &Repository,
    record: &mut InstallRecord,
) -> Result<(), InstallError> {
    if !record.settled() {
        return Err(foreign(repository, FROM));
    }
    record.validate_with_content_at_version(true, FROM)?;
    if record.repository.common_git != repository.common_dir() {
        record.rebase_repository(repository)?;
    }
    record.repair_mount_device(repository)?;
    record
        .plan
        .validate_schema_one_repository_binding(repository, record.discards_done())?;

    record.version = FROM + 1;
    record.plan.version = FROM + 1;
    record.plan.record = bind_record_destination(repository)?;
    record.plan_digest = record.plan.digest()?;
    Ok(())
}
