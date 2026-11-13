//! Pure detection of measurements camping around one shared fixed bound.
//!
//! A shared bound is not an own-history floor. It groups several independently
//! measured loci under one compiled limit, while a ratchet compares one metric
//! only with its own earlier value. Keeping the two inputs as different types
//! prevents this audit from moving a floor or turning a retained report into
//! execution policy.

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::input::Contract;
use crate::measure::Measurement;
use crate::ratchet::Ratchet;
use crate::report::{ReportDecodeError, ReportVersionEvidence};
use crate::verdict::{Run, RunError};

/// The one camping report schema this build emits and admits.
pub const CAMP_REPORT_VERSION: u64 = 1;
const MINIMUM_POPULATION: usize = 4;
const MINIMUM_CLUSTER: u128 = 3;
const NEAR_DENOMINATOR: u128 = 20;
const CLUSTER_NUMERATOR: u128 = 3;
const CLUSTER_DENOMINATOR: u128 = 4;

/// One compiled fixed bound shared by several stable metric loci.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedBound {
    name: String,
    limit: u64,
    loci: Vec<String>,
}

impl SharedBound {
    /// Declare a shared fixed bound.
    ///
    /// Loci are sorted at admission so audit output is independent of caller
    /// order. Four loci are the smallest admitted population: a 75-percent
    /// cluster then requires three independent observations.
    ///
    /// # Errors
    ///
    /// Refuses an unstable name, a zero limit, fewer than four loci, or an
    /// unstable or duplicate locus.
    pub fn new(
        name: impl Into<String>,
        limit: u64,
        loci: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, CampError> {
        let name = name.into();
        if !stable_name(&name) {
            return Err(CampError::InvalidBoundName { bound: name });
        }
        if limit == 0 {
            return Err(CampError::ZeroLimit { bound: name });
        }

        let mut loci: Vec<String> = loci.into_iter().map(Into::into).collect();
        if loci.len() < MINIMUM_POPULATION {
            return Err(CampError::TooFewLoci {
                bound: name,
                population: loci.len(),
            });
        }
        if let Some(locus) = loci.iter().find(|locus| !stable_name(locus)) {
            return Err(CampError::InvalidLocus {
                bound: name,
                locus: locus.clone(),
            });
        }
        loci.sort();
        if let Some(pair) = loci.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(CampError::DuplicateLocus {
                bound: name,
                locus: pair[0].clone(),
            });
        }

        Ok(Self { name, limit, loci })
    }

    /// Stable identity of this bound.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Fixed value all member loci share.
    #[must_use]
    pub const fn limit(&self) -> u64 {
        self.limit
    }

    /// Stable measurement names in audit order.
    #[must_use]
    pub fn loci(&self) -> &[String] {
        &self.loci
    }
}

/// One validated collection of shared-bound distributions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Camp {
    bounds: Vec<SharedBound>,
}

impl Camp {
    /// Admit the compiled shared-bound declarations.
    ///
    /// # Errors
    ///
    /// Refuses duplicate bound names or a metric locus assigned to more than
    /// one shared distribution.
    pub fn new(bounds: impl IntoIterator<Item = SharedBound>) -> Result<Self, CampError> {
        let mut bounds: Vec<SharedBound> = bounds.into_iter().collect();
        bounds.sort_by(|left, right| left.name.cmp(&right.name));
        if let Some(pair) = bounds.windows(2).find(|pair| pair[0].name == pair[1].name) {
            return Err(CampError::DuplicateBound {
                bound: pair[0].name.clone(),
            });
        }

        let mut owners: BTreeMap<&str, &str> = BTreeMap::new();
        for bound in &bounds {
            for locus in &bound.loci {
                if let Some(first) = owners.insert(locus, &bound.name) {
                    return Err(CampError::LocusShared {
                        locus: locus.clone(),
                        first: first.to_owned(),
                        second: bound.name.clone(),
                    });
                }
            }
        }
        Ok(Self { bounds })
    }

    /// Declarations in stable bound-name order.
    #[must_use]
    pub fn bounds(&self) -> &[SharedBound] {
        &self.bounds
    }

