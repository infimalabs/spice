//! Digest-identified native proof runner and its exact attestation exchange.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sill::{Argv, Spec, SpecError, WaitError};
use wire::{Digest, Domain};

use crate::challenge::Challenge;
use crate::container::{
    Captured, ContainerError, Execution, ExecutionCompletion, Limits, is_executable,
    validate_command,
};
use crate::launch::{Launch, LaunchError, System};

/// The first argument every admitted runner is launched with.
pub const PROTOCOL: &str = "spice-proof-native-runner-v1";

/// The marker opening the one attestation line an admitted runner writes.
pub const ATTESTATION_PREFIX: &str = "spice-proof-native-attestation-v1";

/// The boundary an attesting runner establishes, spelled exactly once.
///
/// A runner implementation reads this back rather than restating it, the same
/// way the repository-owned runner reads its admitted commands back from the
/// compiled battery. [`NativeRunner`] admits these claims by exact bytes, so a
/// second copy is the one thing that could leave a runner attesting a boundary
/// this host could never admit -- and that divergence would surface only on a
/// real sealed run, never in the workspace battery.
pub const ISOLATION_CLAIMS: &str = concat!(
    "repository=read-only:",
    "cargo-home=fresh-empty:",
    "cargo-target=fresh-empty:",
    "network=loopback-only:",
    "host-dependencies=denied"
);

/// One canonical native runner executable and the bytes that identify it.
#[derive(Clone, Debug)]
pub struct NativeRunner {
    binary: PathBuf,
    identity: Digest,
    workspace: PathBuf,
    limits: Limits,
    launcher: Arc<dyn Launch>,
}

impl NativeRunner {
    /// Admit an absolute regular executable by its observed bytes.
    ///
    /// # Errors
    ///
    /// Refuses a relative, missing, non-regular, non-executable, or unreadable
    /// runner and a workspace that is not a canonical directory.
    pub fn attest(
        binary: impl AsRef<Path>,
        workspace: impl AsRef<Path>,
        limits: Limits,
    ) -> Result<Self, NativeError> {
        Self::attest_with(
            Arc::new(System),
            binary.as_ref(),
            workspace.as_ref(),
            limits,
        )
    }

    pub(crate) fn attest_with(
        launcher: Arc<dyn Launch>,
        requested: &Path,
        requested_workspace: &Path,
        limits: Limits,
    ) -> Result<Self, NativeError> {
        if !requested.is_absolute() {
            return Err(NativeError::RunnerNotAbsolute(requested.to_path_buf()));
        }
        let binary = fs::canonicalize(requested).map_err(|source| NativeError::InspectRunner {
            path: requested.to_path_buf(),
            source,
        })?;
        inspect_runner(&binary)?;
        let bytes = fs::read(&binary).map_err(|source| NativeError::ReadRunner {
            path: binary.clone(),
            source,
        })?;
        let identity = Digest::compute(Domain::ProofNativeRunner, &bytes);

        let workspace = fs::canonicalize(requested_workspace).map_err(|source| {
            NativeError::InspectWorkspace {
                path: requested_workspace.to_path_buf(),
                source,
            }
        })?;
        if !workspace.is_dir() {
            return Err(NativeError::WorkspaceNotDirectory(workspace));
        }

        Ok(Self {
            binary,
            identity,
            workspace,
            limits,
            launcher,
        })
    }

    /// Digest computed from the exact admitted executable bytes.
    #[must_use]
    pub const fn identity(&self) -> &Digest {
        &self.identity
    }

    /// Canonical executable path inspected during admission.
    #[must_use]
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// Exercise the complete runner admission and isolation handshake before
    /// release staging starts.
    ///
    /// # Errors
    ///
    /// Returns the runner protocol error or refuses a non-clean Cargo version
    /// probe after the attestation has been removed from captured stderr.
    pub fn preflight(&self) -> Result<(), NativeError> {
        let execution = self.execute("cargo", &["--version".to_owned()])?;
        if execution.stdout.truncated
            || !execution.stdout.complete
            || execution.stderr.truncated
            || !execution.stderr.complete
        {
            return Err(NativeError::PreflightOutputIncomplete);
        }
        match execution.completion {
            ExecutionCompletion::Exited {
                success: true,
                code: Some(0),
            } => Ok(()),
            ExecutionCompletion::Exited { code, .. } => Err(NativeError::PreflightFailed { code }),
            ExecutionCompletion::TimedOut { code } => Err(NativeError::PreflightTimedOut { code }),
            ExecutionCompletion::Cancelled { code } => {
                Err(NativeError::PreflightCancelled { code })
            }
        }
    }

