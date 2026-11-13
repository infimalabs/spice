use super::*;

/// One completed ordinary measurement copied out of a current run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportMeasurement {
    pub(super) metric: String,
    pub(super) value: u64,
    pub(super) files: Vec<String>,
}

impl ReportMeasurement {
    fn current(measurement: &Measurement) -> Self {
        Self {
            metric: measurement.metric().to_owned(),
            value: measurement.value(),
            files: measurement.files().to_vec(),
        }
    }

    /// Stable metric name.
    #[must_use]
    pub fn metric(&self) -> &str {
        &self.metric
    }

    /// Completed value.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.value
    }

    /// Exact files behind the completed value.
    #[must_use]
    pub fn files(&self) -> &[String] {
        &self.files
    }

    pub(super) fn measurement(&self) -> Measurement {
        Measurement::new(&self.metric, self.value, self.files.iter().cloned())
    }
}

/// One lower-is-better floor in an ordinary gate tightening proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportFloor {
    pub(super) metric: String,
    pub(super) value: u64,
}

impl ReportFloor {
    /// Stable metric name.
    #[must_use]
    pub fn metric(&self) -> &str {
        &self.metric
    }

    /// Proposed lower-or-equal value.
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.value
    }
}

/// One strict, versioned ordinary gate result body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateReport {
    pub(super) version: u64,
    pub(super) standing: Standing,
    pub(super) findings: Vec<Finding>,
    pub(super) long_lines: Vec<LongLineFinding>,
    pub(super) measurements: Vec<ReportMeasurement>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub(super) tightening: Option<Vec<ReportFloor>>,
    pub(super) verdict: Verdict,
}

impl GateReport {
    /// Copy one completed contract-bound run into a report-only value.
    #[must_use]
    pub fn current(run: &CurrentRun) -> Self {
        let standing = run.receipt().standing();
        let findings = run.receipt().findings().to_vec();
        let long_lines = run.receipt().long_lines().to_vec();
        let tightening = gate_tightening(standing, &findings, &long_lines)
            .expect("a current receipt carries a standing derived from its findings");
        Self {
            version: GATE_REPORT_VERSION,
            standing,
            findings,
            long_lines,
            measurements: run
                .measurements()
                .iter()
                .map(ReportMeasurement::current)
                .collect(),
            tightening,
            verdict: run.receipt().clone(),
        }
    }

    /// Current ordinary report schema.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Standing duplicated from the complete verdict.
    #[must_use]
    pub const fn standing(&self) -> Standing {
        self.standing
    }

    /// Ordered findings duplicated from the complete verdict.
    #[must_use]
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Newly introduced over-limit Rust lines in path and locus order.
    #[must_use]
    pub fn long_lines(&self) -> &[LongLineFinding] {
        &self.long_lines
    }

    /// Every completed measurement in metric order.
    #[must_use]
    pub fn measurements(&self) -> &[ReportMeasurement] {
        &self.measurements
    }

    /// Shrink-only proposal, absent for every refused run.
    #[must_use]
    pub fn tightening(&self) -> Option<&[ReportFloor]> {
        self.tightening.as_deref()
    }

    /// Complete current gate receipt.
    #[must_use]
    pub const fn verdict(&self) -> &Verdict {
        &self.verdict
    }

    /// Deterministic canonical text carrying the complete report.
    #[must_use]
    pub fn render(&self) -> String {
        let mut rendered = format!(
            "gate-report {}\nstanding {}\n",
            self.version,
            self.standing.as_str()
        );
        for finding in &self.findings {
            let _ = writeln!(rendered, "finding {} {}", finding.metric, finding.movement);
        }
        for finding in &self.long_lines {
            let _ = writeln!(
                rendered,
                "finding rust.long-lines {}:{} bytes {} file-debt {}->{}",
                quoted(finding.path()),
                finding.line(),
                finding.bytes(),
                finding.baseline_debt(),
                finding.current_debt(),
            );
        }
        for measurement in &self.measurements {
            let _ = writeln!(
                rendered,
                "measurement {} {}",
                measurement.metric, measurement.value
            );
            for file in &measurement.files {
                let _ = writeln!(
                    rendered,
                    "measurement-file {} {}",
                    measurement.metric,
                    quoted(file)
                );
            }
        }
        match &self.tightening {
            None => rendered.push_str("tightening none\n"),
            Some(floors) => {
                rendered.push_str("tightening proposed\n");
                for floor in floors {
                    let _ = writeln!(rendered, "floor {} {}", floor.metric, floor.value);
                }
            }
        }
        for line in self.verdict.render().lines() {
            let _ = writeln!(rendered, "verdict {line}");
        }
        rendered
    }

