use super::*;

/// Canonical `cargo test` arguments that build every workspace test target
/// with features unified across the whole workspace.
pub(super) fn workspace_test_arguments(inventory: &Inventory) -> Vec<String> {
    let mut arguments = ["test", "--locked", "--workspace", "--all-targets"]
        .map(str::to_owned)
        .to_vec();
    if let Some(features) =
        qualified_required_features(inventory.packages.iter().flat_map(|package| {
            package.targets.iter().flat_map(|target| {
                target
                    .required_features
                    .iter()
                    .map(|feature| (package.name.as_str(), feature.as_str()))
            })
        }))
    {
        arguments.extend(["--features".to_owned(), features]);
    }
    arguments
}

/// Build the selected test targets through the one workspace-unified prebuild
/// proof shards use, bounded by [`WORKSPACE_PREBUILD_DEADLINE`], and take the
/// selected executables from its compiler-artifact messages.
///
/// `inventory` is the whole workspace: unifying features over every package,
/// exactly as proof shards do, is what lets the two paths share compiled
/// units. `selected` names the targets the plan must publish.
pub(super) fn workspace_prebuilt_selection(
    root: &Path,
    environment: &Environment,
    cargo: &OsStr,
    inventory: &Inventory,
    selected: &Inventory,
    budget: WorkspaceTestBudget,
    output: &mut impl Write,
) -> Result<Vec<PrebuiltTest>, WorkspaceTestError> {
    let mut arguments = workspace_test_arguments(inventory);
    arguments.extend([
        "--no-run".to_owned(),
        "--message-format=json-render-diagnostics".to_owned(),
    ]);
    let environment = with_cargo_build_jobs(environment, budget.aggregate_cargo_jobs())?;
    let targets = selected
        .packages
        .iter()
        .map(|package| package.targets.len())
        .sum::<usize>();
    writeln!(
        output,
        "[rust-test] stage=selected-prebuild targets={targets} source=workspace",
    )
    .map_err(output_error)?;
    let started = Instant::now();
    let messages = capture_workspace_test_prebuild(
        root,
        &environment,
        cargo,
        &arguments,
        "selected workspace-test prebuild",
        output,
    )?;
    writeln!(
        output,
        "[rust-test] stage=selected-prebuild-terminal elapsed_ms={} disposition=success source=workspace",
        started.elapsed().as_millis(),
    )
    .map_err(output_error)?;
    let repair_started = Instant::now();
    repair_missing_binary_outputs(root, &environment, cargo, selected, output)?;
    writeln!(
        output,
        "[rust-test] stage=binary-output-repair-terminal elapsed_ms={} disposition=success",
        repair_started.elapsed().as_millis(),
    )
    .map_err(output_error)?;
    let plan_started = Instant::now();
    let plan = prebuilt_test_plan(selected, &messages)?;
    writeln!(
        output,
        "[rust-test] stage=artifact-plan-terminal elapsed_ms={} disposition=success artifacts={}",
        plan_started.elapsed().as_millis(),
        plan.len(),
    )
    .map_err(output_error)?;
    output.flush().map_err(output_error)?;
    Ok(plan)
}

