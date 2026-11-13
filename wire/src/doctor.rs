//! The complete daemonless Doctor report.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ConfigSource;

/// Every ambient variable owned by Spice.
///
/// Doctor reports only whether these names are present. Their values never
/// enter the report. Variants remain in serialized-name order because derived
/// [`Ord`] ranks them by this declaration order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum SpiceVariable {
    /// Preferred agent vendor selected for transcript forensics.
    #[serde(rename = "SPICE_AGENT_VENDOR")] // env-policy: allow
    AgentVendor,
    /// Source identity of the browser app generation the build stamped.
    #[serde(rename = "SPICE_APP_SOURCE_IDENTITY")] // env-policy: allow
    AppSourceIdentity,
    /// Original Cargo home used to seed the immutable source store.
    #[serde(rename = "SPICE_BOOTSTRAP_CARGO_HOME")] // env-policy: allow
    BootstrapCargoHome,
    /// Launch channel a detached `spice up` granted its resident child.
    #[serde(rename = "SPICE_DAEMON_HANDOFF")] // env-policy: allow
    DaemonHandoff,
    /// Debug install fixture executable override.
    #[serde(rename = "SPICE_INSTALL_TEST_EXECUTABLE")] // env-policy: allow
    InstallTestExecutable,
    /// Lane a supervised command is bound to.
    #[serde(rename = "SPICE_LANE")] // env-policy: allow
    Lane,
    /// Native Cargo executable resolved before the private home is installed.
    #[serde(rename = "SPICE_NATIVE_CARGO")] // env-policy: allow
    NativeCargo,
    /// Pin document sealed into the native proof runner as it is built.
    #[serde(rename = "SPICE_NATIVE_PINS")] // env-policy: allow
    NativePins,
    /// Native rustc executable paired with the isolated Cargo authority.
    #[serde(rename = "SPICE_NATIVE_RUSTC")] // env-policy: allow
    NativeRustc,
    /// Native rustdoc executable paired with the isolated Cargo authority.
    #[serde(rename = "SPICE_NATIVE_RUSTDOC")] // env-policy: allow
    NativeRustdoc,
    /// Canonical JSON array of staged paths supplied to one configured pre-commit step.
    #[serde(rename = "SPICE_PRE_COMMIT_PATHS")] // env-policy: allow
    PreCommitPaths,
    /// Immutable staged-source digest supplied to every configured pre-commit step.
    #[serde(rename = "SPICE_PRE_COMMIT_SOURCE")] // env-policy: allow
    PreCommitSource,
    /// Exact RTK executable selected for supervised shell rewriting.
    #[serde(rename = "SPICE_RTK_EXECUTABLE")] // env-policy: allow
    RtkExecutable,
    /// Session identity a lane pinned for a vendor that exports none of its own.
    #[serde(rename = "SPICE_SESSION")] // env-policy: allow
    Session,
    /// Exact installed Spice executable selected by the agent-shell wrapper route.
    #[serde(rename = "SPICE_WRAPPER_SPICE")] // env-policy: allow
    WrapperSpice,
}

impl SpiceVariable {
    /// The complete vocabulary in stable serialized-name order.
    ///
    /// Keeping this order aligned with both checked configuration ledgers makes
    /// additions produce one local diff instead of a merge-only reorder.
    pub const ALL: &[Self] = &[
        Self::AgentVendor,
        Self::AppSourceIdentity,
        Self::BootstrapCargoHome,
        Self::DaemonHandoff,
        Self::InstallTestExecutable,
        Self::Lane,
        Self::NativeCargo,
        Self::NativePins,
        Self::NativeRustc,
        Self::NativeRustdoc,
        Self::PreCommitPaths,
        Self::PreCommitSource,
        Self::RtkExecutable,
        Self::Session,
        Self::WrapperSpice,
    ];

