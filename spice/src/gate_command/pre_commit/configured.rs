//! Ordered configured-command execution under one staged and policy authority.

use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sill::{
    Argv, Cancellation, Completion, Deadlines, Environment, Output, ProcessGroup, Redaction,
    RestartDisposition, Spec, StdinPolicy, StreamPolicy, WaitOutcome, WaitPolicy,
};
use spice::ambient::Ambient;
use spice::config::{PreCommitStep, pre_commit_declarations};
use spice::repository::Repository;
use wire::{ConfigSource, Digest, Outcome, Refusal, RefusalDiagnostic, TrustState};

use crate::trust_command::Surface;

pub(super) const PATHS_VARIABLE: &str = wire::SpiceVariable::PreCommitPaths.as_str();
pub(super) const SOURCE_VARIABLE: &str = wire::SpiceVariable::PreCommitSource.as_str();

// Sill keeps draining each stream after this retained suffix fills. The limit
// bounds resident and serialized failure evidence; it does not bound how much
// legitimate output a configured command may produce.
const OUTPUT_LIMIT: usize = 256 * 1024;
const SOURCE_GATE_OUTPUT_LIMIT: usize = 1024 * 1024;
const OUTPUT_DRAIN: Duration = Duration::from_secs(2);
const REAP_DEADLINE: Duration = Duration::from_secs(2);
const TERMINATION_GRACE: Duration = Duration::from_secs(2);

const SOURCE_GATE_ARGV: [&str; 14] = [
    "spice",
    "dev",
    "cargo",
    "--",
    "run",
    "--quiet",
    "--locked",
    "--package",
    "spice",
    "--bin",
    "spice",
    "--",
    "gate",
    "selection",
];

/// One ordered configured-command result bound to an immutable staged source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ConfiguredReport {
    source: Digest,
    steps: Vec<StepReport>,
}

impl ConfiguredReport {
    /// The capture ceiling bounds retained diagnostics, not command success.
    pub(super) fn admits(&self) -> bool {
        self.steps.iter().all(|step| step.findings.is_empty())
    }

    pub(super) fn source(&self) -> &Digest {
        &self.source
    }

    #[cfg(test)]
    pub(super) fn steps(&self) -> &[StepReport] {
        &self.steps
    }

    pub(super) fn refusal_evidence(&self) -> String {
        let mut evidence = String::new();
        for (ordinal, step) in self.steps.iter().enumerate() {
            for finding in &step.findings {
                let _ = writeln!(
                    evidence,
                    "ordinal={ordinal} label={} argv={} kind={} detail={}",
                    json(&step.label),
                    json(&step.argv),
                    finding.kind.as_str(),
                    json(&finding.detail),
                );
            }
        }
        evidence
    }
}

/// Execution evidence for one authored declaration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StepReport {
    label: String,
    argv: Vec<String>,
    selected_paths: Vec<String>,
    enabled: bool,
    findings: Vec<StepFinding>,
}

#[cfg(test)]
impl StepReport {
    pub(super) fn label(&self) -> &str {
        &self.label
    }

    pub(super) fn selected_paths(&self) -> &[String] {
        &self.selected_paths
    }

    pub(super) const fn enabled(&self) -> bool {
        self.enabled
    }

    pub(super) fn findings(&self) -> &[StepFinding] {
        &self.findings
    }
}

/// One independently classified failure from a configured child.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StepFinding {
    kind: StepFindingKind,
    detail: String,
}

/// Closed failure vocabulary for configured process supervision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum StepFindingKind {
    Exit,
    Signal,
    Spawn,
    Timeout,
    Cancellation,
    Cleanup,
}

impl StepFindingKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Signal => "signal",
            Self::Spawn => "spawn",
            Self::Timeout => "timeout",
            Self::Cancellation => "cancellation",
            Self::Cleanup => "cleanup",
        }
    }
}

/// Pre-execution or authority failure that stops the ordered battery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ConfiguredFailure {
    reason: &'static str,
    detail: String,
    fault: bool,
}

impl ConfiguredFailure {
    pub(super) const fn reason(&self) -> &'static str {
        self.reason
    }

    pub(super) fn detail(&self) -> &str {
        &self.detail
    }

    pub(super) const fn is_fault(&self) -> bool {
        self.fault
    }

    fn refusal(reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
            fault: false,
        }
    }

    fn fault(reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
            fault: true,
        }
    }
}

