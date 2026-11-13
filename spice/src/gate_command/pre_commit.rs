//! One bounded process boundary around the ordinary staged-change gate.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::environment_fault;
use gate::{ArmFamily, ClaimCapture, GateId};
use serde::{Deserialize, Serialize};
use sill::{
    Acquisition, Argv, Cancellation, Completion, Deadlines, LockFile, LockName, Output, Possession,
    ProcessGroup, Redaction, RestartDisposition, Spec, StdinPolicy, StreamPolicy, WaitOutcome,
    WaitPolicy,
};
use spice::ambient::Ambient;
use spice::repository::Repository;
use wire::{Digest, Fault, FaultClass, Outcome, Refusal, RefusalDiagnostic, Reply, ReplyPosition};

mod admission;
mod configured;
mod dispatch;
mod hook_environment;
mod parent;
mod profile;
mod terminal;

use super::output::family_name;
use dispatch::{dispatch, dispatch_captured};
use parent::ParentCancellation;
use terminal::classify;
#[cfg(test)]
use terminal::report_outcome;

const WORKER_ARGUMENT: &str = "__gate-pre-commit-worker";
const REPORT_RECORD: u8 = b'R';
const FAILURE_RECORD: u8 = b'F';
const HARD_DEADLINE: Duration = Duration::from_secs(600);
// Preserve every five-second cleanup phase and one second of outer envelope
// slack around the bounded selected and configured work.
const WORK_RUNTIME: Duration = Duration::from_secs(584);
const TERMINATION_GRACE: Duration = Duration::from_secs(5);
const REAP_DEADLINE: Duration = Duration::from_secs(5);
const OUTPUT_DRAIN: Duration = Duration::from_secs(5);
const OUTPUT_LIMIT: usize = 1024 * 1024;
const DISPATCH_LOCK: &str = "pre-commit.lock";
const APPLIANCE_GENERATION_FENCE_VERSION: u64 = 1;

const _: () = assert!(
    WORK_RUNTIME.as_secs()
        + TERMINATION_GRACE.as_secs()
        + REAP_DEADLINE.as_secs()
        + OUTPUT_DRAIN.as_secs()
        < HARD_DEADLINE.as_secs()
);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "phase", rename_all = "kebab-case")]
enum WorkerPhase {
    Capture,
    Policy,
    TestSelection,
    Configured {
        steps: usize,
    },
    Family {
        family: ArmFamily,
    },
    Arm {
        ordinal: usize,
        gate_id: GateId,
        family: ArmFamily,
    },
}

impl WorkerPhase {
    fn render(&self) -> String {
        match self {
            Self::Capture => "capture".to_owned(),
            Self::Policy => "policy".to_owned(),
            Self::TestSelection => "test-selection".to_owned(),
            Self::Configured { steps } => format!("configured steps={steps}"),
            Self::Family { family } => format!("family family={}", family_name(*family)),
            Self::Arm {
                ordinal,
                gate_id,
                family,
            } => format!(
                "arm ordinal={ordinal} gate={} family={}",
                gate_id.as_str(),
                family_name(*family),
            ),
        }
    }
}

const PRE_COMMIT_REPORT_VERSION: u64 = 1;

/// One complete packaged-plus-configured pre-commit battery result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Report {
    version: u64,
    source: Digest,
    #[serde(skip_serializing_if = "Option::is_none")]
    packaged: Option<spice::change_gate::ChangeGateReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    admission: Option<admission::AdmissionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    configured: Option<configured::ConfiguredReport>,
}

impl Report {
    fn new(
        source: Digest,
        packaged: Option<spice::change_gate::ChangeGateReport>,
        admission: Option<admission::AdmissionReport>,
        configured: Option<configured::ConfiguredReport>,
    ) -> Result<Self, String> {
        let report = Self {
            version: PRE_COMMIT_REPORT_VERSION,
            source,
            packaged,
            admission,
            configured,
        };
        report.validate()?;
        Ok(report)
    }

    pub(crate) const fn packaged(&self) -> Option<&spice::change_gate::ChangeGateReport> {
        self.packaged.as_ref()
    }

    fn configured(&self) -> Option<&configured::ConfiguredReport> {
        self.configured.as_ref()
    }

