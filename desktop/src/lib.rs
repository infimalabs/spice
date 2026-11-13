//! The native window `spice app` opens on the console.
//!
//! The shell is a window over the resident's served app and owns nothing
//! else: the URL it loads has already passed the discovery admission a
//! browser launch applies, and the resident, journal, and lanes never learn
//! which surface is looking at them. It runs on the calling thread, which
//! must be the process main thread, and returns once the operator closes the
//! window.

use std::fmt;

/// Why no window could be shown.
#[derive(Debug)]
pub enum WindowError {
    /// This build has no native webview; the system browser remains the way in.
    Unsupported,
    /// The window system or webview refused to start.
    Platform(String),
}

impl fmt::Display for WindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => formatter.write_str(
                "this build opens no native window; use `spice up --open` for the system browser",
            ),
            Self::Platform(detail) => {
                write!(formatter, "the native window could not open: {detail}")
            }
        }
    }
}

impl std::error::Error for WindowError {}

#[cfg(target_os = "macos")]
mod bundle;
#[cfg(any(target_os = "macos", windows))]
mod header;
#[cfg(target_os = "macos")]
mod icon;
#[cfg(any(target_os = "macos", windows))]
mod link;
#[cfg(windows)]
mod webview2;
#[cfg(target_os = "macos")]
mod window;

#[cfg(target_os = "macos")]
pub use bundle::bundled_executable;

/// The window's opening size; the operator resizes from there.
#[cfg(any(target_os = "macos", windows))]
const INITIAL_SIZE: tao::dpi::LogicalSize<f64> = tao::dpi::LogicalSize::new(1280.0, 860.0);

/// Show `url` in one native window titled `title` and block until it closes.
///
/// `key` names the window frame remembered between launches; one repository
/// keeps one key. Only macOS remembers frames.
///
/// The window stays the console's only webview: a link to an http(s) URL of
/// another origin, and any new-window request for an http(s) URL, leave
/// through `external` for the system browser instead, while the console's
/// own routes stay inside. `external` receives the exact URL the page named
/// and never a non-web URL.
///
/// `opened` runs once, after the window and its webview exist and before the
/// operator can close them; a window that never starts never runs it.
#[cfg(target_os = "macos")]
pub fn show(
    title: &str,
    key: &str,
    url: &str,
    external: impl Fn(&str) + Send + Sync + 'static,
    opened: impl FnOnce(),
) -> Result<(), WindowError> {
    window::show(title, key, url, external, opened)
}

/// Show `url` in one native window titled `title` and block until it closes.
#[cfg(windows)]
pub fn show(
    title: &str,
    _key: &str,
    url: &str,
    external: impl Fn(&str) + Send + Sync + 'static,
    opened: impl FnOnce(),
) -> Result<(), WindowError> {
    webview2::show(title, url, external, opened)
}

/// Show `url` in one native window titled `title` and block until it closes.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn show(
    _title: &str,
    _key: &str,
    _url: &str,
    _external: impl Fn(&str) + Send + Sync + 'static,
    _opened: impl FnOnce(),
) -> Result<(), WindowError> {
    Err(WindowError::Unsupported)
}
