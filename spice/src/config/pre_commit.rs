//! Typed native pre-commit policy declarations.

use std::path::Path;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use wire::ConfigSource;

use super::{CaptureGeneration, Loaded, nonempty_argv, nonempty_string, unique_names};

pub(super) const EMPTY_PRE_COMMIT_MESSAGE: &str =
    "policy.pre_commit_builtins=false requires at least one policy.pre_commit command";

/// Bind every executable declaration to the exact tracked capture that supplied it.
///
/// The returned surface is inert: constructing it neither checks trust nor
/// executes a command. Its path, original document bytes, capture generation,
/// and authored command order are the complete inputs a later dispatcher needs
/// to form an exact executable-configuration capability and reject movement.
#[must_use]
pub fn pre_commit_declarations(loaded: &Loaded) -> Option<PreCommitDeclarations<'_>> {
    let commands = loaded.resolved().root().policy().pre_commit();
    if commands.is_empty() {
        return None;
    }
    Some(PreCommitDeclarations {
        configuration: loaded.locations().tracked(),
        document: loaded
            .tracked()
            .expect("pre-commit commands can only originate in tracked configuration"),
        generation: loaded.generation(),
        commands,
    })
}

/// Exact captured authority for an ordered executable pre-commit list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreCommitDeclarations<'a> {
    configuration: &'a Path,
    document: &'a str,
    generation: &'a CaptureGeneration,
    commands: &'a [PreCommitStep],
}

impl<'a> PreCommitDeclarations<'a> {
    /// Policy layer that is allowed to declare executable pre-commit commands.
    #[must_use]
    pub const fn source(&self) -> ConfigSource {
        ConfigSource::Tracked
    }

    /// Exact tracked configuration path presented in trust diagnostics.
    #[must_use]
    pub const fn configuration(&self) -> &'a Path {
        self.configuration
    }

    /// Exact captured document bytes used as executable capability material.
    #[must_use]
    pub const fn document(&self) -> &'a str {
        self.document
    }

    /// Filesystem generation paired with the captured capability material.
    #[must_use]
    pub const fn generation(&self) -> &'a CaptureGeneration {
        self.generation
    }

    /// Executable declarations in authored order, with no sole-command limit.
    #[must_use]
    pub const fn commands(&self) -> &'a [PreCommitStep] {
        self.commands
    }
}

/// One native pre-commit command admitted by tracked policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreCommitStep {
    #[serde(deserialize_with = "nonempty_string")]
    label: String,
    #[serde(deserialize_with = "nonempty_argv")]
    run: Vec<String>,
    #[serde(default, skip_serializing_if = "PreCommitScopes::is_empty")]
    scopes: PreCommitScopes,
}

impl PreCommitStep {
    /// Human-readable identity shown by the native gate.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Exact executable and arguments, retained in authored order.
    #[must_use]
    pub fn run(&self) -> &[String] {
        &self.run
    }

    /// Optional path selection; an empty selection means every commit.
    #[must_use]
    pub const fn scopes(&self) -> &PreCommitScopes {
        &self.scopes
    }
}

/// Path selection carried by a native pre-commit command.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PreCommitScopes {
    #[serde(deserialize_with = "path_scopes")]
    paths: Vec<String>,
}

impl PreCommitScopes {
    /// Authored path selectors, empty when the command is unscoped.
    #[must_use]
    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

fn path_scopes<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let paths = unique_names(deserializer)?;
    for path in &paths {
        gate::validate_tracked_path_selector(path).map_err(D::Error::custom)?;
    }
    Ok(paths)
}