    /// Canonical JSON representation.
    ///
    /// # Errors
    ///
    /// Returns the canonical encoder failure.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        wire::encode(self)
    }

    /// Decode only the current, canonical, internally coherent report.
    ///
    /// # Errors
    ///
    /// Returns malformed fields, a named foreign version, a second JSON
    /// spelling, or the duplicated evidence that disagreed.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReportDecodeError> {
        let version: ReportVersionEvidence =
            wire::decode(bytes).map_err(ReportDecodeError::Json)?;
        if version.version != GATE_REPORT_VERSION {
            return Err(ReportDecodeError::unsupported(
                "gate report",
                version.version,
                GATE_REPORT_VERSION,
            ));
        }
        let report: Self = wire::decode(bytes).map_err(ReportDecodeError::Json)?;
        report.validate()?;
        let canonical = report.encode().map_err(ReportDecodeError::Json)?;
        if canonical != bytes {
            return Err(ReportDecodeError::NonCanonical { canonical });
        }
        Ok(report)
    }

    fn validate(&self) -> Result<(), ReportDecodeError> {
        if self.standing != self.verdict.standing() {
            return Err(ReportDecodeError::inconsistent(
                "gate report",
                "standing disagrees with the complete verdict",
            ));
        }
        if self.findings != self.verdict.findings() {
            return Err(ReportDecodeError::inconsistent(
                "gate report",
                "ordered findings disagree with the complete verdict",
            ));
        }
        if self.long_lines != self.verdict.long_lines() {
            return Err(ReportDecodeError::inconsistent(
                "gate report",
                "ordered rust.long-lines findings disagree with the complete verdict",
            ));
        }
        validate_long_lines(&self.long_lines)
            .map_err(|evidence| ReportDecodeError::inconsistent("gate report", evidence))?;
        require_strict_names(
            "gate report",
            "measurement metrics",
            self.measurements
                .iter()
                .map(|measurement| &measurement.metric),
        )?;
        require_strict_names(
            "gate report",
            "finding metrics",
            self.findings.iter().map(|finding| &finding.metric),
        )?;
        validate_gate_findings(&self.findings, &self.measurements)
            .map_err(|evidence| ReportDecodeError::inconsistent("gate report", evidence))?;
        let measurements: Vec<Measurement> = self
            .measurements
            .iter()
            .map(ReportMeasurement::measurement)
            .collect();
        if run_evidence_digest(&measurements, self.verdict.head(), &self.long_lines)
            != *self.verdict.measurement()
        {
            return Err(ReportDecodeError::inconsistent(
                "gate report",
                "measurements disagree with the verdict measurement identity",
            ));
        }
        let tightening = gate_tightening(self.standing, &self.findings, &self.long_lines)
            .map_err(|evidence| ReportDecodeError::inconsistent("gate report", evidence))?;
        if self.tightening != tightening {
            return Err(ReportDecodeError::inconsistent(
                "gate report",
                "tightening is not the shrink-only proposal derived from the findings",
            ));
        }
        Ok(())
    }
}

impl CurrentRun {
    /// Copy this completed current run into a report-only crossing value.
    #[must_use]
    pub fn report(&self) -> GateReport {
        GateReport::current(self)
    }
}

fn validate_gate_findings(
    findings: &[Finding],
    measurements: &[ReportMeasurement],
) -> Result<(), &'static str> {
    let mut measurements = measurements.iter().peekable();
    for finding in findings {
        if measurements
            .peek()
            .is_some_and(|measurement| measurement.metric.as_str() < finding.metric.as_str())
        {
            return Err("a completed measurement has no finding");
        }
        let matching = measurements
            .peek()
            .is_some_and(|measurement| measurement.metric.as_str() == finding.metric.as_str());
        if matches!(finding.movement, Movement::Retired { .. }) {
            if matching {
                return Err("a retired finding still has a completed measurement");
            }
            continue;
        }
        if !matching {
            return Err("a non-retired finding has no completed measurement");
        }
        let measured = measurements
            .next()
            .expect("a matching completed measurement was observed")
            .value;
        let agrees = match finding.movement {
            Movement::Held { baseline } => measured == baseline,
            Movement::Shrank {
                baseline,
                measured: observed,
            } => observed == measured && observed < baseline,
            Movement::Regressed {
                baseline,
                measured: observed,
            } => observed == measured && observed > baseline,
            Movement::Unbaselined { measured: observed } => observed == measured,
            Movement::Retired { .. } => unreachable!("retired findings were handled above"),
        };
        if !agrees {
            return Err("a finding movement disagrees with its completed measurement");
        }
    }
    if measurements.next().is_some() {
        return Err("a completed measurement has no finding");
    }
    Ok(())
}

fn gate_tightening(
    standing: Standing,
    findings: &[Finding],
    long_lines: &[LongLineFinding],
) -> Result<Option<Vec<ReportFloor>>, &'static str> {
    let derived =
        if long_lines.is_empty() && findings.iter().all(|finding| finding.movement.passes()) {
            Standing::Clean
        } else {
            Standing::Refused
        };
    if standing != derived {
        return Err("standing is not derived from every ordered finding");
    }
    if standing == Standing::Refused {
        return Ok(None);
    }
    let floors = findings
        .iter()
        .map(|finding| {
            let value = match finding.movement {
                Movement::Held { baseline } => baseline,
                Movement::Shrank { measured, .. } | Movement::Unbaselined { measured } => measured,
                Movement::Regressed { .. } | Movement::Retired { .. } => {
                    unreachable!("a clean standing excludes a failing finding")
                }
            };
            ReportFloor {
                metric: finding.metric.clone(),
                value,
            }
        })
        .collect();
    Ok(Some(floors))
}