    /// Exact process-environment spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentVendor => "SPICE_AGENT_VENDOR", // env-policy: allow
            Self::AppSourceIdentity => "SPICE_APP_SOURCE_IDENTITY", // env-policy: allow
            Self::BootstrapCargoHome => "SPICE_BOOTSTRAP_CARGO_HOME", // env-policy: allow
            Self::DaemonHandoff => "SPICE_DAEMON_HANDOFF", // env-policy: allow
            Self::InstallTestExecutable => {
                "SPICE_INSTALL_TEST_EXECUTABLE" // env-policy: allow
            }
            Self::Lane => "SPICE_LANE", // env-policy: allow
            Self::NativeCargo => "SPICE_NATIVE_CARGO", // env-policy: allow
            Self::NativePins => "SPICE_NATIVE_PINS", // env-policy: allow
            Self::NativeRustc => "SPICE_NATIVE_RUSTC", // env-policy: allow
            Self::NativeRustdoc => "SPICE_NATIVE_RUSTDOC", // env-policy: allow
            Self::PreCommitPaths => "SPICE_PRE_COMMIT_PATHS", // env-policy: allow
            Self::PreCommitSource => "SPICE_PRE_COMMIT_SOURCE", // env-policy: allow
            Self::RtkExecutable => "SPICE_RTK_EXECUTABLE", // env-policy: allow
            Self::Session => "SPICE_SESSION", // env-policy: allow
            Self::WrapperSpice => "SPICE_WRAPPER_SPICE", // env-policy: allow
        }
    }
}

/// Result of one local Doctor observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DoctorObservation {
    /// The declared local input is available.
    Ready {},
    /// A declared local input is absent or unusable.
    Unavailable {
        /// Exact non-secret reason for the observation.
        reason: String,
    },
    /// The executable can be known only when the caller supplies an operand.
    InputRequired {
        /// Exact input needed before inspection is possible.
        reason: String,
    },
    /// The command declares no external tool to inspect.
    NotApplicable {
        /// Why no tool observation applies.
        reason: String,
    },
    /// A truthful observation would cross Doctor's offline, no-execution law.
    Unsupported {
        /// Boundary preventing the observation.
        reason: String,
    },
}

impl DoctorObservation {
    /// Stable status spelling, as the wire tags it.
    #[must_use]
    pub const fn status(&self) -> &'static str {
        match self {
            Self::Ready {} => "ready",
            Self::Unavailable { .. } => "unavailable",
            Self::InputRequired { .. } => "input-required",
            Self::NotApplicable { .. } => "not-applicable",
            Self::Unsupported { .. } => "unsupported",
        }
    }
}

impl fmt::Display for DoctorObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready {} => formatter.write_str("ready"),
            Self::Unavailable { reason }
            | Self::InputRequired { reason }
            | Self::NotApplicable { reason }
            | Self::Unsupported { reason } => write!(formatter, "{}: {reason}", self.status()),
        }
    }
}

/// Stateful subsystem contributing one bounded Doctor diagnosis row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorSubsystem {
    /// Current task heads and board policy admission.
    Board,
    /// Canonical authority schema, cursor, facts, and heads.
    Journal,
    /// Durable steering replay.
    Steering,
    /// Current resident lane head.
    Lane,
    /// Published resident transport discovery.
    Transport,
    /// Durable installation receipt.
    Installation,
    /// Durable release publication facts.
    Release,
}

impl DoctorSubsystem {
    /// The complete stateful subsystem roster in report order.
    pub const ALL: &[Self] = &[
        Self::Board,
        Self::Journal,
        Self::Steering,
        Self::Lane,
        Self::Transport,
        Self::Installation,
        Self::Release,
    ];

    /// Stable subsystem spelling, as the wire names it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Board => "board",
            Self::Journal => "journal",
            Self::Steering => "steering",
            Self::Lane => "lane",
            Self::Transport => "transport",
            Self::Installation => "installation",
            Self::Release => "release",
        }
    }
}

/// Whether a declared external tool may be launched by its command.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorToolMode {
    /// The owning command may launch the tool.
    Launch,
    /// The owning command observes the integration without launching it.
    Observe,
}

impl DoctorToolMode {
    /// The complete closed vocabulary.
    pub const ALL: &[Self] = &[Self::Launch, Self::Observe];
}

/// Whether a tool must exist for its declaring integration to operate.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorToolRequirement {
    /// The command requires this tool.
    Required,
    /// The command requires this tool only after an input predicate selects it.
    Conditional,
    /// Absence disables only an optional integration.
    Optional,
}

impl DoctorToolRequirement {
    /// The complete closed vocabulary.
    pub const ALL: &[Self] = &[Self::Required, Self::Conditional, Self::Optional];
}