    pub(crate) fn admits(&self) -> bool {
        self.packaged
            .as_ref()
            .is_none_or(spice::change_gate::ChangeGateReport::admits)
            && self
                .configured
                .as_ref()
                .is_none_or(configured::ConfiguredReport::admits)
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != PRE_COMMIT_REPORT_VERSION {
            return Err(format!(
                "unsupported pre-commit report version {}; expected {PRE_COMMIT_REPORT_VERSION}",
                self.version,
            ));
        }
        if self.packaged.is_none() && self.admission.is_none() && self.configured.is_none() {
            return Err("pre-commit report carries an empty effective battery".to_owned());
        }
        if self
            .packaged
            .as_ref()
            .is_some_and(|report| report.selection().source() != &self.source)
        {
            return Err("packaged pre-commit report names a different staged source".to_owned());
        }
        if self
            .admission
            .as_ref()
            .is_some_and(|report| report.source() != &self.source)
        {
            return Err("admission report names a different staged source".to_owned());
        }
        if self
            .configured
            .as_ref()
            .is_some_and(|report| report.source() != &self.source)
        {
            return Err("configured pre-commit report names a different staged source".to_owned());
        }
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        let report: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        report.validate()?;
        Ok(report)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerFailure {
    reason: String,
    detail: String,
    fault: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ApplianceGenerationFence {
    version: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    appliance: Option<Digest>,
}

impl ApplianceGenerationFence {
    const fn unbound() -> Self {
        Self {
            version: APPLIANCE_GENERATION_FENCE_VERSION,
            appliance: None,
        }
    }

    const fn bound(appliance: Digest) -> Self {
        Self {
            version: APPLIANCE_GENERATION_FENCE_VERSION,
            appliance: Some(appliance),
        }
    }

    const fn expected(&self) -> Option<&Digest> {
        self.appliance.as_ref()
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != APPLIANCE_GENERATION_FENCE_VERSION {
            return Err(format!(
                "unsupported appliance generation fence version {}; expected {APPLIANCE_GENERATION_FENCE_VERSION}",
                self.version,
            ));
        }
        Ok(())
    }

    fn encode(&self) -> Result<OsString, String> {
        self.validate()?;
        let bytes = wire::encode(self).map_err(|error| error.to_string())?;
        String::from_utf8(bytes)
            .map(OsString::from)
            .map_err(|error| error.to_string())
    }

    fn decode(document: &OsStr) -> Result<Self, String> {
        let bytes = document.as_encoded_bytes();
        let fence: Self = wire::decode(bytes).map_err(|error| error.to_string())?;
        fence.validate()?;
        let canonical = wire::encode(&fence).map_err(|error| error.to_string())?;
        if canonical != bytes {
            return Err("appliance generation fence is not canonical".to_owned());
        }
        Ok(fence)
    }
}

fn encode_claim_capture(claim: &ClaimCapture) -> Result<OsString, String> {
    let bytes = wire::encode(claim).map_err(|error| error.to_string())?;
    String::from_utf8(bytes)
        .map(OsString::from)
        .map_err(|error| error.to_string())
}

fn decode_claim_capture(document: &OsStr) -> Result<ClaimCapture, String> {
    let bytes = document.as_encoded_bytes();
    let claim = wire::decode(bytes).map_err(|error| error.to_string())?;
    let canonical = wire::encode(&claim).map_err(|error| error.to_string())?;
    if canonical != bytes {
        return Err("board claim capture is not canonical".to_owned());
    }
    Ok(claim)
}

struct WorkerFiles {
    root: PathBuf,
    progress: PathBuf,
    terminal: PathBuf,
    report: PathBuf,
    armed: bool,
}

impl WorkerFiles {
    fn create() -> io::Result<Self> {
        let temporary = fs::canonicalize(env::temp_dir())?;
        let stem = format!(
            "spice-commit-gate-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple(),
        );
        let root = temporary.join(&stem);
        sill::authority::create_private_directory(&root)?;
        let progress = root.join("progress.jsonl");
        let terminal = root.join("terminal.bin");
        // The private worker directory is removed before the command returns.
        // A refusing report is its sibling so the path printed to the operator
        // remains readable after supervisor cleanup.
        let report = temporary.join(format!("{stem}-report.json"));
        let files = Self {
            root,
            progress,
            terminal,
            report,
            armed: true,
        };
        drop(sill::authority::create_private_file(&files.progress)?);
        drop(sill::authority::create_private_file(&files.terminal)?);
        Ok(files)
    }

    fn cleanup(mut self) -> io::Result<()> {
        fs::remove_dir_all(&self.root)?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for WorkerFiles {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

pub(super) fn run(
    profile_output: Option<PathBuf>,
    configuration: Option<&spice::config::RepositoryCapture>,
    live: bool,
    ambient: &Ambient,
) -> Outcome<Report> {
    let ambient = hook_environment::normalize(ambient);
    let mut profile = profile::Profile::new(profile_output);
    let outcome = run_profiled(&mut profile, configuration, live, &ambient);
    profile.publish();
    outcome
}

fn run_profiled(
    profile: &mut profile::Profile,
    configuration: Option<&spice::config::RepositoryCapture>,
    live: bool,
    ambient: &Ambient,
) -> Outcome<Report> {
    if let Some(source) = ambient.get(configured::SOURCE_VARIABLE) {
        return configured::recursion_refusal(source);
    }
    let current = match env::current_dir() {
        Ok(current) => current,
        Err(error) => return fault("gate::pre-commit-working-directory-unavailable", error),
    };
    let repository = match Repository::discover(&current) {
        Ok(repository) => repository,
        Err(error) => return fault(spice::repository::ResolveError::REASON, error),
    };
    let _lease = match crate::cargo_command::output_lease(&repository) {
        Ok(lease) => lease,
        Err(error) => return fault("cargo-output::lease-unavailable", error),
    };
    let capture_started = Instant::now();
    let installed = match installed_appliance_invocation(&repository) {
        Ok(installed) => installed,
        Err(failure) => return worker_failure(failure),
    };
    let generation = if installed {
        match capture_appliance_generation_fence(&repository) {
            Ok(generation) => generation,
            Err(failure) => return worker_failure(failure),
        }
    } else {
        ApplianceGenerationFence::unbound()
    };
    profile.appliance_capture(capture_started.elapsed());
    let mut audit = match start_appliance_generation_audit(&repository, &generation) {
        Ok(audit) => audit,
        Err(failure) => return worker_failure(failure),
    };
    let core_started = Instant::now();
    let claim = match capture_gate_claim(&repository, ambient) {
        Ok(claim) => claim,
        Err(failure) => return worker_failure(failure),
    };
    let _dispatch = match DispatchGuard::acquire() {
        Ok(dispatch) => dispatch,
        Err(outcome) => return outcome,
    };
    let prepared = configured::Dispatch::capture_at_with_claim_and_configuration(
        repository.clone(),
        ambient,
        claim.clone(),
        configuration,
    );
    profile.repository_core(core_started.elapsed());
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            if let Err(failure) = finish_appliance_generation_audit(&mut audit, profile) {
                return worker_failure(failure);
            }
            return worker_failure(configured_failure(error));
        }
    };
    // The hook measures the staged index, never the checkout: say so whenever
    // the worktree holds edits that index does not.
    let snapshot = match prepared.worktree_divergence() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            if let Err(failure) = finish_appliance_generation_audit(&mut audit, profile) {
                return worker_failure(failure);
            }
            return worker_failure(configured_failure(error));
        }
    };
    if !snapshot.is_empty() {
        super::output::Progress::stderr("gate pre-commit", live).line(format_args!("{snapshot}"));
    }
    if prepared.admission_enabled() && !prepared.has_configured() {
        return run_admission_inline(claim, prepared, &mut audit, profile);
    }
    if let Err(failure) = finish_appliance_generation_audit(&mut audit, profile) {
        return worker_failure(failure);
    }
    run_with(|files| supervise(files, &generation, &claim, live, ambient))
}

fn run_admission_inline(
    claim: ClaimCapture,
    dispatch: configured::Dispatch<'_>,
    audit: &mut ApplianceGenerationAudit,
    profile: &mut profile::Profile,
) -> Outcome<Report> {
    let started = Instant::now();
    match dispatch_captured(None, started, claim, dispatch, Some(audit), profile) {
        Ok(report) if report.admits() => Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body: report,
        }),
        Ok(_) => fault(
            "gate::pre-commit-inline-report-invalid",
            "an admission-only battery returned a non-admitting report",
        ),
        Err(failure) => worker_failure(failure),
    }
}

