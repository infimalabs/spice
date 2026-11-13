//! The typed release-one configuration root and its complete leaf registry.
//!
//! Domain crates remain the only authorities for admitting values. This
//! module composes those values and publishes metadata used to route layers,
//! render help, and report provenance without carrying a second value parser.

use std::collections::BTreeSet;
use std::path::Path;

use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize};

mod load;
mod mounts;
mod pre_commit;
mod registry;
mod resolve;

/// Maximum bytes admitted from one repository configuration document.
pub const DOCUMENT_BYTE_LIMIT: u64 = 1024 * 1024;
/// Maximum bytes admitted across tracked, worktree, and local documents.
pub const GENERATION_BYTE_LIMIT: u64 = 2 * 1024 * 1024;

pub use load::{
    AcquisitionIssue, AcquisitionKind, CaptureGeneration, LoadError, Loaded, Locations,
    RepositoryCapture, capture_repository, capture_repository_scoped, load, load_repository,
    locations,
};
pub use mounts::CommandMounts;
pub use pre_commit::{
    PreCommitDeclarations, PreCommitScopes, PreCommitStep, pre_commit_declarations,
};
pub use registry::{
    Builtin, Descriptor, FlagMapping, PathSegment, PathTemplate, PolicyClass, ValueKind,
    descriptor, descriptors,
};
pub use resolve::{
    ConcretePath, Document, FlagError, FlagOverride, FlagValue, Inputs, Issue, IssueKind,
    Provenance, ResolveError, Resolved, resolve,
};

/// Every release-one configuration domain.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Root {
    commands: CommandMounts,
    wrappers: steer::Wrappers,
    rtk: RtkConfig,
    judge: JudgeConfig,
    policy: WorkspacePolicy,
    taste: taste::Config,
    consent: consent::Config,
    gate: gate::Config,
    board: board::Config,
    team: board::team_policy::Config,
    lane: lane::Config,
    steer: steer::Config,
    vendor: vendor::Config,
    listen: listen::Config,
    lock: sill::LockConfig,
    serve: serve::Config,
    say: serve::SpeechConfig,
    proof: proof::Config,
    release: dist::ReleaseConfig,
}

impl Root {
    /// Repository-owned commands mounted beneath the generated CLI namespace.
    #[must_use]
    pub const fn commands(&self) -> &CommandMounts {
        &self.commands
    }

    /// Repository-owned routes installed into supervised agent shells.
    #[must_use]
    pub const fn wrappers(&self) -> &steer::Wrappers {
        &self.wrappers
    }

    /// Optional command-output optimizer used by supervised agent shells.
    #[must_use]
    pub const fn rtk(&self) -> &RtkConfig {
        &self.rtk
    }

    /// Whether this repository opts into independent maxim adjudication.
    #[must_use]
    pub const fn judge(&self) -> &JudgeConfig {
        &self.judge
    }

    /// Workspace-owned environment-name policy.
    #[must_use]
    pub const fn policy(&self) -> &WorkspacePolicy {
        &self.policy
    }

    /// Active prose tastes.
    #[must_use]
    pub const fn taste(&self) -> &taste::Config {
        &self.taste
    }

    /// Ordered consent policy.
    #[must_use]
    pub const fn consent(&self) -> &consent::Config {
        &self.consent
    }

    /// Gate walk policy.
    #[must_use]
    pub const fn gate(&self) -> &gate::Config {
        &self.gate
    }

    /// Allocator tuning.
    #[must_use]
    pub const fn board(&self) -> &board::Config {
        &self.board
    }

    /// Config-only rules for unnamed and explicitly bound teams.
    #[must_use]
    pub const fn team(&self) -> &board::team_policy::Config {
        &self.team
    }

    /// Lane supervision policy.
    #[must_use]
    pub const fn lane(&self) -> &lane::Config {
        &self.lane
    }

    /// Steering horizons.
    #[must_use]
    pub const fn steer(&self) -> &steer::Config {
        &self.steer
    }

    /// Vendor-specific launch policy.
    #[must_use]
    pub const fn vendor(&self) -> &vendor::Config {
        &self.vendor
    }

    /// Listening horizon.
    #[must_use]
    pub const fn listen(&self) -> &listen::Config {
        &self.listen
    }

    /// Lock exit and capacity policy.
    #[must_use]
    pub const fn lock(&self) -> &sill::LockConfig {
        &self.lock
    }

    /// App-server packing limits.
    #[must_use]
    pub const fn serve(&self) -> &serve::Config {
        &self.serve
    }

    /// Browser speech playback.
    #[must_use]
    pub const fn say(&self) -> &serve::SpeechConfig {
        &self.say
    }

    /// Proof image policy and machine-local executable locations.
    #[must_use]
    pub const fn proof(&self) -> &proof::Config {
        &self.proof
    }

