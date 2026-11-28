//! Acquisition of one complete toolchain-family observation battery.
//!
//! [`super::measure_toolchain`] validates, orders, and canonicalizes; this
//! module only observes. Every declaration produces exactly one
//! [`ArmObservation`], in compiled order, whether or not this build can analyze
//! it. Every declaration naming a compiled analysis routes to
//! [`super::analyze`];
//! the rest cross as [`ArmOutcome::Unsupported`], which is neither a pass nor a
//! skip, so an unmeasured tree is never admitted by silence.
//!
//! The rim directory, the executable search path, and the selected changes are
//! all ambient configuration. Each is captured at the binary rim and handed in
//! whole rather than read back from the process or the repository here, so two
//! machines cannot silently resolve two different programs or scan two
//! different sets. Reading nothing back is also why a relative rim directory is
//! refused by [`RimCapture::new`] instead of resolved there: resolving it would
//! read the one piece of process state this module declines to consult.
//!
//! A declared program is probed exactly once, at one absolute path, and the
//! probe answers with one of three observations: not found, found and not
//! identifiable, or identified at exactly what it printed. The middle one is
//! why the first two are separate: a tool that is present and broken is a
//! different fact about this machine than a tool that was never installed, and
//! collapsing them would send an operator looking for the wrong thing.
//!
//! One state the family vocabulary cannot express: an arm whose declared tools
//! include both an unusable program and a resolved one at the wrong version.
//! `ToolUnavailable` requires every identified program to be exact and
//! `UnsupportedVersion` requires every program to be identified, so acquisition
//! reports the unusable program and lets [`super::measure_toolchain`] refuse the
//! incoherent pair loudly rather than inventing a third reading.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
#[cfg(windows)]
use std::os::windows::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::{LazyLock, Mutex};
use std::thread;

use crate::measure::spawn;
use sill::{Argv, Completion};
use wire::{Digest, Domain};

use super::analyze::{self, Analysis};
use super::captured::{Captured, Stream};
use super::{
    ArmObservation, ArmOutcome, ProbeFailure, TOOLCHAIN_ARMS, ToolEvidence, ToolRequirement,
    ToolchainDeclaration, ToolchainError, TreeInput, contracted_version,
};
use crate::GateId;
use crate::input::Contract;
use crate::measure::Bounds;
use crate::metrics::BaselineCapture;
use crate::tree::SelectedTree;

type ProgramReader = fn(&Path) -> std::io::Result<Vec<u8>>;
type VersionProbe<'a> = &'a dyn Fn(&Path, &[&str]) -> Result<String, ProbeFailure>;

fn read_program(path: &Path) -> std::io::Result<Vec<u8>> {
    fs::read(path)
}

/// Everything the binary rim observed about itself, captured as one value.
///
/// The three travel together because none of them is complete alone. A
/// search-path entry is allowed to be relative or empty, and such an entry names
/// a directory relative to wherever it was written down; nothing downstream can
/// recover that directory afterwards, because probes run in the selected tree
/// and this process runs wherever it was started. Handing the directory in
/// beside the search path is what makes a resolved program one absolute file
/// rather than one file per reader, and keeping them in one value is what stops
/// a caller from anchoring one rim's search path to another rim's directory.
///
/// The search path is optional because a rim can genuinely have none. That is an
/// observation, not a defect: every declared program then resolves to
/// [`ToolEvidence::Missing`] and the arms that need one say so.
#[derive(Clone, Debug)]
pub struct RimCapture {
    directory: PathBuf,
    search_path: Option<OsString>,
    changes: Vec<String>,
    staged_changes: bool,
}

impl RimCapture {
    /// Capture one rim's directory, executable search path, and changed paths.
    ///
    /// The directory must be absolute, and a relative one is refused here
    /// rather than resolved. This constructor takes everything it holds as an
    /// argument and reads nothing ambient, so the only directory it could
    /// anchor a relative one against is whatever directory this process happens
    /// to have been started in, which is the reader-dependent anchor the
    /// capture exists to remove. Refusing is what makes [`Self::directory`]
    /// absolute for every reader instead of only for the well-behaved caller.
    ///
    /// Changed paths are selected-tree-relative. A path that does not name a
    /// regular file in the captured tree is retained here and dropped by the
    /// scanner that reads it, because a deletion is a change with nothing to
    /// read rather than a defect in the capture.
    ///
    /// # Errors
    ///
    /// Refuses a relative directory, which cannot anchor a search path.
    pub fn new(
        directory: impl Into<PathBuf>,
        search_path: Option<OsString>,
        changes: impl IntoIterator<Item = String>,
    ) -> Result<Self, RimDirectoryError> {
        let directory = directory.into();
        if !directory.is_absolute() {
            return Err(RimDirectoryError(directory));
        }
        Ok(Self {
            directory,
            search_path,
            changes: changes.into_iter().collect(),
            staged_changes: false,
        })
    }