#[derive(Clone, Copy)]
struct ExecutionBounds {
    runtime: Duration,
    output_drain: Duration,
    reap: Duration,
    grace: Duration,
    output_limit: usize,
}

impl ExecutionBounds {
    const fn production(runtime: Duration) -> Self {
        Self {
            runtime,
            output_drain: OUTPUT_DRAIN,
            reap: REAP_DEADLINE,
            grace: TERMINATION_GRACE,
            output_limit: OUTPUT_LIMIT,
        }
    }

    const fn stderr_capture_limit(self, source_gate: bool) -> usize {
        if source_gate && self.output_limit < SOURCE_GATE_OUTPUT_LIMIT {
            SOURCE_GATE_OUTPUT_LIMIT
        } else {
            self.output_limit
        }
    }
}

struct DispatchPolicy {
    builtins: bool,
    surface: Surface,
    spice: Option<SpiceRoute>,
    worktree: PathBuf,
    configuration: PathBuf,
    source: ConfigSource,
    tracked: Option<String>,
    commands: Vec<PreCommitStep>,
}

#[derive(Clone)]
struct SpiceRoute {
    reference: steer::WrapperReference,
    resolved: spice::agent_wrapper::Resolved,
}

impl SpiceRoute {
    fn capture(
        reference: steer::WrapperReference,
        repository: &Repository,
    ) -> Result<Self, ConfiguredFailure> {
        let resolved =
            spice::agent_wrapper::resolve(reference, repository).map_err(wrapper_refusal)?;
        Ok(Self {
            reference,
            resolved,
        })
    }

    fn verify(&self, repository: &Repository) -> Result<(), ConfiguredFailure> {
        let observed =
            spice::agent_wrapper::resolve(self.reference, repository).map_err(wrapper_refusal)?;
        if observed == self.resolved {
            return Ok(());
        }
        Err(ConfiguredFailure::refusal(
            "gate::pre-commit-wrapper-moved",
            format!(
                "expected={} observed={}",
                json(&self.resolved.executable().display().to_string()),
                json(&observed.executable().display().to_string()),
            ),
        ))
    }
}

fn wrapper_refusal(error: spice::agent_wrapper::ResolveError) -> ConfiguredFailure {
    ConfiguredFailure::refusal(
        "gate::pre-commit-wrapper-refused",
        format!(
            "reason={} path={} detail={}",
            error.reason(),
            json(&error.path().display().to_string()),
            json(&error.to_string()),
        ),
    )
}

impl DispatchPolicy {
    #[cfg(test)]
    fn load(repository: &Repository) -> Result<Self, ConfiguredFailure> {
        Self::load_scoped(repository, None)
    }

    fn load_scoped(
        repository: &Repository,
        configuration: Option<&spice::config::RepositoryCapture>,
    ) -> Result<Self, ConfiguredFailure> {
        let loaded = spice::config::capture_repository_scoped(repository, &[], configuration)
            .map_err(|error| {
                ConfiguredFailure::refusal("gate::pre-commit-policy-refused", error.to_string())
            })?;
        let loaded = loaded.as_ref();
        let policy = loaded.resolved().root().policy();
        let builtins = policy.pre_commit_builtins();
        let commands = policy.pre_commit().to_vec();
        let spice = if commands.iter().any(|command| {
            command
                .run()
                .first()
                .is_some_and(|program| program == "spice")
        }) {
            loaded
                .resolved()
                .root()
                .wrappers()
                .spice()
                .map(|reference| SpiceRoute::capture(reference, repository))
                .transpose()?
        } else {
            None
        };
        let (configuration, source) = pre_commit_declarations(loaded).map_or_else(
            || {
                (
                    loaded.locations().tracked().to_path_buf(),
                    ConfigSource::Tracked,
                )
            },
            |declarations| {
                (
                    declarations.configuration().to_path_buf(),
                    declarations.source(),
                )
            },
        );
        Ok(Self {
            builtins,
            surface: Surface::from_loaded(loaded),
            spice,
            worktree: repository.worktree_root().to_path_buf(),
            configuration,
            source,
            tracked: loaded.tracked().map(ToOwned::to_owned),
            commands,
        })
    }

