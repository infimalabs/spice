//! Admission of the target standard libraries the tracked toolchain declares.
//!
//! `rust-toolchain.toml` names the exact release and every cross target the
//! repository builds for. Cargo runs the admitted `rustc` directly rather than
//! through a rustup proxy, so nothing else installs those targets: admission
//! verifies each declared `rust-std` inside the admitted compiler's own sysroot
//! and materializes a missing one through the bootstrap rustup, never through
//! a different toolchain.
//!
//! Provenance is what rustup itself recorded in that sysroot: the channel
//! manifest names the release and which targets it ships, the configuration
//! lists installed components, and each component manifest lists its files.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::IsolationError;

#[cfg(test)]
mod tests;

/// The rustup operations target admission needs, bound to one Rustup home.
pub(super) trait Rustup {
    /// The `rustc` rustup selects for `channel`.
    fn which_rustc(&self, channel: &str) -> Result<PathBuf, IsolationError>;

    /// Install `targets`' standard libraries into `channel`.
    fn add_targets(&self, channel: &str, targets: &[&str]) -> Result<(), IsolationError>;
}

/// Admit every declared target's standard library for the admitted `rustc`.
///
/// `rustc` must be canonical, as [`super::NativeToolchain`] admits it.
pub(super) fn admit(
    channel: &str,
    targets: &[String],
    rustc: &Path,
    rustup: &impl Rustup,
) -> Result<(), IsolationError> {
    if targets.is_empty() {
        return Ok(());
    }
    for target in targets {
        require_target_name(target)?;
    }
    let sysroot = rustc.parent().and_then(Path::parent).ok_or_else(|| {
        IsolationError::new(
            "cargo-isolation::tracked-target-sysroot-unavailable",
            format!("admitted rustc {rustc:?} has no enclosing sysroot"),
        )
    })?;
    let rustlib = sysroot.join("lib/rustlib");
    let manifest = ChannelManifest::read(&rustlib)?;
    manifest.require_channel(channel, &rustlib)?;
    let missing = missing_targets(sysroot, &rustlib, targets)?;
    if missing.is_empty() {
        return Ok(());
    }
    for target in &missing {
        manifest.require_available(target)?;
    }
    let selected = rustup.which_rustc(channel)?;
    let selected = fs::canonicalize(&selected).map_err(|error| {
        IsolationError::io(
            "cargo-isolation::tracked-target-foreign-toolchain",
            &selected,
            error,
        )
    })?;
    if selected != rustc {
        return Err(IsolationError::new(
            "cargo-isolation::tracked-target-foreign-toolchain",
            format!(
                "rustup selects {selected:?} for channel {channel:?}, not the admitted {rustc:?}; \
                 refusing to install {missing:?} into another toolchain"
            ),
        ));
    }
    rustup.add_targets(channel, &missing)?;
    let unresolved = missing_targets(sysroot, &rustlib, targets)?;
    if unresolved.is_empty() {
        Ok(())
    } else {
        Err(IsolationError::new(
            "cargo-isolation::tracked-target-unavailable",
            format!("rustup added {missing:?} but {unresolved:?} is still absent from {sysroot:?}"),
        ))
    }
}

/// A target name becomes a path component, so it must be one plain token.
fn require_target_name(target: &str) -> Result<(), IsolationError> {
    let plain = !target.is_empty()
        && !target.starts_with(['.', '-'])
        && target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if plain {
        Ok(())
    } else {
        Err(IsolationError::new(
            "cargo-isolation::tracked-toolchain-invalid",
            format!("tracked target {target:?} is not a target triple"),
        ))
    }
}

/// Declared targets rustup has not installed; an installed one must be whole.
fn missing_targets<'a>(
    sysroot: &Path,
    rustlib: &Path,
    targets: &'a [String],
) -> Result<Vec<&'a str>, IsolationError> {
    let installed = InstalledComponents::read(rustlib)?;
    let mut missing = Vec::new();
    for target in targets {
        if installed.standard_libraries.contains(target.as_str()) {
            require_complete(sysroot, rustlib, target)?;
        } else {
            missing.push(target.as_str());
        }
    }
    Ok(missing)
}