    /// Capture a command rim whose changed paths must come from its staged selector.
    ///
    /// The battery replaces the empty path inventory only after materializing
    /// the exact index authority that supplies the candidate tree.
    pub fn staged(
        directory: impl Into<PathBuf>,
        search_path: Option<OsString>,
    ) -> Result<Self, RimDirectoryError> {
        let mut capture = Self::new(directory, search_path, [] as [String; 0])?;
        capture.staged_changes = true;
        Ok(capture)
    }

    /// Absolute directory the search path was captured in.
    ///
    /// Absolute by construction: [`Self::new`] admits nothing else.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Search path captured at the rim, if the rim had one.
    #[must_use]
    pub fn search_path(&self) -> Option<&OsStr> {
        self.search_path.as_deref()
    }

    /// Selected-tree-relative paths the rim captured as changed.
    #[must_use]
    pub fn changes(&self) -> &[String] {
        &self.changes
    }

    /// Replace the changed-path view with one validated selected-arm reach.
    pub(crate) fn with_changes(mut self, changes: Vec<String>) -> Self {
        self.changes = changes;
        self
    }

    pub(crate) const fn derives_staged_changes(&self) -> bool {
        self.staged_changes
    }
}

/// A directory that cannot anchor one rim's search path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RimDirectoryError(PathBuf);

impl fmt::Display for RimDirectoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "rim directory must be absolute to anchor a search path: {:?}",
            self.0
        )
    }
}

impl std::error::Error for RimDirectoryError {}

/// One captured tree and the rim that named the programs and changes to use.
///
/// The rim capture is taken at the rim rather than recomputed here. An arm
/// declaring [`super::TreeInput::SelectedChanges`] reads its list and nothing
/// else, so the paths it scans belong to the same instant as the tree they are
/// read from, and the programs it probes belong to the same instant as the
/// directory they resolve against.
#[derive(Debug)]
pub struct ToolchainSource<'a> {
    tree: Option<&'a SelectedTree>,
    root: &'a Path,
    repository: &'a Path,
    paths: &'a [String],
    rim: RimCapture,
    reached_paths: Option<Vec<String>>,
    baselines: Option<&'a BaselineCapture>,
    compiler_environment: &'a [(OsString, OsString)],
}

impl<'a> ToolchainSource<'a> {
    /// Capture one selected tree beside the rim that will supply its programs.
    ///
    /// Standalone toolchain acquisition does not infer ancestry from the live
    /// repository after the tree was selected. The production battery supplies
    /// ancestry from its atomic repository capture instead.
    #[must_use]
    pub fn capture(tree: &'a SelectedTree, rim: RimCapture) -> Self {
        Self {
            tree: Some(tree),
            root: tree.root(),
            repository: tree.repository(),
            paths: tree.paths(),
            rim,
            reached_paths: None,
            baselines: None,
            compiler_environment: &[],
        }
    }

    /// Capture one selected tree with the parents that can prove merge authorship.
    #[must_use]
    pub(crate) fn capture_with_baselines(
        tree: &'a SelectedTree,
        rim: RimCapture,
        baselines: &'a BaselineCapture,
        compiler_environment: &'a [(OsString, OsString)],
        reached_paths: Option<&[String]>,
    ) -> Self {
        Self {
            tree: Some(tree),
            root: tree.root(),
            repository: tree.repository(),
            paths: tree.paths(),
            rim,
            reached_paths: reached_paths.map(<[String]>::to_vec),
            baselines: Some(baselines),
            compiler_environment,
        }
    }

    /// Selected-tree root every probe runs against.
    #[must_use]
    pub const fn root(&self) -> &Path {
        self.root
    }

    /// Repository that owns stable writable build outputs for this selection.
    #[must_use]
    pub const fn repository(&self) -> &Path {
        self.repository
    }

    /// The rim this acquisition resolves and scans on behalf of.
    #[must_use]
    pub const fn rim(&self) -> &RimCapture {
        &self.rim
    }

    /// Ambient compiler environment captured by the command rim.
    pub(super) const fn compiler_environment(&self) -> &[(OsString, OsString)] {
        self.compiler_environment
    }

