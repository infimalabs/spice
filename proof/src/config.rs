//! Pure admission of proof policy and machine-local executable locations.

use std::path::{Path, PathBuf};

use serde::de::{Deserializer, Error as _};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};

/// One lexically absolute executable location supplied by local configuration.
///
/// Filesystem shape, executable permission, runtime identity, and process
/// behavior remain the consuming proof boundary's responsibility.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutablePath(PathBuf);

impl ExecutablePath {
    /// Exact platform path admitted from configuration.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for ExecutablePath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl<'de> Deserialize<'de> for ExecutablePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(D::Error::custom(
                "proof executable path must be an absolute platform path",
            ));
        }
        Ok(Self(path))
    }
}

impl Serialize for ExecutablePath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

/// Optional machine-local proof executable locations.
///
/// Proof image identity is selected from the current lockfile's vendor set,
/// never from authored configuration.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    container: Option<ExecutablePath>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runner: Option<ExecutablePath>,
}

impl Config {
    /// Configured Docker or Podman executable location, when supplied.
    #[must_use]
    pub const fn container(&self) -> Option<&ExecutablePath> {
        self.container.as_ref()
    }

    /// Configured native sealed-runner executable location, when supplied.
    #[must_use]
    pub const fn runner(&self) -> Option<&ExecutablePath> {
        self.runner.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn fixture(container: Option<PathBuf>, runner: Option<PathBuf>) -> Self {
        Self {
            container: container.map(ExecutablePath),
            runner: runner.map(ExecutablePath),
        }
    }
}