    /// Audit completed measurements without changing any floor.
    ///
    /// A locus is near when its absolute distance from the bound is no more
    /// than five percent. A distribution camps only when at least three loci
    /// are near and near loci form at least 75 percent of its population. All
    /// comparisons use exact integer cross-products.
    ///
    /// # Errors
    ///
    /// Refuses duplicate measurement names or a declared locus with no
    /// completed measurement.
    pub fn audit(&self, measurements: &[Measurement]) -> Result<CampReport, CampError> {
        let mut observed: BTreeMap<&str, &Measurement> = BTreeMap::new();
        for measurement in measurements {
            if observed.insert(measurement.metric(), measurement).is_some() {
                return Err(CampError::DuplicateObservation {
                    locus: measurement.metric().to_owned(),
                });
            }
        }

        let mut distributions = Vec::with_capacity(self.bounds.len());
        let mut findings = Vec::new();
        for bound in &self.bounds {
            let mut near = Vec::new();
            for locus in &bound.loci {
                let measurement =
                    observed
                        .get(locus.as_str())
                        .ok_or_else(|| CampError::MissingObservation {
                            bound: bound.name.clone(),
                            locus: locus.clone(),
                        })?;
                let distance = LocusDistance::new(locus.clone(), measurement.value(), bound.limit);
                if distance.is_near() {
                    near.push(distance);
                }
            }

            let population = u64::try_from(bound.loci.len())
                .expect("a Rust collection length fits in u64 on supported targets");
            let near_count = u64::try_from(near.len())
                .expect("a Rust collection length fits in u64 on supported targets");
            let camped = u128::from(near_count) >= MINIMUM_CLUSTER
                && u128::from(near_count) * CLUSTER_DENOMINATOR
                    >= u128::from(population) * CLUSTER_NUMERATOR;
            let standing = if camped {
                CampStanding::Camped
            } else {
                CampStanding::Clean
            };
            distributions.push(DistributionReport {
                bound: bound.name.clone(),
                limit: bound.limit,
                population,
                near: near_count,
                standing,
            });
            if camped {
                findings.push(CampFinding {
                    bound: bound.name.clone(),
                    limit: bound.limit,
                    population,
                    loci: near,
                });
            }
        }

        let standing = if findings.is_empty() {
            CampStanding::Clean
        } else {
            CampStanding::Camped
        };
        Ok(CampReport {
            version: CAMP_REPORT_VERSION,
            standing,
            distributions,
            findings,
        })
    }
}

/// Whether camping was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CampStanding {
    /// No declared distribution met the clustering rule.
    Clean,
    /// At least one declared distribution met the clustering rule.
    Camped,
}

impl fmt::Display for CampStanding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clean => formatter.write_str("clean"),
            Self::Camped => formatter.write_str("camped"),
        }
    }
}

/// One complete, explicit camping audit result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampReport {
    version: u64,
    /// Standing over every declared distribution.
    pub standing: CampStanding,
    /// Every evaluated distribution, including clean ones.
    pub distributions: Vec<DistributionReport>,
    /// Camping findings in bound-name order.
    pub findings: Vec<CampFinding>,
}