    /// Selected-tree-relative paths the rim captured as changed.
    #[must_use]
    pub fn changes(&self) -> &[String] {
        self.rim.changes()
    }

    /// Validated paths for one focused arm, when narrowed.
    fn reached_paths(&self) -> Option<&[String]> {
        self.reached_paths.as_deref()
    }

    /// Paths one analyzer is authorized to consume in this capture.
    ///
    /// Complete batteries retain the complete selected tree. A focused arm
    /// receives dependency-expanded paths through the command rim.
    pub(super) fn analysis_paths(&self) -> &[String] {
        self.reached_paths.as_deref().unwrap_or(self.paths)
    }

    /// Whether this pending merge inherited one selected path verbatim.
    fn merge_inherits(&self, path: &str) -> bool {
        let Some(baselines) = self.baselines else {
            return false;
        };
        fs::read(self.root.join(path)).is_ok_and(|bytes| baselines.merge_inherits(path, &bytes))
    }

    /// Changed paths authored by this pending commit rather than a merge parent.
    fn chargeable_changes(&self) -> Vec<String> {
        self.changes()
            .iter()
            .filter(|path| !self.merge_inherits(path))
            .cloned()
            .collect()
    }
}

/// Acquire one observation for every declared toolchain arm.
///
/// The result is one [`ArmObservation`] per entry in [`TOOLCHAIN_ARMS`], in
/// declaration order, with no arm omitted and none invented. Feed it straight
/// to [`super::measure_toolchain`], which is the only admission authority.
#[must_use]
pub fn acquire_toolchain(source: &ToolchainSource<'_>, contract: &Contract) -> Vec<ArmObservation> {
    acquire_toolchain_observed(source, contract, |_, _| {})
}

/// Acquire every declared observation and report each completed analysis.
///
/// Completion is emitted only after the analyzer has returned its full
/// observation. The repository executor uses this seam to expose a real
/// process-order event without granting the observer execution authority.
pub(crate) fn acquire_toolchain_observed(
    source: &ToolchainSource<'_>,
    contract: &Contract,
    mut completed: impl FnMut(GateId, Option<super::RustAnalysisWorkspace>),
) -> Vec<ArmObservation> {
    let mut observations = Vec::with_capacity(TOOLCHAIN_ARMS.len());
    let mut declarations = TOOLCHAIN_ARMS.into_iter();
    while let Some(declaration) = declarations.next() {
        if declaration.id != GateId::RustTypecheck {
            let observation = observe_declaration(declaration, source, contract);
            completed(declaration.id, None);
            observations.push(observation);
            continue;
        }
        let app = declarations
            .next()
            .expect("app analysis follows native Rust analysis");
        assert_eq!(app.id, GateId::AppWebTypecheck);
        let (rust_observation, app_observation) = thread::scope(|scope| {
            let app = scope.spawn(|| observe_declaration(app, source, contract));
            let rust = observe_declaration(declaration, source, contract);
            let reusable_workspace = (rust.outcome == ArmOutcome::Passed).then(|| {
                analyze::rust_analysis_workspace(source.repository())
                    .expect("passed native Rust analysis must retain its exact reusable workspace")
            });
            completed(declaration.id, reusable_workspace);
            let app = app.join().expect("app toolchain analysis must not panic");
            completed(app.gate_id, None);
            (rust, app)
        });
        observations.extend([rust_observation, app_observation]);
    }
    observations
}

fn observe_declaration(
    declaration: ToolchainDeclaration,
    source: &ToolchainSource<'_>,
    contract: &Contract,
) -> ArmObservation {
    let declaration = scoped_declaration(declaration, source);
    observe(
        declaration,
        source,
        contract.bounds(),
        contract.environment_names(),
    )
}

pub(crate) fn acquire_toolchain_arm(
    source: &ToolchainSource<'_>,
    contract: &Contract,
    gate_id: GateId,
) -> Result<ArmObservation, ToolchainError> {
    let declaration = TOOLCHAIN_ARMS
        .into_iter()
        .find(|declaration| declaration.id == gate_id)
        .ok_or(ToolchainError::ForeignGate(gate_id))?;
    Ok(observe_declaration(declaration, source, contract))
}

