//! Stable compiled gate identities and the configuration layers that feed them.

use std::fmt;

use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize};

/// One stable compiled gate identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[repr(u8)]
pub enum GateId {
    #[serde(rename = "merge-integrity")]
    MergeIntegrity,
    #[serde(rename = "planning-phase")]
    PlanningPhase,
    #[serde(rename = "repo-shape")]
    RepoShape,
    #[serde(rename = "staging")]
    Staging,
    #[serde(rename = "repo-docs")]
    RepoDocs,
    #[serde(rename = "config-key-validity")]
    ConfigKeyValidity,
    #[serde(rename = "config-false-disable")]
    ConfigFalseDisable,
    #[serde(rename = "config-tracked-trust")]
    ConfigTrackedTrust,
    #[serde(rename = "formatters")]
    Formatters,
    #[serde(rename = "local-paths")]
    LocalPaths,
    #[serde(rename = "taste")]
    Taste,
    #[serde(rename = "rust-typecheck")]
    RustTypecheck,
    #[serde(rename = "app-web-typecheck")]
    AppWebTypecheck,
    #[serde(rename = "javascript-unused")]
    JavascriptUnused,
    #[serde(rename = "csharp-unused-candidates")]
    CsharpUnusedCandidates,
    #[serde(rename = "python-typecheck")]
    PythonTypecheck,
    #[serde(rename = "env-policy")]
    EnvPolicy,
    #[serde(rename = "env-name-ledger")]
    EnvNameLedger,
    #[serde(rename = "file-shape")]
    FileShape,
    #[serde(rename = "complexity")]
    Complexity,
    #[serde(rename = "magic-numbers")]
    MagicNumbers,
    #[serde(rename = "markdown-links")]
    MarkdownLinks,
    #[serde(rename = "reachability")]
    Reachability,
    #[serde(rename = "symbol-reachability")]
    SymbolReachability,
    #[serde(rename = "python-unused")]
    PythonUnused,
    #[serde(rename = "assertion-free-tests")]
    AssertionFreeTests,
    #[serde(rename = "private-internals")]
    PrivateInternals,
}

impl GateId {
    /// Every identity executed by the synchronous canonical battery.
    pub const BATTERY: [Self; 27] = [
        Self::MergeIntegrity,
        Self::PlanningPhase,
        Self::RepoShape,
        Self::Staging,
        Self::RepoDocs,
        Self::ConfigKeyValidity,
        Self::ConfigFalseDisable,
        Self::ConfigTrackedTrust,
        Self::Formatters,
        Self::LocalPaths,
        Self::Taste,
        Self::RustTypecheck,
        Self::AppWebTypecheck,
        Self::JavascriptUnused,
        Self::CsharpUnusedCandidates,
        Self::PythonTypecheck,
        Self::EnvPolicy,
        Self::EnvNameLedger,
        Self::FileShape,
        Self::Complexity,
        Self::MagicNumbers,
        Self::MarkdownLinks,
        Self::Reachability,
        Self::SymbolReachability,
        Self::PythonUnused,
        Self::AssertionFreeTests,
        Self::PrivateInternals,
    ];

    /// Every registered instrument, in stable report order.
    pub const ALL: [Self; 27] = [
        Self::MergeIntegrity,
        Self::PlanningPhase,
        Self::RepoShape,
        Self::Staging,
        Self::RepoDocs,
        Self::ConfigKeyValidity,
        Self::ConfigFalseDisable,
        Self::ConfigTrackedTrust,
        Self::Formatters,
        Self::LocalPaths,
        Self::Taste,
        Self::RustTypecheck,
        Self::AppWebTypecheck,
        Self::JavascriptUnused,
        Self::CsharpUnusedCandidates,
        Self::PythonTypecheck,
        Self::EnvPolicy,
        Self::EnvNameLedger,
        Self::FileShape,
        Self::Complexity,
        Self::MagicNumbers,
        Self::MarkdownLinks,
        Self::Reachability,
        Self::SymbolReachability,
        Self::PythonUnused,
        Self::AssertionFreeTests,
        Self::PrivateInternals,
    ];

    /// Exact current spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        const SPELLINGS: [&str; GateId::ALL.len()] = [
            "merge-integrity",
            "planning-phase",
            "repo-shape",
            "staging",
            "repo-docs",
            "config-key-validity",
            "config-false-disable",
            "config-tracked-trust",
            "formatters",
            "local-paths",
            "taste",
            "rust-typecheck",
            "app-web-typecheck",
            "javascript-unused",
            "csharp-unused-candidates",
            "python-typecheck",
            "env-policy",
            "env-name-ledger",
            "file-shape",
            "complexity",
            "magic-numbers",
            "markdown-links",
            "reachability",
            "symbol-reachability",
            "python-unused",
            "assertion-free-tests",
            "private-internals",
        ];
        SPELLINGS[self as usize]
    }

    fn exact(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.as_str() == name)
    }
}

impl fmt::Display for GateId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GateId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::exact(&value).ok_or_else(|| {
            D::Error::custom(format!(
                "unknown compiled gate ID {value:?}; expected one exact current ID"
            ))
        })
    }
}

/// Configuration layer that supplied one battery input.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OriginLayer {
    Compiled,
    System,
    Repository,
    Worktree,
}

impl fmt::Display for OriginLayer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Compiled => "compiled",
            Self::System => "system",
            Self::Repository => "repository",
            Self::Worktree => "worktree",
        })
    }
}
