use super::*;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ToolDeclaration {
    pub(super) name: String,
    pub(super) program: String,
    pub(super) probe_arguments: Vec<String>,
    pub(super) probe_output: String,
}

impl ToolDeclaration {
    fn awk() -> Self {
        Self {
            name: "awk".to_owned(),
            program: AWK.to_owned(),
            probe_arguments: vec![AWK_PROBE.to_owned()],
            probe_output: AWK_PROBE_OUTPUT.to_owned(),
        }
    }

    pub(super) fn render(&self, into: &mut String) {
        let _ = writeln!(into, "tool {} program {}", self.name, self.program);
        for argument in &self.probe_arguments {
            let _ = writeln!(into, "tool {} probe-argument {argument}", self.name);
        }
        let _ = writeln!(
            into,
            "tool {} probe-output {}",
            self.name, self.probe_output
        );
    }
}

/// One compiled Gate executable declaration without execution authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableDeclaration {
    name: String,
    program: String,
}

impl ExecutableDeclaration {
    fn new(name: impl Into<String>, program: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            program: program.into(),
        }
    }

    /// Stable compiled tool name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Compiled program: an absolute path is exact, and a bare name resolves
    /// on the running host's executable search path.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }
}

/// Return every compiled Gate policy executable without reading or starting it.
///
/// # Errors
///
/// Returns a specification error if this build's own declaration set is not
/// internally coherent.
pub fn declared_tools() -> Result<Vec<ExecutableDeclaration>, GateSetError> {
    GateSet::current().map(|gate_set| {
        gate_set
            .tools
            .into_iter()
            .map(|tool| ExecutableDeclaration::new(tool.name, tool.program))
            .collect()
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MetricDeclaration {
    tool: String,
    pub(super) metric: Metric,
}

impl MetricDeclaration {
    fn new(tool: &str, metric: Metric) -> Self {
        Self {
            tool: tool.to_owned(),
            metric,
        }
    }

    fn render(&self, into: &mut String) {
        let _ = writeln!(into, "metric {} tool {}", self.metric.name(), self.tool);
        self.metric.render(into);
    }

    fn render_camp(&self, into: &mut String) {
        let _ = writeln!(
            into,
            "camp-metric {} tool {}",
            self.metric.name(),
            self.tool
        );
        self.metric.render(into);
    }
}

const CAMP_BOUND: &str = "rust.gate-line-length";
const CAMP_LOCI: [(&str, &str); 4] = [
    ("rust.max-line.camp", "gate/src/camp.rs"),
    ("rust.max-line.input", "gate/src/input.rs"),
    ("rust.max-line.report", "gate/src/report.rs"),
    ("rust.max-line.verdict", "gate/src/verdict.rs"),
];

/// The compiled checks this build can execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateSet {
    pub(super) metrics: Vec<MetricDeclaration>,
    pub(super) camp_metrics: Vec<MetricDeclaration>,
    pub(super) camp: Camp,
    pub(super) tools: Vec<ToolDeclaration>,
}

impl GateSet {
    pub(super) fn current() -> Result<Self, GateSetError> {
        let camp_metrics = current_camp_metrics()?;
        let camp = Camp::new([SharedBound::new(
            CAMP_BOUND,
            LONG_LINE_LIMIT as u64,
            CAMP_LOCI.map(|(name, _)| name),
        )
        .map_err(GateSetError::Camp)?])
        .map_err(GateSetError::Camp)?;

        Self::admit(vec![], camp_metrics, camp, vec![ToolDeclaration::awk()])
    }

    pub(super) fn admit(
        mut metrics: Vec<MetricDeclaration>,
        mut camp_metrics: Vec<MetricDeclaration>,
        camp: Camp,
        mut tools: Vec<ToolDeclaration>,
    ) -> Result<Self, GateSetError> {
        metrics.sort_by(|left, right| left.metric.name().cmp(right.metric.name()));
        camp_metrics.sort_by(|left, right| left.metric.name().cmp(right.metric.name()));
        tools.sort_by(|left, right| left.name.cmp(&right.name));

        let mut metric_names = BTreeSet::new();
        for declaration in metrics.iter().chain(&camp_metrics) {
            if !metric_names.insert(declaration.metric.name().to_owned()) {
                return Err(GateSetError::DuplicateMetric {
                    metric: declaration.metric.name().to_owned(),
                });
            }
        }

        validate_camp_declarations(&camp_metrics, &camp)?;

        let mut names = BTreeSet::new();
        let mut programs = BTreeSet::new();
        for tool in &tools {
            if !names.insert(tool.name.clone()) {
                return Err(GateSetError::DuplicateToolName {
                    name: tool.name.clone(),
                });
            }
            if !programs.insert(tool.program.clone()) {
                return Err(GateSetError::DuplicateToolProgram {
                    program: tool.program.clone(),
                });
            }
        }

        let mut used = BTreeSet::new();
        for declaration in metrics.iter().chain(&camp_metrics) {
            let Some(tool) = tools.iter().find(|tool| tool.name == declaration.tool) else {
                return Err(GateSetError::MissingTool {
                    metric: declaration.metric.name().to_owned(),
                    tool: declaration.tool.clone(),
                    parser_program: declaration.metric.parser().program().to_owned(),
                });
            };
            let parser_program = declaration.metric.parser().program();
            if parser_program != tool.program {
                return Err(GateSetError::MismatchedTool {
                    metric: declaration.metric.name().to_owned(),
                    tool: tool.name.clone(),
                    parser_program: parser_program.to_owned(),
                    tool_program: tool.program.clone(),
                });
            }
            used.insert(tool.name.clone());
        }

        if let Some(tool) = tools.iter().find(|tool| !used.contains(&tool.name)) {
            return Err(GateSetError::UnusedTool {
                tool: tool.name.clone(),
                program: tool.program.clone(),
            });
        }

        Ok(Self {
            metrics,
            camp_metrics,
            camp,
            tools,
        })
    }

    /// Current compiled shared-bound profile.
    #[must_use]
    pub const fn camp(&self) -> &Camp {
        &self.camp
    }

    pub(crate) fn camp_metrics(&self) -> impl Iterator<Item = Metric> + '_ {
        self.camp_metrics
            .iter()
            .map(|declaration| declaration.metric.clone())
    }

    /// Canonical compiled declaration, including parser argv and tool probes.
    #[must_use]
    pub fn render(&self) -> String {
        let mut rendered = String::from("gate-set 1\n");
        let _ = writeln!(rendered, "parser-file-limit {}", Parser::FILE_LIMIT);
        for tool in &self.tools {
            tool.render(&mut rendered);
        }
        for declaration in &self.metrics {
            declaration.render(&mut rendered);
        }
        for bound in self.camp.bounds() {
            let _ = writeln!(rendered, "camp {} limit {}", bound.name(), bound.limit());
            for locus in bound.loci() {
                let _ = writeln!(rendered, "camp {} locus {locus}", bound.name());
            }
        }
        for declaration in &self.camp_metrics {
            declaration.render_camp(&mut rendered);
        }
        rendered
    }

    /// Identity of the complete compiled set.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::compute(Domain::GateSet, self.render().as_bytes())
    }
}

fn current_camp_metrics() -> Result<Vec<MetricDeclaration>, GateSetError> {
    CAMP_LOCI
        .iter()
        .map(|(name, path)| {
            let parser = Parser::new(
                AWK,
                ["{ if (length($0) > maximum) maximum = length($0) } END { print maximum + 0 }"],
            )
            .map_err(GateSetError::Metric)?;
            let metric = Metric::new(
                *name,
                "rust",
                "maximum source-line length in one gate implementation locus",
                parser,
                path,
                "",
            )
            .map_err(GateSetError::Metric)?;
            Ok(MetricDeclaration::new("awk", metric))
        })
        .collect()
}

fn validate_camp_declarations(
    metrics: &[MetricDeclaration],
    camp: &Camp,
) -> Result<(), GateSetError> {
    if camp.bounds().is_empty() {
        return Err(GateSetError::EmptyCamp);
    }
    let declared: Vec<String> = camp
        .bounds()
        .iter()
        .flat_map(|bound| bound.loci().iter().cloned())
        .collect();
    let measured: Vec<String> = metrics
        .iter()
        .map(|declaration| declaration.metric.name().to_owned())
        .collect();
    if declared != measured {
        return Err(GateSetError::CampLoci { declared, measured });
    }
    Ok(())
}