    fn verify_selected(&self, selected_root: &Path) -> Result<(), ConfiguredFailure> {
        let selected = match fs::read_to_string(selected_root.join(wire::TRACKED_POLICY_FILE)) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(ConfiguredFailure::fault(
                    "gate::pre-commit-policy-unavailable",
                    error.to_string(),
                ));
            }
        };
        if selected == self.tracked {
            return Ok(());
        }
        Err(ConfiguredFailure::refusal(
            "gate::pre-commit-policy-source-mismatch",
            format!(
                "configuration={} live_present={} staged_present={}",
                json(&self.configuration.display().to_string()),
                self.tracked.is_some(),
                selected.is_some(),
            ),
        ))
    }

    fn verify_current(&self, repository: &Repository) -> Result<(), ConfiguredFailure> {
        match self.surface.is_current(repository) {
            Ok(true) => {}
            Ok(false) => {
                return Err(ConfiguredFailure::refusal(
                    "gate::pre-commit-policy-moved",
                    format!(
                        "configuration={} source={} digest={}",
                        json(&self.configuration.display().to_string()),
                        self.source.as_str(),
                        self.surface.digest(),
                    ),
                ));
            }
            Err(error) => {
                return Err(ConfiguredFailure::refusal(
                    "gate::pre-commit-policy-moved",
                    format!(
                        "configuration={} source={} digest={}; reacquisition={}",
                        json(&self.configuration.display().to_string()),
                        self.source.as_str(),
                        self.surface.digest(),
                        json(&error.to_string()),
                    ),
                ));
            }
        }
        if let Some(spice) = &self.spice {
            spice.verify(repository)?;
        }
        Ok(())
    }

    fn require_approved(&self, repository: &Repository) -> Result<(), ConfiguredFailure> {
        let state = repository.admit_state().map_err(|error| {
            ConfiguredFailure::fault("gate::pre-commit-trust-unavailable", error.to_string())
        })?;
        let reader = journal::JournalReader::open(state.journal()).map_err(|error| {
            ConfiguredFailure::refusal(
                "gate::pre-commit-configuration-untrusted",
                format!(
                    "{} trust_authority={}",
                    self.trust_refusal_detail(None, None),
                    json(&error.to_string()),
                ),
            )
        })?;
        let head = reader
            .read(|read| read.trust_head(self.surface.digest()))
            .map_err(|error| {
                ConfiguredFailure::fault("gate::pre-commit-trust-unavailable", error.to_string())
            })?;
        let standing = head.as_ref().map(|head| head.body().state);
        let producing_worktree = head
            .as_ref()
            .and_then(|head| head.body().worktree.as_deref());
        if standing == Some(TrustState::Granted) && producing_worktree.is_some() {
            return Ok(());
        }
        Err(ConfiguredFailure::refusal(
            "gate::pre-commit-configuration-untrusted",
            self.trust_refusal_detail(standing, producing_worktree),
        ))
    }

    fn trust_refusal_detail(
        &self,
        standing: Option<TrustState>,
        producing_worktree: Option<&str>,
    ) -> String {
        let producing_worktree = producing_worktree.map_or_else(|| "absent".to_owned(), json);
        let digest = self.surface.digest();
        let recovery = format!(
            "a lane holding a claim keeps it and ends the turn, never deferring or freeing it: the turn end pends an absent digest and parks the claim on it, as `spice task require HANDLE --trust-grant {digest}` does; outside a lane's held work the operator pends an absent digest with `spice trust pend` from that worktree; the operator runs `spice trust grant {digest}` only after reviewing these exact bytes and command words",
        );
        format!(
            concat!(
                "surface=configuration digest={} standing={} producer_worktree={} provenance={} ",
                "worktree={} configuration={} commands={}; {}"
            ),
            self.surface.digest(),
            standing.map_or("absent", TrustState::as_str),
            producing_worktree,
            self.source.as_str(),
            json(&self.worktree.display().to_string()),
            json(&self.configuration.display().to_string()),
            json(&self.commands),
            recovery,
        )
    }
}

/// Semantic staged authority retained across configured pre-commit handoffs.
pub(super) struct DispatchAuthority {
    source: Digest,
    selected_paths: Vec<String>,
    staged: gate::tree::StagedAuthority,
}

