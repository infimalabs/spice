//! Canonical Cargo proof shard preparation and execution.

use super::*;

pub(super) const RUNNER: &str = "spice-cargo-proof-shard";
pub(super) const INPUT_STRATEGY: &str = "cargo-package-closure-v1";
pub(super) const TEST_SHARD_DECLARATION: &str = "gate::cargo-workspace-tests::proof-shard-v1";

pub(super) struct Execution {
    shard: CargoGateShard,
    root: PathBuf,
    writable_root: PathBuf,
    environment: Environment,
    cargo: PathBuf,
    source: SourceExecution,
}

#[derive(Clone)]
pub(super) struct SourceExecution {
    pub(super) home: std::sync::Arc<spice::cargo_isolation::PreparedHome>,
    lockfile: Vec<u8>,
    root: PathBuf,
    host: spice::cargo_isolation::Host,
    authority: spice::cargo_isolation::SourceGenerationAuthority,
}

#[derive(Debug)]
pub(super) struct Authority {
    pub(super) cargo: PathBuf,
    pub(super) execution_environment: Vec<(OsString, OsString)>,
    pub(super) action_environment: Vec<(OsString, OsString)>,
}

pub(super) fn prepare(
    staged: &StagedActionSelection,
    manifest: &BatteryManifest,
    gate_set: &Digest,
    request: Request<'_>,
    writable_root: &Path,
) -> Result<Vec<PreparedAction>, ProveFailure> {
    let typecheck_selected = staged
        .selection()
        .gate_ids()
        .contains(&GateId::RustTypecheck);
    if !typecheck_selected && !staged.cargo_tests() {
        return Ok(Vec::new());
    }
    if !typecheck_selected {
        return Err(ProveFailure::refusal(
            "gate::prove-cargo-authority-unavailable",
            "Cargo tests were selected without the canonical Rust toolchain arm",
        ));
    }
    let include_windows_typechecks = gate::windows_typechecks_declared_at(staged.anchor().root());

    let arm = staged
        .anchor()
        .derive_reached_arm(staged.selection(), GateId::RustTypecheck)
        .map_err(ProveFailure::run)?;
    let context = arm.context().map_err(ProveFailure::run)?;
    let mut authority = authority(arm.root(), arm.compiler_environment())?;
    let (source_execution, external_source) =
        source_authority(writable_root, arm.root(), &authority.execution_environment)?;
    for (name, value) in &mut authority.execution_environment {
        if name == OsStr::new(spice::cargo_isolation::CARGO_HOME_VARIABLE) {
            *value = source_execution.home.home().as_os_str().to_owned();
        }
    }
    let environment = Environment::new(authority.execution_environment.iter().cloned())
        .map_err(|error| ProveFailure::refusal("gate::prove-cargo-environment-refused", error))?;
    let base = arm.materialize_staged_base().map_err(ProveFailure::run)?;
    let plan = gate::plan_cargo_gate_shards_at(CargoGateShardPlanning {
        root: arm.root(),
        base: base.as_ref().map(gate::SelectedTree::root),
        source: staged.selection().source().clone(),
        changed_sources: staged.selection().changed_sources(),
        selected_paths: arm.paths(),
        include_windows_typechecks,
        environment: environment.clone(),
        cargo: authority.cargo.as_os_str(),
        external_source: Some(external_source),
        include_tests: staged.cargo_tests(),
    })
    .map_err(ProveFailure::cargo)?;

    let shards = selected_shards(
        request,
        typecheck_selected.then_some(plan.typechecks()),
        staged.cargo_tests().then_some(plan.tests()),
    );
    let preparation = ShardPreparation {
        arm: &arm,
        context: &context,
        gate_set,
        typecheck_declaration: declaration(manifest, GateId::RustTypecheck)?,
        writable_root,
        environment: &environment,
        cargo: &authority.cargo,
        source: &source_execution,
    };
    shards
        .into_iter()
        .map(|shard| prepare_action(&preparation, shard.clone()))
        .collect()
}

fn selected_shards<'a>(
    request: Request<'_>,
    typechecks: Option<&'a [CargoGateShard]>,
    tests: Option<&'a [CargoGateShard]>,
) -> Vec<&'a CargoGateShard> {
    let typechecks = typechecks.unwrap_or_default();
    let tests = tests.unwrap_or_default();
    match request.action {
        None => typechecks.iter().chain(tests).collect(),
        Some("rust-typecheck") => typechecks.iter().collect(),
        Some("cargo-tests") => tests.iter().collect(),
        Some(action) if action.starts_with("cargo:typecheck:") => typechecks
            .iter()
            .filter(|shard| shard.action_id() == action)
            .collect(),
        Some(action) if action.starts_with("cargo:test:") => tests
            .iter()
            .filter(|shard| shard.action_id() == action)
            .collect(),
        Some(_) => Vec::new(),
    }
}