fn capture_gate_claim(
    repository: &Repository,
    ambient: &Ambient,
) -> Result<ClaimCapture, WorkerFailure> {
    let unavailable = |detail: String| WorkerFailure {
        reason: "gate::pre-commit-claim-capture-unavailable".to_owned(),
        detail,
        fault: true,
    };
    let state = repository
        .admit_state()
        .map_err(|error| unavailable(error.to_string()))?;
    match spice::gate::ambient_claim(&state, ambient) {
        Ok(Some((task, phase))) => Ok(ClaimCapture::Active { task, phase }),
        Ok(None) => Ok(ClaimCapture::Idle),
        Err(spice::gate::AmbientClaimError::Invalid(detail)) => Err(WorkerFailure {
            reason: "gate::pre-commit-claim-capture-invalid".to_owned(),
            detail,
            fault: false,
        }),
        Err(spice::gate::AmbientClaimError::Unavailable(detail)) => Err(unavailable(detail)),
    }
}

#[derive(Debug)]
struct DispatchGuard {
    _possession: Possession,
}

impl DispatchGuard {
    fn acquire<T>() -> Result<Self, Outcome<T>> {
        let current = env::current_dir()
            .map_err(|error| fault("gate::pre-commit-working-directory-unavailable", error))?;
        let repository = Repository::discover(&current)
            .map_err(|error| fault(spice::repository::ResolveError::REASON, error))?;
        Self::acquire_in(&repository)
    }

