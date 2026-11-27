//! The Cargo settings every release candidate build compiles under.
//!
//! They live in `dist/release/candidate.toml` rather than here because a
//! hosted job compiles xtask itself before any xtask can answer: that bootstrap
//! reads the same table, so its units are the candidate's units and the
//! candidate build reuses them instead of compiling the workspace again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use serde::Deserialize;

const SETTINGS: &str = ::std::include_str!("../../../dist/release/candidate.toml");

static CANDIDATE: LazyLock<CandidateBuild> = LazyLock::new(|| {
    toml::from_str(SETTINGS).expect("dist/release/candidate.toml is a candidate build table")
});

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CandidateBuild {
    target_dir: String,
    build_dir: String,
    rustflags: String,
    debug_config: String,
    environment: BTreeMap<String, String>,
}

/// The one candidate build definition.
pub(super) fn candidate() -> &'static CandidateBuild {
    &CANDIDATE
}

impl CandidateBuild {
    /// Cargo's `--target-dir` below `build_root`.
    pub(super) fn target_dir(&self, build_root: &Path) -> PathBuf {
        build_root.join(&self.target_dir)
    }

    /// Cargo's build directory below `build_root`.
    pub(super) fn build_dir(&self, build_root: &Path) -> PathBuf {
        build_root.join(&self.build_dir)
    }

    /// `CARGO_ENCODED_RUSTFLAGS` for a workspace rooted at `workspace`.
    pub(super) fn rustflags(&self, workspace: &str) -> String {
        self.rustflags.replace("{workspace}", workspace)
    }

    /// The `--config` override for a target that declares a debug artifact.
    pub(super) fn debug_config(&self) -> &str {
        &self.debug_config
    }

    /// The variables every candidate build sets, by name.
    pub(super) fn environment(&self) -> impl Iterator<Item = (&str, &str)> {
        self.environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }
}
