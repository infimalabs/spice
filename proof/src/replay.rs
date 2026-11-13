//! Exhaustive sealed replay of the generated proof battery.

use std::error::Error;
use std::fmt;

use windows_execution::protocol::Command as HostedCommand;
use wire::manifest::{
    PROOF_RECEIPT_VERSION, ProofEnvironment, ProofReceipt, ProofStanding, ReleaseTarget,
    TargetPayload,
};
use wire::{Digest, Domain};

use crate::container::{Execution, ExecutionCompletion};
use crate::environment::{BatteryEnvironment, BatteryStartError, SealedEnvironment};

/// The complete generated proof battery, in execution order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CheckKind {
    Gate,
    DialectRefusals,
    WaitAndProcess,
    SupplyRebuild,
}

impl CheckKind {
    /// Every required member in deterministic execution order.
    pub const ALL: [Self; 4] = [
        Self::Gate,
        Self::DialectRefusals,
        Self::WaitAndProcess,
        Self::SupplyRebuild,
    ];

    /// Stable spelling mixed into battery and replay identities.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gate => "gate",
            Self::DialectRefusals => "dialect-refusals",
            Self::WaitAndProcess => "wait-and-process",
            Self::SupplyRebuild => "supply-rebuild",
        }
    }
}

/// One explicit command in the generated battery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Check {
    kind: CheckKind,
    program: String,
    arguments: Vec<String>,
}

impl Check {
    fn current(kind: CheckKind) -> Self {
        let arguments = match kind {
            CheckKind::Gate => ["test", "--locked", "-p", "gate", "--all-targets"].as_slice(),
            // The omp dialect is the sole decoder; its suite owns the refused
            // wire, unowned record, and residue contracts.
            CheckKind::DialectRefusals => {
                ["test", "--locked", "-p", "listen", "--test", "omp"].as_slice()
            }
            CheckKind::WaitAndProcess => ["test", "--locked", "-p", "sill", "--lib"].as_slice(),
            CheckKind::SupplyRebuild => {
                ["test", "--locked", "-p", "dist", "--test", "pipeline"].as_slice()
            }
        };
        Self {
            kind,
            program: "cargo".to_owned(),
            arguments: arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> CheckKind {
        self.kind
    }

    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }

    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }
}

/// Exactly one command for every required proof class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Battery {
    checks: Vec<Check>,
    digest: Digest,
}

impl Battery {
    /// The sole compiled battery, generated exhaustively from [`CheckKind::ALL`].
    #[must_use]
    pub fn current() -> Self {
        let checks = CheckKind::ALL
            .into_iter()
            .map(Check::current)
            .collect::<Vec<_>>();
        let digest = battery_digest(&checks);
        Self { checks, digest }
    }

    /// Ordered commands the replay will consume.
    #[must_use]
    pub fn checks(&self) -> &[Check] {
        &self.checks
    }

    /// Identity of every command and argument in the complete inventory.
    #[must_use]
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }
}

/// One member's typed result and complete bounded output evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckVerdict {
    kind: CheckKind,
    standing: CheckStanding,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl CheckVerdict {
    #[must_use]
    pub const fn kind(&self) -> CheckKind {
        self.kind
    }

    #[must_use]
    pub const fn standing(&self) -> CheckStanding {
        self.standing
    }

    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// The trailing output that names why this member failed, or `None` when
    /// it passed.
    ///
    /// Each stream contributes at most its last 2 KiB, starting at a line
    /// boundary: a member's cause is printed near its end, while a complete
    /// build log would bury it.
    #[must_use]
    pub fn diagnosis(&self) -> Option<String> {
        if self.standing.passes() {
            return None;
        }
        let mut diagnosis = String::new();
        for (stream, bytes) in [("stderr", &self.stderr), ("stdout", &self.stdout)] {
            let tail = output_tail(bytes);
            if !tail.is_empty() {
                diagnosis.push_str(stream);
                diagnosis.push_str(":\n");
                diagnosis.push_str(&String::from_utf8_lossy(tail));
                diagnosis.push('\n');
            }
        }
        Some(diagnosis)
    }
}

/// Most trailing bytes of one stream a failing member's diagnosis carries.
const DIAGNOSIS_STREAM_BYTES: usize = 2048;

fn output_tail(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.trim_ascii_end();
    let start = bytes.len().saturating_sub(DIAGNOSIS_STREAM_BYTES);
    let tail = &bytes[start..];
    match tail.iter().position(|byte| *byte == b'\n') {
        Some(newline) if start > 0 => &tail[newline + 1..],
        _ => tail,
    }
}