    pub(crate) fn execute(
        &self,
        program: &str,
        arguments: &[String],
    ) -> Result<Execution, NativeError> {
        validate_command(program, arguments).map_err(NativeError::Command)?;
        self.verify_identity()?;
        let challenge = Challenge::fresh().map_err(NativeError::CreateChallenge)?;
        let spec = self.spec(program, arguments, &challenge)?;
        let mut execution = self.launcher.run(spec).map_err(|error| match error {
            LaunchError::Spawn(source) => NativeError::Spawn(source),
            LaunchError::Wait(source) => NativeError::Wait(source),
            LaunchError::MissingCapture(stream) => NativeError::MissingCapture(stream),
        })?;
        self.verify_attestation(&mut execution.stderr, &challenge)?;
        Ok(execution)
    }

    fn spec(
        &self,
        program: &str,
        arguments: &[String],
        challenge: &Challenge,
    ) -> Result<Spec, NativeError> {
        validate_command(program, arguments).map_err(NativeError::Command)?;
        let mut runner_arguments = vec![
            PROTOCOL.to_owned(),
            self.identity.to_string(),
            challenge.as_str().to_owned(),
            program.to_owned(),
        ];
        runner_arguments.extend(arguments.iter().cloned());
        let argv = Argv::new(
            self.binary.as_os_str().to_owned(),
            runner_arguments.into_iter().map(OsString::from),
        )
        .map_err(NativeError::Spec)?;
        self.limits
            .process_spec(
                argv,
                self.workspace.clone(),
                format!("sealed native proof battery member {program:?}"),
            )
            .map_err(NativeError::Spec)
    }

    fn verify_identity(&self) -> Result<(), NativeError> {
        inspect_runner(&self.binary)?;
        let bytes = fs::read(&self.binary).map_err(|source| NativeError::ReadRunner {
            path: self.binary.clone(),
            source,
        })?;
        let observed = Digest::compute(Domain::ProofNativeRunner, &bytes);
        if observed != self.identity {
            return Err(NativeError::RunnerChanged {
                expected: self.identity.clone(),
                observed,
            });
        }
        Ok(())
    }

    fn verify_attestation(
        &self,
        stderr: &mut Captured,
        challenge: &Challenge,
    ) -> Result<(), NativeError> {
        if stderr.truncated || !stderr.complete {
            return Err(NativeError::IncompleteAttestation {
                truncated: stderr.truncated,
                complete: stderr.complete,
            });
        }
        let expected = format!(
            "{ATTESTATION_PREFIX}:runner={}:challenge={}:{}\n",
            self.identity,
            challenge.as_str(),
            ISOLATION_CLAIMS
        );
        if !stderr.bytes.starts_with(expected.as_bytes()) {
            return Err(NativeError::RunnerNotAttested {
                expected,
                observed: String::from_utf8_lossy(&stderr.bytes).into_owned(),
            });
        }
        stderr.bytes.drain(..expected.len());
        Ok(())
    }
}

