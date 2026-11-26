//! Forward-only install receipt schema crossings retained for this product major.

use super::*;

mod v1;
mod v2;
type Crossing = fn(&Repository, &mut InstallRecord) -> Result<(), InstallError>;

const CROSSINGS: &[(u16, Crossing)] = &[(v1::FROM, v1::cross), (v2::FROM, v2::cross)];

/// Whether `version` has one uninterrupted path into this executable's schema.
pub(super) fn supports(mut version: u16) -> bool {
    if version > RECORD_VERSION {
        return false;
    }
    while version < RECORD_VERSION {
        let Some((_, _)) = CROSSINGS.iter().find(|(from, _)| *from == version) else {
            return false;
        };
        version += 1;
    }
    true
}

/// Advance a decoded canonical receipt through every registered crossing.
///
/// The returned digest names the exact historical plan on disk. Callers use it
/// as the refresh witness while all later planning runs only against the final
/// current-schema model.
pub(super) fn forward(
    repository: &Repository,
    record: &mut InstallRecord,
) -> Result<Option<Digest>, InstallError> {
    if record.version == RECORD_VERSION {
        return Ok(None);
    }
    let original_digest = record.plan_digest.clone();
    while record.version < RECORD_VERSION {
        let from = record.version;
        let Some((_, cross)) = CROSSINGS.iter().find(|(version, _)| *version == from) else {
            return Err(foreign(repository, from));
        };
        cross(repository, record)?;
        if record.version != from + 1 {
            return Err(InstallError::InvalidRecord(format!(
                "install migration from schema {from} did not advance exactly once"
            )));
        }
    }
    if record.version != RECORD_VERSION {
        return Err(foreign(repository, record.version));
    }
    Ok(Some(original_digest))
}

pub(super) fn foreign(repository: &Repository, version: u16) -> InstallError {
    InstallError::ForeignRecord {
        path: record_path(repository).unwrap_or_else(|_| repository.state_root().join(RECORD_PATH)),
        version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_crossings_form_one_uninterrupted_forward_chain() {
        assert!(supports(RECORD_VERSION));
        let first = CROSSINGS.first().expect("at least one retained crossing").0;
        assert_eq!(first, 1);
        assert!(supports(first));
        for pair in CROSSINGS.windows(2) {
            assert_eq!(pair[1].0, pair[0].0 + 1);
        }
        assert_eq!(
            CROSSINGS.last().expect("a final crossing").0 + 1,
            RECORD_VERSION
        );
        assert!(!supports(RECORD_VERSION + 1));
    }
}