impl DispatchAuthority {
    fn capture(anchor: &spice::gate::PreparedArm<'_>) -> Result<Self, ConfiguredFailure> {
        let staged = anchor.staged_authority().cloned().ok_or_else(|| {
            ConfiguredFailure::fault(
                "gate::pre-commit-staged-source-refused",
                "configured pre-commit captured a non-staged authority",
            )
        })?;
        Ok(Self {
            source: anchor.source().clone(),
            selected_paths: anchor.changes().to_vec(),
            staged,
        })
    }

    fn movement(&self, observed: &Self) -> Option<String> {
        let staged = self.staged.differences(&observed.staged);
        let mut movement = Vec::new();
        if staged.contains(&gate::tree::StagedAuthorityDifference::Tree) {
            movement.push("staged-tree");
        }
        if self.selected_paths != observed.selected_paths {
            movement.push("selected-paths");
        }
        if staged.contains(&gate::tree::StagedAuthorityDifference::RenameBase) {
            movement.push("rename-base");
        }
        if staged.contains(&gate::tree::StagedAuthorityDifference::RenameLineage) {
            movement.push("rename-lineage");
        }
        if self.source != observed.source {
            movement.push("source-digest");
        }
        if movement.is_empty() {
            return None;
        }
        Some(format!(
            "movement={} expected_source={} observed_source={} expected_selected_paths={} observed_selected_paths={}",
            json(&movement),
            self.source,
            observed.source,
            json(&self.selected_paths),
            json(&observed.selected_paths),
        ))
    }
}

/// One typed dispatch policy and one staged source retained for the whole battery.
pub(super) struct Dispatch<'a> {
    repository: Repository,
    ambient: &'a Ambient,
    policy: DispatchPolicy,
    claim: gate::ClaimCapture,
    anchor: spice::gate::PreparedArm<'static>,
    authority: DispatchAuthority,
}

pub(super) struct AdmissionDispatch<'a> {
    repository: Repository,
    ambient: &'a Ambient,
    policy: DispatchPolicy,
    claim: gate::ClaimCapture,
    authority: DispatchAuthority,
}

impl<'a> Dispatch<'a> {
    pub(super) fn capture_with_claim(
        claim: gate::ClaimCapture,
        ambient: &'a Ambient,
    ) -> Result<Self, ConfiguredFailure> {
        let current = env::current_dir().map_err(|error| {
            ConfiguredFailure::fault(
                "gate::pre-commit-working-directory-unavailable",
                error.to_string(),
            )
        })?;
        let repository = Repository::discover(&current).map_err(|error| {
            ConfiguredFailure::fault(spice::repository::ResolveError::REASON, error.to_string())
        })?;
        Self::capture_at_with_claim_and_configuration(repository, ambient, claim, None)
    }

    pub(super) fn capture_at_with_claim_and_configuration(
        repository: Repository,
        ambient: &'a Ambient,
        claim: gate::ClaimCapture,
        configuration: Option<&spice::config::RepositoryCapture>,
    ) -> Result<Self, ConfiguredFailure> {
        let state = repository.admit_state().map_err(|error| {
            ConfiguredFailure::fault("gate::pre-commit-state-unavailable", error.to_string())
        })?;
        fs::create_dir_all(state.locks()).map_err(|error| {
            ConfiguredFailure::fault("gate::pre-commit-state-unavailable", error.to_string())
        })?;
        let anchor = spice::gate::prepare_arm_at_with_claim(
            ambient,
            repository.worktree_root(),
            gate::Selector::staged(),
            gate::GateId::Staging,
            claim.clone(),
        )
        .map_err(|error| {
            ConfiguredFailure::refusal("gate::pre-commit-staged-source-refused", error.to_string())
        })?;
        let policy = DispatchPolicy::load_scoped(&repository, configuration)?;
        policy.verify_selected(anchor.root())?;
        let authority = DispatchAuthority::capture(&anchor)?;
        let mut dispatch = Self {
            repository,
            ambient,
            policy,
            claim,
            anchor,
            authority,
        };
        if !dispatch.admission_enabled() {
            dispatch.verify()?;
        }
        Ok(dispatch)
    }