fn inspect_runner(path: &Path) -> Result<(), NativeError> {
    let metadata = fs::metadata(path).map_err(|source| NativeError::InspectRunner {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(NativeError::RunnerNotRegular(path.to_path_buf()));
    }
    if !is_executable(&metadata) {
        return Err(NativeError::RunnerNotExecutable(path.to_path_buf()));
    }
    Ok(())
}

/// Why a native sealed environment was not admitted or attested.
#[derive(Debug)]
pub enum NativeError {
    RunnerNotAbsolute(PathBuf),
    InspectRunner { path: PathBuf, source: io::Error },
    RunnerNotRegular(PathBuf),
    RunnerNotExecutable(PathBuf),
    ReadRunner { path: PathBuf, source: io::Error },
    RunnerChanged { expected: Digest, observed: Digest },
    InspectWorkspace { path: PathBuf, source: io::Error },
    WorkspaceNotDirectory(PathBuf),
    Command(ContainerError),
    CreateChallenge(io::Error),
    Spec(SpecError),
    Spawn(io::Error),
    Wait(WaitError),
    MissingCapture(&'static str),
    IncompleteAttestation { truncated: bool, complete: bool },
    RunnerNotAttested { expected: String, observed: String },
    PreflightOutputIncomplete,
    PreflightFailed { code: Option<i32> },
    PreflightTimedOut { code: Option<i32> },
    PreflightCancelled { code: Option<i32> },
}

impl fmt::Display for NativeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RunnerNotAbsolute(path) => write!(
                formatter,
                "native proof runner path must be absolute: {}",
                path.display()
            ),
            Self::InspectRunner { path, source } => {
                write!(
                    formatter,
                    "cannot inspect native proof runner {}: {source}",
                    path.display()
                )
            }
            Self::RunnerNotRegular(path) => write!(
                formatter,
                "native proof runner is not a regular file: {}",
                path.display()
            ),
            Self::RunnerNotExecutable(path) => write!(
                formatter,
                "native proof runner is not executable: {}",
                path.display()
            ),
            Self::ReadRunner { path, source } => write!(
                formatter,
                "cannot read native proof runner {}: {source}",
                path.display()
            ),
            Self::RunnerChanged { expected, observed } => write!(
                formatter,
                concat!(
                    "native proof runner bytes changed after admission: ",
                    "expected {}, observed {}"
                ),
                expected, observed
            ),
            Self::InspectWorkspace { path, source } => write!(
                formatter,
                "cannot inspect native proof workspace {}: {source}",
                path.display()
            ),
            Self::WorkspaceNotDirectory(path) => write!(
                formatter,
                "native proof workspace is not a directory: {}",
                path.display()
            ),
            Self::Command(source) => write!(formatter, "invalid native proof command: {source}"),
            Self::CreateChallenge(source) => {
                write!(formatter, "cannot create native proof challenge: {source}")
            }
            Self::Spec(source) => write!(formatter, "invalid native proof process: {source}"),
            Self::Spawn(source) => write!(formatter, "cannot start native proof runner: {source}"),
            Self::Wait(source) => write!(formatter, "native proof runner wait failed: {source}"),
            Self::MissingCapture(stream) => {
                write!(
                    formatter,
                    "native proof runner returned no {stream} capture"
                )
            }
            Self::IncompleteAttestation {
                truncated,
                complete,
            } => write!(
                formatter,
                concat!(
                    "native proof runner attestation capture is incomplete: ",
                    "truncated={}, complete={}"
                ),
                truncated, complete
            ),
            Self::RunnerNotAttested { expected, observed } => write!(
                formatter,
                concat!(
                    "native proof runner did not attest its isolation contract: ",
                    "expected {:?}, observed {:?}"
                ),
                expected, observed
            ),
            Self::PreflightOutputIncomplete => {
                formatter.write_str("native proof runner preflight output is incomplete")
            }
            Self::PreflightFailed { code } => {
                write!(
                    formatter,
                    "native proof runner preflight failed with code {code:?}"
                )
            }
            Self::PreflightTimedOut { code } => {
                write!(
                    formatter,
                    "native proof runner preflight timed out with code {code:?}"
                )
            }
            Self::PreflightCancelled { code } => {
                write!(
                    formatter,
                    "native proof runner preflight was cancelled with code {code:?}"
                )
            }
        }
    }
}

