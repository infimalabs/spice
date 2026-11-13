//! Focused reusable gate-action preparation.

use super::*;

pub(super) fn prepare(
    staged: &StagedActionSelection,
    manifest: &BatteryManifest,
    gate_set: &Digest,
    baseline: Option<&str>,
    gate_id: GateId,
    writable_root: &Path,
) -> Result<PreparedAction, ProveFailure> {
    let mut arm = staged
        .anchor()
        .derive_reached_arm(staged.selection(), gate_id)
        .map_err(ProveFailure::run)?;
    let context = arm.context().map_err(ProveFailure::run)?;
    let declaration = declaration(manifest, gate_id)?;
    let reuse = gate::gate_action_reuse_class(gate_id)
        .expect("requested actions are classified as reusable");
    let baseline = resolve_baseline(gate_id, reuse, baseline)?;
    let (tools, action_tool_evidence, tool_authority) = tools::prepare(&arm, &context)?;
    // The app typecheck resolves the staged `Cargo.lock`; the command rim's
    // home may be a seat pinned to an older generation, so the arm borrows the
    // home admitted for that exact lock, as the Cargo shards do.
    let cargo_home = if gate_id == GateId::AppWebTypecheck {
        let authority = cargo::authority(arm.root(), arm.compiler_environment())?;
        let (source, _) =
            cargo::source_authority(writable_root, arm.root(), &authority.execution_environment)?;
        arm.bind_cargo_home(source.home.home());
        Some(source.home)
    } else {
        None
    };
    let action_environment = normalized_action_environment(arm.compiler_environment());
    let (environment_names, environment) = environment(&action_environment)?;
    let reach = staged
        .selection()
        .reach_for(gate_id)
        .expect("requested actions come from the canonical selection");
    let inventory = inventory(
        reach,
        declaration,
        gate_set,
        &context,
        &tool_authority,
        &environment,
        baseline.as_ref(),
    )?;
    let input_manifest = match arm
        .compile_action_input(staged.selection(), gate_id, inventory)
        .map_err(ProveFailure::run)?
    {
        ActionInputClosure::Reusable(manifest) => manifest,
        ActionInputClosure::NonCacheable(reason) => {
            return Err(ProveFailure::refusal(
                "gate::prove-action-not-reusable",
                format!("action {gate_id} has no complete reusable closure: {reason:?}"),
            ));
        }
    };
    let input_manifest = input_manifest
        .digest()
        .map_err(ProveFailure::action_input)?;
    let spec = ActionSpec::new(ActionSpecFields {
        action_id: format!("gate:{gate_id}"),
        runner: "spice-focused-gate".to_owned(),
        runner_version: RUNNER_VERSION,
        declaration: declaration.to_owned(),
        argv: argv(gate_id),
        cwd: ".".to_owned(),
        environment_names,
        tools,
        input_strategy: "selected-reach-v1".to_owned(),
        reuse,
    })
    .map_err(ProveFailure::action_input)?;
    let action_spec = spec.digest().map_err(ProveFailure::action_input)?;
    let action_key = gate::derive_authority_bound_action_key(
        &spec,
        &ActionKeyAuthority {
            input_manifest: input_manifest.clone(),
            policy_authority: ActionPolicyAuthority::PolicyConsuming,
            gate_set: gate_set.clone(),
            tools: tool_authority,
            environment,
            baseline,
        },
    )
    .map_err(ProveFailure::action_input)?;
    Ok(PreparedAction {
        action: gate_id.to_string(),
        action_key,
        action_spec,
        input_manifest,
        execution: PreparedExecution::Gate {
            arm: Box::new(arm),
            tools: action_tool_evidence,
            cargo_home,
        },
    })
}

/// The runner's dispatch, then a toolchain arm's declared analyses, so an arm
/// whose declared commands change answers a new specification and no receipt
/// minted under the earlier commands satisfies it.
fn argv(gate_id: GateId) -> Vec<String> {
    let analysis = gate::TOOLCHAIN_ARMS
        .iter()
        .find(|declaration| declaration.id() == gate_id)
        .map_or(&[][..], |declaration| declaration.analysis());
    ["gate".to_owned(), gate_id.to_string()]
        .into_iter()
        .chain(analysis.iter().map(|&command| command.to_owned()))
        .collect()
}

fn inventory(
    reach: &gate::ChangeArmReach,
    declaration: &str,
    gate_set: &Digest,
    context: &gate::BatteryContext,
    tools: &Digest,
    environment: &Digest,
    baseline: Option<&Digest>,
) -> Result<ActionInputInventory, ProveFailure> {
    let configuration = digest_encoded(Domain::GateConfiguration, &context.configuration())?;
    let inventory = digest_encoded(Domain::GateInventory, reach)?;
    let mut non_files = vec![
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
            ActionNonFileKind::Policy,
            "compiled-policy",
            context.policy().clone(),
        )?,
        non_file(
            ActionNonFileKind::Policy,
            "resolved-configuration",
            configuration,
        )?,
        non_file(
            ActionNonFileKind::Toolchain,
            "discovered-tools",
            tools.clone(),
        )?,
        non_file(
            ActionNonFileKind::Environment,
            "compiler-environment",
            environment.clone(),
        )?,
        non_file(ActionNonFileKind::Inventory, "selected-reach", inventory)?,
        non_file(
            ActionNonFileKind::BoundLedger,
            "gate-state-ledger",
            context.state().clone(),
        )?,
    ];
    if let Some(identity) = baseline {
        non_files.push(non_file(
            ActionNonFileKind::Baseline,
            "committed-baseline",
            identity.clone(),
        )?);
    }
    ActionInputInventory::complete(Vec::new(), non_files).map_err(ProveFailure::action_input)
}

fn resolve_baseline(
    gate_id: GateId,
    reuse: ActionReuseClass,
    baseline: Option<&str>,
) -> Result<Option<Digest>, ProveFailure> {
    match reuse {
        ActionReuseClass::AncestrySensitive => baseline
            .map(|identity| Digest::compute(Domain::GateBaseline, identity.as_bytes()))
            .map(Some)
            .ok_or_else(|| {
                ProveFailure::refusal(
                    "gate::prove-baseline-unavailable",
                    format!("action {gate_id} requires a committed staged baseline"),
                )
            }),
        ActionReuseClass::Cacheable => Ok(None),
        ActionReuseClass::LiveOnly => unreachable!("live-only actions were filtered"),
    }
}
