use wire::WireVersion;
use wire::manifest::{JOURNAL_BACKUP_VERSION, JournalBackupManifest};

use super::BackupVerifyError;
use crate::SCHEMA_VERSION;

/// Admits only a manifest the current build would write: the current bundle
/// version, the current wire, and the current journal schema.
pub(super) fn admit_manifest(manifest: &JournalBackupManifest) -> Result<(), BackupVerifyError> {
    if manifest.version != JOURNAL_BACKUP_VERSION {
        Err(BackupVerifyError::UnsupportedVersion {
            found: manifest.version,
            current: JOURNAL_BACKUP_VERSION,
        })
    } else if !manifest.wire.is_current() {
        Err(BackupVerifyError::UnsupportedWire {
            found: manifest.wire,
            current: WireVersion::CURRENT,
        })
    } else if manifest.journal_schema > SCHEMA_VERSION {
        Err(BackupVerifyError::ForwardSchema {
            found: manifest.journal_schema,
            current: SCHEMA_VERSION,
        })
    } else if manifest.journal_schema != SCHEMA_VERSION {
        Err(BackupVerifyError::SchemaMismatch {
            manifest: manifest.journal_schema,
            authority: SCHEMA_VERSION,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use wire::manifest::JournalBackupMember;
    use wire::{Digest, Domain};

    use super::*;

    fn manifest(version: u16) -> JournalBackupManifest {
        JournalBackupManifest {
            version,
            wire: WireVersion::CURRENT,
            journal_schema: SCHEMA_VERSION,
            authority: "spicejournal.sqlite3".to_owned(),
            members: vec![JournalBackupMember {
                path: "spicejournal.sqlite3".to_owned(),
                mode: 0o600,
                bytes: 0,
                digest: Digest::compute(Domain::JournalBackupMember, &[]),
            }],
        }
    }

    #[test]
    fn only_the_current_bundle_version_is_admitted() {
        assert!(admit_manifest(&manifest(JOURNAL_BACKUP_VERSION)).is_ok());
        for version in [0, JOURNAL_BACKUP_VERSION - 1, JOURNAL_BACKUP_VERSION + 1] {
            assert!(matches!(
                admit_manifest(&manifest(version)),
                Err(BackupVerifyError::UnsupportedVersion { found, current })
                    if found == version && current == JOURNAL_BACKUP_VERSION
            ));
        }
    }
}