/// Authority supplying the exact executable declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorToolSource {
    /// The caller supplies the executable after the command delimiter.
    CommandOperand,
    /// The native bootstrap environment supplies the executable before isolation.
    BootstrapEnvironment,
    /// The compiled current Gate policy contract supplies the executable.
    GatePolicyContract,
    /// The compiled reference-transaction hook contract supplies Git.
    ReferenceTransactionContract,
    /// The sealed Proof contract supplies the OCI runtime.
    ProofRuntime,
    /// The sealed Proof battery supplies a program inside the container.
    ProofBattery,
    /// The task-boundary contract supplies a bounded executable.
    TaskBoundaryContract,
    /// The shared adjudication contract supplies the optional judge executable.
    AdjudicationContract,
    /// The installed-symbol verification contract supplies the DWARF lookup tool.
    SymbolLookupContract,
    /// A worktree-local integration seam supplies the executable.
    WorktreeConfiguration,
    /// Repository vendor configuration supplies the agent program each lane
    /// occupant runs.
    VendorConfiguration,
}

impl DoctorToolSource {
    /// The complete closed vocabulary.
    pub const ALL: &[Self] = &[
        Self::CommandOperand,
        Self::BootstrapEnvironment,
        Self::GatePolicyContract,
        Self::ReferenceTransactionContract,
        Self::ProofRuntime,
        Self::ProofBattery,
        Self::TaskBoundaryContract,
        Self::AdjudicationContract,
        Self::SymbolLookupContract,
        Self::WorktreeConfiguration,
        Self::VendorConfiguration,
    ];
}

/// Authority owning runtime and output bounds.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorToolBounds {
    /// Spice adds no independent limit around the caller's child.
    ChildOwned,
    /// Gate owns runtime, drain, reap, grace, and capture limits.
    GateContract,
    /// Proof owns runtime, drain, reap, grace, and capture limits.
    ProofLimits,
    /// The task boundary owns command deadlines and bounded captured output.
    TaskBoundary,
    /// The shared adjudication service owns deadline and output bounds.
    Adjudication,
    /// Parent Git owns the hook lifetime while reference locks may be held.
    ReferenceTransaction,
    /// The owning command fixes its own deadline, invocation count, and capture limit.
    CommandOwned,
    /// Observation never starts the tool.
    NotExecuted,
}

impl DoctorToolBounds {
    /// The complete closed vocabulary.
    pub const ALL: &[Self] = &[
        Self::ChildOwned,
        Self::GateContract,
        Self::ProofLimits,
        Self::TaskBoundary,
        Self::Adjudication,
        Self::ReferenceTransaction,
        Self::CommandOwned,
        Self::NotExecuted,
    ];
}

/// Who can stop a declared tool invocation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorToolCancellation {
    /// The child and caller own interruption.
    ChildOwned,
    /// The owning limits terminate and reap at their deadline.
    Deadline,
    /// Observation never starts the tool.
    NotExecuted,
}

impl DoctorToolCancellation {
    /// The complete closed vocabulary.
    pub const ALL: &[Self] = &[Self::ChildOwned, Self::Deadline, Self::NotExecuted];
}

/// Network posture of a declared tool invocation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorToolNetwork {
    /// The caller's child declares its own network behavior.
    ChildOwned,
    /// The owning command does not promise an offline boundary.
    Permitted,
    /// The owning command constructs a network-denied boundary.
    Denied,
    /// Observation never starts the tool.
    NotExecuted,
}

impl DoctorToolNetwork {
    /// The complete closed vocabulary.
    pub const ALL: &[Self] = &[
        Self::ChildOwned,
        Self::Permitted,
        Self::Denied,
        Self::NotExecuted,
    ];
}

/// Compiled target named by the running binary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorTarget {
    /// Compiled operating-system family.
    pub os: String,
    /// Compiled processor architecture.
    pub architecture: String,
}

/// Local paths inspected without opening repository authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorLocations {
    /// Process working directory.
    pub working: String,
    /// Canonical worktree root.
    pub repository: String,
    /// Canonical Git common directory.
    pub repository_common: String,
    /// Exact tracked configuration location.
    pub tracked_configuration: String,
    /// Exact worktree-scoped override location.
    pub worktree_configuration: String,
    /// Exact machine-local configuration location.
    pub local_configuration: String,
}

