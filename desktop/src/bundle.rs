//! The application bundle the window process runs from.
//!
//! macOS identifies a text-input client by the bundle LaunchServices
//! registered for its process, and only an executable that lives inside an
//! `.app` directory on disk gets one: an `Info.plist` linked into the
//! executable sets `NSBundle.mainBundle` but never reaches LaunchServices.
//! Dictation needs that identity. `DictationIM` resolves the client's windows
//! through `NSRunningApplication` and the client bundle identifier before it
//! places the microphone indicator at the insertion point; a process without
//! one sends it into a five-second poll that stalls both sides and ends with
//! the indicator hidden and dictation never started.
//!
//! The shell therefore keeps a minimal bundle in the user's cache directory
//! whose executable is the running appliance, hard-linked where the volume
//! allows and copied otherwise, and runs the window from there. The bundle is
//! also the one place the app's name and icon live: `CFBundleName` is what
//! the menu bar, the Dock, and the application switcher call it, and
//! `CFBundleIconFile` names the mark [`crate::icon`] draws. The bundle is
//! verified on every launch: a changed appliance replaces the executable, and
//! a changed plist or icon is rewritten.

use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::WindowError;

/// The bundle identifier the window presents to the system.
const IDENTIFIER: &str = "dev.spice.app";
/// The bundle directory under the user's caches.
const BUNDLE: &str = "Spice.app";
/// The executable name inside the bundle; `CFBundleExecutable` names it.
const EXECUTABLE: &str = "spice";
/// How many bytes a verification read compares at once.
const CHUNK: usize = 1 << 20;
/// `EXDEV`, the errno a hard link across volumes fails with.
const EXDEV: i32 = 18;

/// The icon file name inside `Contents/Resources`; `CFBundleIconFile` names it.
const ICON: &str = "Spice.icns";

/// The property list naming the bundle; one identity for every appliance.
fn info_plist() -> String {
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
            "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\">\n<dict>\n",
            "\t<key>CFBundleIdentifier</key>\n\t<string>{identifier}</string>\n",
            "\t<key>CFBundleName</key>\n\t<string>Spice</string>\n",
            "\t<key>CFBundleExecutable</key>\n\t<string>{executable}</string>\n",
            "\t<key>CFBundleIconFile</key>\n\t<string>{icon}</string>\n",
            "\t<key>CFBundlePackageType</key>\n\t<string>APPL</string>\n",
            "\t<key>NSHighResolutionCapable</key>\n\t<true/>\n",
            "</dict>\n</plist>\n",
        ),
        identifier = IDENTIFIER,
        executable = EXECUTABLE,
        icon = ICON,
    )
}

/// The path to run `executable` from so the process carries a bundle
/// identity: `executable` itself when it already lives in a bundle, else the
/// shell's cached bundle holding it under `home`'s caches.
pub fn bundled_executable(executable: &Path, home: &Path) -> Result<PathBuf, WindowError> {
    if in_bundle(executable) {
        return Ok(executable.to_path_buf());
    }
    let contents = home
        .join("Library/Caches")
        .join(IDENTIFIER)
        .join(BUNDLE)
        .join("Contents");
    let bundled = contents.join("MacOS").join(EXECUTABLE);
    let icon = crate::icon::icns()?;
    ensure(&contents, executable, &bundled, &icon).map_err(|error| {
        WindowError::Platform(format!(
            "the window bundle at {} could not be prepared: {error}",
            contents.display()
        ))
    })?;
    Ok(bundled)
}

/// Whether `executable` already sits at `*.app/Contents/MacOS/<name>`.
fn in_bundle(executable: &Path) -> bool {
    let mut ancestors = executable.ancestors().skip(1);
    let (Some(macos), Some(contents), Some(bundle)) =
        (ancestors.next(), ancestors.next(), ancestors.next())
    else {
        return false;
    };
    macos.file_name().is_some_and(|name| name == "MacOS")
        && contents.file_name().is_some_and(|name| name == "Contents")
        && bundle
            .extension()
            .is_some_and(|extension| extension == "app")
}

/// Bring the bundle under `contents` to the expected plist, icon, and
/// executable. A changed plist or icon also touches the bundle directory,
/// which is what tells LaunchServices to drop the identity it cached for it.
fn ensure(contents: &Path, executable: &Path, bundled: &Path, icon: &[u8]) -> io::Result<()> {
    fs::create_dir_all(contents.join("MacOS"))?;
    fs::create_dir_all(contents.join("Resources"))?;
    let plist = contents.join("Info.plist");
    let expected = info_plist();
    let mut changed = false;
    if fs::read(&plist).ok().as_deref() != Some(expected.as_bytes()) {
        replace(&plist, |staged| fs::write(staged, &expected))?;
        changed = true;
    }
    let icon_path = contents.join("Resources").join(ICON);
    if fs::read(&icon_path).ok().as_deref() != Some(icon) {
        replace(&icon_path, |staged| fs::write(staged, icon))?;
        changed = true;
    }
    if changed && let Some(bundle) = contents.parent() {
        fs::File::open(bundle)?.set_modified(std::time::SystemTime::now())?;
    }
    let source = fs::metadata(executable)?;
    if let Ok(present) = fs::metadata(bundled)
        && same_file(&source, &present)
    {
        return Ok(());
    }
    if let Err(error) = replace(bundled, |staged| fs::hard_link(executable, staged)) {
        if error.raw_os_error() != Some(EXDEV) {
            return Err(error);
        }
        replace(bundled, |staged| fs::copy(executable, staged).map(|_| ()))?;
    }
    let present = fs::metadata(bundled)?;
    if same_file(&source, &present) || same_bytes(executable, bundled)? {
        return Ok(());
    }
    Err(io::Error::other(
        "the bundled executable does not match the running appliance",
    ))
}

/// Install a file at `path` through a staged sibling and an atomic rename.
fn replace(path: &Path, stage: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<()> {
    let staged = path.with_extension(format!("staged-{}", std::process::id()));
    let _ = fs::remove_file(&staged);
    stage(&staged)?;
    fs::rename(&staged, path).inspect_err(|_| {
        let _ = fs::remove_file(&staged);
    })
}

fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino()
}

/// Whether two files hold identical bytes, compared in chunks.
fn same_bytes(left: &Path, right: &Path) -> io::Result<bool> {
    if fs::metadata(left)?.len() != fs::metadata(right)?.len() {
        return Ok(false);
    }
    let mut left = fs::File::open(left)?;
    let mut right = fs::File::open(right)?;
    let mut left_chunk = vec![0; CHUNK];
    let mut right_chunk = vec![0; CHUNK];
    loop {
        let read = left.read(&mut left_chunk)?;
        if read == 0 {
            return Ok(true);
        }
        right.read_exact(&mut right_chunk[..read])?;
        if left_chunk[..read] != right_chunk[..read] {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::in_bundle;
    use std::path::Path;

    #[test]
    fn bundle_membership_requires_the_full_app_layout() {
        assert!(in_bundle(Path::new("/tmp/Spice.app/Contents/MacOS/spice")));
        assert!(!in_bundle(Path::new("/tmp/Spice.app/MacOS/spice")));
        assert!(!in_bundle(Path::new("/tmp/Spice/Contents/MacOS/spice")));
        assert!(!in_bundle(Path::new("/usr/local/bin/spice")));
    }
}