    pub(super) const fn ambient(&self) -> &'a Ambient {
        self.ambient
    }

    pub(super) const fn builtins_enabled(&self) -> bool {
        self.policy.builtins
    }

    pub(super) fn admission_enabled(&self) -> bool {
        self.policy.builtins || self.policy.commands.iter().any(is_source_gate_contract)
    }

    /// Which worktree paths the staged snapshot this dispatch measures left out.
    pub(super) fn worktree_divergence(
        &self,
    ) -> Result<gate::tree::WorktreeDivergence, ConfiguredFailure> {
        self.anchor.worktree_divergence().map_err(|error| {
            ConfiguredFailure::refusal("gate::snapshot-divergence-refused", error.to_string())
        })
    }

    pub(super) fn require_admission_approved(&self) -> Result<(), ConfiguredFailure> {
        if self.policy.commands.iter().any(is_source_gate_contract) {
            self.policy.require_approved(&self.repository)?;
        }
        Ok(())
    }

    pub(super) fn has_configured(&self) -> bool {
        self.policy
            .commands
            .iter()
            .any(|command| !is_source_gate_contract(command))
    }

    pub(super) fn configured_count(&self) -> usize {
        self.policy
            .commands
            .iter()
            .filter(|command| !is_source_gate_contract(command))
            .count()
    }

    pub(super) fn source(&self) -> &Digest {
        &self.authority.source
    }

    pub(super) fn verify(&mut self) -> Result<(), ConfiguredFailure> {
        match self.anchor.verify_refreshable_source().map_err(|error| {
            ConfiguredFailure::refusal(
                "gate::pre-commit-staged-source-moved",
                format!("source={}; {}", self.source(), error.detail()),
            )
        })? {
            gate::tree::RefreshableSourceState::Current => {}
            gate::tree::RefreshableSourceState::IndexChanged => {
                self.refresh_anchor("index-representation-refresh")?;
            }
        }
        self.policy.verify_current(&self.repository)
    }

    fn refresh_anchor(&mut self, trigger: &str) -> Result<(), ConfiguredFailure> {
        let anchor = spice::gate::prepare_arm_at_with_claim(
            self.ambient,
            self.repository.worktree_root(),
            gate::Selector::staged(),
            gate::GateId::Staging,
            self.claim.clone(),
        )
        .map_err(|error| {
            ConfiguredFailure::refusal(
                "gate::pre-commit-staged-source-moved",
                format!(
                    "trigger={}; staged semantic recapture refused: {}",
                    json(trigger),
                    error.detail(),
                ),
            )
        })?;
        let observed = DispatchAuthority::capture(&anchor)?;
        if let Some(movement) = self.authority.movement(&observed) {
            return Err(ConfiguredFailure::refusal(
                "gate::pre-commit-staged-source-moved",
                format!("trigger={}; {movement}", json(trigger)),
            ));
        }
        self.policy.verify_selected(anchor.root())?;
        self.anchor = anchor;
        Ok(())
    }

    pub(super) fn rebind(self, expected: &Digest) -> Result<Self, ConfiguredFailure> {
        let Self {
            repository,
            ambient,
            policy,
            claim,
            anchor,
            authority,
        } = self;
        if &authority.source != expected {
            return Err(ConfiguredFailure::refusal(
                "gate::pre-commit-staged-source-moved",
                format!(
                    "dispatcher source {expected} differs from retained source {}",
                    authority.source,
                ),
            ));
        }
        let mut rebound = Self {
            repository,
            ambient,
            policy,
            claim,
            anchor,
            authority,
        };
        rebound.refresh_anchor("battery-handoff")?;
        rebound.verify()?;
        Ok(rebound)
    }

    pub(super) fn into_admission(
        self,
    ) -> Result<(AdmissionDispatch<'a>, spice::gate::PreparedArm<'static>), ConfiguredFailure> {
        let Self {
            repository,
            ambient,
            policy,
            claim,
            anchor,
            authority,
        } = self;
        Ok((
            AdmissionDispatch {
                repository,
                ambient,
                policy,
                claim,
                authority,
            },
            anchor,
        ))
    }

    pub(super) fn execute_configured(
        &mut self,
        runtime: Duration,
        cancellation: Cancellation,
    ) -> Result<Option<ConfiguredReport>, ConfiguredFailure> {
        if !self.has_configured() {
            return Ok(None);
        }
        // The configured command rim extends a sorted copy of the caller environment.
        let mut base = self.ambient.variables().to_vec();
        base.sort();
        if let Some((_, source)) = base
            .iter()
            .find(|(name, _)| name == OsStr::new(SOURCE_VARIABLE))
        {
            return Err(ConfiguredFailure::refusal(
                "gate::pre-commit-recursion",
                format!(
                    "configured command redispatched spice gate pre-commit from staged source {}",
                    json(&source.to_string_lossy()),
                ),
            ));
        }
        let source = self.source().clone();
        let paths = self.authority.selected_paths.clone();
        let root = self.repository.worktree_root().to_path_buf();
        let commands = self
            .policy
            .commands
            .iter()
            .filter(|command| !is_source_gate_contract(command))
            .cloned()
            .collect::<Vec<_>>();
        let spice = self
            .policy
            .spice
            .as_ref()
            .map(|route| route.resolved.executable().to_path_buf());
        let mut verify = || {
            self.verify()?;
            self.policy.require_approved(&self.repository)
        };
        verify()?;
        let process = StepProcess {
            root: &root,
            base_environment: &base,
            spice: spice.as_deref(),
            bounds: ExecutionBounds::production(runtime),
            cancellation,
        };
        execute_steps(&commands, &source, &paths, &process, &mut verify).map(Some)
    }
}

