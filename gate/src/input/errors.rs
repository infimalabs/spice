use super::*;

/// Why the compiled metrics and executable declarations do not form one closed set.
#[derive(Debug)]
pub enum GateSetError {
    /// One parser or metric declaration is internally invalid.
    Metric(MetricError),
    /// One shared-bound declaration is internally invalid.
    Camp(CampError),
    /// The current gate set declared no camping distribution.
    EmptyCamp,
    /// One metric name appears in more than one execution profile.
    DuplicateMetric {
        /// Repeated metric identity.
        metric: String,
    },
    /// Compiled camping loci and measurement declarations differ.
    CampLoci {
        /// Loci named by every shared bound in canonical order.
        declared: Vec<String>,
        /// Metrics the camping measurement policy would execute.
        measured: Vec<String>,
    },
    /// A metric names no compiled tool declaration.
    MissingTool {
        /// Metric whose parser would otherwise execute without a probe.
        metric: String,
        /// Tool name bound by the metric declaration.
        tool: String,
        /// Executable the parser would invoke.
        parser_program: String,
    },
    /// Two compiled tools share one identity name.
    DuplicateToolName {
        /// Repeated tool name.
        name: String,
    },
    /// One executable maps to more than one compiled tool declaration.
    DuplicateToolProgram {
        /// Repeated executable path.
        program: String,
    },
    /// The named tool probe and metric parser would execute different programs.
    MismatchedTool {
        /// Metric with the unsafe binding.
        metric: String,
        /// Named compiled tool.
        tool: String,
        /// Executable the metric parser would invoke.
        parser_program: String,
        /// Executable the named tool declaration would probe.
        tool_program: String,
    },
    /// A compiled tool has no metric parser consumer.
    UnusedTool {
        /// Unused tool name.
        tool: String,
        /// Executable the unused declaration would probe.
        program: String,
    },
}

impl fmt::Display for GateSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Metric(source) => write!(formatter, "invalid metric declaration: {source}"),
            Self::Camp(source) => write!(formatter, "invalid camp declaration: {source}"),
            Self::EmptyCamp => formatter.write_str("compiled gate set declares no camp bounds"),
            Self::DuplicateMetric { metric } => {
                write!(
                    formatter,
                    "compiled metric {metric} is declared more than once"
                )
            }
            Self::CampLoci { declared, measured } => write!(
                formatter,
                "compiled camp loci {declared:?} do not equal measured metrics {measured:?}"
            ),
            Self::MissingTool {
                metric,
                tool,
                parser_program,
            } => write!(
                formatter,
                "metric {metric} parser {parser_program} names missing tool {tool}"
            ),
            Self::DuplicateToolName { name } => {
                write!(
                    formatter,
                    "compiled tool name {name} is declared more than once"
                )
            }
            Self::DuplicateToolProgram { program } => write!(
                formatter,
                "compiled tool program {program} is declared more than once"
            ),
            Self::MismatchedTool {
                metric,
                tool,
                parser_program,
                tool_program,
            } => write!(
                formatter,
                "metric {metric} parser {parser_program} does not match tool {tool} program \
                 {tool_program}"
            ),
            Self::UnusedTool { tool, program } => {
                write!(
                    formatter,
                    "compiled tool {tool} program {program} has no metric"
                )
            }
        }
    }
}

impl Error for GateSetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Metric(source) => Some(source),
            Self::Camp(source) => Some(source),
            Self::EmptyCamp
            | Self::DuplicateMetric { .. }
            | Self::CampLoci { .. }
            | Self::MissingTool { .. }
            | Self::DuplicateToolName { .. }
            | Self::DuplicateToolProgram { .. }
            | Self::MismatchedTool { .. }
            | Self::UnusedTool { .. } => None,
        }
    }
}