    fn acquire_in<T>(repository: &Repository) -> Result<Self, Outcome<T>> {
        let state = repository
            .admit_state()
            .map_err(|error| fault("gate::pre-commit-state-unavailable", error))?;
        let locks = state.worktree_locks();
        fs::create_dir_all(locks)
            .map_err(|error| fault("gate::pre-commit-lock-unavailable", error))?;
        let name = LockName::new("pre-commit").expect("pre-commit is a valid fixed lock name");
        let lock = LockFile::new(name, locks.join(DISPATCH_LOCK));
        match lock.try_acquire() {
            Ok(Acquisition::Acquired(possession)) => Ok(Self {
                _possession: possession,
            }),
            Ok(Acquisition::Contended) => Err(Outcome::Refusal(Refusal::evidence(
                "gate::pre-commit-dispatch-active",
                format!("lock={}", lock.path().display()),
                "wait for the active commit gate to finish; configured commands cannot recursively redispatch it",
            ))),
            Err(error) => Err(fault("gate::pre-commit-lock-unavailable", error)),
        }
    }
}

fn run_with(supervisor: impl FnOnce(&WorkerFiles) -> Outcome<Report>) -> Outcome<Report> {
    let files = match WorkerFiles::create() {
        Ok(files) => files,
        Err(error) => return fault("gate::pre-commit-supervisor-unavailable", error),
    };
    let answer = supervisor(&files);
    match files.cleanup() {
        Ok(()) => answer,
        Err(error) => fault("gate::pre-commit-cleanup-failed", error),
    }
}

pub(super) fn worker_entry(
    mut arguments: impl Iterator<Item = OsString>,
    ambient: &Ambient,
) -> Option<i32> {
    let _executable = arguments.next()?;
    let worker = arguments.next()?;
    if worker.as_os_str() != OsStr::new(WORKER_ARGUMENT) {
        return None;
    }
    // Reject configured-command recursion before reading caller paths.
    if let Some(source) = ambient.get(configured::SOURCE_VARIABLE) {
        let refusal: Outcome<()> = configured::recursion_refusal(source);
        crate::render::render(&refusal, crate::render::Form::Text)
            .expect("typed recursion refusal renders as text")
            .speak();
        return Some(2);
    }
    let Some(progress) = arguments.next() else {
        eprintln!("commit-gate worker requires a progress path");
        return Some(1);
    };
    let Some(terminal) = arguments.next() else {
        eprintln!("commit-gate worker requires a terminal path");
        return Some(1);
    };
    let generation = arguments.next();
    if generation.is_none() {
        eprintln!("commit-gate worker requires an appliance generation fence");
        return Some(1);
    }
    let claim = arguments.next();
    if claim.is_none() {
        eprintln!("commit-gate worker requires a board claim capture");
        return Some(1);
    }
    if arguments.next().is_some() {
        eprintln!("commit-gate worker received trailing arguments");
        return Some(1);
    }
    let result = worker_run(
        Path::new(&progress),
        Path::new(&terminal),
        generation
            .as_deref()
            .expect("the pre-commit worker requires its appliance generation fence"),
        claim
            .as_deref()
            .expect("the pre-commit worker requires its board claim capture"),
        ambient,
    );
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("commit-gate worker failed to publish its terminal: {error}");
            1
        }
    })
}

