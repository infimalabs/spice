//! Static external-tool posture carried by generated command contracts.

#[cfg(test)]
use std::collections::BTreeSet;

#[cfg(test)]
use super::{Contract, Guard};
pub use wire::{
    DoctorToolBounds as ToolBounds, DoctorToolCancellation as ToolCancellation,
    DoctorToolMode as ToolMode, DoctorToolNetwork as ToolNetwork,
    DoctorToolRequirement as ToolRequirement, DoctorToolSource as ToolSource,
};

/// One external tool declaration carried by the closed command registry.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExternalTool {
    pub name: &'static str,
    pub owner: &'static str,
    pub mode: ToolMode,
    pub requirement: ToolRequirement,
    pub source: ToolSource,
    pub bounds: ToolBounds,
    pub cancellation: ToolCancellation,
    pub network: ToolNetwork,
}

pub(super) const NO_TOOLS: &[ExternalTool] = &[];

pub(super) const DOCTOR_TOOLS: &[ExternalTool] = &[ExternalTool {
    name: "rtk",
    owner: "rtk",
    mode: ToolMode::Observe,
    requirement: ToolRequirement::Optional,
    source: ToolSource::WorktreeConfiguration,
    bounds: ToolBounds::NotExecuted,
    cancellation: ToolCancellation::NotExecuted,
    network: ToolNetwork::NotExecuted,
}];

pub(super) const RTK_QUERY_TOOLS: &[ExternalTool] = &[ExternalTool {
    name: "rtk",
    owner: "spice::rtk_gain",
    mode: ToolMode::Launch,
    requirement: ToolRequirement::Required,
    source: ToolSource::WorktreeConfiguration,
    bounds: ToolBounds::ChildOwned,
    cancellation: ToolCancellation::ChildOwned,
    network: ToolNetwork::Denied,
}];

pub(super) const CALLER_COMMAND: &[ExternalTool] = &[ExternalTool {
    name: "CMD",
    owner: "caller",
    mode: ToolMode::Launch,
    requirement: ToolRequirement::Required,
    source: ToolSource::CommandOperand,
    bounds: ToolBounds::ChildOwned,
    cancellation: ToolCancellation::ChildOwned,
    network: ToolNetwork::ChildOwned,
}];

pub(super) const NATIVE_CARGO_TOOL: &[ExternalTool] = &[ExternalTool {
    name: "cargo",
    owner: "cargo_isolation::dev_cargo",
    mode: ToolMode::Launch,
    requirement: ToolRequirement::Required,
    source: ToolSource::BootstrapEnvironment,
    bounds: ToolBounds::ChildOwned,
    cancellation: ToolCancellation::ChildOwned,
    network: ToolNetwork::Denied,
}];

pub(super) const GATE_POLICY_TOOLS: &[ExternalTool] = &[ExternalTool {
    name: "gate policy executable",
    owner: "gate::Contract",
    mode: ToolMode::Launch,
    requirement: ToolRequirement::Required,
    source: ToolSource::GatePolicyContract,
    bounds: ToolBounds::GateContract,
    cancellation: ToolCancellation::Deadline,
    network: ToolNetwork::Permitted,
}];

pub(super) const REFERENCE_TRANSACTION_TOOLS: &[ExternalTool] = &[ExternalTool {
    name: "git",
    owner: "gate::reference_transaction",
    mode: ToolMode::Launch,
    requirement: ToolRequirement::Conditional,
    source: ToolSource::ReferenceTransactionContract,
    bounds: ToolBounds::ReferenceTransaction,
    cancellation: ToolCancellation::ChildOwned,
    network: ToolNetwork::Permitted,
}];

const TASK_GIT_TOOL: ExternalTool = ExternalTool {
    name: "git",
    owner: "task_git",
    mode: ToolMode::Launch,
    requirement: ToolRequirement::Required,
    source: ToolSource::TaskBoundaryContract,
    bounds: ToolBounds::TaskBoundary,
    cancellation: ToolCancellation::Deadline,
    network: ToolNetwork::Permitted,
};

pub(super) const TASK_GIT_TOOLS: &[ExternalTool] = &[TASK_GIT_TOOL];

#[cfg(target_os = "macos")]
pub(crate) const JUDGE_EXECUTABLE: &str = "afm-cli";
#[cfg(not(target_os = "macos"))]
pub(crate) const JUDGE_EXECUTABLE: &str = "spice-judge";

