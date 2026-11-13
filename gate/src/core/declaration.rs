use crate::GateId;

/// Closed acquisition contract for one compiled core arm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreToolContract {
    /// Pure Rust acquisition from admitted tree/configuration state.
    Compiled { provider: &'static str },
    /// Bounded Git observation with exact permitted argument vectors.
    Git {
        commands: &'static [GitCommandContract],
        timeout: CoreTimeoutContract,
    },
}

/// One exact permitted `git` invocation and its accepted completion codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitCommandContract {
    arguments: &'static [&'static str],
    accepted_exit_codes: &'static [i32],
}

impl GitCommandContract {
    /// Arguments after the fixed `git` program name.
    #[must_use]
    pub const fn arguments(self) -> &'static [&'static str] {
        self.arguments
    }

    /// Exit codes that produce a measured observation for this invocation.
    #[must_use]
    pub const fn accepted_exit_codes(self) -> &'static [i32] {
        self.accepted_exit_codes
    }
}

/// Process deadline contract for external observation acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreTimeoutContract {
    /// Integration must supply a finite non-zero deadline.
    Required,
}

/// One compiled core-arm declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoreArmDeclaration {
    id: GateId,
    tool: CoreToolContract,
    schema: &'static str,
}

impl CoreArmDeclaration {
    /// Stable current gate identity.
    #[must_use]
    pub const fn id(self) -> GateId {
        self.id
    }

    /// Closed acquisition tool contract.
    #[must_use]
    pub const fn tool(self) -> CoreToolContract {
        self.tool
    }

    /// Stable metric or finding schema name.
    #[must_use]
    pub const fn schema(self) -> &'static str {
        self.schema
    }
}

const MERGE_COMMANDS: &[GitCommandContract] = &[
    command(&["rev-parse", "--git-path", "MERGE_HEAD"], &[0]),
    command(&["rev-parse", "HEAD^{tree}"], &[0]),
    command(
        &[
            "merge-tree",
            "--write-tree",
            "-z",
            "--name-only",
            "HEAD",
            "MERGE_HEAD",
        ],
        &[0, 1],
    ),
    command(
        &[
            "--literal-pathspecs",
            "ls-tree",
            "-z",
            "--name-only",
            "HEAD",
            "--",
            "<conflicted-paths>",
        ],
        &[0],
    ),
];
const STAGING_COMMANDS: &[GitCommandContract] = &[
    command(
        &["diff", "--cached", "--name-only", "--diff-filter=ACMR"],
        &[0],
    ),
    command(&["diff", "--name-only"], &[0]),
];

const fn command(
    arguments: &'static [&'static str],
    accepted_exit_codes: &'static [i32],
) -> GitCommandContract {
    GitCommandContract {
        arguments,
        accepted_exit_codes,
    }
}

const DECLARATIONS: [CoreArmDeclaration; 8] = [
    CoreArmDeclaration {
        id: GateId::MergeIntegrity,
        tool: CoreToolContract::Git {
            commands: MERGE_COMMANDS,
            timeout: CoreTimeoutContract::Required,
        },
        schema: "merge-tree-comparison-v1",
    },
    CoreArmDeclaration {
        id: GateId::PlanningPhase,
        tool: CoreToolContract::Compiled {
            provider: "board active-claim workflow head",
        },
        schema: "active-claim-phase-v1",
    },
    CoreArmDeclaration {
        id: GateId::RepoShape,
        tool: CoreToolContract::Compiled {
            provider: "gate immutable selected tree and admitted shape policy",
        },
        schema: "repo-shape-findings-v1",
    },
    CoreArmDeclaration {
        id: GateId::Staging,
        tool: CoreToolContract::Git {
            commands: STAGING_COMMANDS,
            timeout: CoreTimeoutContract::Required,
        },
        schema: "staging-integrity-v1",
    },
    CoreArmDeclaration {
        id: GateId::RepoDocs,
        tool: CoreToolContract::Compiled {
            provider: "gate immutable selected tree and resolved bound state",
        },
        schema: "repo-doc-character-bounds-v1",
    },
    CoreArmDeclaration {
        id: GateId::ConfigKeyValidity,
        tool: CoreToolContract::Compiled {
            provider: "spice admitted layered configuration",
        },
        schema: "config-admission-findings-v1",
    },
    CoreArmDeclaration {
        id: GateId::ConfigFalseDisable,
        tool: CoreToolContract::Compiled {
            provider: "spice compiled false-disable registry inventory",
        },
        schema: "config-declaration-consumer-inventory-v1",
    },
    CoreArmDeclaration {
        id: GateId::ConfigTrackedTrust,
        tool: CoreToolContract::Compiled {
            provider: "spice compiled tracked-config trust inventory",
        },
        schema: "config-declaration-consumer-inventory-v1",
    },
];

/// The eight declarations in fixed battery order.
#[must_use]
pub const fn core_declarations() -> &'static [CoreArmDeclaration; 8] {
    &DECLARATIONS
}
