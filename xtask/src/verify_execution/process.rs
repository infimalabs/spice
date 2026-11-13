use super::*;

pub(crate) fn run_process<I, S, P>(
    label: &str,
    program: P,
    arguments: I,
    cancellation: &CancellationToken,
    cargo_jobs: usize,
    overrides: &[(&str, &OsStr)],
    ambient: &Ambient,
) -> Result<CommandRun, RunError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    P: Into<OsString>,
{
    let cwd = std::env::current_dir().map_err(RunError::Launch)?;
    let boundary = ProcessBoundary {
        label,
        cwd: &cwd,
        cancellation,
        cargo_jobs: Some(cargo_jobs),
        overrides,
        ambient,
        bounds: ACTION_PROCESS_BOUNDS,
    };
    let spec = bounded_process_spec(&boundary, program, arguments)?;
    let running = sill::spawn(spec).map_err(RunError::Launch)?;
    let outcome = wait_bounded(running, cancellation, boundary.bounds)?;
    command_run_from_outcome(label, &outcome)
}

/// Where, under whose cancellation, and within which bounds one verifier
/// process runs; the program and its arguments are the only inputs that vary
/// per call.
#[derive(Clone, Copy)]
struct ProcessBoundary<'a> {
    label: &'a str,
    cwd: &'a Path,
    cancellation: &'a CancellationToken,
    cargo_jobs: Option<usize>,
    overrides: &'a [(&'a str, &'a OsStr)],
    ambient: &'a Ambient,
    bounds: ProcessBounds,
}

fn wait_bounded(
    mut running: sill::Running,
    cancellation: &CancellationToken,
    bounds: ProcessBounds,
) -> Result<sill::WaitOutcome, RunError> {
    loop {
        if cancellation.is_cancelled() || running.runtime_remaining().is_zero() {
            return wait_after_stop_request(running, bounds);
        }
        let interval = SCHEDULER_POLL.min(running.runtime_remaining());
        match running
            .wait_for(interval)
            .map_err(|error| RunError::Cleanup(error.to_string()))?
        {
            WaitStep::Exited(outcome) => return Ok(outcome),
            WaitStep::Running(next) => running = *next,
        }
    }
}

fn wait_after_stop_request(
    running: sill::Running,
    bounds: ProcessBounds,
) -> Result<sill::WaitOutcome, RunError> {
    #[cfg(not(unix))]
    let _ = bounds;
    #[cfg(unix)]
    let mut faults = Vec::new();
    #[cfg(unix)]
    let descendants = match sill::DescendantGroups::discover(running.id()) {
        Ok(mut descendants) => {
            if let Err(error) = descendants.terminate() {
                faults.push(format!(
                    "discoverable descendant termination failed: {error}"
                ));
            }
            Some(descendants)
        }
        Err(error) => {
            faults.push(format!("descendant discovery failed: {error}"));
            None
        }
    };

    #[cfg(unix)]
    let outcome = match descendants {
        Some(mut descendants) => {
            let root_terminal = AtomicBool::new(false);
            let (outcome, descendant_reap) = thread::scope(|scope| {
                let reaper = scope.spawn(|| {
                    descendants.reap_while_root_terminates(
                        bounds.grace,
                        bounds.reap,
                        bounds.output_drain,
                        &root_terminal,
                    )
                });
                let outcome = running.wait();
                root_terminal.store(true, Ordering::Release);
                (outcome, reaper.join())
            });
            match descendant_reap {
                Ok(Ok(())) => {}
                Ok(Err(error)) => faults.push(format!("descendant reap failed: {error}")),
                Err(_) => faults.push("descendant reap worker panicked".to_owned()),
            }
            outcome
        }
        None => running.wait(),
    };
    #[cfg(not(unix))]
    let outcome = running.wait();

    #[cfg(unix)]
    if !faults.is_empty() {
        if let Err(error) = outcome {
            faults.push(format!("action process-group reap failed: {error}"));
        }
        return Err(RunError::Cleanup(faults.join("; ")));
    }

    outcome.map_err(|error| RunError::Cleanup(error.to_string()))
}

fn bounded_process_spec<I, S, P>(
    boundary: &ProcessBoundary<'_>,
    program: P,
    arguments: I,
) -> Result<Spec, RunError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    P: Into<OsString>,
{
    let ProcessBoundary {
        label,
        cwd,
        cancellation,
        cargo_jobs,
        overrides,
        ambient,
        bounds,
    } = *boundary;
    let environment =
        selected_process_environment(ambient_environment(ambient)?, cargo_jobs, overrides)?;
    let deadlines =
        Deadlines::new(bounds.runtime, bounds.output_drain, bounds.reap).map_err(|error| {
            RunError::Launch(io::Error::new(
                io::ErrorKind::InvalidInput,
                error.to_string(),
            ))
        })?;
    let wait = WaitPolicy::new(
        format!("verifier action {label}"),
        Cancellation::Token(cancellation.clone()),
        RestartDisposition::Never,
    )
    .map_err(|error| {
        RunError::Launch(io::Error::new(
            io::ErrorKind::InvalidInput,
            error.to_string(),
        ))
    })?;
    Ok(Spec {
        argv: Argv::new(program, arguments).map_err(|error| {
            RunError::Launch(io::Error::new(
                io::ErrorKind::InvalidInput,
                error.to_string(),
            ))
        })?,
        cwd: cwd.to_owned(),
        environment,
        group: ProcessGroup::New,
        deadlines,
        wait,
        output: SillOutput::new(
            StdinPolicy::Null,
            StreamPolicy::Capture {
                limit: bounds.output_limit,
            },
            StreamPolicy::Capture {
                limit: bounds.output_limit,
            },
        ),
        redaction: Redaction::None,
        grace: bounds.grace,
    })
}