/// Probe only the external executables declared by one focused arm.
///
/// Reusable proof preparation calls this before deriving an action key and
/// compares the result with the evidence retained by the executed arm. That
/// keeps executable bytes inside the reusable authority without running the
/// arm's analysis twice.
pub(crate) fn acquire_toolchain_arm_tools(
    source: &ToolchainSource<'_>,
    contract: &Contract,
    gate_id: GateId,
) -> Result<Vec<ToolEvidence>, ToolchainError> {
    let mut tools: Vec<ToolEvidence> = TOOLCHAIN_ARMS
        .into_iter()
        .find(|declaration| declaration.id == gate_id)
        .map(|declaration| {
            declaration
                .tools
                .iter()
                .map(|requirement| probe(requirement, source, contract.bounds()))
                .collect()
        })
        .unwrap_or_default();
    if matches!(gate_id, GateId::MergeIntegrity | GateId::Staging) {
        tools.push(git_tool_evidence(source, contract.bounds()));
    }
    Ok(tools)
}

/// Digest the complete executable-file closure used by reusable toolchain arms.
///
/// Unlike the arm probe this starts no subprocess, so a warm fingerprint lookup
/// can reject stale tool bytes without paying action-planning cost.
pub(crate) fn toolchain_executable_authority(
    source: &ToolchainSource<'_>,
) -> Result<Digest, String> {
    let mut requirements = TOOLCHAIN_ARMS
        .into_iter()
        .flat_map(|declaration| declaration.tools)
        .map(|requirement| (requirement.name, requirement.program))
        .collect::<Vec<_>>();
    requirements.sort_unstable();
    requirements.dedup();
    let records = requirements
        .into_iter()
        .map(|(name, program)| executable_authority_record(source, name, program))
        .collect::<Result<Vec<_>, String>>()?;
    let git =
        crate::git::executable_authority(source.rim().directory(), source.rim().search_path())?;
    let bytes = wire::encode(&(records, git))
        .map_err(|error| format!("encode proof tool authority: {error}"))?;
    Ok(Digest::compute(Domain::GateTool, &bytes))
}

type ExecutableAuthorityRecord<'a> = (&'a str, &'a str, Option<(String, Digest, bool)>);

#[cfg(unix)]
#[derive(Clone, Copy, Eq, PartialEq)]
struct ExecutableStamp {
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(windows)]
#[derive(Clone, Copy, Eq, PartialEq)]
struct ExecutableStamp {
    length: u64,
    attributes: u32,
    created: u64,
    modified: u64,
}

#[cfg(unix)]
type ExecutableDigestCache = BTreeMap<PathBuf, (ExecutableStamp, Digest, bool)>;
// Metadata is only a process-local rehash trigger; canonical authority remains
// the exact byte digest and executable bit returned below.
#[cfg(unix)]
static EXECUTABLE_DIGEST_CACHE: LazyLock<Mutex<ExecutableDigestCache>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn executable_authority_record<'a>(
    source: &ToolchainSource<'_>,
    name: &'a str,
    program: &'a str,
) -> Result<ExecutableAuthorityRecord<'a>, String> {
    let Some(resolved) = resolve(
        program,
        source.rim().directory(),
        source.rim().search_path(),
    ) else {
        return Ok((name, program, None));
    };
    let metadata = fs::metadata(&resolved)
        .map_err(|error| format!("inspect proof tool {}: {error}", resolved.display()))?;
    let stamp = executable_stamp(&metadata);
    #[cfg(unix)]
    if let Some((_, digest, executable)) = EXECUTABLE_DIGEST_CACHE
        .lock()
        .expect("executable digest cache is not poisoned")
        .get(&resolved)
        .filter(|(cached, _, _)| cached == &stamp)
    {
        return Ok((
            name,
            program,
            Some((
                resolved.to_string_lossy().into_owned(),
                digest.clone(),
                *executable,
            )),
        ));
    }
    let bytes = fs::read(&resolved)
        .map_err(|error| format!("read proof tool {}: {error}", resolved.display()))?;
    let observed = fs::metadata(&resolved)
        .map_err(|error| format!("inspect proof tool {}: {error}", resolved.display()))?;
    let observed_stamp = executable_stamp(&observed);
    if observed_stamp != stamp {
        return Err(format!(
            "proof tool {} changed while its authority was read",
            resolved.display(),
        ));
    }
    let executable = is_executable(&resolved, &observed);
    let digest = Digest::compute(Domain::GateTool, &bytes);
    #[cfg(unix)]
    EXECUTABLE_DIGEST_CACHE
        .lock()
        .expect("executable digest cache is not poisoned")
        .insert(resolved.clone(), (stamp, digest.clone(), executable));
    Ok((
        name,
        program,
        Some((resolved.to_string_lossy().into_owned(), digest, executable)),
    ))
}

