//! Daemonless resource-lock commands at the native binary rim.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use crate::control_signal::ControlRelay;
use sill::{
    Argv, Cancellation, Completion, Contention, Deadlines, Environment, LockAttempt, LockReport,
    LockSurfaceError, LockTopology, Output, ProcessGroup, Redaction, RestartDisposition, Spec,
    StdinPolicy, StreamPolicy, WaitPolicy,
};
use spice::ambient::Ambient;
use spice::config::LoadError;
use spice::repository::Repository;
use wire::{Fault, FaultClass, Outcome, Refusal, Reply, ReplyPosition};

const OUTPUT_DRAIN: Duration = Duration::from_secs(1);
const REAP_DEADLINE: Duration = Duration::from_secs(5);
const TERMINATION_GRACE: Duration = Duration::from_secs(5);

/// Result of a lock-held caller command at the process boundary.
pub(crate) enum RunResult {
    Child(ExitStatus),
    Rejected { outcome: Outcome<()>, code: i32 },
}

/// Report the effective configured topology without opening or creating it.
pub(crate) fn report(
    configuration: Option<&spice::config::RepositoryCapture>,
) -> Outcome<LockReport> {
    let invocation = match Invocation::acquire(configuration) {
        Ok(invocation) => invocation,
        Err(failure) => return failure.outcome(),
    };
    Outcome::Reply(Reply {
        position: ReplyPosition::Direct {},
        body: match invocation.topology.report() {
            Ok(report) => report,
            Err(error) => return Failure::surface(error).outcome(),
        },
    })
}

/// Acquire one configured shard and hold it for the exact child lifetime.
pub(crate) fn run(
    matches: &clap::ArgMatches,
    configuration: Option<&spice::config::RepositoryCapture>,
    ambient: &Ambient,
) -> RunResult {
    match run_inner(matches, configuration, ambient) {
        Ok(status) => RunResult::Child(status),
        Err(rejection) => {
            let Rejection { outcome, code } = *rejection;
            RunResult::Rejected { outcome, code }
        }
    }
}

fn run_inner(
    matches: &clap::ArgMatches,
    configuration: Option<&spice::config::RepositoryCapture>,
    ambient: &Ambient,
) -> Result<ExitStatus, Box<Rejection>> {
    let name = matches
        .get_one::<String>("NAME")
        .expect("the generated parser requires a lock name");
    let requested = matches
        .get_one::<String>("shard")
        .map(|value| {
            value.parse::<u32>().map_err(|error| {
                Failure::refusal(
                    "lock::shard-refused",
                    "one zero-based configured shard number",
                    value,
                    format!("supply a configured shard number: {error}"),
                )
                .rejection(2)
            })
        })
        .transpose()?;
    let command = matches
        .get_many::<OsString>("CMD")
        .expect("the generated parser requires a trailing command")
        .cloned()
        .collect::<Vec<_>>();
    let invocation = Invocation::acquire(configuration).map_err(Failure::standard_rejection)?;
    let attempt = invocation
        .topology
        .try_acquire(name, requested)
        .map_err(|error| Failure::surface(error).standard_rejection())?;
    let mut held = match attempt {
        LockAttempt::Acquired(held) => held,
        LockAttempt::Contended { kind, code } => {
            return Err(contention(name, requested, kind).rejection(code.get()));
        }
    };
    let (program, arguments) = command
        .split_first()
        .expect("the generated parser requires a nonempty command");
    let cancellation = sill::CancellationToken::new();
    let relay = ControlRelay::install({
        let cancellation = cancellation.clone();
        move || cancellation.cancel()
    })
    .map_err(|error| {
        Failure::fault(
            FaultClass::Environment,
            "lock::signals-unavailable",
            error.to_string(),
        )
        .rejection(1)
    })?;
    let spec = child_spec(
        invocation.working,
        ambient.environment().map_err(|error| {
            Failure::fault(
                FaultClass::Environment,
                "lock::environment-unavailable",
                error.to_string(),
            )
            .rejection(1)
        })?,
        cancellation,
        program,
        arguments,
    )
    .map_err(|error| {
        Failure::fault(
            FaultClass::Protocol,
            "lock::command-invalid",
            error.to_string(),
        )
        .rejection(1)
    })?;
    let running = held
        .spawn_protected(spec)
        .map_err(|error| launch_failure(program, error))?;
    let waited = running.wait().map_err(|error| {
        Failure::fault(
            FaultClass::Environment,
            "lock::child-wait-failed",
            error.to_string(),
        )
        .rejection(1)
    })?;
    drop(relay);
    let status = match waited.completion() {
        Completion::Exited(status)
        | Completion::TimedOut(status)
        | Completion::Cancelled(status) => *status,
    };
    drop(held);
    Ok(status)
}

fn child_spec(
    working: PathBuf,
    environment: Environment,
    cancellation: sill::CancellationToken,
    program: &OsString,
    arguments: &[OsString],
) -> Result<Spec, sill::SpecError> {
    Ok(Spec {
        argv: Argv::new(program, arguments.iter().cloned())?,
        cwd: working,
        environment,
        group: ProcessGroup::New,
        deadlines: Deadlines::new(Duration::MAX, OUTPUT_DRAIN, REAP_DEADLINE)?,
        wait: WaitPolicy::new(
            "spice lock run child",
            Cancellation::Token(cancellation),
            RestartDisposition::Never,
        )?,
        output: Output::new(
            StdinPolicy::Inherit,
            StreamPolicy::Inherit,
            StreamPolicy::Inherit,
        ),
        redaction: Redaction::None,
        grace: TERMINATION_GRACE,
    })
}