impl CampReport {
    /// Current camping report schema.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Canonical text representation.
    #[must_use]
    pub fn render(&self) -> String {
        let mut rendered = format!("camp-report {}\nstanding {}\n", self.version, self.standing);
        for distribution in &self.distributions {
            let _ = writeln!(
                rendered,
                "bound {} limit {} population {} near {} standing {}",
                distribution.bound,
                distribution.limit,
                distribution.population,
                distribution.near,
                distribution.standing
            );
        }
        for finding in &self.findings {
            for locus in &finding.loci {
                let _ = writeln!(
                    rendered,
                    "locus {} {} measured {} relation {} distance {}/{}",
                    finding.bound,
                    locus.locus,
                    locus.measured,
                    locus.relation,
                    locus.delta,
                    locus.denominator
                );
            }
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
    /// spelling, or the camping evidence that disagreed.
    pub fn decode(bytes: &[u8]) -> Result<Self, ReportDecodeError> {
        let version: ReportVersionEvidence =
            wire::decode(bytes).map_err(ReportDecodeError::Json)?;
        if version.version != CAMP_REPORT_VERSION {
            return Err(ReportDecodeError::unsupported(
                "camp report",
                version.version,
                CAMP_REPORT_VERSION,
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
        if self
            .distributions
            .windows(2)
            .any(|pair| pair[0].bound >= pair[1].bound)
            || self
                .findings
                .windows(2)
                .any(|pair| pair[0].bound >= pair[1].bound)
        {
            return Err(camp_inconsistent(
                "distribution and finding names must be sorted and unique",
            ));
        }
        for distribution in &self.distributions {
            if !stable_name(&distribution.bound)
                || distribution.limit == 0
                || distribution.population < MINIMUM_POPULATION as u64
                || distribution.near > distribution.population
            {
                return Err(camp_inconsistent(
                    "a distribution has an invalid name, limit, population, or near count",
                ));
            }
            let camped = u128::from(distribution.near) >= MINIMUM_CLUSTER
                && u128::from(distribution.near) * CLUSTER_DENOMINATOR
                    >= u128::from(distribution.population) * CLUSTER_NUMERATOR;
            let standing = if camped {
                CampStanding::Camped
            } else {
                CampStanding::Clean
            };
            if distribution.standing != standing {
                return Err(camp_inconsistent(
                    "distribution standing disagrees with its exact cluster counts",
                ));
            }
        }
        let camped: Vec<&DistributionReport> = self
            .distributions
            .iter()
            .filter(|distribution| distribution.standing == CampStanding::Camped)
            .collect();
        let standing = if camped.is_empty() {
            CampStanding::Clean
        } else {
            CampStanding::Camped
        };
        if self.standing != standing || camped.len() != self.findings.len() {
            return Err(camp_inconsistent(
                "overall standing or finding count disagrees with distributions",
            ));
        }
        for (distribution, finding) in camped.into_iter().zip(&self.findings) {
            if !stable_name(&finding.bound)
                || finding.bound != distribution.bound
                || finding.limit != distribution.limit
                || finding.population != distribution.population
                || u64::try_from(finding.loci.len()).ok() != Some(distribution.near)
            {
                return Err(camp_inconsistent(
                    "a finding disagrees with its camped distribution",
                ));
            }
            if finding
                .loci
                .windows(2)
                .any(|pair| pair[0].locus >= pair[1].locus)
            {
                return Err(camp_inconsistent("finding loci must be sorted and unique"));
            }
            for locus in &finding.loci {
                if !stable_name(&locus.locus) {
                    return Err(camp_inconsistent("a locus has an unstable name"));
                }
                let expected =
                    LocusDistance::new(locus.locus.clone(), locus.measured, distribution.limit);
                if locus != &expected || !locus.is_near() {
                    return Err(camp_inconsistent(
                        "a locus does not carry the exact near-bound distance",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn camp_inconsistent(evidence: &str) -> ReportDecodeError {
    ReportDecodeError::inconsistent("camp report", evidence)
}

/// Summary of one evaluated shared-bound distribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionReport {
    /// Stable bound identity.
    pub bound: String,
    /// Fixed shared limit.
    pub limit: u64,
    /// Declared locus count.
    pub population: u64,
    /// Loci within the inclusive five-percent band.
    pub near: u64,
    /// Standing of this distribution.
    pub standing: CampStanding,
}

/// One shared bound whose observations clustered around its limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CampFinding {
    /// Stable bound identity.
    pub bound: String,
    /// Fixed shared limit.
    pub limit: u64,
    /// Declared locus count.
    pub population: u64,
    /// Every near locus in metric-name order.
    pub loci: Vec<LocusDistance>,
}

/// Which side of a fixed bound one observation occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoundRelation {
    /// Measurement is below the bound.
    Below,
    /// Measurement equals the bound.
    At,
    /// Measurement is above the bound.
    Above,
}

impl fmt::Display for BoundRelation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Below => formatter.write_str("below"),
            Self::At => formatter.write_str("at"),
            Self::Above => formatter.write_str("above"),
        }
    }
}

/// Exact dimensionless distance of one clustered measurement from its bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocusDistance {
    /// Stable metric name used as the locus.
    pub locus: String,
    /// Completed measurement value.
    pub measured: u64,
    /// Which side of the bound the value occupies.
    pub relation: BoundRelation,
    /// Absolute numerator of the distance ratio.
    pub delta: u64,
    /// Shared bound used as the ratio denominator.
    pub denominator: u64,
}

impl LocusDistance {
    fn new(locus: String, measured: u64, limit: u64) -> Self {
        let relation = match measured.cmp(&limit) {
            std::cmp::Ordering::Less => BoundRelation::Below,
            std::cmp::Ordering::Equal => BoundRelation::At,
            std::cmp::Ordering::Greater => BoundRelation::Above,
        };
        Self {
            locus,
            measured,
            relation,
            delta: measured.abs_diff(limit),
            denominator: limit,
        }
    }

    fn is_near(&self) -> bool {
        u128::from(self.delta) * NEAR_DENOMINATOR <= u128::from(self.denominator)
    }
}

/// Why a shared-bound audit could not be constructed or completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CampError {
    /// A bound name is empty or carries whitespace.
    InvalidBoundName {
        /// Refused name.
        bound: String,
    },
    /// A fixed bound is zero, so normalized distance is undefined.
    ZeroLimit {
        /// Bound whose limit was zero.
        bound: String,
    },
    /// A distribution does not have four independent loci.
    TooFewLoci {
        /// Bound with the undersized population.
        bound: String,
        /// Population supplied.
        population: usize,
    },
    /// A locus name is empty or carries whitespace.
    InvalidLocus {
        /// Bound declaring the locus.
        bound: String,
        /// Refused locus.
        locus: String,
    },
    /// A bound declares one locus more than once.
    DuplicateLocus {
        /// Bound declaring the locus.
        bound: String,
        /// Repeated locus.
        locus: String,
    },
    /// Two declarations use the same bound name.
    DuplicateBound {
        /// Repeated bound.
        bound: String,
    },
    /// One locus was assigned to two shared distributions.
    LocusShared {
        /// Reused locus.
        locus: String,
        /// First owning bound.
        first: String,
        /// Second owning bound.
        second: String,
    },
    /// Completed measurements repeat one metric name.
    DuplicateObservation {
        /// Repeated metric locus.
        locus: String,
    },
    /// A declared locus has no completed measurement.
    MissingObservation {
        /// Bound requiring the locus.
        bound: String,
        /// Missing metric locus.
        locus: String,
    },
}

impl fmt::Display for CampError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBoundName { bound } => {
                write!(
                    formatter,
                    "shared bound name {bound:?} is not one stable word"
                )
            }
            Self::ZeroLimit { bound } => {
                write!(formatter, "shared bound {bound:?} has zero limit")
            }
            Self::TooFewLoci { bound, population } => write!(
                formatter,
                "shared bound {bound:?} has {population} loci; at least four are required"
            ),
            Self::InvalidLocus { bound, locus } => write!(
                formatter,
                "shared bound {bound:?} locus {locus:?} is not one stable word"
            ),
            Self::DuplicateLocus { bound, locus } => {
                write!(formatter, "shared bound {bound:?} repeats locus {locus:?}")
            }
            Self::DuplicateBound { bound } => {
                write!(formatter, "shared bound {bound:?} is declared twice")
            }
            Self::LocusShared {
                locus,
                first,
                second,
            } => write!(
                formatter,
                "locus {locus:?} belongs to both shared bounds {first:?} and {second:?}"
            ),
            Self::DuplicateObservation { locus } => {
                write!(formatter, "completed measurements repeat locus {locus:?}")
            }
            Self::MissingObservation { bound, locus } => write!(
                formatter,
                "shared bound {bound:?} has no completed measurement for locus {locus:?}"
            ),
        }
    }
}

impl std::error::Error for CampError {}

/// One live observation from a shared-bound measurement campaign.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CampProgress<'a> {
    /// Metric currently being measured, absent before measurement starts.
    pub metric: Option<&'a str>,
    /// Measurements that completed and passed snapshot verification.
    pub completed: usize,
    /// Metrics in the compiled camping policy.
    pub total: usize,
    /// Shared-bound distributions the completed measurements will audit.
    pub distributions: usize,
    /// Declared loci across those distributions.
    pub loci: usize,
}