/// Every file rustup recorded for `target`'s standard library is present.
fn require_complete(sysroot: &Path, rustlib: &Path, target: &str) -> Result<(), IsolationError> {
    let manifest = rustlib.join(format!("manifest-rust-std-{target}"));
    let text = fs::read_to_string(&manifest).map_err(|error| {
        IsolationError::io("cargo-isolation::tracked-target-corrupt", &manifest, error)
    })?;
    for line in text.lines().filter(|line| !line.is_empty()) {
        let relative = line
            .strip_prefix("file:")
            .or_else(|| line.strip_prefix("dir:"))
            .filter(|relative| {
                Path::new(relative)
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
            })
            .ok_or_else(|| {
                IsolationError::new(
                    "cargo-isolation::tracked-target-corrupt",
                    format!("{manifest:?} records an unrecognized entry {line:?}"),
                )
            })?;
        let path = sysroot.join(relative);
        if fs::symlink_metadata(&path).is_err() {
            return Err(IsolationError::new(
                "cargo-isolation::tracked-target-corrupt",
                format!("{target} standard library is missing {path:?} recorded by {manifest:?}"),
            ));
        }
    }
    Ok(())
}

struct InstalledComponents {
    standard_libraries: BTreeSet<String>,
}

#[derive(Deserialize)]
struct ComponentConfiguration {
    components: Vec<InstalledComponent>,
}

#[derive(Deserialize)]
struct InstalledComponent {
    pkg: String,
    target: String,
}

impl InstalledComponents {
    fn read(rustlib: &Path) -> Result<Self, IsolationError> {
        let path = rustlib.join("multirust-config.toml");
        let text = fs::read_to_string(&path).map_err(|error| {
            IsolationError::io(
                "cargo-isolation::tracked-target-provenance-unavailable",
                &path,
                error,
            )
        })?;
        let configuration = toml::from_str::<ComponentConfiguration>(&text).map_err(|error| {
            IsolationError::new(
                "cargo-isolation::tracked-target-corrupt",
                format!("{path:?} is invalid: {error}"),
            )
        })?;
        Ok(Self {
            standard_libraries: configuration
                .components
                .into_iter()
                .filter(|component| component.pkg == "rust-std")
                .map(|component| component.target)
                .collect(),
        })
    }
}

#[derive(Deserialize)]
struct ChannelManifest {
    pkg: ChannelPackages,
}

#[derive(Deserialize)]
struct ChannelPackages {
    #[serde(rename = "rust-std")]
    standard_library: ChannelPackage,
}

#[derive(Deserialize)]
struct ChannelPackage {
    version: String,
    target: std::collections::BTreeMap<String, ChannelTarget>,
}

#[derive(Deserialize)]
struct ChannelTarget {
    available: bool,
}

impl ChannelManifest {
    fn read(rustlib: &Path) -> Result<Self, IsolationError> {
        let path = rustlib.join("multirust-channel-manifest.toml");
        let text = fs::read_to_string(&path).map_err(|error| {
            IsolationError::io(
                "cargo-isolation::tracked-target-provenance-unavailable",
                &path,
                error,
            )
        })?;
        toml::from_str(&text).map_err(|error| {
            IsolationError::new(
                "cargo-isolation::tracked-target-provenance-unavailable",
                format!("{path:?} is invalid: {error}"),
            )
        })
    }

    /// The sysroot's standard libraries come from exactly the declared release.
    fn require_channel(&self, channel: &str, rustlib: &Path) -> Result<(), IsolationError> {
        let version = &self.pkg.standard_library.version;
        if version.split_whitespace().next() == Some(channel) {
            Ok(())
        } else {
            Err(IsolationError::new(
                "cargo-isolation::tracked-target-wrong-channel",
                format!(
                    "{rustlib:?} ships rust-std {version:?}, not the tracked release {channel:?}"
                ),
            ))
        }
    }

    fn require_available(&self, target: &str) -> Result<(), IsolationError> {
        match self.pkg.standard_library.target.get(target) {
            Some(ChannelTarget { available: true }) => Ok(()),
            _ => Err(IsolationError::new(
                "cargo-isolation::tracked-target-unavailable",
                format!("the tracked release ships no rust-std for {target}"),
            )),
        }
    }
}