impl<'a> AdmissionDispatch<'a> {
    pub(super) const fn repository(&self) -> &Repository {
        &self.repository
    }

    pub(super) const fn ambient(&self) -> &'a Ambient {
        self.ambient
    }

    pub(super) fn resume(
        self,
        staged: spice::change_gate::StagedActionSelection,
    ) -> Result<Dispatch<'a>, ConfiguredFailure> {
        let anchor = staged.into_anchor();
        let observed = DispatchAuthority::capture(&anchor)?;
        if let Some(movement) = self.authority.movement(&observed) {
            return Err(ConfiguredFailure::refusal(
                "gate::pre-commit-staged-source-moved",
                format!("trigger=admission-handoff; {movement}"),
            ));
        }
        self.policy.verify_selected(anchor.root())?;
        let mut dispatch = Dispatch {
            repository: self.repository,
            ambient: self.ambient,
            policy: self.policy,
            claim: self.claim,
            anchor,
            authority: observed,
        };
        dispatch.verify()?;
        Ok(dispatch)
    }
}

pub(super) const fn cleanup_reserve() -> Duration {
    OUTPUT_DRAIN
        .saturating_add(REAP_DEADLINE)
        .saturating_add(TERMINATION_GRACE)
}

/// How every configured step of one run is spawned: its working root, the
/// ambient environment each step extends, the resolved `spice` wrapper, and
/// the shared bounds and cancellation.
struct StepProcess<'a> {
    root: &'a Path,
    base_environment: &'a [(OsString, OsString)],
    spice: Option<&'a Path>,
    bounds: ExecutionBounds,
    cancellation: Cancellation,
}

fn execute_steps(
    commands: &[PreCommitStep],
    source: &Digest,
    staged_paths: &[String],
    process: &StepProcess<'_>,
    verify: &mut impl FnMut() -> Result<(), ConfiguredFailure>,
) -> Result<ConfiguredReport, ConfiguredFailure> {
    let StepProcess {
        root,
        base_environment,
        spice,
        bounds,
        ref cancellation,
    } = *process;
    let started = Instant::now();
    let mut reports = Vec::with_capacity(commands.len());
    for command in commands {
        verify()?;
        let selected_paths = scoped_paths(command, staged_paths)?;
        if !command.scopes().paths().is_empty() && selected_paths.is_empty() {
            reports.push(step_report(
                command,
                command.run().to_vec(),
                selected_paths,
                false,
                Vec::new(),
            ));
            continue;
        }
        let argv = effective_argv(command);
        let process_argv = process_argv(&argv, spice)?;
        let remaining = bounds.runtime.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            reports.push(step_report(
                command,
                argv,
                selected_paths,
                true,
                vec![finding(
                    StepFindingKind::Timeout,
                    "shared configured-command deadline elapsed before spawn",
                )],
            ));
            break;
        }
        let environment = command_environment(base_environment, source, &selected_paths)?;
        let spec = command_spec(
            command,
            &process_argv,
            root,
            environment,
            remaining,
            bounds,
            cancellation.clone(),
        )?;
        let findings = match sill::spawn(spec) {
            Ok(running) => match running.wait() {
                Ok(waited) => classify_wait(&waited),
                Err(error) => vec![finding(StepFindingKind::Cleanup, error.to_string())],
            },
            Err(error) => vec![finding(StepFindingKind::Spawn, error.to_string())],
        };
        let terminal = findings.iter().any(|finding| {
            matches!(
                finding.kind,
                StepFindingKind::Timeout | StepFindingKind::Cancellation | StepFindingKind::Cleanup
            )
        });
        reports.push(step_report(command, argv, selected_paths, true, findings));
        verify()?;
        if terminal {
            break;
        }
    }
    verify()?;
    Ok(ConfiguredReport {
        source: source.clone(),
        steps: reports,
    })
}

