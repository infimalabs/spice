//! Durable identity and provider-run state for resumable hosted releases.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use spice::repository::Repository;
use windows_execution::hosted::client::{
    Attach, HostedRunBinding, HostedRunObserver, ReleaseInputs, ReleaseWorkflowBlobs,
    WindowsHostedError,
};

const SCHEMA: u32 = 1;

/// One native platform in the aggregate hosted release.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum NativeTarget {
    Linux,
    Macos,
    Windows,
}

impl NativeTarget {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Linux => "Linux",
            Self::Macos => "macOS",
            Self::Windows => "Windows",
        }
    }

    fn workflow(self, workflows: &ReleaseWorkflowBlobs) -> &str {
        match self {
            Self::Linux => &workflows.linux,
            Self::Macos => &workflows.macos,
            Self::Windows => &workflows.windows,
        }
    }
}

/// Workflow selection for the next observation or dispatch of one target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TargetWorkflow {
    Source,
    Current,
    Exact(String),
}

/// Exclusive, atomically published session state for one provider preflight.
pub(crate) struct ReleaseSession {
    path: PathBuf,
    state: Arc<Mutex<SessionFile>>,
    _possession: sill::Possession,
}

impl ReleaseSession {
    pub(crate) fn open(
        inputs_run: u64,
        workflows: ReleaseWorkflowBlobs,
        inputs: &ReleaseInputs,
    ) -> Result<Self, SessionError> {
        let current = std::env::current_dir().map_err(|source| SessionError::Io {
            path: PathBuf::from("."),
            source,
        })?;
        let repository = Repository::discover(&current)
            .map_err(|source| SessionError::Repository(source.to_string()))?;
        let state_paths = repository
            .admit_state()
            .map_err(|source| SessionError::Repository(source.to_string()))?;
        let directory = state_paths.release_operations().join("hosted");
        fs::create_dir_all(&directory).map_err(|source| SessionError::Io {
            path: directory.clone(),
            source,
        })?;
        fs::create_dir_all(state_paths.locks()).map_err(|source| SessionError::Io {
            path: state_paths.locks().to_path_buf(),
            source,
        })?;
        let path = directory.join(format!("{inputs_run}.json"));
        let lock_name = sill::LockName::new(format!("hosted-release-{inputs_run}"))
            .expect("a positive decimal run id produces one normal lock name");
        let lock_path = state_paths
            .locks()
            .join(format!("hosted-release-{inputs_run}"));
        let possession = match sill::LockFile::new(lock_name, &lock_path)
            .try_acquire()
            .map_err(|source| SessionError::Io {
                path: lock_path,
                source,
            })? {
            sill::Acquisition::Acquired(possession) => possession,
            sill::Acquisition::Contended => return Err(SessionError::Contended { inputs_run }),
        };
        let identity = ReleaseInputIdentity::from(inputs);
        let state = match fs::read(&path) {
            Ok(bytes) => {
                let state: SessionFile =
                    serde_json::from_slice(&bytes).map_err(SessionError::Decode)?;
                state.admit(inputs_run, &workflows, &identity)?;
                state
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                let state = SessionFile::new(inputs_run, workflows, identity);
                publish(&path, &state)?;
                state
            }
            Err(source) => {
                return Err(SessionError::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        Ok(Self {
            path,
            state: Arc::new(Mutex::new(state)),
            _possession: possession,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn attachment(&self, target: NativeTarget) -> Result<Option<Attach>, SessionError> {
        let state = self.lock()?;
        Ok(state.target(target).attachment())
    }

    pub(crate) fn workflow(&self, target: NativeTarget) -> Result<TargetWorkflow, SessionError> {
        let state = self.lock()?;
        Ok(state.target(target).workflow())
    }

    pub(crate) fn recorder(&self, target: NativeTarget) -> TargetRecorder {
        TargetRecorder {
            target,
            path: self.path.clone(),
            state: Arc::clone(&self.state),
        }
    }

    pub(crate) fn succeeded(&self, target: NativeTarget) -> Result<(), SessionError> {
        self.update(target, TargetState::succeeded)
    }

    pub(crate) fn failed(
        &self,
        target: NativeTarget,
        terminal: bool,
        schema: bool,
        detail: String,
    ) -> Result<(), SessionError> {
        if !self.lock()?.target(target).has_attempt() {
            return Ok(());
        }
        self.update(target, |state| state.failed(terminal, schema, detail))
    }

    fn update(
        &self,
        target: NativeTarget,
        mutate: impl FnOnce(&mut TargetState) -> Result<(), SessionError>,
    ) -> Result<(), SessionError> {
        let mut state = self.lock()?;
        let mut next = state.clone();
        mutate(next.target_mut(target))?;
        publish(&self.path, &next)?;
        *state = next;
        Ok(())
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, SessionFile>, SessionError> {
        self.state
            .lock()
            .map_err(|_| SessionError::Refused("release session lock is poisoned".to_owned()))
    }
}

/// Observer dedicated to one native target.
pub(crate) struct TargetRecorder {
    target: NativeTarget,
    path: PathBuf,
    state: Arc<Mutex<SessionFile>>,
}

impl HostedRunObserver for TargetRecorder {
    fn observe(&self, binding: &HostedRunBinding) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "release session lock is poisoned".to_owned())?;
        let mut next = state.clone();
        next.record(self.target, binding)
            .map_err(|error| error.to_string())?;
        publish(&self.path, &next).map_err(|error| error.to_string())?;
        *state = next;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionFile {
    schema: u32,
    inputs_run: u64,
    workflows: ReleaseWorkflowBlobs,
    release_inputs: ReleaseInputIdentity,
    targets: Targets,
}

impl SessionFile {
    fn new(
        inputs_run: u64,
        workflows: ReleaseWorkflowBlobs,
        release_inputs: ReleaseInputIdentity,
    ) -> Self {
        Self {
            schema: SCHEMA,
            inputs_run,
            workflows,
            release_inputs,
            targets: Targets::default(),
        }
    }

    fn admit(
        &self,
        inputs_run: u64,
        workflows: &ReleaseWorkflowBlobs,
        release_inputs: &ReleaseInputIdentity,
    ) -> Result<(), SessionError> {
        if self.schema != SCHEMA {
            return Err(SessionError::Refused(format!(
                "release session schema is {}, expected {SCHEMA}",
                self.schema
            )));
        }
        if self.inputs_run != inputs_run {
            return Err(SessionError::Refused(format!(
                "release session names preflight {}, not {inputs_run}",
                self.inputs_run
            )));
        }
        if &self.workflows != workflows {
            return Err(SessionError::Refused(
                "hosted release workflow identities changed; refusing session reuse".to_owned(),
            ));
        }
        if &self.release_inputs != release_inputs {
            return Err(SessionError::Refused(
                "hosted release input identities changed; refusing session reuse".to_owned(),
            ));
        }
        Ok(())
    }

    fn target(&self, target: NativeTarget) -> &TargetState {
        self.targets.get(target)
    }

    fn target_mut(&mut self, target: NativeTarget) -> &mut TargetState {
        self.targets.get_mut(target)
    }

    fn record(
        &mut self,
        target: NativeTarget,
        binding: &HostedRunBinding,
    ) -> Result<(), SessionError> {
        let source_workflow = target.workflow(&self.workflows);
        let state = self.targets.get_mut(target);
        if !binding.attached
            && !state.accepts_fresh_workflow(source_workflow, &binding.workflow_blob)
        {
            return Err(SessionError::Refused(format!(
                "{} run {} uses workflow {}, which is not admissible after its previous attempt",
                target.name(),
                binding.run_id,
                binding.workflow_blob,
            )));
        }
        state.record(binding)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Targets {
    linux: TargetState,
    macos: TargetState,
    windows: TargetState,
}

impl Targets {
    fn get(&self, target: NativeTarget) -> &TargetState {
        match target {
            NativeTarget::Linux => &self.linux,
            NativeTarget::Macos => &self.macos,
            NativeTarget::Windows => &self.windows,
        }
    }

    fn get_mut(&mut self, target: NativeTarget) -> &mut TargetState {
        match target {
            NativeTarget::Linux => &mut self.linux,
            NativeTarget::Macos => &mut self.macos,
            NativeTarget::Windows => &mut self.windows,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct TargetState {
    attempts: Vec<Attempt>,
}

impl TargetState {
    fn attachment(&self) -> Option<Attach> {
        self.attempts
            .last()
            .and_then(|attempt| match attempt.outcome {
                AttemptOutcome::Failed { .. } | AttemptOutcome::SchemaFailed { .. } => None,
                AttemptOutcome::Running { .. } | AttemptOutcome::Succeeded => {
                    Some(Attach::Run(attempt.run_id))
                }
            })
    }

    /// The source workflow until one of its results proves unadmittable; from
    /// then on every fresh dispatch binds the branch's current workflow, so a
    /// later ordinary failure retries instead of refusing the source again.
    fn workflow(&self) -> TargetWorkflow {
        match self.attempts.last() {
            None => TargetWorkflow::Source,
            Some(Attempt {
                outcome: AttemptOutcome::Failed { .. } | AttemptOutcome::SchemaFailed { .. },
                ..
            }) if self
                .attempts
                .iter()
                .any(|attempt| matches!(attempt.outcome, AttemptOutcome::SchemaFailed { .. })) =>
            {
                TargetWorkflow::Current
            }
            Some(Attempt {
                outcome: AttemptOutcome::Failed { .. },
                ..
            }) => TargetWorkflow::Source,
            Some(attempt) => TargetWorkflow::Exact(attempt.workflow_blob.clone()),
        }
    }

    fn accepts_fresh_workflow(&self, source: &str, observed: &str) -> bool {
        match self.workflow() {
            TargetWorkflow::Source => observed == source,
            TargetWorkflow::Current => true,
            TargetWorkflow::Exact(_) => false,
        }
    }

    fn has_attempt(&self) -> bool {
        !self.attempts.is_empty()
    }

    fn record(&mut self, binding: &HostedRunBinding) -> Result<(), SessionError> {
        if let Some(attempt) = self
            .attempts
            .iter_mut()
            .find(|attempt| attempt.run_id == binding.run_id)
        {
            if attempt.challenge != binding.challenge
                || attempt.workflow_blob != binding.workflow_blob
            {
                return Err(SessionError::Refused(format!(
                    "provider run {} changed its bound challenge or workflow",
                    binding.run_id
                )));
            }
            if !binding.attached {
                return Err(SessionError::Refused(format!(
                    "fresh dispatch reused recorded provider run {}",
                    binding.run_id
                )));
            }
            if matches!(attempt.outcome, AttemptOutcome::Running { .. }) {
                attempt.outcome = AttemptOutcome::Running { last_error: None };
            }
            return Ok(());
        }
        if binding.attached {
            return Err(SessionError::Refused(format!(
                "attached provider run {} is absent from the release session",
                binding.run_id
            )));
        }
        if self.attempts.last().is_some_and(|attempt| {
            !matches!(
                attempt.outcome,
                AttemptOutcome::Failed { .. } | AttemptOutcome::SchemaFailed { .. }
            )
        }) {
            return Err(SessionError::Refused(
                "a healthy provider run is already recorded for this target".to_owned(),
            ));
        }
        self.attempts.push(Attempt {
            challenge: binding.challenge.clone(),
            run_id: binding.run_id,
            workflow_blob: binding.workflow_blob.clone(),
            outcome: AttemptOutcome::Running { last_error: None },
        });
        Ok(())
    }

    fn succeeded(&mut self) -> Result<(), SessionError> {
        let attempt = self.current_mut()?;
        attempt.outcome = AttemptOutcome::Succeeded;
        Ok(())
    }

    fn failed(&mut self, terminal: bool, schema: bool, detail: String) -> Result<(), SessionError> {
        let attempt = self.current_mut()?;
        if !terminal && matches!(attempt.outcome, AttemptOutcome::Succeeded) {
            return Ok(());
        }
        attempt.outcome = if schema {
            AttemptOutcome::SchemaFailed { detail }
        } else if terminal {
            AttemptOutcome::Failed { detail }
        } else {
            AttemptOutcome::Running {
                last_error: Some(detail),
            }
        };
        Ok(())
    }

    fn current_mut(&mut self) -> Result<&mut Attempt, SessionError> {
        self.attempts.last_mut().ok_or_else(|| {
            SessionError::Refused(
                "native outcome arrived before its provider run binding".to_owned(),
            )
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    challenge: String,
    run_id: u64,
    workflow_blob: String,
    outcome: AttemptOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
enum AttemptOutcome {
    Running { last_error: Option<String> },
    Succeeded,
    Failed { detail: String },
    SchemaFailed { detail: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReleaseInputIdentity {
    challenge: String,
    source_commit: String,
    source: String,
    archive_sha256: String,
    tree: String,
    app: String,
    canvas: String,
    proof_identity: String,
    proof_archive_sha256: String,
    workflow_sha: String,
    run_id: u64,
}

impl From<&ReleaseInputs> for ReleaseInputIdentity {
    fn from(inputs: &ReleaseInputs) -> Self {
        let manifest = inputs.manifest();
        Self {
            challenge: manifest.challenge.clone(),
            source_commit: manifest.source_commit.clone(),
            source: manifest.source.to_string(),
            archive_sha256: manifest.archive_sha256.clone(),
            tree: manifest.tree.clone(),
            app: manifest.app.to_string(),
            canvas: manifest.canvas.to_string(),
            proof_identity: manifest.proof_identity.clone(),
            proof_archive_sha256: manifest.proof_archive_sha256.clone(),
            workflow_sha: manifest.workflow_sha.clone(),
            run_id: manifest.run_id,
        }
    }
}

fn publish(path: &Path, state: &SessionFile) -> Result<(), SessionError> {
    let destination = atomic::Destination::new(path).map_err(SessionError::Atomic)?;
    let mut staged = destination.stage().map_err(SessionError::Atomic)?;
    serde_json::to_writer_pretty(&mut staged, state).map_err(SessionError::Encode)?;
    staged.write_all(b"\n").map_err(|source| SessionError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    staged.commit().map_err(SessionError::Atomic)
}

/// Whether this error proves the provider run itself reached failure, or that
/// its finished result can never be admitted by this client. An observed run
/// left whole after it concluded in failure is still a failed run.
pub(crate) fn terminal_provider_failure(error: &WindowsHostedError) -> bool {
    match error {
        WindowsHostedError::Detached { reason, .. } => terminal_provider_failure(reason),
        _ => matches!(
            error,
            WindowsHostedError::Command {
                operation: "workflow execution",
                ..
            } | WindowsHostedError::Cancelled { run_id: Some(_) }
                | WindowsHostedError::ResultSchema(_)
                | WindowsHostedError::OutputContract(_)
        ),
    }
}

/// Whether a finished, authentic run's output can never be admitted by this
/// client, so only a fresh dispatch of the current workflow can supply it.
pub(crate) fn schema_provider_failure(error: &WindowsHostedError) -> bool {
    matches!(
        error,
        WindowsHostedError::ResultSchema(_) | WindowsHostedError::OutputContract(_)
    )
}

#[derive(Debug)]
pub(crate) enum SessionError {
    Repository(String),
    Contended {
        inputs_run: u64,
    },
    Refused(String),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Decode(serde_json::Error),
    Encode(serde_json::Error),
    Atomic(atomic::Error),
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(detail) | Self::Refused(detail) => formatter.write_str(detail),
            Self::Contended { inputs_run } => write!(
                formatter,
                "hosted release session for preflight {inputs_run} is already active"
            ),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Decode(source) => write!(
                formatter,
                "hosted release session does not decode: {source}"
            ),
            Self::Encode(source) => write!(
                formatter,
                "hosted release session does not encode: {source}"
            ),
            Self::Atomic(source) => {
                write!(formatter, "cannot publish hosted release session: {source}")
            }
        }
    }
}

impl Error for SessionError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn workflows(suffix: &str) -> ReleaseWorkflowBlobs {
        ReleaseWorkflowBlobs {
            release_inputs: format!("inputs-{suffix}"),
            linux: format!("linux-{suffix}"),
            macos: format!("macos-{suffix}"),
            windows: format!("windows-{suffix}"),
        }
    }

    fn identity(challenge: &str) -> ReleaseInputIdentity {
        ReleaseInputIdentity {
            challenge: challenge.to_owned(),
            source_commit: "source".to_owned(),
            source: "archive".to_owned(),
            archive_sha256: "sha".to_owned(),
            tree: "tree".to_owned(),
            app: "app".to_owned(),
            canvas: "canvas".to_owned(),
            proof_identity: "proof".to_owned(),
            proof_archive_sha256: "proof-archive".to_owned(),
            workflow_sha: "workflow".to_owned(),
            run_id: 41,
        }
    }

    fn binding(run_id: u64, workflow_blob: &str, attached: bool) -> HostedRunBinding {
        HostedRunBinding {
            challenge: format!("challenge-{run_id}"),
            run_id,
            workflow_blob: workflow_blob.to_owned(),
            attached,
        }
    }

    #[test]
    fn failed_target_retries_without_redispatching_successful_siblings() {
        let workflows = workflows("a");
        let mut session = SessionFile::new(41, workflows.clone(), identity("inputs"));
        session
            .record(NativeTarget::Linux, &binding(101, &workflows.linux, false))
            .unwrap();
        session
            .record(NativeTarget::Macos, &binding(102, &workflows.macos, false))
            .unwrap();
        session
            .record(
                NativeTarget::Windows,
                &binding(103, &workflows.windows, false),
            )
            .unwrap();
        session
            .targets
            .linux
            .failed(true, false, "provider failed".to_owned())
            .unwrap();
        session.targets.macos.succeeded().unwrap();
        session.targets.windows.succeeded().unwrap();

        assert_eq!(session.target(NativeTarget::Linux).attachment(), None);
        assert_eq!(
            session.target(NativeTarget::Macos).attachment(),
            Some(Attach::Run(102))
        );
        assert_eq!(
            session.target(NativeTarget::Windows).attachment(),
            Some(Attach::Run(103))
        );
    }

    #[test]
    fn an_observed_run_that_concluded_in_failure_is_redispatched() {
        let workflows = workflows("a");
        let mut session = SessionFile::new(41, workflows.clone(), identity("inputs"));
        session
            .record(
                NativeTarget::Windows,
                &binding(103, &workflows.windows, false),
            )
            .unwrap();
        let interrupted = WindowsHostedError::Detached {
            run_id: 103,
            reason: Box::new(WindowsHostedError::Interrupted),
        };
        session
            .targets
            .windows
            .failed(
                terminal_provider_failure(&interrupted),
                schema_provider_failure(&interrupted),
                interrupted.to_string(),
            )
            .unwrap();
        assert_eq!(
            session.target(NativeTarget::Windows).attachment(),
            Some(Attach::Run(103)),
            "a run merely left unobserved is reattached",
        );

        let concluded = WindowsHostedError::Detached {
            run_id: 103,
            reason: Box::new(WindowsHostedError::Command {
                operation: "workflow execution",
                code: Some(1),
                stdout: String::new(),
                stderr: "provider run concluded \"failure\"".to_owned(),
            }),
        };
        session
            .targets
            .windows
            .failed(
                terminal_provider_failure(&concluded),
                schema_provider_failure(&concluded),
                concluded.to_string(),
            )
            .unwrap();
        assert_eq!(session.target(NativeTarget::Windows).attachment(), None);
    }

    #[test]
    fn only_an_unadmittable_result_schema_releases_a_succeeded_run() {
        let workflows = workflows("a");
        let mut session = SessionFile::new(41, workflows.clone(), identity("inputs"));
        session
            .record(NativeTarget::Linux, &binding(101, &workflows.linux, false))
            .unwrap();
        session.targets.linux.succeeded().unwrap();

        let identity_refusal =
            WindowsHostedError::ProviderResponse("hosted Linux result differs".to_owned());
        session
            .targets
            .linux
            .failed(
                terminal_provider_failure(&identity_refusal),
                schema_provider_failure(&identity_refusal),
                identity_refusal.to_string(),
            )
            .unwrap();
        assert_eq!(
            session.target(NativeTarget::Linux).attachment(),
            Some(Attach::Run(101))
        );

        let schema = WindowsHostedError::ResultSchema(
            serde_json::from_str::<u8>("{}").expect_err("not a byte"),
        );
        session
            .targets
            .linux
            .failed(
                terminal_provider_failure(&schema),
                schema_provider_failure(&schema),
                schema.to_string(),
            )
            .unwrap();
        assert_eq!(session.target(NativeTarget::Linux).attachment(), None);
        assert_eq!(
            session.target(NativeTarget::Linux).workflow(),
            TargetWorkflow::Current
        );
        session
            .record(NativeTarget::Linux, &binding(104, "linux-b", false))
            .unwrap();
        assert_eq!(
            session.target(NativeTarget::Linux).workflow(),
            TargetWorkflow::Exact("linux-b".to_owned())
        );
        assert_eq!(
            session.target(NativeTarget::Linux).attachment(),
            Some(Attach::Run(104))
        );
        session
            .record(NativeTarget::Linux, &binding(104, "linux-b", true))
            .unwrap();

        session
            .targets
            .linux
            .failed(true, false, "provider failed".to_owned())
            .unwrap();
        assert_eq!(
            session.target(NativeTarget::Linux).workflow(),
            TargetWorkflow::Current
        );
        session
            .record(NativeTarget::Linux, &binding(105, "linux-c", false))
            .unwrap();
    }

    /// A completed producer that omitted a required record retires only its
    /// own attempt for the current workflow; a declared record not yet
    /// verifiable keeps its run attached, and siblings stay attached.
    #[test]
    fn an_omitted_required_subject_redispatches_only_its_target_on_the_current_workflow() {
        let workflows = workflows("a");
        let mut session = SessionFile::new(41, workflows.clone(), identity("inputs"));
        session
            .record(NativeTarget::Linux, &binding(101, &workflows.linux, false))
            .unwrap();
        session
            .record(NativeTarget::Macos, &binding(102, &workflows.macos, false))
            .unwrap();
        session
            .record(
                NativeTarget::Windows,
                &binding(103, &workflows.windows, false),
            )
            .unwrap();
        session.targets.linux.succeeded().unwrap();
        session.targets.windows.succeeded().unwrap();

        let pending = WindowsHostedError::Command {
            operation: "artifact attestation",
            code: Some(1),
            stdout: String::new(),
            stderr: "no attestations found".to_owned(),
        };
        session
            .targets
            .macos
            .failed(
                terminal_provider_failure(&pending),
                schema_provider_failure(&pending),
                pending.to_string(),
            )
            .unwrap();
        assert_eq!(
            session.target(NativeTarget::Macos).attachment(),
            Some(Attach::Run(102)),
            "a declared subject not yet verifiable stays attached"
        );

        let omitted = WindowsHostedError::OutputContract(
            "completed producer omitted required attestation subjects: stage.log".to_owned(),
        );
        session
            .targets
            .macos
            .failed(
                terminal_provider_failure(&omitted),
                schema_provider_failure(&omitted),
                omitted.to_string(),
            )
            .unwrap();
        assert_eq!(session.target(NativeTarget::Macos).attachment(), None);
        assert_eq!(
            session.target(NativeTarget::Macos).workflow(),
            TargetWorkflow::Current
        );
        assert_eq!(
            session.target(NativeTarget::Linux).attachment(),
            Some(Attach::Run(101))
        );
        assert_eq!(
            session.target(NativeTarget::Windows).attachment(),
            Some(Attach::Run(103))
        );
    }

    #[test]
    fn changed_bound_identity_refuses_session_reuse() {
        let session = SessionFile::new(41, workflows("a"), identity("inputs-a"));
        let workflow_error = session
            .admit(41, &workflows("b"), &identity("inputs-a"))
            .unwrap_err();
        assert!(
            workflow_error
                .to_string()
                .contains("workflow identities changed")
        );
        let input_error = session
            .admit(41, &workflows("a"), &identity("inputs-b"))
            .unwrap_err();
        assert!(input_error.to_string().contains("input identities changed"));
    }
}