/// Exit code for a member whose scope selected nothing.
///
/// The original spice gritctl exited 5 when its scope selected nothing, after
/// pytest's 5 for "no tests collected", and every member here keeps that
/// meaning. It is a standing of its own because an empty scope must stay
/// distinguishable from a real failure just as much as from a clean run.
const EMPTY_SCOPE: i32 = 5;

/// Why one declared battery member did or did not pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckStanding {
    Clean,
    EmptyScope,
    Failed { code: Option<i32> },
    TimedOut { code: Option<i32> },
    Cancelled { code: Option<i32> },
    OutputTruncated,
    OutputIncomplete,
}

impl CheckStanding {
    #[must_use]
    pub const fn passes(self) -> bool {
        matches!(self, Self::Clean)
    }

    /// Stable spelling mixed into replay identities and shown to operators.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::EmptyScope => "empty-scope",
            Self::Failed { .. } => "failed",
            Self::TimedOut { .. } => "timed-out",
            Self::Cancelled { .. } => "cancelled",
            Self::OutputTruncated => "output-truncated",
            Self::OutputIncomplete => "output-incomplete",
        }
    }

    /// Exit code this standing reports, which for an empty scope is the
    /// conventional 5 for selecting nothing rather than the 0 the tool exited.
    #[must_use]
    pub const fn code(self) -> Option<i32> {
        match self {
            Self::EmptyScope => Some(EMPTY_SCOPE),
            Self::Failed { code } | Self::TimedOut { code } | Self::Cancelled { code } => code,
            Self::Clean | Self::OutputTruncated | Self::OutputIncomplete => None,
        }
    }
}

/// One typed verdict over the complete battery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayVerdict {
    target: ReleaseTarget,
    environment: ProofEnvironment,
    battery: Digest,
    replay: Digest,
    standing: ProofStanding,
    checks: Vec<CheckVerdict>,
}

impl ReplayVerdict {
    #[must_use]
    pub const fn target(&self) -> ReleaseTarget {
        self.target
    }

    #[must_use]
    pub const fn environment(&self) -> &ProofEnvironment {
        &self.environment
    }

    #[must_use]
    pub const fn battery_digest(&self) -> &Digest {
        &self.battery
    }

    #[must_use]
    pub const fn replay_digest(&self) -> &Digest {
        &self.replay
    }

    #[must_use]
    pub const fn standing(&self) -> ProofStanding {
        self.standing
    }

    #[must_use]
    pub fn checks(&self) -> &[CheckVerdict] {
        &self.checks
    }