fn is_source_gate_contract(command: &PreCommitStep) -> bool {
    command.scopes().paths().is_empty()
        && command
            .run()
            .iter()
            .map(String::as_str)
            .eq(SOURCE_GATE_ARGV)
}

fn effective_argv(command: &PreCommitStep) -> Vec<String> {
    command.run().to_vec()
}

fn process_argv(argv: &[String], spice: Option<&Path>) -> Result<Vec<OsString>, ConfiguredFailure> {
    let Some((program, arguments)) = argv.split_first() else {
        unreachable!("typed pre-commit argv is nonempty");
    };
    if program != "spice" {
        return Ok(argv.iter().map(OsString::from).collect());
    }
    let executable = spice.ok_or_else(|| {
        ConfiguredFailure::refusal(
            "gate::pre-commit-wrapper-missing",
            "configured command names spice but wrappers.spice is absent",
        )
    })?;
    let mut resolved = Vec::with_capacity(argv.len());
    resolved.push(executable.as_os_str().to_owned());
    resolved.extend(arguments.iter().map(OsString::from));
    Ok(resolved)
}

fn scoped_paths(
    command: &PreCommitStep,
    staged_paths: &[String],
) -> Result<Vec<String>, ConfiguredFailure> {
    let scopes = command.scopes().paths();
    if scopes.is_empty() {
        return Ok(staged_paths.to_vec());
    }
    let mut selected = Vec::new();
    for path in staged_paths {
        let mut matches = false;
        for scope in scopes {
            matches |= gate::tracked_path_selector_matches(scope, path).map_err(|error| {
                ConfiguredFailure::fault("gate::pre-commit-scope-invalid", error)
            })?;
        }
        if matches {
            selected.push(path.clone());
        }
    }
    Ok(selected)
}

fn command_spec(
    command: &PreCommitStep,
    argv: &[OsString],
    root: &Path,
    environment: Environment,
    runtime: Duration,
    bounds: ExecutionBounds,
    cancellation: Cancellation,
) -> Result<Spec, ConfiguredFailure> {
    let (program, arguments) = argv
        .split_first()
        .expect("typed pre-commit argv is nonempty");
    let stdout = StreamPolicy::CaptureTail {
        limit: bounds.output_limit,
    };
    let stderr = StreamPolicy::CaptureTail {
        limit: bounds.stderr_capture_limit(is_source_gate_contract(command)),
    };
    Ok(Spec {
        argv: Argv::new(program.clone(), arguments.iter().cloned()).map_err(|error| {
            ConfiguredFailure::fault("gate::pre-commit-command-invalid", error.to_string())
        })?,
        cwd: root.to_path_buf(),
        environment,
        group: ProcessGroup::New,
        deadlines: Deadlines::new(runtime, bounds.output_drain, bounds.reap).map_err(|error| {
            ConfiguredFailure::fault("gate::pre-commit-command-invalid", error.to_string())
        })?,
        wait: WaitPolicy::new(
            format!("configured pre-commit step {}", command.label()),
            cancellation,
            RestartDisposition::Never,
        )
        .map_err(|error| {
            ConfiguredFailure::fault("gate::pre-commit-command-invalid", error.to_string())
        })?,
        output: Output::new(StdinPolicy::Null, stdout, stderr),
        redaction: Redaction::All,
        grace: bounds.grace,
    })
}

