//! External executable authority for focused reusable gate actions.

use gate::{ActionToolRequirement, ArmEvidence, BatteryContext, BatteryState, BatteryToolEvidence};
use wire::{Digest, Domain};

use super::{ProveFailure, digest_encoded};

pub(super) fn prepare(
    arm: &spice::gate::PreparedArm<'_>,
    context: &BatteryContext,
) -> Result<(Vec<ActionToolRequirement>, Vec<gate::ToolEvidence>, Digest), ProveFailure> {
    let evidence = arm.action_tools().map_err(ProveFailure::run)?;
    let mut requirements = action_requirements(context.tools())?;
    requirements.extend(tool_evidence_requirements(&evidence)?);
    requirements.sort();
    requirements.dedup();
    let authority = digest_encoded(Domain::GateTool, &(context.tools(), &evidence))?;
    Ok((requirements, evidence, authority))
}

pub(super) fn execute(
    mut arm: spice::gate::PreparedArm<'static>,
    expected: Vec<gate::ToolEvidence>,
    captures: &super::output::Captures,
) -> Result<Digest, ProveFailure> {
    if arm.action_tools().map_err(ProveFailure::run)? != expected {
        return Err(moved("before execution"));
    }
    let report = arm.execute().map_err(ProveFailure::run)?;
    if !report_tools_match(&report, &expected)
        || arm.action_tools().map_err(ProveFailure::run)? != expected
    {
        return Err(moved("during execution"));
    }
    let encoded = report
        .encode()
        .map_err(|error| ProveFailure::fault("gate::prove-result-unencodable", error))?;
    if !report.admits() {
        return Err(ProveFailure::refusal(
            "gate::prove-action-regressed",
            regression(&report, &encoded, captures),
        ));
    }
    Ok(Digest::compute(Domain::GateFocusedReport, &encoded))
}

/// A regressed action's reason, its complete typed report retained by digest,
/// and the command that renders every finding.
fn regression(
    report: &gate::FocusedArmReport,
    encoded: &[u8],
    captures: &super::output::Captures,
) -> String {
    let retained = match super::output::retain(captures, encoded) {
        Ok(export) => format!("complete report: {} ({} bytes)", export.path, export.bytes),
        Err(error) => format!("complete report was not retained: {error}"),
    };
    format!(
        "action={} state={}\n{}\n{retained}\nfull-evidence: spice dev xtask -- gate --report {}",
        report.gate_id(),
        report.result().state().as_str(),
        super::output::bounded(captures, report.result().reason()),
        report.gate_id(),
    )
}

pub(super) fn action_requirements(
    tools: &[BatteryToolEvidence],
) -> Result<Vec<ActionToolRequirement>, ProveFailure> {
    let mut requirements = tools
        .iter()
        .map(|tool| ActionToolRequirement::new(tool.name(), tool.program()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProveFailure::action_input)?;
    requirements.sort();
    requirements.dedup();
    Ok(requirements)
}

fn tool_evidence_requirements(
    tools: &[gate::ToolEvidence],
) -> Result<Vec<ActionToolRequirement>, ProveFailure> {
    tools
        .iter()
        .map(|tool| match tool {
            gate::ToolEvidence::Missing {
                name,
                requested_program,
            }
            | gate::ToolEvidence::Unprobed {
                name,
                requested_program,
                ..
            }
            | gate::ToolEvidence::Identified {
                name,
                requested_program,
                ..
            } => ActionToolRequirement::new(name, requested_program),
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(ProveFailure::action_input)
}

fn executed_tools(report: &gate::FocusedArmReport) -> &[gate::ToolEvidence] {
    match report.result().evidence() {
        ArmEvidence::Toolchain { report } => &report.tools,
        _ => &[],
    }
}

fn report_tools_match(report: &gate::FocusedArmReport, expected: &[gate::ToolEvidence]) -> bool {
    let observed = executed_tools(report);
    if matches!(
        report.gate_id(),
        gate::GateId::MergeIntegrity | gate::GateId::Staging
    ) {
        return observed.is_empty();
    }
    observed == expected
        || (observed.is_empty()
            && matches!(
                report.result().state(),
                BatteryState::NotApplicable | BatteryState::Excluded
            ))
}

fn moved(stage: &str) -> ProveFailure {
    ProveFailure::refusal(
        "gate::prove-tool-moved",
        format!("focused action executable authority changed {stage}"),
    )
}
