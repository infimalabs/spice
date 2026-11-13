//! The native per-user registration facility behind the keeper lifecycle.
//!
//! One trait names exactly what the shared owner needs. Relaunch and lifecycle
//! policy are exercised against recording implementations; each host adapter
//! renders its own definition and binds these operations to its native manager.

use std::io;
#[cfg(any(windows, target_os = "linux", target_os = "macos", all(test, unix)))]
use std::path::Path;

#[cfg(any(windows, target_os = "linux", target_os = "macos", all(test, unix)))]
/// Hex digits of the repository digest that identify one native registration.
const REPOSITORY_KEY_HEX: usize = 16;

/// Stable native-registration key for one exact Git common directory.
#[cfg(any(windows, target_os = "linux", target_os = "macos", all(test, unix)))]
pub(super) fn repository_key(common_dir: &Path) -> String {
    let digest = blake3::hash(common_dir.to_string_lossy().as_bytes()).to_hex();
    digest[..REPOSITORY_KEY_HEX].to_owned()
}

#[cfg(any(target_os = "macos", test))]
pub(super) mod launchd;
#[cfg(any(target_os = "linux", all(test, unix)))]
pub(super) mod linux;
#[cfg(windows)]
pub(super) mod windows;

/// XML text and attribute escaping for rendered native definitions.
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn xml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// One repository's native registration and its complete manager definition.
pub(crate) struct Registration {
    /// Stable native registration identity.
    pub(crate) name: String,
    /// Definition rendered in the native manager's format.
    pub(crate) definition: String,
}

/// What the native manager currently holds for one registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Status {
    /// Whether the manager will start the registration on its trigger or on `run`.
    pub(crate) enabled: bool,
    /// Whether an instance of the task is executing now.
    pub(crate) running: bool,
}

/// One native per-user registration facility.
pub(crate) trait Manager {
    /// Create or replace `registration` from `definition`; the result is enabled.
    fn register(&mut self, registration: &str, definition: &str) -> io::Result<()>;
    /// The registration's status, or `None` when it is absent.
    fn status(&mut self, registration: &str) -> io::Result<Option<Status>>;
    /// Allow or forbid the manager to start the registration.
    fn set_enabled(&mut self, registration: &str, enabled: bool) -> io::Result<()>;
    /// Start an instance now; an instance already running is left alone.
    fn run(&mut self, registration: &str) -> io::Result<()>;
    /// End every running instance.
    fn stop(&mut self, registration: &str) -> io::Result<()>;
    /// Remove the registration; an absent registration is already removed.
    fn delete(&mut self, registration: &str) -> io::Result<()>;
}