    /// Machine-local Developer ID names for Darwin release staging.
    #[must_use]
    pub const fn release(&self) -> &dist::ReleaseConfig {
        &self.release
    }
}

/// Optional RTK command-output optimizer configuration.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RtkConfig {
    executable: String,
}

impl RtkConfig {
    /// Exact basename or absolute executable path selected for RTK.
    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }
}

impl Default for RtkConfig {
    fn default() -> Self {
        Self {
            executable: "rtk".to_owned(),
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawRtkConfig {
    executable: String,
}

impl Default for RawRtkConfig {
    fn default() -> Self {
        Self {
            executable: RtkConfig::default().executable,
        }
    }
}

impl<'de> Deserialize<'de> for RtkConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawRtkConfig::deserialize(deserializer)?;
        let path = Path::new(&raw.executable);
        let basename = path
            .file_name()
            .is_some_and(|name| name == path.as_os_str());
        if raw.executable.is_empty()
            || raw.executable.contains('\0')
            || (!path.is_absolute()
                && (!basename || raw.executable.chars().any(char::is_whitespace)))
        {
            return Err(D::Error::custom(
                "rtk.executable must be one non-empty executable basename or absolute path",
            ));
        }
        Ok(Self {
            executable: raw.executable,
        })
    }
}

/// Optional resident maxim-adjudication policy.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct JudgeConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
}

impl JudgeConfig {
    /// Whether resident taste candidates cross the shared judge boundary.
    #[must_use]
    pub const fn enabled(&self) -> Option<bool> {
        self.enabled
    }
}

/// Tracked workspace environment vocabulary used by Gate and Doctor.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkspacePolicy {
    env_names: Vec<String>,
    pre_commit_builtins: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pre_commit: Vec<PreCommitStep>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    test_engines: Vec<TestEngine>,
    commit_message: gate::CommitMessagePolicy,
    repo_truth: gate::RepoTruthPolicy,
    file_shape: gate::FileShapePolicy,
    languages: gate::LanguagesPolicy,
    package_roots: Vec<String>,
    generated_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name_cluster_threshold: Option<usize>,
    test_paths: Vec<String>,
    package: gate::PackagePolicy,
    admission: Vec<String>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WorkspacePolicyInput {
    #[serde(deserialize_with = "unique_names")]
    env_names: Vec<String>,
    pre_commit_builtins: bool,
    pre_commit: Vec<PreCommitStep>,
    #[serde(deserialize_with = "unique_test_engines")]
    test_engines: Vec<TestEngine>,
    commit_message: gate::CommitMessagePolicy,
    repo_truth: gate::RepoTruthPolicy,
    file_shape: gate::FileShapePolicy,
    languages: gate::LanguagesPolicy,
    package_roots: Vec<String>,
    generated_paths: Vec<String>,
    name_cluster_threshold: Option<usize>,
    test_paths: Vec<String>,
    package: gate::PackagePolicy,
    admission: Vec<String>,
}

impl Default for WorkspacePolicyInput {
    fn default() -> Self {
        Self {
            env_names: Vec::new(),
            pre_commit_builtins: true,
            pre_commit: Vec::new(),
            test_engines: Vec::new(),
            commit_message: gate::CommitMessagePolicy::default(),
            repo_truth: gate::RepoTruthPolicy::default(),
            file_shape: gate::FileShapePolicy::default(),
            languages: gate::LanguagesPolicy::default(),
            package_roots: Vec::new(),
            generated_paths: Vec::new(),
            name_cluster_threshold: None,
            test_paths: Vec::new(),
            package: gate::PackagePolicy::default(),
            admission: Vec::new(),
        }
    }
}

impl From<WorkspacePolicyInput> for WorkspacePolicy {
    fn from(input: WorkspacePolicyInput) -> Self {
        Self {
            env_names: input.env_names,
            pre_commit_builtins: input.pre_commit_builtins,
            pre_commit: input.pre_commit,
            test_engines: input.test_engines,
            commit_message: input.commit_message,
            repo_truth: input.repo_truth,
            file_shape: input.file_shape,
            languages: input.languages,
            package_roots: input.package_roots,
            generated_paths: input.generated_paths,
            name_cluster_threshold: input.name_cluster_threshold,
            test_paths: input.test_paths,
            package: input.package,
            admission: input.admission,
        }
    }
}

impl Default for WorkspacePolicy {
    fn default() -> Self {
        WorkspacePolicyInput::default().into()
    }
}

impl<'de> Deserialize<'de> for WorkspacePolicy {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let input = WorkspacePolicyInput::deserialize(deserializer)?;
        if !input.pre_commit_builtins && input.pre_commit.is_empty() {
            return Err(D::Error::custom(pre_commit::EMPTY_PRE_COMMIT_MESSAGE));
        }
        Ok(input.into())
    }
}