fn supervise(
    files: &WorkerFiles,
    generation: &ApplianceGenerationFence,
    claim: &ClaimCapture,
    live: bool,
    ambient: &Ambient,
) -> Outcome<Report> {
    let parent = match ParentCancellation::capture() {
        Ok(parent) => parent,
        Err(error) => return fault("gate::pre-commit-parent-observer-unavailable", error),
    };
    let executable = match env::current_exe() {
        Ok(executable) => executable,
        Err(error) => return fault("gate::pre-commit-executable-unavailable", error),
    };
    let generation = match generation.encode() {
        Ok(generation) => generation,
        Err(error) => return fault("gate::pre-commit-worker-invalid", error),
    };
    let claim = match encode_claim_capture(claim) {
        Ok(claim) => claim,
        Err(error) => return fault("gate::pre-commit-worker-invalid", error),
    };
    let spec = match worker_spec_with_cancellation(
        executable,
        [
            OsString::from(WORKER_ARGUMENT),
            files.progress.as_os_str().to_owned(),
            files.terminal.as_os_str().to_owned(),
            generation,
            claim,
        ],
        ProcessBounds::production(),
        parent.cancellation(),
        ambient,
    ) {
        Ok(spec) => spec,
        Err(error) => return fault("gate::pre-commit-worker-invalid", error),
    };
    let running = match sill::spawn(spec) {
        Ok(running) => running,
        Err(error) => return fault("gate::pre-commit-worker-unavailable", error),
    };
    // A passing hook prints no reply and no completion line. A run that
    // outlasts one heartbeat interval names its phase while it runs, because
    // its outcome is not yet known; a long refusal or fault also names its end.
    let progress = super::output::Progress::stderr("gate pre-commit", live);
    let (waited, elapsed) = progress.watching(|| last_phase(&files.progress), || running.wait());
    let outcome = match waited {
        Ok(waited) => classify(files, &waited),
        Err(error) => worker_lost(
            &files.progress,
            format!("worker wait or reap failed: {error}"),
        ),
    };
    if progress.beat_during(elapsed) && !matches!(outcome, Outcome::Reply(_)) {
        progress.line(format_args!(
            "{} after {}",
            super::output::verdict(&outcome),
            super::output::Elapsed(elapsed)
        ));
    }
    outcome
}

fn worker_run(
    progress_path: &Path,
    terminal: &Path,
    generation: &OsStr,
    claim: &OsStr,
    ambient: &Ambient,
) -> io::Result<()> {
    let _containment = gate::measure::inherit_process_group().map_err(io::Error::other)?;
    let mut progress = OpenOptions::new().append(true).open(progress_path)?;
    let started = Instant::now();
    let mut profile = profile::Profile::disabled();
    let result = ApplianceGenerationFence::decode(generation)
        .map_err(|detail| WorkerFailure {
            reason: "gate::pre-commit-generation-fence-invalid".to_owned(),
            detail,
            fault: true,
        })
        .and_then(|generation| {
            verify_appliance_generation_fence(&generation)?;
            let claim = decode_claim_capture(claim).map_err(|detail| WorkerFailure {
                reason: "gate::pre-commit-claim-capture-invalid".to_owned(),
                detail,
                fault: true,
            })?;
            write_phase(&mut progress, &WorkerPhase::Capture).map_err(worker_io_failure)?;
            dispatch(&mut progress, started, claim, &mut profile, ambient)
        });
    match result {
        Ok(report) => {
            let mut document = vec![REPORT_RECORD];
            document.extend(report.encode().map_err(io::Error::other)?);
            fs::write(terminal, document)
        }
        Err(failure) => write_failure(terminal, &failure),
    }
}

