//! Repository command mounts merged into the generated CLI contract tree.

use std::collections::BTreeSet;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use clap::ArgMatches;

use super::{MOUNT_ARGUMENT, command_from, contracts, selected_matches, selected_path};

/// One admitted repository command mount.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mount {
    path: Vec<String>,
    argv: Vec<String>,
    repository: PathBuf,
}

impl Mount {
    /// Dotted CLI path as individual generated command segments.
    #[must_use]
    pub fn path(&self) -> &[String] {
        &self.path
    }

    /// Exact fixed argv declared by the repository.
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// Canonical worktree root used as the child's working directory.
    #[must_use]
    pub fn repository(&self) -> &Path {
        &self.repository
    }
}

/// Deterministically admitted mounts for one invocation.
#[derive(Clone, Debug, Default)]
pub struct MountRegistry {
    mounts: Vec<Mount>,
    configuration: Option<crate::config::RepositoryCapture>,
}

impl PartialEq for MountRegistry {
    fn eq(&self, other: &Self) -> bool {
        self.mounts == other.mounts
    }
}

impl Eq for MountRegistry {}

impl MountRegistry {
    /// Load the sole typed repository configuration and validate its CLI paths.
    ///
    /// An ordinary directory outside Git has no repository mounts. A malformed
    /// repository shape or configuration refuses instead of silently becoming
    /// an empty registry.
    ///
    /// # Errors
    ///
    /// Returns a repository, configuration, or generated-path collision error.
    pub fn load(start: &Path) -> Result<Self, MountLoadError> {
        let repository = match crate::repository::Repository::discover(start) {
            Ok(repository) => repository,
            Err(crate::repository::ResolveError::NoRepository { .. }) => {
                return Ok(Self::default());
            }
            Err(error) => return Err(MountLoadError::Repository(error)),
        };
        let loaded = crate::config::capture_repository(&repository, &[])
            .map_err(MountLoadError::Configuration)?;
        let mut registry = Self::discover(
            repository.worktree_root(),
            loaded.resolved().root().commands(),
        )
        .map_err(MountLoadError::Registry)?;
        registry.configuration = Some(crate::config::RepositoryCapture::new(repository, loaded));
        Ok(registry)
    }

    /// Validate an already admitted typed mount table against generated paths.
    ///
    /// # Errors
    ///
    /// Returns every exact collision in deterministic command-path order.
    pub fn discover(
        repository: &Path,
        commands: &crate::config::CommandMounts,
    ) -> Result<Self, MountRegistryError> {
        let generated = generated_paths();
        let mut collisions = Vec::new();
        let mut mounts = Vec::new();
        for (name, argv) in commands.iter() {
            let path = name.split('.').map(str::to_owned).collect::<Vec<_>>();
            if generated.contains(&path) {
                collisions.push(path);
            } else {
                mounts.push(Mount {
                    path,
                    argv: argv.to_vec(),
                    repository: repository.to_path_buf(),
                });
            }
        }
        if collisions.is_empty() {
            Ok(Self {
                mounts,
                configuration: None,
            })
        } else {
            Err(MountRegistryError { collisions })
        }
    }

    /// Configuration generation already parsed to admit this invocation's mounts.
    #[must_use]
    pub fn configuration(&self) -> Option<&crate::config::RepositoryCapture> {
        self.configuration.as_ref()
    }

    /// Build the parser by merging mounts into the one compiled contract tree.
    #[must_use]
    pub fn command(&self) -> clap::Command {
        command_from(contracts(), &self.mounts)
    }

    /// Resolve the selected mount and retain every trailing argument verbatim.
    #[must_use]
    pub fn selected<'a>(&'a self, matches: &ArgMatches) -> Option<MountSelection<'a>> {
        let path = selected_path(matches);
        let mount = self.mounts.iter().find(|mount| {
            mount
                .path
                .iter()
                .map(String::as_str)
                .eq(path.iter().copied())
        })?;
        let arguments = selected_matches(matches)
            .get_many::<OsString>(MOUNT_ARGUMENT)
            .map_or_else(Vec::new, |values| values.cloned().collect());
        Some(MountSelection { mount, arguments })
    }

    #[cfg(test)]
    pub(super) fn mounts(&self) -> &[Mount] {
        &self.mounts
    }
}

/// One selected mount and the caller arguments appended to its fixed argv.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountSelection<'a> {
    mount: &'a Mount,
    arguments: Vec<OsString>,
}

impl MountSelection<'_> {
    /// Selected admitted declaration.
    #[must_use]
    pub const fn mount(&self) -> &Mount {
        self.mount
    }

    /// Exact caller arguments following the mounted command path.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
}

fn generated_paths() -> BTreeSet<Vec<String>> {
    let mut paths = BTreeSet::new();
    for contract in contracts() {
        for length in 1..=contract.path.len() {
            paths.insert(
                contract.path[..length]
                    .iter()
                    .map(|segment| (*segment).to_owned())
                    .collect(),
            );
        }
    }
    paths
}

/// Failure loading repository mounts before parser generation.
#[derive(Debug)]
pub enum MountLoadError {
    /// The enclosing Git shape is present but malformed or inaccessible.
    Repository(crate::repository::ResolveError),
    /// The sole typed repository configuration refused.
    Configuration(crate::config::LoadError),
    /// One or more mounts collide with generated command paths.
    Registry(MountRegistryError),
}

impl MountLoadError {
    /// Whether this refusal names authority the command-mount rim owns.
    ///
    /// A generated built-in still owns syntax and domain errors in unrelated
    /// configuration. Mount collisions and structured `commands.*` issues must
    /// instead refuse before that built-in dispatches.
    #[must_use]
    pub fn affects_mounts(&self) -> bool {
        match self {
            Self::Registry(_) => true,
            Self::Repository(_) => false,
            Self::Configuration(error) => error.resolution().is_some_and(|resolution| {
                resolution.issues().iter().any(|issue| {
                    issue.path().is_some_and(|path| {
                        let path = path.to_string();
                        path == "commands" || path.starts_with("commands.")
                    })
                })
            }),
        }
    }
}

impl fmt::Display for MountLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(error) => error.fmt(formatter),
            Self::Configuration(error) => formatter.write_str(&error.diagnostic()),
            Self::Registry(error) => error.fmt(formatter),
        }
    }
}

impl Error for MountLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Repository(error) => Some(error),
            Self::Configuration(error) => Some(error),
            Self::Registry(error) => Some(error),
        }
    }
}

/// Deterministic set of mounts colliding with the generated CLI tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountRegistryError {
    collisions: Vec<Vec<String>>,
}

impl MountRegistryError {
    /// Exact colliding paths in deterministic order.
    #[must_use]
    pub fn collisions(&self) -> &[Vec<String>] {
        &self.collisions
    }
}

impl fmt::Display for MountRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, path) in self.collisions.iter().enumerate() {
            if index > 0 {
                formatter.write_str("; ")?;
            }
            write!(
                formatter,
                "[commands] entry {:?} shadows generated spice command path {:?}",
                path.join("."),
                format!("spice {}", path.join(" "))
            )?;
        }
        Ok(())
    }
}

impl Error for MountRegistryError {}

#[cfg(test)]
mod tests;