fn selected_process_environment(
    environment: Environment,
    cargo_jobs: Option<usize>,
    overrides: &[(&str, &OsStr)],
) -> Result<Environment, RunError> {
    let mut environment = environment
        .entries()
        .iter()
        .cloned()
        .collect::<BTreeMap<_, _>>();
    #[cfg(not(target_arch = "wasm32"))]
    for variable in spice::cargo_isolation::DEVELOPER_CHILD_EXCLUDED_VARIABLES {
        environment.remove(OsStr::new(variable));
    }
    if let Some(cargo_jobs) = cargo_jobs {
        environment.insert(
            OsString::from("CARGO_BUILD_JOBS"),
            OsString::from(cargo_jobs.to_string()),
        );
    }
    for (key, value) in overrides {
        environment.insert(OsString::from(key), (*value).to_owned());
    }
    let environment = Environment::new(environment).map_err(|error| {
        RunError::Launch(io::Error::new(
            io::ErrorKind::InvalidInput,
            error.to_string(),
        ))
    })?;
    Ok(environment)
}

fn command_run_from_outcome(
    label: &str,
    outcome: &sill::WaitOutcome,
) -> Result<CommandRun, RunError> {
    if let Some(failure) = outcome.observation_failure() {
        return Err(RunError::Cleanup(format!(
            "{label} live output observation failed: {failure}"
        )));
    }
    if !outcome.outputs_drained() {
        return Err(RunError::Cleanup(format!(
            "{label} output did not drain before cleanup"
        )));
    }
    let stdout = captured(label, "stdout", outcome.stdout())?;
    let stderr = captured(label, "stderr", outcome.stderr())?;
    let status = outcome.completion().status();
    let output = ProcessOutput {
        status: Status {
            success: status.success(),
            code: status.code(),
        },
        stdout,
        stderr,
    };
    Ok(match outcome.completion() {
        Completion::Exited(_) => CommandRun::Exited(output),
        Completion::Cancelled(_) => CommandRun::Cancelled(output),
        Completion::TimedOut(_) => CommandRun::TimedOut(output),
    })
}

fn ambient_environment(ambient: &Ambient) -> Result<Environment, RunError> {
    ambient.environment().map_err(|error| {
        RunError::Launch(io::Error::new(
            io::ErrorKind::InvalidInput,
            error.to_string(),
        ))
    })
}

fn captured(
    label: &str,
    stream: &str,
    output: Option<&sill::CapturedOutput>,
) -> Result<Vec<u8>, RunError> {
    let output = output.ok_or_else(|| {
        RunError::Cleanup(format!("{label} {stream} capture is unexpectedly absent"))
    })?;
    if output.truncated() || !output.complete() {
        return Err(RunError::Cleanup(format!(
            "{label} {stream} capture exceeded its bounded complete-output contract"
        )));
    }
    Ok(output.bytes().to_vec())
}

#[cfg(unix)]
pub(super) struct SignalGuard {
    handle: signal_hook::iterator::Handle,
    join: Option<thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl SignalGuard {
    pub(super) fn install(
        cancellation: CancellationToken,
        interrupted: Arc<AtomicBool>,
    ) -> Result<Self, VerifyError> {
        use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};

        let mut signals = signal_hook::iterator::Signals::new([SIGHUP, SIGINT, SIGTERM])
            .map_err(VerifyError::InterruptInstall)?;
        let handle = signals.handle();
        let join = thread::Builder::new()
            .name("verify-signal-bridge".to_owned())
            .spawn(move || {
                if signals.forever().next().is_some() {
                    interrupted.store(true, Ordering::Release);
                    cancellation.cancel();
                }
            })
            .map_err(VerifyError::InterruptInstall)?;
        Ok(Self {
            handle,
            join: Some(join),
        })
    }
}

#[cfg(unix)]
impl Drop for SignalGuard {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(not(unix))]
pub(super) struct SignalGuard;

#[cfg(not(unix))]
impl SignalGuard {
    pub(super) fn install(
        _cancellation: CancellationToken,
        _interrupted: Arc<AtomicBool>,
    ) -> Result<Self, VerifyError> {
        Ok(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXCLUDED: &str = spice::cargo_isolation::DEVELOPER_CHILD_EXCLUDED_VARIABLES[0];

    fn fixture_environment() -> Environment {
        Environment::new([(EXCLUDED, "fixture-supervisor"), ("PATH", "/fixture/bin")])
            .expect("fixture environment")
    }

    fn value<'a>(environment: &'a Environment, name: &str) -> Option<&'a OsStr> {
        environment
            .entries()
            .iter()
            .find_map(|(key, value)| (key == name).then_some(value.as_os_str()))
    }

    #[test]
    fn verifier_children_exclude_supervisor_state_unless_the_caller_opts_in() {
        let isolated = selected_process_environment(fixture_environment(), Some(3), &[])
            .expect("isolated environment");

        assert_eq!(value(&isolated, "PATH"), Some(OsStr::new("/fixture/bin")));
        assert_eq!(value(&isolated, "CARGO_BUILD_JOBS"), Some(OsStr::new("3")));
        for variable in spice::cargo_isolation::DEVELOPER_CHILD_EXCLUDED_VARIABLES {
            assert_eq!(value(&isolated, variable), None);
        }

        let opted_in = selected_process_environment(
            fixture_environment(),
            None,
            &[(EXCLUDED, OsStr::new("owned-child"))],
        )
        .expect("opted-in environment");
        assert_eq!(value(&opted_in, EXCLUDED), Some(OsStr::new("owned-child")));
    }
}