/// Why tracked gate intent did not produce one walk policy.
#[derive(Debug)]
pub enum PolicyError {
    /// The shared tracked document is not valid current gate configuration.
    Toml {
        /// Fixed policy path.
        path: PathBuf,
        /// Typed TOML decoder refusal with source position.
        source: toml::de::Error,
    },
    /// A tracked policy field did not survive compilation into named rules.
    ///
    /// The tracked document is the only thing that decides rule policy, so this
    /// refusal always has one repository path to name and one field within it.
    Rules {
        /// Fixed policy path.
        path: PathBuf,
        /// Typed parse refusal naming the exact tracked field.
        source: crate::policy::ParseError,
    },
    /// A compiled default skip is not a current jurisdiction spelling.
    Selector {
        /// Fixed policy path.
        path: PathBuf,
        /// Exact field path.
        field: String,
        /// Jurisdiction refusal.
        source: IgnoreError,
    },
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Toml { path, source } => {
                write!(
                    formatter,
                    "{} is not valid gate configuration: {source}",
                    path.display()
                )
            }
            Self::Rules { path, source } => write!(
                formatter,
                "{} does not compile into current rule policy: {source}",
                path.display()
            ),
            Self::Selector {
                path,
                field,
                source,
            } => write!(formatter, "{} field {field}: {source}", path.display()),
        }
    }
}

impl Error for PolicyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Toml { source, .. } => Some(source),
            Self::Rules { source, .. } => Some(source),
            Self::Selector { source, .. } => Some(source),
        }
    }
}

/// Why one compiled tool could not be identified as operational.
#[derive(Debug)]
pub enum ToolError {
    /// The one compiled executable path was unreadable.
    Read {
        /// Compiled tool name.
        name: String,
        /// Compiled executable path.
        program: PathBuf,
        /// Filesystem refusal.
        source: io::Error,
    },
    /// The probe argv was not a SILL command.
    Spec {
        /// Compiled tool name.
        name: String,
        /// SILL refusal.
        source: SpecError,
    },
    /// The executable could not be started.
    Spawn {
        /// Compiled tool name.
        name: String,
        /// Operating-system refusal.
        source: io::Error,
    },
    /// The bounded probe could not be completed and reaped.
    Wait {
        /// Compiled tool name.
        name: String,
        /// Bounded-wait refusal.
        source: WaitError,
    },
    /// The probe ran but did not answer its compiled contract.
    NonOperational {
        /// Compiled tool name.
        name: String,
        /// Exact observed outcome.
        evidence: String,
    },
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read {
                name,
                program,
                source,
            } => write!(
                formatter,
                "declared tool {name} at {} is unreadable: {source}",
                program.display()
            ),
            Self::Spec { name, source } => write!(formatter, "declared tool {name}: {source}"),
            Self::Spawn { name, source } => {
                write!(formatter, "declared tool {name} could not start: {source}")
            }
            Self::Wait { name, source } => {
                write!(formatter, "declared tool {name} did not complete: {source}")
            }
            Self::NonOperational { name, evidence } => {
                write!(
                    formatter,
                    "declared tool {name} is not operational: {evidence}"
                )
            }
        }
    }
}

impl Error for ToolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Spawn { source, .. } => Some(source),
            Self::Spec { source, .. } => Some(source),
            Self::Wait { source, .. } => Some(source),
            Self::NonOperational { .. } => None,
        }
    }
}

/// Why no complete current execution contract was loaded.
#[derive(Debug)]
pub enum ContractError {
    /// The fixed tracked policy path was unreadable.
    Read {
        /// Exact path.
        path: PathBuf,
        /// Filesystem refusal.
        source: io::Error,
    },
    /// Tracked gate intent was refused.
    Policy(PolicyError),
    /// The compiled metrics and tool declarations did not form a closed set.
    GateSet(GateSetError),
    /// A compiled metric declaration was internally invalid.
    Metric(MetricError),
    /// Compiled process bounds were internally invalid.
    Bounds(SpecError),
    /// A compiled tool was absent or non-operational.
    Tool(ToolError),
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(
                    formatter,
                    "read tracked gate input {}: {source}",
                    path.display()
                )
            }
            Self::Policy(source) => write!(formatter, "gate policy: {source}"),
            Self::GateSet(source) => write!(formatter, "compiled gate set: {source}"),
            Self::Metric(source) => write!(formatter, "compiled gate set: {source}"),
            Self::Bounds(source) => write!(formatter, "compiled gate bounds: {source}"),
            Self::Tool(source) => write!(formatter, "gate tool preflight: {source}"),
        }
    }
}

impl Error for ContractError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Policy(source) => Some(source),
            Self::GateSet(source) => Some(source),
            Self::Metric(source) => Some(source),
            Self::Bounds(source) => Some(source),
            Self::Tool(source) => Some(source),
        }
    }
}