const JUDGE_TOOL: ExternalTool = ExternalTool {
    name: JUDGE_EXECUTABLE,
    owner: "judge::Process",
    mode: ToolMode::Launch,
    requirement: ToolRequirement::Optional,
    source: ToolSource::AdjudicationContract,
    bounds: ToolBounds::Adjudication,
    cancellation: ToolCancellation::Deadline,
    network: ToolNetwork::Permitted,
};

pub(super) const TASK_DONE_TOOLS: &[ExternalTool] = &[TASK_GIT_TOOL, JUDGE_TOOL];
pub(super) const UP_TOOLS: &[ExternalTool] = &[JUDGE_TOOL];

/// The configured vendor program the detached supervisor runs for a lane's
/// occupant, under that supervisor's runtime, grace, and reap deadlines.
pub(super) const OCCUPANT_LAUNCH_TOOLS: &[ExternalTool] = &[ExternalTool {
    name: "omp",
    owner: "resident::supervisor",
    mode: ToolMode::Launch,
    requirement: ToolRequirement::Required,
    source: ToolSource::VendorConfiguration,
    bounds: ToolBounds::CommandOwned,
    cancellation: ToolCancellation::Deadline,
    network: ToolNetwork::Permitted,
}];

pub(super) const INSTALL_SYMBOLS_TOOLS: &[ExternalTool] = &[ExternalTool {
    name: "dwarfdump",
    owner: "install::DwarfdumpLookup",
    mode: ToolMode::Launch,
    requirement: ToolRequirement::Conditional,
    source: ToolSource::SymbolLookupContract,
    bounds: ToolBounds::CommandOwned,
    cancellation: ToolCancellation::Deadline,
    network: ToolNetwork::Permitted,
}];

pub(super) const PROOF_TOOLS: &[ExternalTool] = &[
    ExternalTool {
        name: "OCI runtime",
        owner: "proof::OciRuntime",
        mode: ToolMode::Launch,
        requirement: ToolRequirement::Required,
        source: ToolSource::ProofRuntime,
        bounds: ToolBounds::ProofLimits,
        cancellation: ToolCancellation::Deadline,
        network: ToolNetwork::Denied,
    },
    ExternalTool {
        name: "sealed battery member",
        owner: "proof::Battery",
        mode: ToolMode::Launch,
        requirement: ToolRequirement::Required,
        source: ToolSource::ProofBattery,
        bounds: ToolBounds::ProofLimits,
        cancellation: ToolCancellation::Deadline,
        network: ToolNetwork::Denied,
    },
    ExternalTool {
        name: "git",
        owner: "windows_execution::hosted::client",
        mode: ToolMode::Launch,
        requirement: ToolRequirement::Conditional,
        source: ToolSource::BootstrapEnvironment,
        bounds: ToolBounds::ProofLimits,
        cancellation: ToolCancellation::Deadline,
        network: ToolNetwork::Permitted,
    },
    ExternalTool {
        name: "GitHub CLI",
        owner: "windows_execution::hosted::client",
        mode: ToolMode::Launch,
        requirement: ToolRequirement::Conditional,
        source: ToolSource::BootstrapEnvironment,
        bounds: ToolBounds::ProofLimits,
        cancellation: ToolCancellation::Deadline,
        network: ToolNetwork::Permitted,
    },
];

#[cfg(test)]
pub(super) fn posture_violations(contracts: &[Contract]) -> Vec<String> {
    let mut violations = Vec::new();
    for contract in contracts {
        validate_contract_tools(contract, &mut violations);
    }
    violations
}

#[cfg(test)]
fn validate_contract_tools(contract: &Contract, violations: &mut Vec<String>) {
    let path = contract.path.join(" ");
    let launches = contract
        .tools
        .iter()
        .any(|tool| tool.mode == ToolMode::Launch);
    if launches != contract.guards.contains(&Guard::Toolful) {
        violations.push(format!(
            "{path}: toolful guard and launch declarations disagree"
        ));
    }
    let mut declarations = BTreeSet::new();
    for tool in contract.tools {
        validate_tool_shape(&path, tool, &mut declarations, violations);
        validate_execution_posture(&path, tool, violations);
        validate_source_posture(&path, tool, violations);
    }
}