/// What every selected Cargo shard of one proving run shares.
struct ShardPreparation<'a, 'arm> {
    arm: &'a spice::gate::PreparedArm<'arm>,
    context: &'a gate::BatteryContext,
    gate_set: &'a Digest,
    typecheck_declaration: &'a str,
    writable_root: &'a Path,
    environment: &'a Environment,
    cargo: &'a Path,
    source: &'a SourceExecution,
}

fn prepare_action(
    preparation: &ShardPreparation<'_, '_>,
    shard: CargoGateShard,
) -> Result<PreparedAction, ProveFailure> {
    let ShardPreparation {
        arm,
        context,
        gate_set,
        typecheck_declaration,
        writable_root,
        environment: execution_environment,
        cargo,
        source,
    } = *preparation;
    require_reusable_shard(&shard)?;
    let execution_environment = spice::cargo_isolation::bind_windows_msvc_environment(
        arm.root(),
        source.host.bootstrap_home(),
        shard.argv(),
        execution_environment.clone(),
    )
    .map_err(ProveFailure::cargo_isolation)?;
    let cargo_environment = normalized_action_environment(execution_environment.entries());
    let tools = action_tools(context.tools())?;
    let (environment_names, environment) = environment(&cargo_environment)?;
    let tool_authority = digest_encoded(Domain::GateTool, &context.tools())?;
    let inventory = digest_encoded(Domain::GateInventory, &shard)?;
    let declaration = match shard.kind() {
        CargoGateShardKind::Typecheck => typecheck_declaration,
        CargoGateShardKind::Test => TEST_SHARD_DECLARATION,
    };
    let non_files = vec![
        non_file(
            ActionNonFileKind::Declaration,
            "compiled-declaration",
            Digest::compute(Domain::GateBatteryManifest, declaration.as_bytes()),
        )?,
        non_file(
            ActionNonFileKind::GateSet,
            "canonical-gate-set",
            gate_set.clone(),
        )?,
        non_file(
            ActionNonFileKind::Toolchain,
            "discovered-tools",
            tool_authority.clone(),
        )?,
        non_file(
            ActionNonFileKind::Environment,
            "compiler-environment",
            environment.clone(),
        )?,
        non_file(ActionNonFileKind::Inventory, "cargo-shard", inventory)?,
    ];
    let input_manifest = arm
        .compile_explicit_action_input(shard.input_paths().to_vec(), non_files)
        .map_err(ProveFailure::run)?;
    let input_manifest = input_manifest
        .digest()
        .map_err(ProveFailure::action_input)?;
    let mut argv = Vec::with_capacity(shard.argv().len() + 1);
    argv.push("cargo".to_owned());
    argv.extend_from_slice(shard.argv());
    let spec = ActionSpec::new(ActionSpecFields {
        action_id: shard.action_id().to_owned(),
        runner: RUNNER.to_owned(),
        runner_version: RUNNER_VERSION,
        declaration: declaration.to_owned(),
        argv,
        cwd: ".".to_owned(),
        environment_names,
        tools,
        input_strategy: INPUT_STRATEGY.to_owned(),
        reuse: ActionReuseClass::Cacheable,
    })
    .map_err(ProveFailure::action_input)?;
    let action_spec = spec.digest().map_err(ProveFailure::action_input)?;
    let action_key = gate::derive_authority_bound_action_key(
        &spec,
        &ActionKeyAuthority {
            input_manifest: input_manifest.clone(),
            policy_authority: ActionPolicyAuthority::ResolvedExecution,
            gate_set: gate_set.clone(),
            tools: tool_authority,
            environment,
            baseline: None,
        },
    )
    .map_err(ProveFailure::action_input)?;
    Ok(PreparedAction {
        action: shard.action_id().to_owned(),
        action_key,
        action_spec,
        input_manifest,
        execution: PreparedExecution::Cargo(Box::new(Execution {
            shard,
            root: arm.root().to_path_buf(),
            writable_root: writable_root.to_path_buf(),
            environment: execution_environment,
            cargo: cargo.to_path_buf(),
            source: source.clone(),
        })),
    })
}