fn contention(name: &str, shard: Option<u32>, kind: Contention) -> Failure {
    let selected = shard.map_or_else(
        || format!("the first free shard of {name:?}"),
        |shard| format!("{name:?} shard {shard}"),
    );
    Failure::refusal(
        kind.reason(),
        format!("{selected} to be available"),
        format!("{selected} is held"),
        "wait for the current holder to finish or select another configured shard",
    )
}

fn launch_failure(program: &OsString, error: io::Error) -> Box<Rejection> {
    let code = match error.kind() {
        io::ErrorKind::NotFound => 127,
        io::ErrorKind::PermissionDenied => 126,
        _ => 1,
    };
    Failure::fault(
        FaultClass::Environment,
        "lock::child-launch-failed",
        format!("cannot launch {program:?}: {error}"),
    )
    .rejection(code)
}

struct Invocation {
    working: PathBuf,
    topology: LockTopology,
}

impl Invocation {
    fn acquire(configuration: Option<&spice::config::RepositoryCapture>) -> Result<Self, Failure> {
        let working = std::env::current_dir().map_err(|error| {
            Failure::fault(
                FaultClass::Environment,
                "lock::working-directory-unavailable",
                error.to_string(),
            )
        })?;
        let repository = Repository::discover(&working).map_err(|error| {
            Failure::fault(
                FaultClass::Environment,
                spice::repository::ResolveError::REASON,
                error.to_string(),
            )
        })?;
        let loaded = spice::config::capture_repository_scoped(&repository, &[], configuration)
            .map_err(Failure::configuration)?;
        repository
            .admit_state()
            .map_err(|error| match crate::state_failure(&error) {
                crate::StateFailure::Refusal(refusal) => Failure::Refusal(refusal),
                crate::StateFailure::Fault(fault) => Failure::Fault(fault),
            })?;
        let topology =
            LockTopology::new(repository.worktree_root(), loaded.resolved().root().lock());
        Ok(Self { working, topology })
    }
}

struct Rejection {
    outcome: Outcome<()>,
    code: i32,
}

enum Failure {
    Refusal(Refusal),
    Fault(Fault),
}

impl Failure {
    fn refusal(
        reason: &'static str,
        expected: impl Into<String>,
        observed: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self::Refusal(Refusal::bare(
            wire::RefusalDiagnostic::new(
                reason.to_owned(),
                None,
                Some(expected.into()),
                Some(observed.into()),
            )
            .with_detail(detail),
        ))
    }

    fn fault(class: FaultClass, reason: &'static str, detail: impl Into<String>) -> Self {
        Self::Fault(Fault {
            class,
            reason: reason.to_owned(),
            detail: detail.into(),
        })
    }

    fn surface(error: LockSurfaceError) -> Self {
        match error {
            LockSurfaceError::InvalidName(name) => Self::refusal(
                "lock::name-refused",
                "one configured normal path component",
                name,
                "select a lock name from the topology reported by `spice lock`",
            ),
            LockSurfaceError::UnknownName(name) => Self::refusal(
                "lock::unknown",
                "one configured lock name",
                name,
                "select a lock name from the topology reported by `spice lock`",
            ),
            error @ LockSurfaceError::ShardOnFile { .. } => Self::refusal(
                "lock::shard-refused",
                "no shard selection for a configured single-file lock",
                error.to_string(),
                "remove --shard or select a configured lock pool",
            ),
            error @ LockSurfaceError::ShardOutOfRange { .. } => Self::refusal(
                "lock::shard-refused",
                "one in-range configured shard",
                error.to_string(),
                "select a shard from the topology reported by `spice lock`",
            ),
            error @ (LockSurfaceError::Prepare { .. }
            | LockSurfaceError::Acquire { .. }
            | LockSurfaceError::Inspect { .. }) => Self::fault(
                FaultClass::Store,
                "lock::state-unavailable",
                error.to_string(),
            ),
        }
    }

    fn configuration(error: LoadError) -> Self {
        if let Some(repository) = error.repository() {
            return Self::fault(
                FaultClass::Environment,
                spice::repository::ResolveError::REASON,
                repository.to_string(),
            );
        }
        let diagnostics = error.diagnostic();
        if error.unreadable() {
            return Self::fault(
                FaultClass::Environment,
                "lock::configuration-unreadable",
                diagnostics,
            );
        }
        Self::refusal(
            "lock::configuration-refused",
            "admitted tracked policy and machine-local lock configuration",
            diagnostics,
            "repair the named configuration values before running the lock command again",
        )
    }

    fn outcome<B>(self) -> Outcome<B> {
        match self {
            Self::Refusal(refusal) => Outcome::Refusal(refusal),
            Self::Fault(fault) => Outcome::Fault(fault),
        }
    }

    fn rejection(self, code: i32) -> Box<Rejection> {
        Box::new(Rejection {
            outcome: self.outcome(),
            code,
        })
    }

    fn standard_rejection(self) -> Box<Rejection> {
        let code = match &self {
            Self::Refusal(_) => 2,
            Self::Fault(_) => 1,
        };
        self.rejection(code)
    }
}