    /// Bind this complete replay to one exact receipt-free target payload.
    pub fn receipt(
        &self,
        payload: &TargetPayload,
        payload_digest: Digest,
    ) -> Result<ProofReceipt, ReceiptError> {
        if payload.target != self.target {
            return Err(ReceiptError {
                payload: payload.target,
                replay: self.target,
            });
        }
        Ok(ProofReceipt {
            version: PROOF_RECEIPT_VERSION,
            target: self.target,
            payload: payload_digest,
            battery: self.battery.clone(),
            replay: self.replay.clone(),
            environment: self.environment.clone(),
            standing: self.standing,
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture_clean(
        target: ReleaseTarget,
        environment: ProofEnvironment,
        battery: Digest,
    ) -> Self {
        Self::fixture_with(
            target,
            environment,
            battery,
            [CheckStanding::Clean; CheckKind::ALL.len()],
        )
    }

    #[cfg(test)]
    pub(crate) fn fixture_with(
        target: ReleaseTarget,
        environment: ProofEnvironment,
        battery: Digest,
        standings: [CheckStanding; CheckKind::ALL.len()],
    ) -> Self {
        let checks = CheckKind::ALL
            .into_iter()
            .zip(standings)
            .map(|(kind, standing)| CheckVerdict {
                kind,
                standing,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
            .collect::<Vec<_>>();
        let standing = if checks.iter().all(|check| check.standing.passes()) {
            ProofStanding::Clean
        } else {
            ProofStanding::Refused
        };
        let replay = replay_digest(target, &environment, &battery, standing, &checks);
        Self {
            target,
            environment,
            battery,
            replay,
            standing,
            checks,
        }
    }
}

/// Execute the complete current battery through one sealed environment.
///
/// # Errors
///
/// Returns the exact member whose process or adjacent battery boundary could
/// not be established or observed. Ordinary nonzero exits and timeouts remain
/// inside the one typed [`ReplayVerdict`].
pub fn replay(environment: &SealedEnvironment) -> Result<ReplayVerdict, ReplayError> {
    let battery = Battery::current();
    let commands = battery
        .checks
        .iter()
        .map(|check| HostedCommand {
            program: match check.program() {
                "cargo" => "@cargo".to_owned(),
                program => program.to_owned(),
            },
            arguments: check.arguments().to_vec(),
        })
        .collect::<Vec<_>>();
    let executor = environment
        .battery(&commands)
        .map_err(|source| ReplayError::from_start(CheckKind::Gate, source))?;
    let replay = run_with(&executor, &battery);
    let cleanup = executor.close();
    match (replay, cleanup) {
        (Ok(verdict), Ok(())) => Ok(verdict),
        (Ok(_), Err(source)) => Err(ReplayError::execution(CheckKind::SupplyRebuild, source)),
        (Err(error), Ok(())) => Err(error),
        (Err(mut error), Err(cleanup)) => {
            error
                .source
                .push_str(&format!("; battery scratch cleanup also failed: {cleanup}"));
            Err(error)
        }
    }
}

trait Executor {
    fn target(&self) -> ReleaseTarget;
    fn environment(&self) -> ProofEnvironment;
    fn execute(&self, check: &Check) -> Result<Execution, String>;
}

impl Executor for BatteryEnvironment<'_> {
    fn target(&self) -> ReleaseTarget {
        self.target()
    }

    fn environment(&self) -> ProofEnvironment {
        self.identity()
    }

    fn execute(&self, check: &Check) -> Result<Execution, String> {
        self.execute(check.program(), check.arguments())
    }
}

fn run_with<E: Executor>(executor: &E, battery: &Battery) -> Result<ReplayVerdict, ReplayError> {
    let mut checks = Vec::with_capacity(battery.checks.len());
    for check in &battery.checks {
        let execution = executor
            .execute(check)
            .map_err(|source| ReplayError::execution(check.kind, source))?;
        checks.push(classify(check.kind, execution));
    }
    let standing = if checks.iter().all(|check| check.standing.passes()) {
        ProofStanding::Clean
    } else {
        ProofStanding::Refused
    };
    let target = executor.target();
    let environment = executor.environment();
    let replay = replay_digest(target, &environment, battery.digest(), standing, &checks);
    Ok(ReplayVerdict {
        target,
        environment,
        battery: battery.digest.clone(),
        replay,
        standing,
        checks,
    })
}

fn classify(kind: CheckKind, execution: Execution) -> CheckVerdict {
    let standing = match execution.completion {
        ExecutionCompletion::Exited {
            success: false,
            code,
        } => CheckStanding::Failed { code },
        ExecutionCompletion::TimedOut { code } => CheckStanding::TimedOut { code },
        ExecutionCompletion::Cancelled { code } => CheckStanding::Cancelled { code },
        ExecutionCompletion::Exited { success: true, .. }
            if execution.stdout.truncated || execution.stderr.truncated =>
        {
            CheckStanding::OutputTruncated
        }
        ExecutionCompletion::Exited { success: true, .. }
            if !execution.stdout.complete || !execution.stderr.complete =>
        {
            CheckStanding::OutputIncomplete
        }
        ExecutionCompletion::Exited { success: true, .. }
            if !proved_a_corpus(kind, &execution.stdout.bytes) =>
        {
            CheckStanding::EmptyScope
        }
        ExecutionCompletion::Exited { success: true, .. } => CheckStanding::Clean,
    };
    CheckVerdict {
        kind,
        standing,
        stdout: execution.stdout.bytes,
        stderr: execution.stderr.bytes,
    }
}

/// Whether a clean member proved it had a corpus to test.
///
/// Every test runner in this battery selects nothing without complaining: a
/// clean exit is also what it gives for an empty scope, so the exit code alone
/// cannot carry this. The retained libtest summary proves at least one test ran.
fn proved_a_corpus(_kind: CheckKind, stdout: &[u8]) -> bool {
    executed_a_test(stdout)
}

/// Whether a clean cargo test run proved it executed a test, not merely announced one.
///
/// A selection that matched nothing is not an error to libtest either, and the
/// package and target names cargo was given cannot stand in for the reading:
/// they refuse a name that no longer exists, but say nothing about a target
/// whose tests were emptied or all filtered out. Measured against the installed
/// harness, the dialect member's binary run with `--exact __no_such_test__`
/// prints `running 0 tests`, then `test result: ok. 0 passed; 0 failed; 0
/// ignored; 0 measured; 7 filtered out`, and exits 0 -- the same exit it gives
/// after `running 7 tests`.
///
/// The announcement is not that proof, because it counts the ignored alongside
/// the executed. Measured on the same harness, `cargo test --locked -p sill
/// --lib macos_guardian_exact_descriptor_fixture` prints `running 1 test`, then
/// `test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 39 filtered
/// out`, and exits 0: one test announced and none run. A target whose tests are
/// all `#[ignore]`d is the emptiness this refuses, so the summary is read
/// instead, where the executed and the skipped are finally separate numbers.
///
/// Only the passed count is read, because a member that had a failure did not
/// exit 0 and was classified before reaching here, so on this path the passed
/// count is the whole of what ran.
///
/// One target executing nothing is not that vacuity and is not refused: a member
/// runs every target its package selects, and 22 of them announce zero in a
/// passing workspace run, because a crate with no unit tests still has a lib
/// target to announce. It is the member as a whole that has to have run
/// something, so one target that ran a test answers for it.
fn executed_a_test(stdout: &[u8]) -> bool {
    test_summaries(stdout)
        .iter()
        .any(|summary| summary.passed > 0)
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TestSummary {
    passed: u64,
    failed: u64,
    ignored: u64,
    measured: u64,
    filtered: u64,
}

fn test_summaries(stdout: &[u8]) -> Vec<TestSummary> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| {
            let (_, counts) = line.strip_prefix("test result: ")?.split_once(". ")?;
            let fields = counts.split_whitespace().collect::<Vec<_>>();
            Some(TestSummary {
                passed: fields.first()?.parse().ok()?,
                failed: fields.get(2)?.parse().ok()?,
                ignored: fields.get(4)?.parse().ok()?,
                measured: fields.get(6)?.parse().ok()?,
                filtered: fields.get(8)?.parse().ok()?,
            })
        })
        .collect()
}

fn battery_digest(checks: &[Check]) -> Digest {
    let mut bytes = Vec::new();
    frame_usize(&mut bytes, checks.len());
    for check in checks {
        frame_bytes(&mut bytes, check.kind.as_str().as_bytes());
        frame_bytes(&mut bytes, check.program.as_bytes());
        frame_usize(&mut bytes, check.arguments.len());
        for argument in &check.arguments {
            frame_bytes(&mut bytes, argument.as_bytes());
        }
    }
    Digest::compute(Domain::ProofBattery, &bytes)
}

fn replay_digest(
    target: ReleaseTarget,
    environment: &ProofEnvironment,
    battery: &Digest,
    standing: ProofStanding,
    checks: &[CheckVerdict],
) -> Digest {
    let mut bytes = Vec::new();
    frame_bytes(&mut bytes, target.as_str().as_bytes());
    frame_bytes(
        &mut bytes,
        &wire::encode(environment).expect("ProofEnvironment always has canonical wire bytes"),
    );
    frame_bytes(&mut bytes, battery.as_str().as_bytes());
    frame_bytes(
        &mut bytes,
        match standing {
            ProofStanding::Clean => b"clean",
            ProofStanding::Refused => b"refused",
        },
    );
    frame_usize(&mut bytes, checks.len());
    for check in checks {
        frame_bytes(&mut bytes, check.kind.as_str().as_bytes());
        frame_bytes(&mut bytes, check.standing.as_str().as_bytes());
        match check.standing.code() {
            Some(code) => {
                bytes.push(1);
                bytes.extend_from_slice(&code.to_be_bytes());
            }
            None => bytes.push(0),
        }
        frame_check_evidence(&mut bytes, check);
    }
    Digest::compute(Domain::ProofReplay, &bytes)
}

/// Bind reproducible evidence for clean runs and full diagnostics for refusals.
///
/// Cargo and libtest print elapsed times, fresh output paths, and parallel
/// compiler scheduling to their transcripts. Those bytes remain on
/// [`CheckVerdict`] for diagnosis, but cannot identify a replay another
/// execution can reproduce. A successful member is instead identified by the
/// structured corpus measurement that made it non-empty. A refused member has
/// no publishable receipt to reproduce, so its complete bounded transcript
/// remains part of the refusal identity.
fn frame_check_evidence(bytes: &mut Vec<u8>, check: &CheckVerdict) {
    match check.standing {
        CheckStanding::Clean | CheckStanding::EmptyScope => {
            bytes.push(0);
            bytes.push(1);
            let mut summaries = test_summaries(&check.stdout);
            summaries.sort_unstable();
            frame_usize(bytes, summaries.len());
            for summary in summaries {
                for count in [
                    summary.passed,
                    summary.failed,
                    summary.ignored,
                    summary.measured,
                    summary.filtered,
                ] {
                    frame_u64(bytes, count);
                }
            }
        }
        CheckStanding::Failed { .. }
        | CheckStanding::TimedOut { .. }
        | CheckStanding::Cancelled { .. }
        | CheckStanding::OutputTruncated
        | CheckStanding::OutputIncomplete => {
            bytes.push(1);
            frame_bytes(bytes, &check.stdout);
            frame_bytes(bytes, &check.stderr);
        }
    }
}

fn frame_usize(bytes: &mut Vec<u8>, value: usize) {
    frame_u64(bytes, value as u64);
}

fn frame_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn frame_bytes(bytes: &mut Vec<u8>, value: &[u8]) {
    frame_usize(bytes, value.len());
    bytes.extend_from_slice(value);
}

/// Stable class of infrastructure failure preventing one replay verdict.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayFailure {
    Execution,
    HostedAttestationPrerequisite,
}

/// Infrastructure failure that prevented one complete replay verdict.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayError {
    check: CheckKind,
    failure: ReplayFailure,
    source: String,
}

impl ReplayError {
    #[must_use]
    pub const fn check(&self) -> CheckKind {
        self.check
    }