/// Why the compiled Camp profile did not produce a completed audit.
#[derive(Debug)]
pub enum CampRunError {
    /// Snapshot-safe measurement did not complete.
    Run(RunError),
    /// Completed measurements did not satisfy the compiled declaration.
    Audit(CampError),
}

impl fmt::Display for CampRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Run(source) => write!(formatter, "camp measurement failed: {source}"),
            Self::Audit(source) => write!(formatter, "camp audit failed: {source}"),
        }
    }
}

impl std::error::Error for CampRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Run(source) => Some(source),
            Self::Audit(source) => Some(source),
        }
    }
}

impl Contract {
    /// Measure and audit this contract's compiled shared-bound profile while
    /// observing canonical campaign progress.
    ///
    /// This uses the same snapshot-verified walk as an ordinary run and an
    /// empty ratchet because a shared fixed bound is not own-history state.
    ///
    /// # Errors
    ///
    /// Returns the measurement or declaration failure. A partial measurement
    /// never reaches the audit.
    pub fn audit_camp_with_progress(
        &self,
        root: &std::path::Path,
        progress: &mut dyn FnMut(CampProgress<'_>),
    ) -> Result<CampReport, CampRunError> {
        let distributions = self.camp().bounds().len();
        let loci = self
            .camp()
            .bounds()
            .iter()
            .map(|bound| bound.loci().len())
            .sum();
        let total = self.camp_policy().metrics().len();
        progress(CampProgress {
            metric: None,
            completed: 0,
            total,
            distributions,
            loci,
        });
        let run = Run::adjudicate_with_progress(
            self.camp_policy(),
            root,
            &Ratchet::default(),
            self.bounds(),
            &mut |metric, completed, total| {
                progress(CampProgress {
                    metric,
                    completed,
                    total,
                    distributions,
                    loci,
                });
            },
        )
        .map_err(CampRunError::Run)?;
        self.camp()
            .audit(run.measurements())
            .map_err(CampRunError::Audit)
    }
}

fn stable_name(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use wire::{Outcome, Reply, ReplyPosition};

    use super::*;

    #[test]
    fn exact_five_percent_and_three_of_four_camp_with_signed_evidence() {
        let report = camp([bound("shared", 100, ["a", "b", "c", "d"])])
            .audit(&measurements([
                ("a", 95),
                ("b", 100),
                ("c", 105),
                ("d", 106),
            ]))
            .expect("complete audit");

        assert_eq!(report.standing, CampStanding::Camped);
        assert_eq!(report.distributions[0].near, 3);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(
            report.findings[0]
                .loci
                .iter()
                .map(|locus| (
                    &*locus.locus,
                    locus.relation,
                    locus.delta,
                    locus.denominator
                ))
                .collect::<Vec<_>>(),
            [
                ("a", BoundRelation::Below, 5, 100),
                ("b", BoundRelation::At, 0, 100),
                ("c", BoundRelation::Above, 5, 100),
            ]
        );
    }

    #[test]
    fn fewer_than_four_loci_refuses_and_two_of_four_is_explicitly_clean() {
        let refused = SharedBound::new("small", 100, ["a", "b", "c"])
            .expect_err("three loci are not a distribution");
        assert!(matches!(
            refused,
            CampError::TooFewLoci { population: 3, .. }
        ));

        let report = camp([bound("shared", 100, ["a", "b", "c", "d"])])
            .audit(&measurements([
                ("a", 95),
                ("b", 105),
                ("c", 94),
                ("d", 106),
            ]))
            .expect("complete clean audit");
        assert_eq!(report.standing, CampStanding::Clean);
        assert_eq!(report.distributions[0].standing, CampStanding::Clean);
        assert_eq!(report.distributions[0].near, 2);
        assert!(report.findings.is_empty());
        assert!(
            report
                .render()
                .starts_with("camp-report 1\nstanding clean\n")
        );
    }

    #[test]
    fn common_scaling_cannot_move_the_camping_decision() {
        let small = camp([bound("shared", 100, ["a", "b", "c", "d"])])
            .audit(&measurements([
                ("a", 95),
                ("b", 100),
                ("c", 105),
                ("d", 106),
            ]))
            .expect("small audit");
        let large = camp([bound("shared", 10_000, ["a", "b", "c", "d"])])
            .audit(&measurements([
                ("a", 9_500),
                ("b", 10_000),
                ("c", 10_500),
                ("d", 10_600),
            ]))
            .expect("scaled audit");

        assert_eq!(small.standing, large.standing);
        assert_eq!(small.distributions[0].near, large.distributions[0].near);
    }

    #[test]
    fn declaration_and_observation_order_cannot_move_report_bytes() {
        let first = camp([
            bound("zeta", 200, ["h", "g", "f", "e"]),
            bound("alpha", 100, ["d", "c", "b", "a"]),
        ])
        .audit(&measurements([
            ("h", 200),
            ("g", 200),
            ("f", 200),
            ("e", 220),
            ("d", 106),
            ("c", 105),
            ("b", 100),
            ("a", 95),
        ]))
        .expect("first audit");
        let second = camp([
            bound("alpha", 100, ["a", "b", "c", "d"]),
            bound("zeta", 200, ["e", "f", "g", "h"]),
        ])
        .audit(&measurements([
            ("a", 95),
            ("b", 100),
            ("c", 105),
            ("d", 106),
            ("e", 220),
            ("f", 200),
            ("g", 200),
            ("h", 200),
        ]))
        .expect("second audit");

        assert_eq!(first, second);
        assert_eq!(first.render(), second.render());
        assert_eq!(
            first.encode().expect("first JSON"),
            second.encode().expect("second JSON")
        );
    }

    #[test]
    fn malformed_declarations_and_incomplete_observations_refuse_by_name() {
        assert!(matches!(
            SharedBound::new("", 100, ["a", "b", "c", "d"]),
            Err(CampError::InvalidBoundName { .. })
        ));
        assert!(matches!(
            SharedBound::new("zero", 0, ["a", "b", "c", "d"]),
            Err(CampError::ZeroLimit { .. })
        ));
        assert!(matches!(
            SharedBound::new("duplicate", 100, ["a", "a", "b", "c"]),
            Err(CampError::DuplicateLocus { .. })
        ));
        assert!(matches!(
            SharedBound::new("unstable", 100, ["a", "bad locus", "c", "d"]),
            Err(CampError::InvalidLocus { locus, .. }) if locus == "bad locus"
        ));
        assert!(matches!(
            Camp::new([
                bound("same", 100, ["a", "b", "c", "d"]),
                bound("same", 200, ["e", "f", "g", "h"]),
            ]),
            Err(CampError::DuplicateBound { bound }) if bound == "same"
        ));
        assert!(matches!(
            Camp::new([
                bound("first", 100, ["a", "b", "c", "d"]),
                bound("second", 200, ["d", "e", "f", "g"]),
            ]),
            Err(CampError::LocusShared { locus, .. }) if locus == "d"
        ));

        let audit = camp([bound("shared", 100, ["a", "b", "c", "d"])]);
        assert!(matches!(
            audit.audit(&measurements([("a", 100), ("a", 100), ("c", 100), ("d", 100)])),
            Err(CampError::DuplicateObservation { locus }) if locus == "a"
        ));
        assert!(matches!(
            audit.audit(&measurements([("a", 100), ("b", 100), ("c", 100)])),
            Err(CampError::MissingObservation { locus, .. }) if locus == "d"
        ));
    }

    #[test]
    fn report_json_round_trips_and_unknown_fields_refuse() {
        let report = camp([bound("shared", 100, ["a", "b", "c", "d"])])
            .audit(&measurements([
                ("a", 95),
                ("b", 100),
                ("c", 105),
                ("d", 106),
            ]))
            .expect("complete audit");
        let encoded = report.encode().expect("encode report");
        let decoded = CampReport::decode(&encoded).expect("decode report");
        assert_eq!(decoded, report);
        let outcome = Outcome::Reply(Reply {
            position: ReplyPosition::Direct {},
            body: report.clone(),
        });
        assert_eq!(
            wire::encode(&outcome).expect("encode direct camp body"),
            wire::encode(&outcome).expect("encode direct camp body again")
        );

        let unknown =
            String::from_utf8(encoded)
                .expect("JSON text")
                .replacen("{", "{\"unknown\":true,", 1);
        CampReport::decode(unknown.as_bytes()).expect_err("unknown report fields must refuse");

        let nested_unknown = String::from_utf8(report.encode().expect("encode nested unknown"))
            .expect("report JSON")
            .replacen("\"limit\":100", "\"limit\":100,\"unknown\":true", 1);
        CampReport::decode(nested_unknown.as_bytes())
            .expect_err("unknown distribution fields must refuse");

        let mut alternate = vec![b' '];
        alternate.extend_from_slice(&report.encode().expect("encode report again"));
        let refused = CampReport::decode(&alternate).expect_err("second spelling refuses");
        assert!(refused.canonical().is_some());

        let foreign = String::from_utf8(report.encode().expect("encode report for version"))
            .expect("report JSON")
            .replacen("\"version\":1", "\"version\":77", 1);
        let refused = CampReport::decode(foreign.as_bytes()).expect_err("foreign version refuses");
        assert!(matches!(
            refused,
            crate::report::ReportDecodeError::UnsupportedVersion { observed: 77, .. }
        ));
    }

    #[test]
    fn every_camp_report_field_moves_its_canonical_document() {
        let report = camp([bound("shared", 100, ["a", "b", "c", "d"])])
            .audit(&measurements([
                ("a", 95),
                ("b", 100),
                ("c", 105),
                ("d", 106),
            ]))
            .expect("complete audit");
        let encoded = report.encode().expect("encode original report");
        let assert_moved = |changed: CampReport, field: &str| {
            assert_ne!(
                changed.encode().expect("encode changed report"),
                encoded,
                "camp report field {field} is not retained"
            );
        };

        let mut changed = report.clone();
        changed.version += 1;
        assert_moved(changed, "version");
        let mut changed = report.clone();
        changed.standing = CampStanding::Clean;
        assert_moved(changed, "standing");
        let mut changed = report.clone();
        changed.distributions[0].bound.push('!');
        assert_moved(changed, "distribution.bound");
        let mut changed = report.clone();
        changed.distributions[0].limit += 1;
        assert_moved(changed, "distribution.limit");
        let mut changed = report.clone();
        changed.distributions[0].population += 1;
        assert_moved(changed, "distribution.population");
        let mut changed = report.clone();
        changed.distributions[0].near -= 1;
        assert_moved(changed, "distribution.near");
        let mut changed = report.clone();
        changed.distributions[0].standing = CampStanding::Clean;
        assert_moved(changed, "distribution.standing");
        let mut changed = report.clone();
        changed.findings[0].bound.push('!');
        assert_moved(changed, "finding.bound");
        let mut changed = report.clone();
        changed.findings[0].limit += 1;
        assert_moved(changed, "finding.limit");
        let mut changed = report.clone();
        changed.findings[0].population += 1;
        assert_moved(changed, "finding.population");
        let mut changed = report.clone();
        changed.findings[0].loci[0].locus.push('!');
        assert_moved(changed, "locus.locus");
        let mut changed = report.clone();
        changed.findings[0].loci[0].measured += 1;
        assert_moved(changed, "locus.measured");
        let mut changed = report.clone();
        changed.findings[0].loci[0].relation = BoundRelation::Above;
        assert_moved(changed, "locus.relation");
        let mut changed = report.clone();
        changed.findings[0].loci[0].delta += 1;
        assert_moved(changed, "locus.delta");
        let mut changed = report;
        changed.findings[0].loci[0].denominator += 1;
        assert_moved(changed, "locus.denominator");
    }

    fn camp(bounds: impl IntoIterator<Item = SharedBound>) -> Camp {
        Camp::new(bounds).expect("valid camp declarations")
    }

    fn bound<const N: usize>(name: &str, limit: u64, loci: [&str; N]) -> SharedBound {
        SharedBound::new(name, limit, loci).expect("valid shared bound")
    }

    fn measurements<const N: usize>(values: [(&str, u64); N]) -> Vec<Measurement> {
        values
            .into_iter()
            .map(|(locus, value)| Measurement::new(locus, value, [format!("{locus}.rs")]))
            .collect()
    }
}