impl Error for NativeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InspectRunner { source, .. }
            | Self::ReadRunner { source, .. }
            | Self::InspectWorkspace { source, .. }
            | Self::CreateChallenge(source)
            | Self::Spawn(source) => Some(source),
            Self::Command(source) => Some(source),
            Self::Spec(source) => Some(source),
            Self::Wait(source) => Some(source),
            Self::RunnerNotAbsolute(_)
            | Self::RunnerNotRegular(_)
            | Self::RunnerNotExecutable(_)
            | Self::RunnerChanged { .. }
            | Self::WorkspaceNotDirectory(_)
            | Self::MissingCapture(_)
            | Self::IncompleteAttestation { .. }
            | Self::RunnerNotAttested { .. }
            | Self::PreflightOutputIncomplete
            | Self::PreflightFailed { .. }
            | Self::PreflightTimedOut { .. }
            | Self::PreflightCancelled { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use sill::{Deadlines, ProcessGroup, Spec, StdinPolicy, StreamPolicy};

    use super::{ATTESTATION_PREFIX, ISOLATION_CLAIMS, NativeError, NativeRunner, PROTOCOL};
    use crate::challenge::Challenge;
    use crate::container::{Captured, Execution, ExecutionCompletion, Limits};
    use crate::environment::SealedEnvironment;
    use crate::launch::{Launch, LaunchError};
    use crate::replay::{CheckKind, replay};
    use crate::test_support::{arguments, exited, whole};

    /// How one native runner answers the exchange it was launched with.
    #[derive(Clone, Copy, Debug)]
    enum Runner {
        /// Speaks the protocol, attests the identity and challenge it was
        /// handed, and reports the command it ran.
        Attesting,
        /// Attests correctly, then the command it ran fails.
        AttestingFailure(i32),
        /// Writes nothing and succeeds.
        Silent,
        /// Writes nothing and fails.
        Failing(i32),
        /// Writes one public marker, whatever it was handed.
        FixedMarker,
        /// Answers a challenge from some other exchange.
        StaleChallenge,
        /// Attests an identity other than the admitted bytes.
        WrongIdentity,
        /// Is stopped by its runtime deadline before it writes anything.
        TimedOut,
    }

    impl Runner {
        fn marker(identity: &str, challenge: &str) -> String {
            format!(
                "{ATTESTATION_PREFIX}:runner={identity}:challenge={challenge}:{ISOLATION_CLAIMS}\n"
            )
        }
    }

    impl Launch for Runner {
        fn run(&self, spec: Spec) -> Result<Execution, LaunchError> {
            let arguments = arguments(&spec);
            if arguments.first().map(String::as_str) != Some(PROTOCOL) {
                return Ok(exited(71, "", ""));
            }
            let (identity, challenge) = (&arguments[1], &arguments[2]);
            Ok(match *self {
                Self::Attesting => exited(
                    0,
                    format!(
                        "challenge:{challenge}\nexecuted:{}\n",
                        arguments[3..].join(" ")
                    ),
                    Self::marker(identity, challenge),
                ),
                Self::AttestingFailure(code) => exited(code, "", Self::marker(identity, challenge)),
                Self::Silent => exited(0, "", ""),
                Self::Failing(code) => exited(code, "", ""),
                Self::FixedMarker => exited(0, "", Self::marker("fixed", "fixed")),
                Self::StaleChallenge => exited(0, "", Self::marker(identity, "stale-challenge")),
                Self::WrongIdentity => exited(0, "", Self::marker(&"0".repeat(64), challenge)),
                Self::TimedOut => Execution {
                    completion: ExecutionCompletion::TimedOut { code: None },
                    stdout: whole(""),
                    stderr: whole(""),
                },
            })
        }
    }

    #[test]
    fn native_spec_uses_the_shared_sealed_process_laws() {
        let fixture = Fixture::new(Runner::Attesting);
        let runner = fixture.runner();
        let spec = runner
            .spec(
                "cargo",
                &["test".to_owned(), "--locked".to_owned()],
                &Challenge::fixture("fixture-challenge"),
            )
            .expect("native spec");
        let arguments = arguments(&spec);

        assert_eq!(spec.group, ProcessGroup::New);
        assert!(spec.environment.entries().is_empty());
        assert_eq!(
            spec.cwd,
            fixture.workspace.canonicalize().expect("workspace")
        );
        assert_eq!(spec.output.stdin(), StdinPolicy::Null);
        assert!(matches!(
            spec.output.stdout(),
            StreamPolicy::Capture { limit: 4096 }
        ));
        assert!(matches!(
            spec.output.stderr(),
            StreamPolicy::Capture { limit: 4096 }
        ));
        assert_eq!(spec.deadlines.runtime(), Duration::from_secs(30));
        assert_eq!(
            arguments[0], "spice-proof-native-runner-v1",
            "the versioned protocol is the first and only runner operation"
        );
        assert_eq!(arguments[1], runner.identity().as_str());
        assert_eq!(arguments[2], "fixture-challenge");
        assert_eq!(&arguments[3..], ["cargo", "test", "--locked"]);
    }

    #[test]
    fn every_battery_member_runs_and_attestation_never_enters_evidence() {
        let fixture = Fixture::new(Runner::Attesting);
        let environment = SealedEnvironment::fixture_native(fixture.runner());
        let verdict = replay(&environment).expect("all native exchanges attest");
        let mut challenges = std::collections::BTreeSet::new();

        assert_eq!(verdict.checks().len(), CheckKind::ALL.len());
        for (verdict, expected) in verdict.checks().iter().zip(CheckKind::ALL) {
            assert_eq!(verdict.kind(), expected);
            assert!(verdict.stderr().is_empty());
            let output = std::str::from_utf8(verdict.stdout()).expect("fixture output is UTF-8");
            let mut lines = output.lines();
            let challenge = lines
                .next()
                .and_then(|line| line.strip_prefix("challenge:"))
                .expect("fixture reports the received challenge");
            assert_eq!(challenge.len(), 64);
            assert!(challenges.insert(challenge.to_owned()));
            assert!(
                lines
                    .next()
                    .is_some_and(|line| line.starts_with("executed:cargo "))
            );
        }
        assert_eq!(challenges.len(), CheckKind::ALL.len());
    }

    #[test]
    fn fixed_stale_wrong_missing_failed_and_timed_out_attestations_refuse_execution() {
        for answer in [
            Runner::Silent,
            Runner::FixedMarker,
            Runner::StaleChallenge,
            Runner::WrongIdentity,
            Runner::Failing(70),
            Runner::TimedOut,
        ] {
            let fixture = Fixture::new(answer);
            let error = fixture
                .runner()
                .execute("cargo", &["test".to_owned()])
                .expect_err("invalid attestation cannot create Execution");
            assert!(
                matches!(error, NativeError::RunnerNotAttested { .. }),
                "runner {answer:?} refused for another reason: {error:?}"
            );
        }
    }

    #[test]
    fn preflight_requires_an_attested_clean_cargo_probe() {
        Fixture::new(Runner::Attesting)
            .runner()
            .preflight()
            .expect("valid runner preflight");

        let error = Fixture::new(Runner::AttestingFailure(70))
            .runner()
            .preflight()
            .expect_err("nonzero attested probe is not a clean preflight");
        assert!(matches!(
            error,
            NativeError::PreflightFailed { code: Some(70) }
        ));
    }

    #[test]
    fn truncated_and_incomplete_attestation_captures_refuse() {
        let fixture = Fixture::new(Runner::Attesting);
        let runner = fixture.runner();
        let challenge = Challenge::fixture("fixture-challenge");
        for (truncated, complete) in [(true, true), (false, false), (true, false)] {
            let mut capture = Captured {
                bytes: Vec::new(),
                truncated,
                complete,
            };
            assert!(matches!(
                runner.verify_attestation(&mut capture, &challenge),
                Err(NativeError::IncompleteAttestation { .. })
            ));
        }
    }

    #[test]
    fn runner_byte_mutation_after_admission_refuses_before_process_start() {
        let fixture = Fixture::new(Runner::Attesting);
        let runner = fixture.runner();
        crate::test_support::executable(&fixture.executable, b"mutated runner");

        assert!(matches!(
            runner.execute("cargo", &["test".to_owned()]),
            Err(NativeError::RunnerChanged { .. })
        ));
    }

    struct Fixture {
        root: std::path::PathBuf,
        executable: std::path::PathBuf,
        workspace: std::path::PathBuf,
        answer: Runner,
    }

    impl Fixture {
        fn new(answer: Runner) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "spice-proof-native-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let executable = root.join("runner");
            let workspace = root.join("workspace");
            fs::create_dir_all(&workspace).expect("fixture workspace");
            crate::test_support::executable(&executable, b"admitted runner");
            Self {
                root,
                executable,
                workspace,
                answer,
            }
        }

        fn runner(&self) -> NativeRunner {
            NativeRunner::attest_with(
                Arc::new(self.answer),
                &self.executable,
                &self.workspace,
                limits(),
            )
            .expect("fixture runner admits")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("remove fixture");
        }
    }

    fn limits() -> Limits {
        Limits::new(
            Deadlines::new(
                Duration::from_secs(30),
                Duration::from_secs(5),
                Duration::from_secs(5),
            )
            .expect("deadlines"),
            Duration::from_millis(10),
            4096,
        )
        .expect("limits")
    }
}