fn installed_appliance_invocation(repository: &Repository) -> Result<bool, WorkerFailure> {
    let current = env::current_exe().map_err(|error| WorkerFailure {
        reason: "gate::pre-commit-executable-unavailable".to_owned(),
        detail: error.to_string(),
        fault: true,
    })?;
    let installed = spice::install::installed_binary(repository);
    if current == installed {
        return Ok(true);
    }
    let installed = match fs::canonicalize(&installed) {
        Ok(installed) => installed,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(WorkerFailure {
                reason: "gate::pre-commit-installed-appliance-unavailable".to_owned(),
                detail: format!("cannot resolve {}: {error}", installed.display()),
                fault: true,
            });
        }
    };
    let current = fs::canonicalize(&current).map_err(|error| WorkerFailure {
        reason: "gate::pre-commit-executable-unavailable".to_owned(),
        detail: format!("cannot resolve {}: {error}", current.display()),
        fault: true,
    })?;
    Ok(current == installed)
}

fn capture_appliance_generation_fence(
    repository: &Repository,
) -> Result<ApplianceGenerationFence, WorkerFailure> {
    let generation =
        spice::install::capture_generation(repository).map_err(|error| WorkerFailure {
            reason: "gate::pre-commit-appliance-audit-failed".to_owned(),
            detail: error.to_string(),
            fault: true,
        })?;
    generation
        .map(ApplianceGenerationFence::bound)
        .ok_or_else(|| appliance_generation_moved(None, "installation receipt is absent"))
}

fn verify_appliance_generation_fence(
    generation: &ApplianceGenerationFence,
) -> Result<(), WorkerFailure> {
    generation.validate().map_err(|detail| WorkerFailure {
        reason: "gate::pre-commit-generation-fence-invalid".to_owned(),
        detail,
        fault: true,
    })?;
    let Some(_) = generation.expected() else {
        return Ok(());
    };
    let current = env::current_dir().map_err(|error| WorkerFailure {
        reason: "gate::pre-commit-working-directory-unavailable".to_owned(),
        detail: error.to_string(),
        fault: true,
    })?;
    let repository = Repository::discover(&current).map_err(|error| WorkerFailure {
        reason: spice::repository::ResolveError::REASON.to_owned(),
        detail: error.to_string(),
        fault: true,
    })?;
    verify_appliance_generation_fence_at(&repository, generation)
}

fn verify_appliance_generation_fence_at(
    repository: &Repository,
    generation: &ApplianceGenerationFence,
) -> Result<(), WorkerFailure> {
    generation.validate().map_err(|detail| WorkerFailure {
        reason: "gate::pre-commit-generation-fence-invalid".to_owned(),
        detail,
        fault: true,
    })?;
    let Some(_) = generation.expected() else {
        return Ok(());
    };
    current_appliance(repository, generation.expected()).map(|_| ())
}

fn start_appliance_generation_audit(
    repository: &Repository,
    generation: &ApplianceGenerationFence,
) -> Result<ApplianceGenerationAudit, WorkerFailure> {
    generation.validate().map_err(|detail| WorkerFailure {
        reason: "gate::pre-commit-generation-fence-invalid".to_owned(),
        detail,
        fault: true,
    })?;
    if generation.expected().is_none() {
        return Ok(ApplianceGenerationAudit { handle: None });
    }
    let repository = repository.clone();
    let generation = generation.clone();
    Ok(ApplianceGenerationAudit {
        handle: Some(thread::spawn(move || {
            let started = Instant::now();
            (
                verify_appliance_generation_fence_at(&repository, &generation),
                started.elapsed(),
            )
        })),
    })
}

fn finish_appliance_generation_audit(
    audit: &mut ApplianceGenerationAudit,
    profile: &mut profile::Profile,
) -> Result<(), WorkerFailure> {
    audit.handle.take().map_or(Ok(()), |audit| {
        let (result, elapsed) = audit.join().map_err(|_| WorkerFailure {
            reason: "gate::pre-commit-appliance-audit-failed".to_owned(),
            detail: "appliance audit thread panicked".to_owned(),
            fault: true,
        })?;
        profile.installed_appliance_exact_byte_audit(elapsed);
        result
    })
}

struct ApplianceGenerationAudit {
    handle: Option<JoinHandle<(Result<(), WorkerFailure>, Duration)>>,
}

impl Drop for ApplianceGenerationAudit {
    fn drop(&mut self) {
        if let Some(audit) = self.handle.take() {
            let _ = audit.join();
        }
    }
}