#[cfg(test)]
fn validate_tool_shape(
    path: &str,
    tool: &ExternalTool,
    declarations: &mut BTreeSet<&'static str>,
    violations: &mut Vec<String>,
) {
    if tool.name.trim().is_empty() {
        violations.push(format!("{path}: external tool has no name"));
    }
    if tool.owner.trim().is_empty() {
        violations.push(format!(
            "{path}: external tool {:?} has no owner",
            tool.name
        ));
    }
    if !declarations.insert(tool.name) {
        violations.push(format!(
            "{path}: external tool {:?} is declared more than once",
            tool.name
        ));
    }
}

#[cfg(test)]
fn validate_execution_posture(path: &str, tool: &ExternalTool, violations: &mut Vec<String>) {
    let execution = (tool.bounds, tool.cancellation, tool.network);
    let not_executed = (
        ToolBounds::NotExecuted,
        ToolCancellation::NotExecuted,
        ToolNetwork::NotExecuted,
    );
    let invalid = match tool.mode {
        ToolMode::Launch => {
            execution.0 == ToolBounds::NotExecuted
                || execution.1 == ToolCancellation::NotExecuted
                || execution.2 == ToolNetwork::NotExecuted
        }
        ToolMode::Observe => execution != not_executed,
    };
    if invalid {
        let detail = match tool.mode {
            ToolMode::Launch => "has an observation-only posture",
            ToolMode::Observe => "carries execution posture",
        };
        violations.push(format!("{path}: tool {:?} {detail}", tool.name));
    }
}

#[cfg(test)]
fn validate_source_posture(path: &str, tool: &ExternalTool, violations: &mut Vec<String>) {
    if tool.source == ToolSource::WorktreeConfiguration {
        let expected = match tool.mode {
            ToolMode::Observe => (
                ToolRequirement::Optional,
                ToolBounds::NotExecuted,
                ToolCancellation::NotExecuted,
                ToolNetwork::NotExecuted,
            ),
            ToolMode::Launch => (
                ToolRequirement::Required,
                ToolBounds::ChildOwned,
                ToolCancellation::ChildOwned,
                ToolNetwork::Denied,
            ),
        };
        let observed = (
            tool.requirement,
            tool.bounds,
            tool.cancellation,
            tool.network,
        );
        if observed != expected {
            violations.push(format!(
                "{path}: worktree tool {:?} escaped its configured integration posture",
                tool.name
            ));
        }
        return;
    }
    let expected = match tool.source {
        ToolSource::CommandOperand => (
            ToolMode::Launch,
            ToolBounds::ChildOwned,
            ToolCancellation::ChildOwned,
            ToolNetwork::ChildOwned,
        ),
        ToolSource::BootstrapEnvironment => (
            ToolMode::Launch,
            ToolBounds::ChildOwned,
            ToolCancellation::ChildOwned,
            ToolNetwork::Denied,
        ),
        ToolSource::GatePolicyContract => (
            ToolMode::Launch,
            ToolBounds::GateContract,
            ToolCancellation::Deadline,
            ToolNetwork::Permitted,
        ),
        ToolSource::ReferenceTransactionContract => (
            ToolMode::Launch,
            ToolBounds::ReferenceTransaction,
            ToolCancellation::ChildOwned,
            ToolNetwork::Permitted,
        ),
        ToolSource::ProofRuntime | ToolSource::ProofBattery => (
            ToolMode::Launch,
            ToolBounds::ProofLimits,
            ToolCancellation::Deadline,
            ToolNetwork::Denied,
        ),
        ToolSource::TaskBoundaryContract => (
            ToolMode::Launch,
            ToolBounds::TaskBoundary,
            ToolCancellation::Deadline,
            ToolNetwork::Permitted,
        ),
        ToolSource::AdjudicationContract => (
            ToolMode::Launch,
            ToolBounds::Adjudication,
            ToolCancellation::Deadline,
            ToolNetwork::Permitted,
        ),
        ToolSource::SymbolLookupContract | ToolSource::VendorConfiguration => (
            ToolMode::Launch,
            ToolBounds::CommandOwned,
            ToolCancellation::Deadline,
            ToolNetwork::Permitted,
        ),
        ToolSource::WorktreeConfiguration => unreachable!(),
    };
    let observed = (tool.mode, tool.bounds, tool.cancellation, tool.network);
    if observed != expected {
        violations.push(format!(
            "{path}: tool {:?} escaped its source contract",
            tool.name
        ));
    }
}