    #[must_use]
    pub const fn failure(&self) -> ReplayFailure {
        self.failure
    }

    #[must_use]
    pub fn evidence(&self) -> &str {
        &self.source
    }

    fn execution(check: CheckKind, source: String) -> Self {
        Self {
            check,
            failure: ReplayFailure::Execution,
            source,
        }
    }

    fn from_start(check: CheckKind, error: BatteryStartError) -> Self {
        match error {
            BatteryStartError::HostedAttestationPrerequisite {
                repository,
                visibility,
            } => Self {
                check,
                failure: ReplayFailure::HostedAttestationPrerequisite,
                source: format!(
                    "public hosted-attestation prerequisite is unsatisfied: repository {repository} has visibility {visibility:?}"
                ),
            },
            BatteryStartError::Execution(source) => Self::execution(check, source),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(check: CheckKind, source: impl Into<String>) -> Self {
        Self::execution(check, source.into())
    }
}

impl fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "sealed {} battery member could not execute: {}",
            self.check.as_str(),
            self.source
        )
    }
}

impl Error for ReplayError {}

/// A replay cannot mint a receipt for another release target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiptError {
    payload: ReleaseTarget,
    replay: ReleaseTarget,
}

impl fmt::Display for ReceiptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cannot bind {} replay to {} target payload",
            self.replay, self.payload
        )
    }
}