/// Presence of one known ambient variable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorEnvironment {
    /// Closed variable name.
    pub variable: SpiceVariable,
    /// Presence only; the ambient value never crosses.
    pub observation: DoctorObservation,
}

/// One stateful subsystem's side-effect-free diagnosis.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorSubsystemDiagnosis {
    /// Closed subsystem identity.
    pub subsystem: DoctorSubsystem,
    /// Current bounded observation.
    pub observation: DoctorObservation,
}

/// Winning source for one effective configuration leaf.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorConfigProvenance {
    /// Canonical concrete configuration path.
    pub path: String,
    /// Highest-precedence source that supplied the effective value.
    pub source: ConfigSource,
}

/// One declared external tool and its local observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorTool {
    /// Registry-owned tool name.
    pub name: String,
    /// Component owning the declaration.
    pub owner: String,
    /// Launch or observation posture.
    pub mode: DoctorToolMode,
    /// Required or optional standing.
    pub requirement: DoctorToolRequirement,
    /// Authority supplying the executable.
    pub source: DoctorToolSource,
    /// Runtime and output limit owner.
    pub bounds: DoctorToolBounds,
    /// Cancellation owner.
    pub cancellation: DoctorToolCancellation,
    /// Network posture.
    pub network: DoctorToolNetwork,
    /// Current local observation.
    pub observation: DoctorObservation,
}

/// Tool posture for one generated command leaf.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorCommand {
    /// Space-separated generated command path.
    pub command: String,
    /// Aggregate posture, never ready when a declared row is non-ready.
    pub observation: DoctorObservation,
    /// Registry declarations in stable order.
    pub tools: Vec<DoctorTool>,
}

/// Extension namespace inspected by Doctor.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DoctorExtensionNamespace {
    /// Agent CLI vendors.
    Vendor,
    /// Shell command wrappers.
    Wrapper,
    /// Repository gates.
    Gate,
}

impl DoctorExtensionNamespace {
    /// Stable namespace spelling, as the wire names it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Vendor => "vendor",
            Self::Wrapper => "wrapper",
            Self::Gate => "gate",
        }
    }
}

/// One discovered extension declaration or registry diagnostic.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorExtension {
    /// Namespace containing the declaration.
    pub namespace: DoctorExtensionNamespace,
    /// Stable selected name, or `registry` for a namespace-wide diagnostic.
    pub name: String,
    /// Distribution or compiled component supplying the declaration.
    pub provider: String,
    /// Ready for a coherent declaration; unsupported for broken discovery.
    pub observation: DoctorObservation,
}

/// Complete successful Doctor answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorReport<C> {
    /// Compiled target identity.
    pub target: DoctorTarget,
    /// Working, repository, and configuration paths.
    pub locations: DoctorLocations,
    /// Presence of every known ambient variable.
    pub environment: Vec<DoctorEnvironment>,
    /// One deterministic row for every stateful subsystem.
    pub subsystems: Vec<DoctorSubsystemDiagnosis>,
    /// Fully admitted effective typed configuration.
    pub configuration: C,
    /// One source row for every concrete effective leaf.
    pub provenance: Vec<DoctorConfigProvenance>,
    /// One deterministic row for every generated command contract.
    pub commands: Vec<DoctorCommand>,
    /// Deterministically discovered vendor, wrapper, and gate declarations.
    pub extensions: Vec<DoctorExtension>,
}

/// The person-facing page: every stateful subsystem and extension, counts of
/// the commands and configuration leaves that need nothing, and each command
/// whose tools are not ready, with its reason. Ambient variables are optional
/// inputs, so only the ones present are named; an absent one is no finding.
impl<C> fmt::Display for DoctorReport<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "doctor {}-{} repository {}",
            self.target.os, self.target.architecture, self.locations.repository
        )?;
        writeln!(formatter, "subsystems")?;
        for row in &self.subsystems {
            writeln!(
                formatter,
                "  {} {}",
                row.subsystem.as_str(),
                row.observation
            )?;
        }
        writeln!(formatter, "extensions")?;
        for row in &self.extensions {
            writeln!(
                formatter,
                "  {} {} {}",
                row.namespace.as_str(),
                row.name,
                row.observation
            )?;
        }
        self.fmt_configuration(formatter)?;
        self.fmt_environment(formatter)?;
        self.fmt_commands(formatter)
    }
}