fn current_appliance(
    repository: &Repository,
    expected: Option<&Digest>,
) -> Result<Digest, WorkerFailure> {
    let executable = env::current_exe().map_err(|error| WorkerFailure {
        reason: "gate::pre-commit-executable-unavailable".to_owned(),
        detail: error.to_string(),
        fault: true,
    })?;
    let audit = spice::install::audit_appliance(repository, &executable).map_err(|error| {
        WorkerFailure {
            reason: "gate::pre-commit-appliance-audit-failed".to_owned(),
            detail: error.to_string(),
            fault: true,
        }
    })?;
    if let Some(observed) = audit.receipt().filter(|observed| {
        audit.is_current() && expected.is_none_or(|expected| *observed == expected)
    }) {
        let observed = observed.clone();
        let recaptured =
            spice::install::capture_generation(repository).map_err(|error| WorkerFailure {
                reason: "gate::pre-commit-appliance-audit-failed".to_owned(),
                detail: error.to_string(),
                fault: true,
            })?;
        if recaptured.as_ref() == Some(&observed) {
            return Ok(observed);
        }
        return Err(appliance_generation_moved(
            expected,
            format!(
                "receipt moved after asset audit to {}",
                recaptured.as_ref().map_or("absent", Digest::as_str),
            ),
        ));
    }
    let observed = audit.receipt();
    Err(appliance_generation_moved(
        expected,
        format!(
            "state={}; receipt={}; detail={}",
            audit.state().as_str(),
            observed.map_or("absent", Digest::as_str),
            audit.detail(),
        ),
    ))
}

fn appliance_generation_moved(
    appliance: Option<&Digest>,
    observed: impl std::fmt::Display,
) -> WorkerFailure {
    WorkerFailure {
        reason: "gate::pre-commit-appliance-generation-moved".to_owned(),
        detail: format!(
            "appliance={}; observed={observed}",
            appliance.map_or("unavailable", Digest::as_str),
        ),
        fault: false,
    }
}

fn worker_io_failure(error: io::Error) -> WorkerFailure {
    WorkerFailure {
        reason: "gate::pre-commit-progress-unavailable".to_owned(),
        detail: error.to_string(),
        fault: true,
    }
}

fn configured_failure(error: configured::ConfiguredFailure) -> WorkerFailure {
    WorkerFailure {
        reason: error.reason().to_owned(),
        detail: error.detail().to_owned(),
        fault: error.is_fault(),
    }
}

fn write_phase(file: &mut File, phase: &WorkerPhase) -> io::Result<()> {
    serde_json::to_writer(&mut *file, phase).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.flush()
}

fn write_failure(path: &Path, failure: &WorkerFailure) -> io::Result<()> {
    let mut document = vec![FAILURE_RECORD];
    document.extend(serde_json::to_vec(failure).map_err(io::Error::other)?);
    fs::write(path, document)
}

/// A worker failure as the hook reports it; detail beyond the diagnostic
/// tail bound is retained whole by digest rather than echoed.
fn worker_failure(mut failure: WorkerFailure) -> Outcome<Report> {
    failure.detail = super::output::bounded(&super::output::current_captures(), &failure.detail);
    if failure.fault {
        environment_fault(failure.reason, failure.detail)
    } else if matches!(
        failure.reason.as_str(),
        "gate::change-gate-tests-refused" | "gate::change-gate-test-coverage-unknown"
    ) {
        Outcome::Refusal(Refusal::evidence(
            failure.reason,
            failure.detail,
            "repair the named staged-source test selection or failing selected target",
        ))
    } else {
        let observed = failure.detail;
        let remedy = if super::proof_absent(&failure.reason) {
            "the staged tree is not proved yet; run each named command, then retry the same git commit"
        } else {
            "repair the named capture input; the selected change gate is the sole execution path"
        };
        Outcome::Refusal(Refusal::bare(
            RefusalDiagnostic::new(
                failure.reason,
                None,
                Some("one complete selected change-gate capture".to_owned()),
                Some(observed.clone()),
            )
            .with_detail(format!("{observed}\n{remedy}")),
        ))
    }
}

fn change_gate_failure(error: spice::change_gate::ChangeGateError) -> WorkerFailure {
    WorkerFailure {
        reason: error.reason().to_owned(),
        detail: error.detail().to_owned(),
        fault: error.is_fault(),
    }
}