impl Error for ReceiptError {}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use wire::manifest::{ProofEnvironment, ReleaseTarget};

    use super::{
        Battery, Check, CheckKind, CheckStanding, Executor, ProofStanding, ReplayError,
        ReplayFailure, run_with,
    };
    use crate::container::{Captured, Execution, ExecutionCompletion};
    use crate::environment::BatteryStartError;

    #[test]
    fn private_repository_refusal_keeps_its_typed_prerequisite() {
        let error = ReplayError::from_start(
            CheckKind::Gate,
            BatteryStartError::HostedAttestationPrerequisite {
                repository: "infimalabs/spice".to_owned(),
                visibility: "private".to_owned(),
            },
        );
        assert_eq!(
            error.failure(),
            ReplayFailure::HostedAttestationPrerequisite
        );
        assert_eq!(error.check(), CheckKind::Gate);
    }
    #[test]
    fn the_current_battery_pins_every_member_and_command_in_order() {
        let battery = Battery::current();
        let commands = battery
            .checks()
            .iter()
            .map(|check| {
                (
                    check.kind(),
                    check.program(),
                    check
                        .arguments()
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            commands,
            [
                (
                    CheckKind::Gate,
                    "cargo",
                    vec!["test", "--locked", "-p", "gate", "--all-targets"]
                ),
                (
                    CheckKind::DialectRefusals,
                    "cargo",
                    vec!["test", "--locked", "-p", "listen", "--test", "omp"]
                ),
                (
                    CheckKind::WaitAndProcess,
                    "cargo",
                    vec!["test", "--locked", "-p", "sill", "--lib"]
                ),
                (
                    CheckKind::SupplyRebuild,
                    "cargo",
                    vec!["test", "--locked", "-p", "dist", "--test", "pipeline"]
                ),
            ]
        );
    }

    /// One clean test run, in the words the installed libtest harness uses.
    ///
    /// Transcribed from the dialect member's own binary on this tree rather
    /// than composed, because what the guard reads is a spelling no argument
    /// can settle.
    const TEST_EVIDENCE: &[u8] = concat!(
        "running 7 tests\n",
        "test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; ",
        "0 filtered out; finished in 0.00s\n",
    )
    .as_bytes();

    const TEST_EVIDENCE_OTHER_RUNTIME: &[u8] = concat!(
        "running 7 tests\n",
        "test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; ",
        "0 filtered out; finished in 4.27s\n",
    )
    .as_bytes();

    /// The same clean exit after the member's tests stopped being selected.
    ///
    /// Taken from running that member's own binary with `--exact
    /// __no_such_test__`, which is what an emptied or renamed target leaves
    /// behind: nothing ran, and the harness still exits 0.
    const EMPTY_SELECTION_EVIDENCE: &[u8] = concat!(
        "running 0 tests\n",
        "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; ",
        "7 filtered out; finished in 0.00s\n",
    )
    .as_bytes();

    /// The same clean exit after the member's tests were all `#[ignore]`d.
    ///
    /// Transcribed from `cargo test --locked -p sill --lib
    /// macos_guardian_exact_descriptor_fixture` on this tree. The announcement
    /// counts the ignored test, so the target claims a corpus of one while
    /// running none of it; only the summary tells the two apart.
    const ALL_IGNORED_EVIDENCE: &[u8] = concat!(
        "running 1 test\n",
        "test spawn::guardian::tests::macos_guardian_exact_descriptor_fixture ",
        "... ignored, subprocess fixture isolates RLIMIT_NOFILE mutation\n",
        "test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; ",
        "39 filtered out; finished in 0.00s\n",
    )
    .as_bytes();

    /// A member whose package holds more targets than it holds tests.
    ///
    /// The ordinary shape of a real member, transcribed from one passing
    /// workspace run: the `app` lib target announces zero because its tests all
    /// live in its integration targets, and `dialect`'s lib announces the
    /// singular `1 test`. Twenty-two targets announce zero in that run, so a
    /// reading that called any of them empty would refuse the whole battery.
    const MIXED_TARGET_EVIDENCE: &[u8] = concat!(
        "running 0 tests\n",
        "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; ",
        "0 filtered out; finished in 0.00s\n",
        "running 1 test\n",
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; ",
        "0 filtered out; finished in 0.00s\n",
    )
    .as_bytes();

    #[test]
    fn replay_consumes_every_member_and_returns_one_typed_verdict() {
        let battery = Battery::current();
        let executor = FakeExecutor::new(vec![
            exited(true, TEST_EVIDENCE),
            exited(true, TEST_EVIDENCE),
            exited(true, TEST_EVIDENCE),
            exited(true, TEST_EVIDENCE),
        ]);

        let verdict = run_with(&executor, &battery).expect("complete replay");

        assert_eq!(verdict.standing(), ProofStanding::Clean);
        assert_eq!(verdict.checks().len(), CheckKind::ALL.len());
        assert_eq!(executor.ran.borrow().as_slice(), CheckKind::ALL);
        assert_eq!(verdict.battery_digest(), battery.digest());
        assert_eq!(verdict.environment(), &environment());
        assert_eq!(verdict.target(), ReleaseTarget::X86_64UnknownLinuxMusl);
    }

    #[test]
    fn clean_replay_identity_uses_corpus_measurements_not_incidental_transcript_bytes() {
        let first = run_with(
            &FakeExecutor::new(vec![
                exited_with_stderr(true, TEST_EVIDENCE, b"Running /fresh/one/gate"),
                exited(true, TEST_EVIDENCE),
                exited(true, TEST_EVIDENCE),
                exited(true, TEST_EVIDENCE),
            ]),
            &Battery::current(),
        )
        .expect("first clean replay");
        let repeated = run_with(
            &FakeExecutor::new(vec![
                exited_with_stderr(
                    true,
                    TEST_EVIDENCE_OTHER_RUNTIME,
                    b"Running /fresh/two/gate",
                ),
                exited(true, TEST_EVIDENCE_OTHER_RUNTIME),
                exited(true, TEST_EVIDENCE_OTHER_RUNTIME),
                exited(true, TEST_EVIDENCE_OTHER_RUNTIME),
            ]),
            &Battery::current(),
        )
        .expect("repeated clean replay");

        assert_ne!(first.checks()[0].stdout(), repeated.checks()[0].stdout());
        assert_ne!(first.checks()[0].stderr(), repeated.checks()[0].stderr());
        assert_eq!(first.replay_digest(), repeated.replay_digest());
    }

    #[test]
    fn a_refused_gate_and_a_later_failure_remain_in_the_one_complete_verdict() {
        let battery = Battery::current();
        let executor = FakeExecutor::new(vec![
            exited(false, b"gate failed"),
            exited(false, b"dialect failed"),
            exited(true, TEST_EVIDENCE),
            exited(true, TEST_EVIDENCE),
        ]);

        let verdict = run_with(&executor, &battery).expect("typed refused replay");

        assert_eq!(verdict.standing(), ProofStanding::Refused);
        assert_eq!(
            verdict.checks()[0].standing(),
            CheckStanding::Failed { code: Some(1) }
        );
        assert_eq!(
            verdict.checks()[1].standing(),
            CheckStanding::Failed { code: Some(1) }
        );
        assert_eq!(executor.ran.borrow().as_slice(), CheckKind::ALL);
    }

    /// A refused battery names each failing member's own cause.
    ///
    /// The decisive line sits at the end of a member's output, after a build
    /// log far larger than any refusal should carry, so the diagnosis keeps a
    /// bounded tail of whole lines and passing members contribute nothing.
    #[test]
    fn failing_members_diagnose_from_a_bounded_tail_of_their_own_output() {
        let mut build_log = (0..400)
            .map(|line| format!("   Compiling crate-{line} v0.1.0\n"))
            .collect::<String>();
        build_log.push_str("error: verification failed at final unit\n");
        let executor = FakeExecutor::new(vec![
            exited(true, TEST_EVIDENCE),
            exited(false, build_log.as_bytes()),
            exited_with_stderr(
                false,
                b"",
                b"error: no test target named `corpus` in `listen` package\n",
            ),
            exited(true, TEST_EVIDENCE),
        ]);

        let verdict = run_with(&executor, &Battery::current()).expect("typed refused replay");

        assert_eq!(verdict.standing(), ProofStanding::Refused);
        assert_eq!(verdict.checks()[0].diagnosis(), None);
        let diagnosis = verdict.checks()[1]
            .diagnosis()
            .expect("a failed member diagnoses");
        assert!(diagnosis.ends_with("error: verification failed at final unit\n"));
        assert!(diagnosis.len() <= super::DIAGNOSIS_STREAM_BYTES + "stdout:\n\n".len());
        let first = diagnosis
            .strip_prefix("stdout:\n")
            .expect("only stdout carried output");
        assert!(first.starts_with("   Compiling crate-"));
        assert!(!diagnosis.contains("crate-0 "));
        assert_eq!(
            verdict.checks()[2].diagnosis().as_deref(),
            Some("stderr:\nerror: no test target named `corpus` in `listen` package\n"),
        );
    }

    /// A test member that ran no test is the same emptiness, and exits 0 too.
    ///
    /// The dialect member is a whole test binary selected by package and target
    /// name, and both names survive the tests inside them being emptied,
    /// renamed, or filtered out. libtest then prints this and exits 0, which is
    /// the exit a passing member gives, so the member would be sealed into a
    /// receipt naming a refusal corpus that never ran.
    #[test]
    fn a_test_member_that_ran_no_tests_refuses_the_battery() {
        let executor = FakeExecutor::new(vec![
            exited(true, TEST_EVIDENCE),
            exited(true, EMPTY_SELECTION_EVIDENCE),
            exited(true, TEST_EVIDENCE),
            exited(true, TEST_EVIDENCE),
        ]);

        let verdict =
            run_with(&executor, &Battery::current()).expect("typed empty-selection replay");

        assert_eq!(verdict.checks()[1].kind(), CheckKind::DialectRefusals);
        assert_eq!(verdict.checks()[1].standing(), CheckStanding::EmptyScope);
        assert_eq!(verdict.checks()[1].standing().code(), Some(5));
        assert_eq!(verdict.standing(), ProofStanding::Refused);
    }

    /// A member that announced a test and ran none is the same emptiness.
    ///
    /// The wait-and-process member is the one that already carries an
    /// `#[ignore]`d test, so `#[ignore]` reaching the rest of its target is a
    /// live shape rather than a hypothetical one. libtest counts the skipped
    /// test in its announcement and exits 0, so a member read by what it
    /// announced would be sealed into a receipt naming process supervision that
    /// no test exercised.
    #[test]
    fn a_test_member_whose_tests_were_all_ignored_refuses_the_battery() {
        let executor = FakeExecutor::new(vec![
            exited(true, TEST_EVIDENCE),
            exited(true, TEST_EVIDENCE),
            exited(true, ALL_IGNORED_EVIDENCE),
            exited(true, TEST_EVIDENCE),
        ]);

        let verdict = run_with(&executor, &Battery::current()).expect("typed all-ignored replay");

        assert_eq!(verdict.checks()[2].kind(), CheckKind::WaitAndProcess);
        assert_eq!(verdict.checks()[2].standing(), CheckStanding::EmptyScope);
        assert_eq!(verdict.checks()[2].standing().code(), Some(5));
        assert_eq!(verdict.standing(), ProofStanding::Refused);
    }

    /// One target announcing zero tests is not an empty member.
    ///
    /// A member runs every target its scope selects, and a package with no unit
    /// tests in its lib still announces that target at zero. Reading the member
    /// as empty unless every target ran something would refuse the ordinary
    /// shape of a real run, so what is asked of the member is that something in
    /// it ran, not that everything did.
    #[test]
    fn a_member_whose_other_target_ran_nothing_still_passes() {
        let executor = FakeExecutor::new(vec![
            exited(true, TEST_EVIDENCE),
            exited(true, TEST_EVIDENCE),
            exited(true, MIXED_TARGET_EVIDENCE),
            exited(true, TEST_EVIDENCE),
        ]);

        let verdict = run_with(&executor, &Battery::current()).expect("typed mixed-target replay");

        assert_eq!(verdict.checks()[2].kind(), CheckKind::WaitAndProcess);
        assert_eq!(verdict.checks()[2].standing(), CheckStanding::Clean);
        assert_eq!(verdict.standing(), ProofStanding::Clean);
    }

    #[test]
    fn receipt_construction_refuses_a_payload_for_another_target() {
        use wire::manifest::{BuildPins, TargetPayload};
        use wire::{Digest, Domain};

        let verdict = run_with(
            &FakeExecutor::new(vec![
                exited(true, TEST_EVIDENCE),
                exited(true, TEST_EVIDENCE),
                exited(true, TEST_EVIDENCE),
                exited(true, TEST_EVIDENCE),
            ]),
            &Battery::current(),
        )
        .expect("fixture replay");
        let digest = || Digest::compute(Domain::DistArtifact, b"fixture");
        let payload = TargetPayload {
            version: wire::manifest::TARGET_PAYLOAD_VERSION,
            target: ReleaseTarget::Aarch64AppleDarwin,
            source: digest(),
            archive: digest(),
            app: digest(),
            canvas: digest(),
            native: digest(),
            symbols: None,
            pins: BuildPins {
                rust: String::new(),
                wasm_target: String::new(),
                trunk: String::new(),
                proof_container: ENVIRONMENT.to_owned(),
            },
            pins_digest: digest(),
            inventory: Vec::new(),
            inventory_digest: digest(),
        };

        assert!(verdict.receipt(&payload, digest()).is_err());
    }

    const ENVIRONMENT: &str = concat!(
        "registry.example/proof@sha256:",
        "4444444444444444444444444444444444444444444444444444444444444444"
    );

    fn environment() -> ProofEnvironment {
        ProofEnvironment::oci(ENVIRONMENT).expect("fixture image is pinned")
    }

    struct FakeExecutor {
        outcomes: RefCell<VecDeque<Execution>>,
        ran: RefCell<Vec<CheckKind>>,
    }

    impl FakeExecutor {
        fn new(outcomes: Vec<Execution>) -> Self {
            Self {
                outcomes: RefCell::new(outcomes.into()),
                ran: RefCell::new(Vec::new()),
            }
        }
    }

    impl Executor for FakeExecutor {
        fn target(&self) -> ReleaseTarget {
            ReleaseTarget::X86_64UnknownLinuxMusl
        }

        fn environment(&self) -> ProofEnvironment {
            environment()
        }

        fn execute(&self, check: &Check) -> Result<Execution, String> {
            self.ran.borrow_mut().push(check.kind());
            self.outcomes
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| "fixture has no execution outcome".to_owned())
        }
    }

    fn exited(success: bool, stdout: &[u8]) -> Execution {
        exited_with_stderr(success, stdout, &[])
    }

    fn exited_with_stderr(success: bool, stdout: &[u8], stderr: &[u8]) -> Execution {
        Execution {
            completion: ExecutionCompletion::Exited {
                success,
                code: Some(if success { 0 } else { 1 }),
            },
            stdout: Captured {
                bytes: stdout.to_vec(),
                truncated: false,
                complete: true,
            },
            stderr: Captured {
                bytes: stderr.to_vec(),
                truncated: false,
                complete: true,
            },
        }
    }
}