pub(super) fn prebuilt_test_plan(
    inventory: &Inventory,
    messages: &[u8],
) -> Result<Vec<PrebuiltTest>, WorkspaceTestError> {
    let selected = inventory
        .packages
        .iter()
        .flat_map(|package| {
            package.targets.iter().map(|target| {
                (
                    (package.id.clone(), target.kind.clone(), target.name.clone()),
                    (package.name.clone(), package.root.clone(), target.clone()),
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    let mut artifacts = BTreeMap::new();
    let mut observed_test_artifacts = BTreeSet::new();
    for (line_index, line) in messages.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let message: CargoBuildMessage = serde_json::from_slice(line).map_err(|error| {
            WorkspaceTestError::new(format!(
                "selected prebuild message {} was invalid Cargo JSON: {error}",
                line_index + 1,
            ))
        })?;
        if message.reason != "compiler-artifact"
            || !message.profile.is_some_and(|profile| profile.test)
        {
            continue;
        }
        let (Some(package_id), Some(target), Some(executable)) =
            (message.package_id, message.target, message.executable)
        else {
            continue;
        };
        let key = (package_id, target.kind.join("+"), target.name);
        observed_test_artifacts.insert(key.clone());
        if !selected.contains_key(&key) {
            continue;
        }
        if !executable_file(&executable) {
            return Err(WorkspaceTestError::new(format!(
                "selected prebuild artifact {} is missing or not executable",
                executable.display(),
            )));
        }
        if let Some(previous) = artifacts.insert(key.clone(), executable.clone()) {
            return Err(WorkspaceTestError::new(format!(
                "selected prebuild published duplicate test artifact {key:?}: {} and {}",
                previous.display(),
                executable.display(),
            )));
        }
    }
    let missing = selected
        .keys()
        .filter(|key| !artifacts.contains_key(*key))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(WorkspaceTestError::new(format!(
            "selected prebuild omitted test artifact(s): {missing:?}; observed reusable test artifacts: {observed_test_artifacts:?}",
        )));
    }
    let mut plan = selected
        .into_iter()
        .map(|(key, (package, package_root, target))| PrebuiltTest {
            package,
            package_root,
            target,
            executable: artifacts
                .remove(&key)
                .expect("complete artifact comparison retained every selected key"),
            partition: None,
        })
        .collect::<Vec<_>>();
    plan.sort_by(|left, right| {
        right
            .target
            .source_bytes
            .cmp(&left.target.source_bytes)
            .then_with(|| left.package.cmp(&right.package))
            .then_with(|| left.target.cmp(&right.target))
    });
    Ok(plan)
}

/// Split only the largest prebuilt artifacts into deterministic libtest complements.
///
/// The direct coordinator orders ordinary artifacts by selected source bytes. Inspecting
/// at most five ordinary candidates per worker lane keeps discovery bounded while
/// reaching the process-heavy integration harnesses that otherwise become the terminal
/// execution tail. One additional binary candidate admits explicitly isolated
/// process-timing tests without displacing an ordinary large harness. Every partition
/// executes one disjoint complement under libtest's exact skip semantics. Exclusive
/// complements lead the queue so their barriers cannot strand a process lane behind an
/// already-running ordinary wave. Ordinary complements retain their parent artifact's
/// scheduling priority so a split long-running harness cannot fall behind smaller
/// unsplit targets.
pub(super) fn partition_largest_prebuilt_tests(
    tests: Vec<PrebuiltTest>,
    environment: &Environment,
    partitions_per_target: usize,
    discovery_concurrency: usize,
    output: &mut impl Write,
) -> Result<Vec<PrebuiltTest>, WorkspaceTestError> {
    partition_largest_prebuilt_tests_with_deadline(
        tests,
        environment,
        partitions_per_target,
        discovery_concurrency,
        METADATA_DEADLINE,
        output,
    )
}

fn partition_largest_prebuilt_tests_with_deadline(
    mut tests: Vec<PrebuiltTest>,
    environment: &Environment,
    partitions_per_target: usize,
    discovery_concurrency: usize,
    discovery_deadline: Duration,
    output: &mut impl Write,
) -> Result<Vec<PrebuiltTest>, WorkspaceTestError> {
    const CANDIDATES_PER_LANE: usize = 5;
    const CASES_PER_PARTITION: usize = 5;

    let started = Instant::now();
    let original_count = tests.len();
    if partitions_per_target <= 1 {
        writeln!(
            output,
            "[rust-test] stage=partition artifacts={original_count} candidates=0 shards={original_count}",
        )
        .map_err(output_error)?;
        writeln!(
            output,
            "[rust-test] stage=partition-terminal elapsed_ms={} disposition=success",
            started.elapsed().as_millis(),
        )
        .map_err(output_error)?;
        output.flush().map_err(output_error)?;
        return Ok(tests);
    }
    let candidate_limit = discovery_concurrency
        .saturating_mul(CANDIDATES_PER_LANE)
        .saturating_add(usize::from(
            tests.iter().any(|test| test.target.kind == "bin"),
        ));
    let candidate_count = candidate_limit.min(tests.len());
    let remaining = tests.split_off(candidate_count);
    let discovery_started = Instant::now();
    let discoveries =
        run_bounded_discovery(&tests, discovery_concurrency, |test, cancellation| {
            prebuilt_test_cases_with_deadline(test, environment, cancellation, discovery_deadline)
        })?;
    let discovery_elapsed = discovery_started.elapsed();
    report_prebuilt_discoveries(
        &tests,
        &discoveries.completed,
        discovery_elapsed,
        discovery_concurrency,
        output,
    )?;
    if let Some(failure_index) = discoveries.causal_failure {
        writeln!(
            output,
            "[rust-test] stage=partition-terminal elapsed_ms={} disposition=failed",
            started.elapsed().as_millis(),
        )
        .map_err(output_error)?;
        output.flush().map_err(output_error)?;
        return Err(discoveries
            .completed
            .into_iter()
            .find(|discovery| discovery.ordinal == failure_index)
            .and_then(|discovery| discovery.cases.err())
            .expect("recorded discovery failure retains its error"));
    }
    if discoveries
        .completed
        .iter()
        .any(|discovery| discovery.cases.is_err())
    {
        return Err(WorkspaceTestError::new(
            "prebuilt test discovery recorded sibling cancellation without a causal failure",
        ));
    }
    if discoveries.completed.len() != candidate_count {
        return Err(WorkspaceTestError::new(format!(
            "prebuilt test discovery completed {} of {candidate_count} candidate(s) without reporting a failure",
            discoveries.completed.len(),
        )));
    }

    let mut partitioned = Vec::new();
    for (test, discovery) in tests.into_iter().zip(discoveries.completed) {
        let cases = discovery
            .cases
            .expect("complete discovery was checked before partitioning");
        let partitions = cases
            .len()
            .div_ceil(CASES_PER_PARTITION)
            .clamp(1, partitions_per_target);
        partitioned.extend(test.into_partitions(&cases, partitions));
    }
    partitioned.extend(remaining);
    partitioned.sort_by(|left, right| {
        right
            .requires_exclusive_lane()
            .cmp(&left.requires_exclusive_lane())
            .then_with(|| right.target.source_bytes.cmp(&left.target.source_bytes))
            .then_with(|| left.package.cmp(&right.package))
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| left.partition_index().cmp(&right.partition_index()))
    });
    writeln!(
        output,
        "[rust-test] stage=partition artifacts={original_count} candidates={candidate_count} shards={}",
        partitioned.len(),
    )
    .map_err(output_error)?;
    writeln!(
        output,
        "[rust-test] stage=partition-terminal elapsed_ms={} disposition=success",
        started.elapsed().as_millis(),
    )
    .map_err(output_error)?;
    output.flush().map_err(output_error)?;
    Ok(partitioned)
}

#[derive(Debug)]
struct PrebuiltTestDiscovery {
    ordinal: usize,
    elapsed: Duration,
    cases: Result<Vec<String>, WorkspaceTestError>,
}

fn report_prebuilt_discoveries(
    tests: &[PrebuiltTest],
    discoveries: &[PrebuiltTestDiscovery],
    elapsed: Duration,
    concurrency: usize,
    output: &mut impl Write,
) -> Result<(), WorkspaceTestError> {
    let mut failed = false;
    for discovery in discoveries {
        let test = &tests[discovery.ordinal];
        let (disposition, cases) = match &discovery.cases {
            Ok(cases) => ("success", cases.len().to_string()),
            Err(_) => {
                failed = true;
                ("failed", "unknown".to_owned())
            }
        };
        writeln!(
            output,
            "[rust-test] stage=discovery-terminal package={:?} target={:?} elapsed_ms={} disposition={disposition} cases={cases}",
            test.package,
            test.target.name,
            discovery.elapsed.as_millis(),
        )
        .map_err(output_error)?;
    }
    let disposition = if failed { "failed" } else { "success" };
    writeln!(
        output,
        "[rust-test] stage=discovery-batch-terminal elapsed_ms={} disposition={disposition} candidates={} completed={} lanes={}",
        elapsed.as_millis(),
        tests.len(),
        discoveries.len(),
        concurrency.min(tests.len()),
    )
    .map_err(output_error)?;
    output.flush().map_err(output_error)?;
    Ok(())
}

#[derive(Debug)]
struct PrebuiltDiscoveryBatch {
    /// Progress records retained in candidate ordinal order.
    completed: Vec<PrebuiltTestDiscovery>,
    /// First independently observed failure, excluding cancellation caused by a sibling.
    causal_failure: Option<usize>,
}

struct PrebuiltDiscoveryAdmission {
    next: usize,
    causal_failure: Option<usize>,
}

fn run_bounded_discovery<T, F>(
    candidates: &[T],
    concurrency: usize,
    discover: F,
) -> Result<PrebuiltDiscoveryBatch, WorkspaceTestError>
where
    T: Sync,
    F: Fn(&T, CancellationToken) -> Result<Vec<String>, WorkspaceTestError> + Sync,
{
    if !(1..=crate::PREBUILT_WORKSPACE_TEST_CONCURRENCY_CAP).contains(&concurrency) {
        return Err(WorkspaceTestError::new(format!(
            "prebuilt test discovery concurrency must be between 1 and {}, got {concurrency}",
            crate::PREBUILT_WORKSPACE_TEST_CONCURRENCY_CAP,
        )));
    }
    if candidates.is_empty() {
        return Ok(PrebuiltDiscoveryBatch {
            completed: Vec::new(),
            causal_failure: None,
        });
    }

    let lanes = concurrency.min(candidates.len());
    let admission = Mutex::new(PrebuiltDiscoveryAdmission {
        next: 0,
        causal_failure: None,
    });
    let cancellation = CancellationToken::new();
    let mut completed = thread::scope(|scope| {
        let mut workers = Vec::with_capacity(lanes);
        for _ in 0..lanes {
            let discover = &discover;
            let admission = &admission;
            let cancellation = &cancellation;
            workers.push(scope.spawn(move || {
                let mut completed = Vec::new();
                loop {
                    let ordinal = {
                        let mut admission = admission.lock().map_err(|_| {
                            WorkspaceTestError::new(
                                "prebuilt test discovery admission lock was poisoned",
                            )
                        })?;
                        if cancellation.is_cancelled() {
                            break;
                        }
                        let ordinal = admission.next;
                        admission.next = admission.next.saturating_add(1);
                        ordinal
                    };
                    let Some(candidate) = candidates.get(ordinal) else {
                        break;
                    };
                    let started = Instant::now();
                    let cases = discover(candidate, cancellation.clone());
                    if cases.is_err() {
                        let mut admission = admission.lock().map_err(|_| {
                            WorkspaceTestError::new(
                                "prebuilt test discovery admission lock was poisoned",
                            )
                        })?;
                        if cases
                            .as_ref()
                            .is_err_and(|error| !error.is_synthetic_cancellation())
                        {
                            admission.causal_failure.get_or_insert(ordinal);
                        }
                        cancellation.cancel();
                    }
                    completed.push(PrebuiltTestDiscovery {
                        ordinal,
                        elapsed: started.elapsed(),
                        cases,
                    });
                }
                Ok(completed)
            }));
        }
        workers
            .into_iter()
            .map(|worker| {
                worker.join().map_err(|_| {
                    WorkspaceTestError::new("prebuilt test discovery worker thread panicked")
                })?
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|completed| completed.into_iter().flatten().collect::<Vec<_>>())
    })?;
    completed.sort_by_key(|result| result.ordinal);
    let causal_failure = admission
        .into_inner()
        .map_err(|_| {
            WorkspaceTestError::new("prebuilt test discovery admission lock was poisoned")
        })?
        .causal_failure;
    Ok(PrebuiltDiscoveryBatch {
        completed,
        causal_failure,
    })
}

fn prebuilt_test_cases_with_deadline(
    test: &PrebuiltTest,
    environment: &Environment,
    cancellation: CancellationToken,
    deadline: Duration,
) -> Result<Vec<String>, WorkspaceTestError> {
    let owner = format!(
        "prebuilt test discovery for package {:?} target {:?}",
        test.package, test.target.name,
    );
    let spec = process_spec(
        test.executable.as_os_str(),
        ["--list", "--format", "terse"],
        &test.package_root,
        environment.clone(),
        deadline,
        Cancellation::Token(cancellation),
        &owner,
    )?;
    let running = crate::measure::spawn(spec)
        .map_err(|error| WorkspaceTestError::new(format!("{owner} launch failed: {error}")))?;
    let process_id = running.id();
    let outcome = running
        .wait()
        .map_err(|error| WorkspaceTestError::new(format!("{owner} wait failed: {error}")))?;
    let capture_failed = !outcome.outputs_drained()
        || outcome.stdout().is_some_and(|output| output.truncated())
        || outcome.stderr().is_some_and(|output| output.truncated());
    if matches!(outcome.completion(), Completion::Cancelled(_)) {
        return Err(WorkspaceTestError::synthetic_cancellation(format!(
            "{owner} process-id={process_id} ended as {}: stdout-tail={:?}; stderr-tail={:?}",
            completion_name(outcome.completion()),
            captured_excerpt(outcome.stdout()),
            captured_excerpt(outcome.stderr()),
        )));
    }
    if !matches!(outcome.completion(), Completion::Exited(status) if status.success())
        || capture_failed
    {
        return Err(WorkspaceTestError::new(format!(
            "{owner} process-id={process_id} ended as {}: stdout-tail={:?}; stderr-tail={:?}",
            completion_name(outcome.completion()),
            captured_excerpt(outcome.stdout()),
            captured_excerpt(outcome.stderr()),
        )));
    }
    let stdout = outcome
        .stdout()
        .ok_or_else(|| WorkspaceTestError::new(format!("{owner} captured no stdout")))?;
    let listing = std::str::from_utf8(stdout.bytes())
        .map_err(|error| WorkspaceTestError::new(format!("{owner} emitted non-UTF-8: {error}")))?;
    let mut cases = listing
        .lines()
        .map(|line| {
            line.strip_suffix(": test")
                .or_else(|| line.strip_suffix(": benchmark"))
                .map(str::to_owned)
                .ok_or_else(|| {
                    WorkspaceTestError::new(format!(
                        "{owner} emitted an unrecognized list row {line:?}",
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    cases.sort();
    if cases.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(WorkspaceTestError::new(format!(
            "{owner} emitted duplicate test identities",
        )));
    }
    Ok(cases)
}

fn repair_missing_binary_outputs(
    root: &Path,
    environment: &Environment,
    cargo: &OsStr,
    inventory: &Inventory,
    output: &mut impl Write,
) -> Result<(), WorkspaceTestError> {
    for missing in missing_binary_outputs(inventory, environment)? {
        let command = format!(
            "cargo build --locked --package {:?} --bin {:?}",
            missing.binary.package_id, missing.binary.target_name,
        );
        let owner = format!(
            "CARGO_BIN_EXE repair for package {:?} binary target {:?} at {}",
            missing.binary.package_name,
            missing.binary.target_name,
            missing.path.display(),
        );
        run_prebuild_command(
            root,
            environment,
            cargo,
            output,
            &command,
            [
                OsStr::new("build"),
                OsStr::new("--locked"),
                OsStr::new("--package"),
                OsStr::new(&missing.binary.package_id),
                OsStr::new("--bin"),
                OsStr::new(&missing.binary.target_name),
            ],
            &owner,
        )?;
    }
    validate_prebuilt_binary_outputs(inventory, environment)
}

pub(super) fn validate_prebuilt_binary_outputs(
    inventory: &Inventory,
    environment: &Environment,
) -> Result<(), WorkspaceTestError> {
    let missing = missing_binary_outputs(inventory, environment)?;
    if missing.is_empty() {
        return Ok(());
    }
    let detail = missing
        .iter()
        .map(|missing| {
            format!(
                "{} (package {:?}, binary target {:?})",
                missing.path.display(),
                missing.binary.package_name,
                missing.binary.target_name,
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(WorkspaceTestError::new(format!(
        "prebuilt CARGO_BIN_EXE output is missing or not executable: {detail}"
    )))
}

fn missing_binary_outputs<'a>(
    inventory: &'a Inventory,
    environment: &Environment,
) -> Result<Vec<MissingBinaryOutput<'a>>, WorkspaceTestError> {
    let target_directory = environment
        .entries()
        .iter()
        .find_map(|(name, value)| {
            (name == OsStr::new(CARGO_TARGET_DIR)).then(|| PathBuf::from(value))
        })
        .ok_or_else(|| {
            WorkspaceTestError::new("prebuilt CARGO_BIN_EXE validation requires CARGO_TARGET_DIR")
        })?;
    Ok(inventory
        .binary_outputs
        .iter()
        .filter_map(|binary| {
            let path = target_directory.join("debug").join(format!(
                "{}{}",
                binary.target_name,
                std::env::consts::EXE_SUFFIX,
            ));
            (!executable_file(&path)).then_some(MissingBinaryOutput { binary, path })
        })
        .collect())
}

pub(super) fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(target_family = "unix")]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(target_family = "unix"))]
    {
        true
    }
}

pub(super) fn run_prebuild_command<I, S>(
    root: &Path,
    environment: &Environment,
    cargo: &OsStr,
    output: &mut impl Write,
    command: &str,
    arguments: I,
    owner: &str,
) -> Result<(), WorkspaceTestError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    run_prebuild_command_capture(root, environment, cargo, output, command, arguments, owner)
        .map(|_| ())
}

fn run_prebuild_command_capture<I, S>(
    root: &Path,
    environment: &Environment,
    cargo: &OsStr,
    output: &mut impl Write,
    command: &str,
    arguments: I,
    owner: &str,
) -> Result<Vec<u8>, WorkspaceTestError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let spec = prebuild_process_spec(
        cargo,
        arguments,
        root,
        environment.clone(),
        SHARD_DEADLINE,
        Cancellation::Never,
        owner,
    )?;
    capture_prebuild(spec, output, command, owner)
}

/// Build every workspace test target once under [`WORKSPACE_PREBUILD_DEADLINE`]
/// and return Cargo's JSON messages.
pub(super) fn capture_workspace_test_prebuild(
    root: &Path,
    environment: &Environment,
    cargo: &OsStr,
    arguments: &[String],
    owner: &str,
    output: &mut impl Write,
) -> Result<Vec<u8>, WorkspaceTestError> {
    let spec = prebuild_process_spec(
        cargo,
        arguments,
        root,
        environment.clone(),
        WORKSPACE_PREBUILD_DEADLINE,
        Cancellation::Never,
        owner,
    )?;
    capture_prebuild(
        spec,
        output,
        &format!("cargo {}", arguments.join(" ")),
        owner,
    )
}

fn capture_prebuild(
    spec: Spec,
    output: &mut impl Write,
    command: &str,
    owner: &str,
) -> Result<Vec<u8>, WorkspaceTestError> {
    writeln!(output, "[rust-test] stage=prebuild command={command:?}").map_err(output_error)?;
    output.flush().map_err(output_error)?;
    let started = Instant::now();
    let outcome = crate::measure::spawn(spec)
        .map_err(|error| WorkspaceTestError::new(format!("prebuild launch failed: {error}")))?
        .wait()
        .map_err(|error| WorkspaceTestError::new(format!("prebuild wait failed: {error}")))?;
    let capture_failed = !outcome.outputs_drained()
        || outcome
            .stdout()
            .is_some_and(|captured| captured.truncated())
        || outcome
            .stderr()
            .is_some_and(|captured| captured.truncated());
    let disposition = match outcome.completion() {
        Completion::Exited(status) if status.success() && !capture_failed => "success".to_owned(),
        Completion::Exited(status) if capture_failed => {
            format!(
                "capture-failed after {}",
                completion_name(outcome.completion())
            )
        }
        completion => completion_name(completion),
    };
    writeln!(
        output,
        "[rust-test] stage=prebuild-terminal elapsed_ms={} disposition={}",
        started.elapsed().as_millis(),
        disposition,
    )
    .map_err(output_error)?;
    output.flush().map_err(output_error)?;
    match outcome.completion() {
        Completion::Exited(status) if status.success() && !capture_failed => outcome
            .stdout()
            .map(|stdout| stdout.bytes().to_vec())
            .ok_or_else(|| WorkspaceTestError::new(format!("{owner} captured no stdout"))),
        completion => Err(WorkspaceTestError::new(format!(
            "{owner} ended as {}: stdout-tail={:?}; stderr-tail={:?}",
            completion_name(completion),
            captured_excerpt(outcome.stdout()),
            captured_excerpt(outcome.stderr()),
        ))),
    }
}

fn prebuild_process_spec<I, S>(
    cargo: &OsStr,
    arguments: I,
    root: &Path,
    environment: Environment,
    runtime: Duration,
    cancellation: Cancellation,
    owner: &str,
) -> Result<Spec, WorkspaceTestError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut spec = process_spec(
        cargo,
        arguments,
        root,
        environment,
        crate::measure::bounded_runtime_deadline(runtime),
        cancellation,
        owner,
    )?;
    spec.group = crate::measure::bounded_process_group();
    Ok(spec)
}

#[derive(Clone, Debug)]
pub(super) struct PrebuiltTest {
    pub(super) package: String,
    pub(super) package_root: PathBuf,
    pub(super) target: TargetIdentity,
    pub(super) executable: PathBuf,
    pub(super) partition: Option<PrebuiltTestPartition>,
}

impl PrebuiltTest {
    pub(super) fn requires_exclusive_lane(&self) -> bool {
        self.partition
            .as_ref()
            .is_some_and(|partition| partition.exclusive)
    }

    pub(super) fn into_partitions(self, cases: &[String], count: usize) -> Vec<Self> {
        const EXCLUSIVE_LANE_PREFIX: &str = "exclusive_lane_";

        let (exclusive, ordinary): (Vec<_>, Vec<_>) = cases.iter().partition(|case| {
            case.rsplit("::")
                .next()
                .is_some_and(|name| name.starts_with(EXCLUSIVE_LANE_PREFIX))
        });
        if count <= 1 && exclusive.is_empty() || cases.is_empty() {
            return vec![self];
        }

        let ordinary_partitions = if ordinary.is_empty() {
            0
        } else {
            count
                .saturating_sub(usize::from(!exclusive.is_empty()))
                .max(1)
                .min(ordinary.len())
        };
        let mut groups = Vec::new();
        if !exclusive.is_empty() {
            groups.push((true, exclusive));
        }
        for index in 0..ordinary_partitions {
            groups.push((
                false,
                ordinary
                    .iter()
                    .enumerate()
                    .filter(|(case_index, _)| case_index % ordinary_partitions == index)
                    .map(|(_, case)| *case)
                    .collect(),
            ));
        }
        let partition_count = groups.len();
        groups
            .into_iter()
            .enumerate()
            .map(|(index, (exclusive, included))| {
                let skipped = cases
                    .iter()
                    .filter(|case| !included.contains(case))
                    .cloned()
                    .collect();
                Self {
                    partition: Some(PrebuiltTestPartition {
                        index,
                        count: partition_count,
                        skipped,
                        exclusive,
                    }),
                    ..self.clone()
                }
            })
            .collect()
    }

    pub(super) fn partition_index(&self) -> usize {
        self.partition
            .as_ref()
            .map_or(0, |partition| partition.index)
    }

    pub(super) fn partition_count(&self) -> usize {
        self.partition
            .as_ref()
            .map_or(1, |partition| partition.count)
    }

    pub(super) fn direct_arguments(&self, threads: usize) -> Vec<OsString> {
        let mut arguments = Vec::new();
        if let Some(partition) = &self.partition {
            arguments.push(OsString::from("--exact"));
            for skipped in &partition.skipped {
                arguments.extend([OsString::from("--skip"), OsString::from(skipped)]);
            }
        }
        let threads = if self.requires_exclusive_lane() {
            1
        } else {
            threads
        };
        arguments.push(OsString::from(format!("--test-threads={threads}")));
        arguments
    }
}

#[derive(Clone, Debug)]
pub(super) struct PrebuiltTestPartition {
    pub(super) index: usize,
    pub(super) count: usize,
    pub(super) skipped: Vec<String>,
    pub(super) exclusive: bool,
}

/// Test executables a workspace prebuild published for one package's
/// `kind:name` targets, in `targets` order.
pub(super) fn package_test_executables(
    messages: &[u8],
    manifest: &Path,
    targets: &[String],
) -> Result<Vec<PathBuf>, WorkspaceTestError> {
    let manifest = fs::canonicalize(manifest).map_err(|error| {
        WorkspaceTestError::new(format!(
            "package manifest {} is unavailable: {error}",
            manifest.display()
        ))
    })?;
    let mut found = BTreeMap::new();
    for (line_index, line) in messages.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let message: CargoBuildMessage = serde_json::from_slice(line).map_err(|error| {
            WorkspaceTestError::new(format!(
                "workspace prebuild message {} was invalid Cargo JSON: {error}",
                line_index + 1,
            ))
        })?;
        if message.reason != "compiler-artifact"
            || !message.profile.is_some_and(|profile| profile.test)
        {
            continue;
        }
        let (Some(target), Some(executable), Some(path)) =
            (message.target, message.executable, message.manifest_path)
        else {
            continue;
        };
        let key = format!("{}:{}", target.kind.join("+"), target.name);
        if !targets.contains(&key) || fs::canonicalize(&path).ok().as_ref() != Some(&manifest) {
            continue;
        }
        if !executable_file(&executable) {
            return Err(WorkspaceTestError::new(format!(
                "workspace prebuild artifact {} is missing or not executable",
                executable.display(),
            )));
        }
        if let Some(previous) = found.insert(key, executable.clone()) {
            return Err(WorkspaceTestError::new(format!(
                "workspace prebuild published duplicate test artifacts {} and {}",
                previous.display(),
                executable.display(),
            )));
        }
    }
    targets
        .iter()
        .map(|target| {
            found.remove(target).ok_or_else(|| {
                WorkspaceTestError::new(format!(
                    "workspace prebuild omitted test artifact {target} of {}",
                    manifest.display(),
                ))
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct CargoBuildMessage {
    reason: String,
    #[serde(default)]
    package_id: Option<String>,
    #[serde(default)]
    target: Option<CargoBuildTarget>,
    #[serde(default)]
    profile: Option<CargoBuildProfile>,
    #[serde(default)]
    executable: Option<PathBuf>,
    #[serde(default)]
    manifest_path: Option<PathBuf>,
}

#[derive(Deserialize)]
struct CargoBuildTarget {
    name: String,
    kind: Vec<String>,
}

#[derive(Deserialize)]
struct CargoBuildProfile {
    test: bool,
}