impl<C> DoctorReport<C> {
    fn fmt_configuration(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "configuration {} leaves", self.provenance.len())?;
        for source in ConfigSource::ALL {
            let leaves = self
                .provenance
                .iter()
                .filter(|row| row.source == *source)
                .count();
            if leaves > 0 {
                write!(formatter, ", {leaves} {}", source.as_str())?;
            }
        }
        writeln!(formatter)?;
        let locations = &self.locations;
        writeln!(formatter, "  tracked {}", locations.tracked_configuration)?;
        writeln!(formatter, "  local {}", locations.local_configuration)?;
        writeln!(formatter, "  worktree {}", locations.worktree_configuration)
    }

    fn fmt_environment(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let present = self
            .environment
            .iter()
            .filter(|row| row.observation == DoctorObservation::Ready {});
        let count = present.clone().count();
        write!(
            formatter,
            "environment {count} of {} optional variables set",
            self.environment.len()
        )?;
        if count > 0 {
            formatter.write_str(":")?;
        }
        for row in present {
            write!(formatter, " {}", row.variable.as_str())?;
        }
        writeln!(formatter)
    }

    fn fmt_commands(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = |status: &str| {
            self.commands
                .iter()
                .filter(|row| row.observation.status() == status)
                .count()
        };
        writeln!(
            formatter,
            "commands {} ready, {} without external tools",
            count("ready"),
            count("not-applicable")
        )?;
        for row in self.commands.iter().filter(|row| {
            !matches!(
                row.observation,
                DoctorObservation::Ready {} | DoctorObservation::NotApplicable { .. }
            )
        }) {
            writeln!(formatter, "  {} {}", row.command, row.observation)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DoctorObservation, DoctorReport, DoctorSubsystem, SpiceVariable};

    #[test]
    fn observation_objects_reject_unknown_fields_and_variants() {
        for document in [
            br#"{"status":"ready","reason":"not allowed"}"#.as_slice(),
            br#"{"status":"future"}"#.as_slice(),
        ] {
            assert!(
                crate::decode::<DoctorObservation>(document).is_err(),
                "Doctor observations have one closed spelling",
            );
        }
    }

    #[test]
    fn environment_names_round_trip_without_values() {
        for variable in SpiceVariable::ALL {
            let bytes = crate::encode(variable).expect("variable encodes");
            assert_eq!(
                crate::decode::<SpiceVariable>(&bytes).expect("variable decodes"),
                *variable,
            );
            assert_eq!(bytes, format!("\"{}\"", variable.as_str()).as_bytes());
        }
    }

    #[test]
    fn subsystem_roster_has_one_stable_wire_spelling_per_row() {
        let expected = [
            "\"board\"",
            "\"journal\"",
            "\"steering\"",
            "\"lane\"",
            "\"transport\"",
            "\"installation\"",
            "\"release\"",
        ];
        assert_eq!(DoctorSubsystem::ALL.len(), expected.len());
        for (subsystem, expected) in DoctorSubsystem::ALL.iter().zip(expected) {
            let bytes = crate::encode(subsystem).expect("subsystem encodes");
            assert_eq!(bytes, expected.as_bytes());
            assert_eq!(
                crate::decode::<DoctorSubsystem>(&bytes).expect("subsystem decodes"),
                *subsystem,
            );
        }
    }

    #[test]
    fn report_structures_reject_unknown_fields_at_every_level() {
        const REPORT: &str = concat!(
            r#"{"target":{"os":"linux","architecture":"x86_64"}"#,
            r#","locations":{"working":"/repo","repository":"/repo""#,
            r#","repository_common":"/repo/.git""#,
            r#","tracked_configuration":"/repo/spice.toml""#,
            r#","worktree_configuration":"/repo/.git/.spice/worktree.toml""#,
            r#","local_configuration":"/repo/.git/.spice/config.toml"}"#,
            r#","environment":[{"variable":"SPICE_LANE""#, // env-policy: allow
            r#","observation":{"status":"ready"}}]"#,
            r#","subsystems":[{"subsystem":"journal""#,
            r#","observation":{"status":"ready"}}]"#,
            r#","configuration":null"#,
            r#","provenance":[{"path":"lane.lease","source":"builtin"}]"#,
            r#","commands":[{"command":"doctor""#,
            r#","observation":{"status":"ready"},"tools":[{"name":"rtk""#,
            r#","owner":"rtk","mode":"observe","requirement":"optional""#,
            r#","source":"worktree-configuration","bounds":"not-executed""#,
            r#","cancellation":"not-executed","network":"not-executed""#,
            r#","observation":{"status":"ready"}}]}]"#,
            r#","extensions":[{"namespace":"vendor","name":"omp""#,
            r#","provider":"vendor","observation":{"status":"ready"}}]}"#,
        );

        let admitted: DoctorReport<()> =
            crate::decode(REPORT.as_bytes()).expect("complete report decodes");
        assert_eq!(admitted.commands[0].command, "doctor");
        assert_eq!(admitted.subsystems[0].subsystem, DoctorSubsystem::Journal);

        for pointer in [
            "",
            "/target",
            "/locations",
            "/environment/0",
            "/subsystems/0",
            "/provenance/0",
            "/commands/0",
            "/commands/0/tools/0",
            "/extensions/0",
        ] {
            let mut document: serde_json::Value =
                serde_json::from_str(REPORT).expect("fixture is JSON");
            document
                .pointer_mut(pointer)
                .expect("fixture pointer exists")
                .as_object_mut()
                .expect("fixture pointer names an object")
                .insert("future".to_owned(), serde_json::json!(true));
            let bytes = serde_json::to_vec(&document).expect("mutated fixture encodes");
            assert!(
                crate::decode::<DoctorReport<()>>(&bytes).is_err(),
                "unknown field at {pointer:?} crossed",
            );
        }
    }

    #[test]
    fn the_page_surfaces_only_commands_that_need_attention_and_present_variables() {
        use super::{DoctorCommand, DoctorEnvironment, DoctorLocations, DoctorTarget};

        let command = |command: &str, observation| DoctorCommand {
            command: command.to_owned(),
            observation,
            tools: Vec::new(),
        };
        let reason = |text: &str| text.to_owned();
        let report = DoctorReport {
            target: DoctorTarget {
                os: "linux".to_owned(),
                architecture: "x86_64".to_owned(),
            },
            locations: DoctorLocations {
                working: "/repo".to_owned(),
                repository: "/repo".to_owned(),
                repository_common: "/repo/.git".to_owned(),
                tracked_configuration: "/repo/spice.toml".to_owned(),
                worktree_configuration: "/repo/.git/worktree.toml".to_owned(),
                local_configuration: "/repo/.git/config.toml".to_owned(),
            },
            environment: vec![
                DoctorEnvironment {
                    variable: SpiceVariable::Session,
                    observation: DoctorObservation::Ready {},
                },
                DoctorEnvironment {
                    variable: SpiceVariable::RtkExecutable,
                    observation: DoctorObservation::Unavailable {
                        reason: reason("absent"),
                    },
                },
            ],
            subsystems: Vec::new(),
            configuration: (),
            provenance: Vec::new(),
            commands: vec![
                command("doctor", DoctorObservation::Ready {}),
                command(
                    "board",
                    DoctorObservation::NotApplicable {
                        reason: reason("no tools"),
                    },
                ),
                command(
                    "run",
                    DoctorObservation::InputRequired {
                        reason: reason("caller supplies CMD"),
                    },
                ),
            ],
            extensions: Vec::new(),
        };

        let page = report.to_string();
        let listed = |name: &str| {
            page.lines().any(|line| {
                line.strip_prefix("  ")
                    .is_some_and(|row| row.starts_with(name))
            })
        };
        assert!(page.contains("caller supplies CMD"), "{page}");
        assert!(listed("run "), "a command needing input is listed: {page}");
        assert!(
            !listed("doctor "),
            "a ready command is only counted: {page}"
        );
        assert!(
            !listed("board "),
            "a tool-less command is only counted: {page}"
        );
        assert!(page.contains(SpiceVariable::Session.as_str()), "{page}");
        assert!(
            !page.contains(SpiceVariable::RtkExecutable.as_str()),
            "an absent optional variable is no finding: {page}"
        );
    }
}