#[cfg(unix)]
fn executable_stamp(metadata: &fs::Metadata) -> ExecutableStamp {
    ExecutableStamp {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        mode: metadata.mode(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

#[cfg(windows)]
fn executable_stamp(metadata: &fs::Metadata) -> ExecutableStamp {
    ExecutableStamp {
        length: metadata.len(),
        attributes: metadata.file_attributes(),
        created: metadata.creation_time(),
        modified: metadata.last_write_time(),
    }
}

#[cfg(unix)]
fn is_executable(_: &Path, metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
fn is_executable(path: &Path, metadata: &fs::Metadata) -> bool {
    metadata.is_file()
        && path.extension().is_some_and(|extension| {
            ["exe", "com", "bat", "cmd"]
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn scoped_declaration(
    mut declaration: ToolchainDeclaration,
    source: &ToolchainSource<'_>,
) -> ToolchainDeclaration {
    if source.reached_paths().is_some() && super::accepts_selected_scope(declaration.id) {
        declaration.jurisdiction.input = TreeInput::SelectedChanges;
    }
    declaration
}

fn observe(
    declaration: ToolchainDeclaration,
    source: &ToolchainSource<'_>,
    bounds: &Bounds,
    environment_names: &[String],
) -> ArmObservation {
    if let Some(analysis) = analyze::before_tools(
        declaration,
        source.root(),
        source.reached_paths(),
        source.analysis_paths(),
    ) {
        return observation(declaration, analysis, Vec::new());
    }
    let tools: Vec<ToolEvidence> = declaration
        .tools
        .iter()
        .map(|requirement| probe(requirement, source, bounds))
        .collect();
    let analysis = examine(declaration, source, bounds, environment_names, &tools);
    let tools = if matches!(
        analysis.outcome,
        ArmOutcome::NotApplicable | ArmOutcome::Excluded
    ) {
        Vec::new()
    } else {
        tools
    };
    observation(declaration, analysis, tools)
}

fn observation(
    declaration: ToolchainDeclaration,
    analysis: Analysis,
    tools: Vec<ToolEvidence>,
) -> ArmObservation {
    ArmObservation {
        gate_id: declaration.id,
        tree_input: declaration.jurisdiction.input,
        outcome: analysis.outcome,
        reason: analysis.reason,
        tools,
        findings: analysis.findings,
        exit_code: analysis.exit_code,
        timing: analysis.timing,
    }
}

/// Run the analysis this arm names, once its declared programs are usable.
///
/// Evidence is read before analysis because an arm whose program is absent or
/// at the wrong version has nothing to measure with; the refusal it earns says
/// so and no analysis is attempted. Every identity is named outright, so a new
/// [`GateId`] fails to compile here until its one current analysis is supplied.
fn examine(
    declaration: ToolchainDeclaration,
    source: &ToolchainSource<'_>,
    bounds: &Bounds,
    environment_names: &[String],
    tools: &[ToolEvidence],
) -> Analysis {
    if let Some(refusal) = analyzer_refusal(declaration, tools) {
        return refusal;
    }
    let Some(owed) = owed_analyzer(declaration.id) else {
        return match declaration.id {
            GateId::Formatters => analyze::formatters(
                declaration,
                source.root,
                source.reached_paths(),
                bounds,
                &resolved_programs(declaration, tools),
            ),
            GateId::RustTypecheck => {
                let programs = resolved_programs(declaration, tools);
                let base = match cargo_base(source) {
                    Ok(base) => base,
                    Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
                };
                analyze::rust_typecheck(
                    declaration,
                    source.root,
                    base.as_ref().map(SelectedTree::root),
                    source.repository,
                    source.changes(),
                    bounds,
                    analyze::CompilerResources::new(&programs, source.compiler_environment()),
                )
            }
            GateId::AppWebTypecheck => {
                let programs = resolved_programs(declaration, tools);
                let base = match cargo_base(source) {
                    Ok(base) => base,
                    Err(reason) => return Analysis::terminal(ArmOutcome::ParseFailure, reason),
                };
                analyze::app_web_typecheck(
                    declaration,
                    analyze::CargoSelection::new(
                        source.root,
                        base.as_ref().map(SelectedTree::root),
                        source.changes(),
                    ),
                    source.repository,
                    bounds,
                    analyze::CompilerResources::new(&programs, source.compiler_environment()),
                    refusal(declaration, tools),
                )
            }
            GateId::LocalPaths => {
                let changes = source.chargeable_changes();
                analyze::local_paths(declaration, source.root, &changes)
            }
            GateId::Taste => {
                let changes = source.chargeable_changes();
                analyze::taste(declaration, source.root, &changes)
            }
            GateId::JavascriptUnused => {
                analyze::javascript_unused(declaration, source.root, source.analysis_paths())
            }
            GateId::CsharpUnusedCandidates => {
                analyze::csharp_unused(declaration, source.root, source.analysis_paths())
            }
            GateId::PythonTypecheck => analyze::python_typecheck(
                declaration,
                source.root,
                source.analysis_paths(),
                bounds,
                &resolved_programs(declaration, tools),
            ),
            GateId::EnvPolicy => {
                let changes = source.chargeable_changes();
                analyze::env_policy(declaration, source.root, &changes)
            }
            GateId::EnvNameLedger => analyze::env_name_ledger(
                declaration,
                source.root,
                source.analysis_paths(),
                environment_names,
                |path| !source.merge_inherits(path),
            ),
            id => Analysis::terminal(
                ArmOutcome::ParseFailure,
                format!(
                    "{} is declared as a toolchain arm this family compiles no analysis for",
                    id.as_str()
                ),
            ),
        };
    };
    Analysis::terminal(
        ArmOutcome::Unsupported,
        format!(
            "{} has no compiled analyzer in this build; {owed}",
            declaration.id.as_str()
        ),
    )
}

fn cargo_base(source: &ToolchainSource<'_>) -> Result<Option<SelectedTree>, String> {
    if !crate::workspace_tests::cargo_workspace_base_inventory_required(
        source.root,
        source.changes(),
    )
    .map_err(|error| error.to_string())?
    {
        return Ok(None);
    }
    source
        .tree
        .ok_or_else(|| "removed Rust source has no selected-tree base authority".to_owned())?
        .materialize_staged_base()
        .map_err(|error| error.to_string())?
        .map(Some)
        .ok_or_else(|| "removed Rust source requires a committed staged base tree".to_owned())
}

/// The refusal this arm's probed evidence earns before any analysis runs.
///
/// `None` means every declared program resolved at its exact version. Neither
/// `NotApplicable` nor `Excluded` is ever produced here, because both are skips
/// that admit the battery and evidence alone has measured nothing to earn one.
///
/// An absent program and an unusable one earn the same refusal, because neither
/// leaves the arm anything to measure with, and the sentence says which of the
/// two happened to each program so the refusal is actionable rather than merely
/// correct.
fn refusal(declaration: ToolchainDeclaration, tools: &[ToolEvidence]) -> Option<Analysis> {
    refusal_for(declaration.id, declaration.tools, tools)
}

/// The app may decide from exact Cargo metadata that Trunk is outside the
/// selected closure. Every other analyzer requires all declared programs
/// before it can establish applicability.
fn analyzer_refusal(declaration: ToolchainDeclaration, tools: &[ToolEvidence]) -> Option<Analysis> {
    if declaration.id != GateId::AppWebTypecheck {
        return refusal(declaration, tools);
    }
    let cargo = declaration
        .tools
        .iter()
        .position(|requirement| requirement.program == "cargo")
        .expect("app declaration retains its Cargo metadata authority");
    refusal_for(
        declaration.id,
        std::slice::from_ref(&declaration.tools[cargo]),
        std::slice::from_ref(&tools[cargo]),
    )
}

fn refusal_for(
    gate_id: GateId,
    declared: &[ToolRequirement],
    tools: &[ToolEvidence],
) -> Option<Analysis> {
    let unusable: Vec<String> = tools
        .iter()
        .filter_map(|evidence| match evidence {
            ToolEvidence::Missing {
                requested_program, ..
            } => Some(format!("{requested_program} did not resolve")),
            ToolEvidence::Unprobed {
                requested_program,
                resolved_program,
                failure,
                ..
            } => Some(format!(
                "{requested_program} resolved to {resolved_program} and {failure}"
            )),
            ToolEvidence::Identified { .. } => None,
        })
        .collect();
    if !unusable.is_empty() {
        return Some(Analysis::terminal(
            ArmOutcome::ToolUnavailable,
            format!(
                "{} has no usable declared program: {}",
                gate_id.as_str(),
                unusable.join("; ")
            ),
        ));
    }

    let drifted: Vec<String> = declared
        .iter()
        .zip(tools)
        .filter_map(|(requirement, evidence)| match evidence {
            ToolEvidence::Identified { version, .. }
                if contracted_version(version) != requirement.exact_version =>
            {
                Some(format!(
                    "{} is {:?} and not {:?}",
                    requirement.name,
                    contracted_version(version),
                    requirement.exact_version
                ))
            }
            _ => None,
        })
        .collect();
    if !drifted.is_empty() {
        return Some(Analysis::terminal(
            ArmOutcome::UnsupportedVersion,
            format!(
                "{} left its exact version contract: {}",
                gate_id.as_str(),
                drifted.join("; ")
            ),
        ));
    }

    None
}

/// Every declared program of one arm, keyed by the name that requested it.
///
/// Normally reached once [`refusal`] has ruled out every unusable program.
/// The app may arrive with only exact Cargo so metadata can decide whether Trunk
/// belongs to the selected closure. Every retained path is the one its probe
/// already ran, so an analysis cannot reach a second file under the same name.
fn resolved_programs(
    declaration: ToolchainDeclaration,
    tools: &[ToolEvidence],
) -> BTreeMap<&str, &str> {
    declaration
        .tools
        .iter()
        .zip(tools)
        .filter_map(|(requirement, evidence)| match evidence {
            ToolEvidence::Identified {
                resolved_program, ..
            } => Some((requirement.program, resolved_program.as_str())),
            ToolEvidence::Missing { .. } | ToolEvidence::Unprobed { .. } => None,
        })
        .collect()
}

/// The exact analyzer one identity is still owed, or `None` once nothing is.
///
/// Every compiled variant is named explicitly so adding an arm or completing
/// an analyzer changes this exhaustive contract at compile time.
pub(super) const fn owed_analyzer(id: GateId) -> Option<&'static str> {
    match id {
        GateId::Formatters
        | GateId::LocalPaths
        | GateId::Taste
        | GateId::RustTypecheck
        | GateId::AppWebTypecheck
        | GateId::JavascriptUnused
        | GateId::CsharpUnusedCandidates
        | GateId::PythonTypecheck
        | GateId::EnvPolicy
        | GateId::EnvNameLedger => None,
        GateId::MergeIntegrity
        | GateId::PlanningPhase
        | GateId::RepoShape
        | GateId::Staging
        | GateId::RepoDocs
        | GateId::ConfigKeyValidity
        | GateId::ConfigFalseDisable
        | GateId::ConfigTrackedTrust
        | GateId::FileShape
        | GateId::Complexity
        | GateId::MagicNumbers
        | GateId::MarkdownLinks
        | GateId::Reachability
        | GateId::SymbolReachability
        | GateId::PythonUnused
        | GateId::AssertionFreeTests
        | GateId::PrivateInternals => None,
    }
}

/// Probe one declared executable into evidence.
///
/// Nothing here is an error, because a program this machine cannot run is an
/// observation the report is supposed to carry. Only an unresolved program is
/// [`ToolEvidence::Missing`]; one that resolved and then failed keeps the exact
/// file it resolved to and says what the probe did, so an operator is never
/// sent looking for a program that is already installed.
///
/// The resolved path is computed once and used for the digest and the probe
/// both, so the bytes that were hashed are the bytes that answered.
fn probe(
    requirement: &ToolRequirement,
    source: &ToolchainSource<'_>,
    bounds: &Bounds,
) -> ToolEvidence {
    probe_with(requirement, source, read_program, &|program, arguments| {
        version_output(program, arguments, source.root, bounds)
    })
}

fn git_tool_evidence(source: &ToolchainSource<'_>, bounds: &Bounds) -> ToolEvidence {
    let rim = source.rim();
    let Ok(selected) = crate::git::program(rim.directory(), rim.search_path()) else {
        return ToolEvidence::Missing {
            name: "git".to_owned(),
            requested_program: "git".to_owned(),
        };
    };
    let requested = selected.path().to_string_lossy().into_owned();
    probe_resolved_with(
        "git",
        &requested,
        &["--version"],
        selected.path().to_path_buf(),
        read_program,
        &|program, arguments| version_output(program, arguments, source.root, bounds),
    )
}

/// Probe one executable through the supplied byte reader and version probe.
///
/// Both boundaries keep the identity contract testable in process without
/// changing the production path, where [`fs::read`] remains the sole source of
/// executable bytes and [`version_output`] the sole launcher.
fn probe_with(
    requirement: &ToolRequirement,
    source: &ToolchainSource<'_>,
    read_program: ProgramReader,
    version: VersionProbe<'_>,
) -> ToolEvidence {
    let rim = source.rim();
    let Some(resolved) = resolve(requirement.program, rim.directory(), rim.search_path()) else {
        return ToolEvidence::Missing {
            name: requirement.name.to_owned(),
            requested_program: requirement.program.to_owned(),
        };
    };
    probe_resolved_with(
        requirement.name,
        requirement.program,
        requirement.version_argv,
        resolved,
        read_program,
        version,
    )
}

fn probe_resolved_with(
    name: &str,
    requested_program: &str,
    version_argv: &[&str],
    resolved: PathBuf,
    read_program: ProgramReader,
    version: VersionProbe<'_>,
) -> ToolEvidence {
    let unprobed = |failure| ToolEvidence::Unprobed {
        name: name.to_owned(),
        requested_program: requested_program.to_owned(),
        resolved_program: resolved.to_string_lossy().into_owned(),
        failure,
    };
    let Ok(bytes) = read_program(&resolved) else {
        return unprobed(ProbeFailure::Unreadable);
    };
    let launchable =
        fs::metadata(&resolved).is_ok_and(|metadata| is_executable(&resolved, &metadata));
    if !launchable {
        return unprobed(ProbeFailure::NotStarted);
    }
    match version(&resolved, version_argv) {
        Ok(version) => ToolEvidence::Identified {
            name: name.to_owned(),
            requested_program: requested_program.to_owned(),
            resolved_program: resolved.to_string_lossy().into_owned(),
            version,
            digest: Digest::compute(Domain::GateTool, &bytes)
                .as_str()
                .to_owned(),
        },
        Err(failure) => unprobed(failure),
    }
}

/// Resolve one program name against the captured search path.
///
/// Every entry is anchored to the captured rim directory before the program is
/// joined onto it, which leaves an absolute entry exactly as written and turns
/// a relative or empty one into a single absolute file. Without that, the same
/// entry would name one file to this process, which reads and hashes it from
/// wherever it was started, and another to the probe, which runs in the
/// selected tree: two different programs behind one recorded identity.
#[cfg(unix)]
fn resolve(program: &str, rim: &Path, search_path: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(search_path?)
        .map(|directory| rim.join(directory).join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(windows)]
fn resolve(program: &str, rim: &Path, search_path: Option<&OsStr>) -> Option<PathBuf> {
    let extension = Path::new(program).extension();
    std::env::split_paths(search_path?).find_map(|directory| {
        let candidate = rim.join(directory).join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
        extension.is_none().then(|| {
            ["exe", "com", "bat", "cmd"]
                .into_iter()
                .map(|suffix| candidate.with_extension(suffix))
                .find(|path| path.is_file())
        })?
    })
}

/// Run the declared version probe and retain exactly what it printed.
///
/// Nothing is removed. The printed bytes are the identity evidence, and the one
/// normalization the exact-version contracts imply belongs to the comparison
/// that reads them, not to the record that carries them. Output that is not
/// text identifies nothing, and neither does output with no version in it, so
/// both say so rather than being reported as an identity.
fn version_output(
    program: &Path,
    arguments: &[&str],
    root: &Path,
    bounds: &Bounds,
) -> Result<String, ProbeFailure> {
    let argv = Argv::new(program, arguments.iter().map(OsStr::new))
        .map_err(|_| ProbeFailure::NotStarted)?;
    let outcome = spawn(bounds.spec(argv, root))
        .map_err(|_| ProbeFailure::NotStarted)?
        .wait()
        .map_err(|_| ProbeFailure::NotStarted)?;
    let captured = Captured::of(&outcome);
    printed_version(captured.completion, captured.stdout)
}

/// What one reaped version probe identifies, from its ending and stdout.
fn printed_version(
    completion: &Completion,
    stdout: Option<Stream<'_>>,
) -> Result<String, ProbeFailure> {
    match completion {
        Completion::Exited(status) if status.success() => {}
        Completion::Exited(status) => {
            return Err(status.code().map_or(ProbeFailure::Signaled, |code| {
                ProbeFailure::Refused { code }
            }));
        }
        Completion::TimedOut(_) => return Err(ProbeFailure::TimedOut),
        Completion::Cancelled(_) => return Err(ProbeFailure::Signaled),
    }
    let stdout = stdout.ok_or(ProbeFailure::Incomplete)?;
    if !stdout.whole() {
        return Err(ProbeFailure::Incomplete);
    }
    let printed =
        String::from_utf8(stdout.bytes.to_vec()).map_err(|_| ProbeFailure::Undecodable)?;
    if contracted_version(&printed).is_empty() {
        return Err(ProbeFailure::Incomplete);
    }
    Ok(printed)
}

#[cfg(test)]
mod tests;