fn deadline(progress: &Path) -> Outcome<Report> {
    let phase = last_phase(progress);
    Outcome::Refusal(Refusal::bare(
        RefusalDiagnostic::new(
            "gate::pre-commit-deadline-elapsed",
            Some("canonical-commit-gate".to_owned()),
            Some(format!(
                "one complete selected change gate and process-group cleanup inside {} seconds",
                HARD_DEADLINE.as_secs(),
            )),
            Some(format!(
                "phase={phase}; findings=incomplete; worker=timed-out; work_cutoff_seconds={}",
                WORK_RUNTIME.as_secs(),
            )),
        )
        .with_detail(format!(
            "phase={phase}; repair or reduce this gate phase, then retry the same plain git commit"
        )),
    ))
}

fn worker_lost(progress: &Path, detail: String) -> Outcome<Report> {
    let phase = last_phase(progress);
    environment_fault(
        "gate::pre-commit-worker-lost",
        format!("phase={phase}; findings=incomplete; {detail}"),
    )
}

fn fault<T>(reason: &str, error: impl std::fmt::Display) -> Outcome<T> {
    environment_fault(reason, error.to_string())
}

fn last_phase(path: &Path) -> String {
    let Ok(file) = File::open(path) else {
        return "unavailable".to_owned();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            serde_json::from_str::<WorkerPhase>(&line)
                .map(|phase| phase.render())
                .ok()
                .or_else(|| {
                    line.strip_prefix("[change-gate-phase] stage=")
                        .map(|phase| format!("change-gate {phase}"))
                })
                .or_else(|| {
                    line.strip_prefix("[change-gate-policy] gate=")
                        .and_then(|progress| progress.split_whitespace().next())
                        .filter(|gate| GateId::BATTERY.iter().any(|known| known.as_str() == *gate))
                        .map(|gate| format!("arm gate={gate}"))
                })
                .or_else(|| {
                    line.strip_prefix("[rust-test] ")
                        .map(|phase| format!("selected-tests {phase}"))
                })
        })
        .last()
        .unwrap_or_else(|| "unavailable".to_owned())
}

fn captured(output: Option<&sill::CapturedOutput>) -> String {
    output.map_or_else(
        || "<none>".to_owned(),
        |output| String::from_utf8_lossy(output.bytes()).trim().to_owned(),
    )
}

#[derive(Clone, Copy)]
struct ProcessBounds {
    runtime: Duration,
    output_drain: Duration,
    reap: Duration,
    grace: Duration,
}

impl ProcessBounds {
    const fn production() -> Self {
        Self {
            runtime: WORK_RUNTIME,
            output_drain: OUTPUT_DRAIN,
            reap: REAP_DEADLINE,
            grace: TERMINATION_GRACE,
        }
    }
}

fn worker_spec_with_cancellation(
    executable: impl Into<OsString>,
    arguments: impl IntoIterator<Item = OsString>,
    bounds: ProcessBounds,
    cancellation: Cancellation,
    ambient: &Ambient,
) -> Result<Spec, String> {
    let current = env::current_dir().map_err(|error| error.to_string())?;
    worker_spec_at_with_cancellation(
        executable,
        arguments,
        &current,
        bounds,
        cancellation,
        ambient,
    )
}

fn worker_spec_at_with_cancellation(
    executable: impl Into<OsString>,
    arguments: impl IntoIterator<Item = OsString>,
    root: &Path,
    bounds: ProcessBounds,
    cancellation: Cancellation,
    ambient: &Ambient,
) -> Result<Spec, String> {
    let capture = StreamPolicy::Capture {
        limit: OUTPUT_LIMIT,
    };
    Ok(Spec {
        argv: Argv::new(executable, arguments).map_err(|error| error.to_string())?,
        cwd: root.to_path_buf(),
        environment: ambient.environment().map_err(|error| error.to_string())?,
        group: ProcessGroup::New,
        deadlines: Deadlines::new(bounds.runtime, bounds.output_drain, bounds.reap)
            .map_err(|error| error.to_string())?,
        wait: WaitPolicy::new(
            "canonical commit gate worker",
            cancellation,
            RestartDisposition::Never,
        )
        .map_err(|error| error.to_string())?,
        output: Output::new(StdinPolicy::Null, capture, capture),
        redaction: Redaction::All,
        grace: bounds.grace,
    })
}

#[cfg(test)]
#[path = "pre_commit/tests.rs"]
mod extended_tests;