pub(super) fn authority(
    root: &Path,
    compiler_environment: &[(OsString, OsString)],
) -> Result<Authority, ProveFailure> {
    let host =
        spice::cargo_isolation::Host::from_environment(root, compiler_environment.iter().cloned())
            .map_err(ProveFailure::cargo_isolation)?;
    let environment = host
        .compiler_environment()
        .map_err(ProveFailure::cargo_isolation)?;
    let execution_environment = environment.entries().to_vec();
    Ok(Authority {
        cargo: host.cargo().to_path_buf(),
        action_environment: normalized_action_environment(&execution_environment),
        execution_environment,
    })
}

pub(super) fn source_authority(
    root: &Path,
    selected_root: &Path,
    environment: &[(OsString, OsString)],
) -> Result<(SourceExecution, CargoExternalSourceAuthority), ProveFailure> {
    let host =
        spice::cargo_isolation::Host::from_environment(selected_root, environment.iter().cloned())
            .map_err(ProveFailure::cargo_isolation)?;
    let cargo_home = environment_value(environment, "CARGO_HOME")?;
    // The home belongs to the live worktree; the lockfile belongs to the
    // immutable staged source. Neither authority can stand in for the other.
    let lockfile = fs::read(selected_root.join("Cargo.lock"))
        .map_err(|error| ProveFailure::refusal("gate::prove-cargo-source-unavailable", error))?;
    let (home, source) = spice::cargo_isolation::select_source_generation(
        Path::new(cargo_home),
        &lockfile,
        root,
        &host,
    )
    .map_err(|error| ProveFailure::refusal("gate::prove-cargo-source-unavailable", error))?;
    let external_source = CargoExternalSourceAuthority::new(
        source.generation().to_owned(),
        source.manifest().to_owned(),
    )
    .map_err(ProveFailure::cargo)?;
    Ok((
        SourceExecution {
            home: std::sync::Arc::new(home),
            lockfile,
            root: root.to_path_buf(),
            host,
            authority: source,
        },
        external_source,
    ))
}

pub(super) fn environment_value<'a>(
    environment: &'a [(OsString, OsString)],
    name: &str,
) -> Result<&'a OsStr, ProveFailure> {
    environment
        .iter()
        .find(|(candidate, _)| candidate == OsStr::new(name))
        .map(|(_, value)| value.as_os_str())
        .ok_or_else(|| {
            ProveFailure::refusal(
                "gate::prove-cargo-source-unavailable",
                format!("captured compiler environment has no {name}"),
            )
        })
}

fn require_reusable_shard(shard: &CargoGateShard) -> Result<(), ProveFailure> {
    if let Some(reason) = shard.non_cacheable_reason() {
        return Err(ProveFailure::refusal(
            "gate::prove-cargo-action-not-reusable",
            format!(
                "Cargo action {} is not reusable: {reason}",
                shard.action_id(),
            ),
        ));
    }
    Ok(())
}

pub(super) fn request(request: Request<'_>) -> bool {
    (request.missing && request.action.is_none())
        || request.action.is_some_and(|action| {
            action == "rust-typecheck"
                || action == "cargo-tests"
                || action.starts_with("cargo:typecheck:")
                || action.starts_with("cargo:test:")
        })
}

/// Run one shard, capturing its output. A pass retains the capture; a refusal
/// carries its bounded tail and the retained capture's path.
pub(super) fn execute(
    execution: Execution,
    captures: &output::Captures,
) -> Result<(Digest, Option<Export>), ProveFailure> {
    let Execution {
        shard,
        root,
        writable_root,
        environment,
        cargo,
        source,
    } = execution;
    let mut capture = Vec::new();
    let result = gate::run_cargo_gate_shard_at(
        &root,
        &writable_root,
        environment,
        cargo.as_os_str(),
        &shard,
        &mut capture,
    )
    .map_err(|error| {
        ProveFailure::refusal(
            "gate::prove-cargo-shard-refused",
            format!("{error}\n{}", output::failure_evidence(captures, &capture)),
        )
    })?;
    let retained = output::retain(captures, &capture).ok();
    let observed = spice::cargo_isolation::readmit_source_generation(
        source.home.home(),
        &source.lockfile,
        &source.root,
        &source.host,
    )
    .map_err(|error| ProveFailure::refusal("gate::prove-cargo-source-moved", error))?;
    if observed != source.authority {
        return Err(ProveFailure::refusal(
            "gate::prove-cargo-source-moved",
            format!(
                "expected generation={} manifest={}, observed generation={} manifest={}",
                source.authority.generation(),
                source.authority.manifest(),
                observed.generation(),
                observed.manifest(),
            ),
        ));
    }
    let digest = Digest::compute(
        Domain::GateFocusedReport,
        &wire::encode(&result)
            .map_err(|error| ProveFailure::fault("gate::prove-result-unencodable", error))?,
    );
    Ok((digest, retained))
}