impl WorkspacePolicy {
    /// Environment names declared by the tracked workspace.
    #[must_use]
    pub fn env_names(&self) -> &[String] {
        &self.env_names
    }

    /// Whether the installed Spice pre-commit dispatcher runs its packaged battery.
    #[must_use]
    pub const fn pre_commit_builtins(&self) -> bool {
        self.pre_commit_builtins
    }

    /// Repository-owned commands appended in authored pre-commit order.
    #[must_use]
    pub fn pre_commit(&self) -> &[PreCommitStep] {
        &self.pre_commit
    }

    /// Packaged test engines this repository explicitly enables.
    #[must_use]
    pub fn test_engines(&self) -> &[TestEngine] {
        &self.test_engines
    }

    /// Documents the workspace measures as repository truth.
    ///
    /// Gate compiles this into its rule policy from the same tracked document,
    /// so admitting it here is what lets a repository declare it at all.
    #[must_use]
    pub const fn repo_truth(&self) -> &gate::RepoTruthPolicy {
        &self.repo_truth
    }

    /// Commit trailer policy the native `commit-msg` hook enforces.
    #[must_use]
    pub const fn commit_message(&self) -> &gate::CommitMessagePolicy {
        &self.commit_message
    }

    /// Files the workspace measures shape over.
    ///
    /// Gate requires this from the same tracked document, so admitting it here
    /// is what lets the document carrying it load at all.
    #[must_use]
    pub const fn file_shape(&self) -> &gate::FileShapePolicy {
        &self.file_shape
    }

    /// Files the workspace offers each language arm.
    #[must_use]
    pub const fn languages(&self) -> &gate::LanguagesPolicy {
        &self.languages
    }

    /// Roots the repository declares its packages under.
    #[must_use]
    pub fn package_roots(&self) -> &[String] {
        &self.package_roots
    }

    /// Paths the repository declares as generated rather than authored.
    #[must_use]
    pub fn generated_paths(&self) -> &[String] {
        &self.generated_paths
    }

    /// Cluster size at which the repository wants related names reported.
    #[must_use]
    pub const fn name_cluster_threshold(&self) -> Option<usize> {
        self.name_cluster_threshold
    }

    /// Roots the repository declares its tests under.
    #[must_use]
    pub fn test_paths(&self) -> &[String] {
        &self.test_paths
    }

    /// Module-name shape the repository declares.
    #[must_use]
    pub const fn package(&self) -> &gate::PackagePolicy {
        &self.package
    }

    /// Command that admits one integrated tree before it is published.
    ///
    /// Task completion merges the lane onto a baseline it never saw, so the
    /// tree it publishes is an artifact no lane authored and no verifier ran.
    /// This is the cheap load that tree must survive: it answers whether the
    /// repository can be read at all, not whether it passes. A repository
    /// declaring nothing here publishes synthesized trees unadmitted, which is
    /// how a clean textual merge of two valid manifests reached a baseline as a
    /// duplicate table.
    #[must_use]
    pub fn admission(&self) -> &[String] {
        &self.admission
    }
}

/// One compiled test executor a repository may explicitly enable.
///
/// The declaration selects a packaged command contract; it never carries an
/// executable path or argument vector from repository data.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TestEngine {
    /// Canonical Cargo metadata reach and bounded workspace target execution.
    CargoWorkspace,
}

impl TestEngine {
    /// Stable report and configuration identity.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CargoWorkspace => "cargo-workspace",
        }
    }
}

fn unique_names<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let names = Vec::<String>::deserialize(deserializer)?;
    let mut seen = BTreeSet::new();
    for name in &names {
        if name.is_empty() {
            return Err(D::Error::custom("configuration names must be nonempty"));
        }
        if !seen.insert(name) {
            return Err(D::Error::custom(format!(
                "configuration names must be unique; duplicate {name:?}"
            )));
        }
    }
    Ok(names)
}

fn unique_test_engines<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<TestEngine>, D::Error> {
    let engines = Vec::<TestEngine>::deserialize(deserializer)?;
    let mut seen = BTreeSet::new();
    for engine in &engines {
        if !seen.insert(*engine) {
            return Err(D::Error::custom(format!(
                "policy.test_engines must be unique; duplicate {:?}",
                engine.as_str(),
            )));
        }
    }
    Ok(engines)
}

fn nonempty_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.trim().is_empty() {
        Err(D::Error::custom(
            "pre-commit labels must contain non-whitespace characters",
        ))
    } else {
        Ok(value)
    }
}

fn nonempty_argv<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let arguments = Vec::<String>::deserialize(deserializer)?;
    if arguments.is_empty() {
        return Err(D::Error::custom("pre-commit run argv must not be empty"));
    }
    if arguments.iter().any(String::is_empty) {
        return Err(D::Error::custom(
            "pre-commit run argv entries must be nonempty",
        ));
    }
    Ok(arguments)
}
