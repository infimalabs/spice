//! Owner-only write permission for gate-owned trees.

use std::fs;
use std::io;
#[cfg(target_family = "unix")]
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

/// Leave `path` writable by its owner: mode `0o700` for a directory or `0o600`
/// for a file on Unix, and elsewhere a cleared read-only attribute.
pub(crate) fn set_owner_writable(path: &Path, directory: bool) -> io::Result<()> {
    #[cfg(target_family = "unix")]
    {
        let mode = if directory { 0o700 } else { 0o600 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }
    #[cfg(not(target_family = "unix"))]
    {
        let _ = directory;
        let mut permissions = fs::metadata(path)?.permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "this arm compiles only off Unix, where the read-only attribute is the sole write permission; the lint's world-writable hazard is the Unix mode the other arm sets explicitly"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)
    }
}
