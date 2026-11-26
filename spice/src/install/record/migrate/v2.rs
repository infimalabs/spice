use super::*;

pub(super) const FROM: u16 = 2;

pub(super) fn cross(
    repository: &Repository,
    record: &mut InstallRecord,
) -> Result<(), InstallError> {
    if !record.settled() {
        return Err(foreign(repository, FROM));
    }
    record.validate_with_content_at_version(true, FROM)?;

    record.version = FROM + 1;
    record.plan.version = FROM + 1;
    record.plan_digest = record.plan.digest()?;
    Ok(())
}