fn command_environment(
    base: &[(OsString, OsString)],
    source: &Digest,
    paths: &[String],
) -> Result<Environment, ConfiguredFailure> {
    let mut entries = base
        .iter()
        .filter(|(name, _)| !spice::gate::git_local_environment(name))
        .cloned()
        .collect::<BTreeMap<_, _>>();
    entries.remove(OsStr::new(PATHS_VARIABLE));
    entries.remove(OsStr::new(SOURCE_VARIABLE));
    entries.insert(OsString::from(PATHS_VARIABLE), OsString::from(json(paths)));
    entries.insert(
        OsString::from(SOURCE_VARIABLE),
        OsString::from(source.to_string()),
    );
    Environment::new(entries).map_err(|error| {
        ConfiguredFailure::fault("gate::pre-commit-environment-invalid", error.to_string())
    })
}

fn classify_wait(waited: &WaitOutcome) -> Vec<StepFinding> {
    let mut findings = Vec::new();
    if let Some(failure) = waited.observation_failure() {
        findings.push(finding(
            StepFindingKind::Cleanup,
            format!("live source-gate progress failed: {failure}"),
        ));
    }
    if !waited.outputs_drained() {
        findings.push(finding(
            StepFindingKind::Cleanup,
            "captured output did not drain before its cleanup deadline",
        ));
    }
    match waited.completion() {
        Completion::TimedOut(status) => findings.push(finding(
            StepFindingKind::Timeout,
            format!(
                "status={}; stdout={}; stderr={}",
                status_text(status),
                captured(waited.stdout()),
                captured(waited.stderr())
            ),
        )),
        Completion::Cancelled(status) => findings.push(finding(
            StepFindingKind::Cancellation,
            format!(
                "status={}; stdout={}; stderr={}",
                status_text(status),
                captured(waited.stdout()),
                captured(waited.stderr())
            ),
        )),
        Completion::Exited(status) if !status.success() => {
            let (kind, completion) = ended_by_signal(status).map_or_else(
                || (StepFindingKind::Exit, format!("code={:?}", status.code())),
                |signal| (StepFindingKind::Signal, format!("signal={signal}")),
            );
            findings.push(finding(
                kind,
                format!(
                    "{completion}; stdout={}; stderr={}",
                    captured(waited.stdout()),
                    captured(waited.stderr())
                ),
            ));
        }
        Completion::Exited(_) => {}
    }
    findings
}

fn status_text(status: &std::process::ExitStatus) -> String {
    ended_by_signal(status).map_or_else(
        || format!("code={:?}", status.code()),
        |signal| format!("signal={signal}"),
    )
}

/// The signal that ended `status`. Only Unix ends processes by signal; a
/// Windows process always leaves an exit code, even when it was killed.
fn ended_by_signal(status: &std::process::ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status.signal()
    }
    #[cfg(windows)]
    {
        let _ = status;
        None
    }
}

fn captured(output: Option<&sill::CapturedOutput>) -> String {
    output.map_or_else(
        || "null".to_owned(),
        |output| {
            let retained = json(&String::from_utf8_lossy(output.bytes()));
            if output.truncated() {
                let retained_end = if output.retains_suffix() {
                    "suffix"
                } else {
                    "prefix"
                };
                format!("{retained} (capture truncated to retained {retained_end})")
            } else {
                retained
            }
        },
    )
}

fn step_report(
    command: &PreCommitStep,
    argv: Vec<String>,
    selected_paths: Vec<String>,
    enabled: bool,
    findings: Vec<StepFinding>,
) -> StepReport {
    StepReport {
        label: command.label().to_owned(),
        argv,
        selected_paths,
        enabled,
        findings,
    }
}

fn finding(kind: StepFindingKind, detail: impl Into<String>) -> StepFinding {
    StepFinding {
        kind,
        detail: detail.into(),
    }
}

pub(super) fn recursion_refusal<T>(source: &OsStr) -> Outcome<T> {
    Outcome::Refusal(Refusal::bare(
        RefusalDiagnostic::new(
            "gate::pre-commit-recursion",
            Some("one non-recursive configured pre-commit dispatch".to_owned()),
            Some("SPICE_PRE_COMMIT_SOURCE absent at the command rim".to_owned()),
            Some(format!(
                "SPICE_PRE_COMMIT_SOURCE={}",
                json(&source.to_string_lossy()),
            )),
        )
        .with_detail(
            "remove `spice gate pre-commit` from policy.pre_commit; configured commands cannot redispatch their own gate",
        ),
    ))
}

fn json(value: &(impl Serialize + ?Sized)) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"<unencodable>\"".to_owned())
}

#[cfg(test)]
#[path = "configured_tests.rs"]
mod tests;
