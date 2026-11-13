use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use wire::{Digest, Domain};

use super::manifest_contract::{
    ArmDisposition, BatteryManifest, BatteryManifestArm, CANONICAL_BATTERY_MANIFEST,
    GATE_BATTERY_VERSION,
};
use super::{
    ArmEvidence, ArmFamily, BatteryContext, BatteryError, BatteryInputs, BatteryState,
    ResultProvenance, inline_evidence,
};
use crate::manifest::GateId;

/// Complete executor output for one compiled arm.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArmExecution {
    state: BatteryState,
    reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance: Option<ResultProvenance>,
    evidence: ArmEvidence,
}

impl ArmExecution {
    /// Validate one explicit terminal state and its retained family evidence.
    pub fn new(
        state: BatteryState,
        reason: impl Into<String>,
        provenance: Option<ResultProvenance>,
        evidence: ArmEvidence,
    ) -> Result<Self, BatteryError> {
        let execution = Self {
            state,
            reason: reason.into(),
            provenance,
            evidence,
        };
        execution.validate()?;
        Ok(execution)
    }

    fn validate(&self) -> Result<(), BatteryError> {
        self.evidence
            .validate_execution(self.state, &self.reason, self.provenance.as_ref())
    }

    fn validate_expected(
        &self,
        family: ArmFamily,
        gate_id: GateId,
        declaration: &str,
        source: &Digest,
    ) -> Result<(), BatteryError> {
        self.validate()?;
        self.evidence
            .validate_expected(family, gate_id, declaration, source)
    }
}

/// One compiled row passed to the executor in canonical order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledArm {
    position: usize,
    gate_id: GateId,
    family: ArmFamily,
    declaration: String,
}

impl CompiledArm {
    /// Zero-based canonical report position.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.position
    }

    /// Stable compiled identity.
    #[must_use]
    pub const fn gate_id(&self) -> GateId {
        self.gate_id
    }

    /// Owning family.
    #[must_use]
    pub const fn family(&self) -> ArmFamily {
        self.family
    }

    /// Exact compiled declaration identity.
    #[must_use]
    pub fn declaration(&self) -> &str {
        &self.declaration
    }
}

/// Rust execution boundary used exactly once for every compiled row.
pub trait BatteryExecutor {
    /// Execute one arm from the already captured immutable authorities.
    fn execute(
        &mut self,
        arm: &CompiledArm,
        inputs: &BatteryInputs<'_>,
    ) -> Result<ArmExecution, BatteryError>;
}

/// One visible result at one canonical report position.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatteryArmResult {
    position: usize,
    gate_id: GateId,
    declaration: String,
    family: ArmFamily,
    state: BatteryState,
    reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provenance: Option<ResultProvenance>,
    evidence: ArmEvidence,
}

impl BatteryArmResult {
    /// Zero-based canonical position.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.position
    }

    /// Compiled gate identity.
    #[must_use]
    pub const fn gate_id(&self) -> GateId {
        self.gate_id
    }

    /// Exact compiled declaration identity.
    #[must_use]
    pub fn declaration(&self) -> &str {
        &self.declaration
    }

    /// Owning compiled family.
    #[must_use]
    pub const fn family(&self) -> ArmFamily {
        self.family
    }

    /// Typed terminal state.
    #[must_use]
    pub const fn state(&self) -> BatteryState {
        self.state
    }

    /// Exact retained reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Required applicability/exclusion provenance when relevant.
    #[must_use]
    pub const fn provenance(&self) -> Option<&ResultProvenance> {
        self.provenance.as_ref()
    }

    /// Family-qualified decisions and measurements.
    #[must_use]
    pub const fn evidence(&self) -> &ArmEvidence {
        &self.evidence
    }
}

/// One canonical focused gate run with the immutable inputs needed to verify it.
///
/// Direct deserialization validates the selected arm against the compiled
/// manifest and its captured source. Standalone retained bytes additionally
/// cross [`Self::decode`], which enforces their canonical encoding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FocusedArmReport {
    version: u64,
    gate_set: Digest,
    context: BatteryContext,
    result: BatteryArmResult,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFocusedArmReport {
    version: u64,
    gate_set: Digest,
    context: BatteryContext,
    result: BatteryArmResult,
}

impl From<RawFocusedArmReport> for FocusedArmReport {
    fn from(raw: RawFocusedArmReport) -> Self {
        Self {
            version: raw.version,
            gate_set: raw.gate_set,
            context: raw.context,
            result: raw.result,
        }
    }
}

impl<'de> Deserialize<'de> for FocusedArmReport {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let report = Self::from(RawFocusedArmReport::deserialize(deserializer)?);
        report.validate().map_err(D::Error::custom)?;
        Ok(report)
    }
}

impl FocusedArmReport {
    /// Registered instrument this report executed.
    #[must_use]
    pub const fn gate_id(&self) -> GateId {
        self.result.gate_id()
    }

    /// Typed terminal arm result.
    #[must_use]
    pub const fn result(&self) -> &BatteryArmResult {
        &self.result
    }

    /// Complete immutable run inputs retained for this arm.
    #[must_use]
    pub const fn context(&self) -> &BatteryContext {
        &self.context
    }

    /// Whether this focused result admits publication.
    #[must_use]
    pub const fn admits(&self) -> bool {
        self.result.state().admits()
    }

    /// Canonical JSON bytes for this focused report.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        wire::encode(self)
    }

    /// Decode only canonical focused evidence for one registered instrument.
    pub fn decode(bytes: &[u8]) -> Result<Self, BatteryError> {
        let raw: RawFocusedArmReport = wire::decode(bytes)
            .map_err(|error| BatteryError::new(format!("invalid focused arm report: {error}")))?;
        let report = Self::from(raw);
        report.validate()?;
        let canonical = report.encode().map_err(|error| {
            BatteryError::new(format!("cannot encode focused arm report: {error}"))
        })?;
        if canonical != bytes {
            return Err(BatteryError::new(
                "focused arm report bytes are not canonical",
            ));
        }
        Ok(report)
    }

    fn validate(&self) -> Result<(), BatteryError> {
        if self.version != GATE_BATTERY_VERSION {
            return Err(BatteryError::new(format!(
                "unsupported focused arm report version {}; expected {GATE_BATTERY_VERSION}",
                self.version
            )));
        }
        self.context.validate()?;
        let manifest = BatteryManifest::canonical()?;
        if self.gate_set != gate_set_digest(&manifest) {
            return Err(BatteryError::new(
                "focused arm report gate-set identity disagrees with the canonical manifest",
            ));
        }
        let Some(arm) = manifest.arms.get(self.result.position) else {
            return Err(BatteryError::new(format!(
                "focused arm position {} is outside the canonical manifest",
                self.result.position
            )));
        };
        validate_result(
            self.result.position,
            &self.result,
            arm,
            self.context.source(),
        )
    }

    /// Consume the focused context wrapper and return its arm result.
    #[must_use]
    pub fn into_result(self) -> BatteryArmResult {
        self.result
    }
}

/// Complete canonical battery report.
///
/// This type deliberately does not implement [`Deserialize`]. A derived one
/// would let any caller lift retained bytes straight into a report without the
/// completeness check that [`Self::decode`] performs. The only way to hold one
/// is to have run the battery or to have decoded through the private shape
/// below.
///
/// ```compile_fail,E0277
/// let report: gate::BatteryReport = serde_json::from_slice(b"{}").expect("no way in");
/// ```
#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatteryReport {
    version: u64,
    gate_set: Digest,
    context: BatteryContext,
    results: Vec<BatteryArmResult>,
    #[serde(skip)]
    canonical: OnceLock<Arc<[u8]>>,
}

impl PartialEq for BatteryReport {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && self.gate_set == other.gate_set
            && self.context == other.context
            && self.results == other.results
    }
}

impl Eq for BatteryReport {}

/// The report shape exactly as it is spelled on disk, admitted nowhere else.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawReport {
    version: u64,
    gate_set: Digest,
    context: BatteryContext,
    results: Vec<BatteryArmResult>,
}

impl From<RawReport> for BatteryReport {
    fn from(raw: RawReport) -> Self {
        Self {
            version: raw.version,
            gate_set: raw.gate_set,
            context: raw.context,
            results: raw.results,
            canonical: OnceLock::new(),
        }
    }
}

impl BatteryReport {
    /// Current report version.
    #[must_use]
    pub const fn version(&self) -> u64 {
        self.version
    }

    /// Manifest-and-declaration identity computed before execution.
    #[must_use]
    pub const fn gate_set(&self) -> &Digest {
        &self.gate_set
    }

    /// Complete immutable run inputs retained once for every result.
    #[must_use]
    pub const fn context(&self) -> &BatteryContext {
        &self.context
    }

    /// Every visible entry in canonical order.
    #[must_use]
    pub fn results(&self) -> &[BatteryArmResult] {
        &self.results
    }

    /// Whether every result passed or has contract-owned non-execution.
    #[must_use]
    pub fn admits(&self) -> bool {
        self.results.iter().all(|result| result.state.admits())
    }

    /// Canonical JSON bytes for the complete report.
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        self.canonical().map(|bytes| bytes.as_ref().to_vec())
    }

    /// Shared canonical bytes for receipt minting and durable evidence.
    ///
    /// The report is immutable and its cache is excluded from both serialized
    /// and equality identity. Every proof consumer therefore reuses the exact
    /// first encoding without creating a second representation of the report.
    pub(super) fn canonical(&self) -> Result<Arc<[u8]>, serde_json::Error> {
        if let Some(bytes) = self.canonical.get() {
            return Ok(Arc::clone(bytes));
        }
        let bytes = Arc::<[u8]>::from(wire::encode(self)?);
        let _ = self.canonical.set(Arc::clone(&bytes));
        Ok(self.canonical.get().map_or(bytes, Arc::clone))
    }

    /// Decode only a canonical, complete, correctly ordered current report.
    pub fn decode(bytes: &[u8]) -> Result<Self, BatteryError> {
        let raw: RawReport = wire::decode(bytes)
            .map_err(|error| BatteryError::new(format!("invalid battery report: {error}")))?;
        let report = Self::from(raw);
        report.validate()?;
        let canonical = report
            .encode()
            .map_err(|error| BatteryError::new(format!("cannot encode battery report: {error}")))?;
        if canonical != bytes {
            return Err(BatteryError::new("battery report bytes are not canonical"));
        }
        Ok(report)
    }

    /// Refuse anything but a complete, correctly ordered, internally agreeing
    /// vector.
    ///
    /// [`Self::decode`] is not the only way a report reaches a reader: one
    /// also arrives as a nested field whenever a receipt is decoded. Both
    /// doors decode through [`RawReport`] and both call this, and no third
    /// door exists because the public type carries no [`Deserialize`].
    pub(super) fn validate(&self) -> Result<(), BatteryError> {
        if self.version != GATE_BATTERY_VERSION {
            return Err(BatteryError::new(format!(
                "unsupported battery report version {}; expected {GATE_BATTERY_VERSION}",
                self.version
            )));
        }
        self.context.validate()?;
        let manifest = BatteryManifest::canonical()?;
        if self.gate_set != gate_set_digest(&manifest) {
            return Err(BatteryError::new(
                "battery report gate-set identity disagrees with the canonical manifest",
            ));
        }
        if self.results.len() != manifest.arms.len() {
            return Err(BatteryError::new(format!(
                "complete battery report requires {} results, received {}",
                manifest.arms.len(),
                self.results.len()
            )));
        }
        for (position, (result, arm)) in self.results.iter().zip(&manifest.arms).enumerate() {
            validate_result(position, result, arm, self.context.source())?;
        }
        Ok(())
    }
}

/// Execute the one canonical battery and retain a total result vector.
pub fn run_battery(
    inputs: &BatteryInputs<'_>,
    executor: &mut impl BatteryExecutor,
) -> Result<BatteryReport, BatteryError> {
    let manifest = BatteryManifest::canonical()?;
    let gate_set = gate_set_digest(&manifest);
    let context = inputs.snapshot()?;
    let mut results = Vec::with_capacity(manifest.arms.len());
    for (position, arm) in manifest.arms.iter().enumerate() {
        results.push(execute_arm(position, arm, inputs, executor));
    }
    let report = BatteryReport {
        version: GATE_BATTERY_VERSION,
        gate_set,
        context,
        results,
        canonical: OnceLock::new(),
    };
    report.validate()?;
    Ok(report)
}

/// Execute exactly one named canonical arm against the same validated inputs.
///
/// The result retains its canonical position and declaration identity, so it
/// can be compared directly with the matching row of [`run_battery`]. No
/// partial [`BatteryReport`] is constructed: reports remain complete by law.
pub fn run_battery_arm(
    inputs: &BatteryInputs<'_>,
    executor: &mut impl BatteryExecutor,
    gate_id: GateId,
) -> Result<BatteryArmResult, BatteryError> {
    run_focused_arm(inputs, executor, gate_id).map(FocusedArmReport::into_result)
}

/// Execute exactly one named canonical arm and retain its verifiable inputs.
pub fn run_focused_arm(
    inputs: &BatteryInputs<'_>,
    executor: &mut impl BatteryExecutor,
    gate_id: GateId,
) -> Result<FocusedArmReport, BatteryError> {
    let manifest = BatteryManifest::canonical()?;
    let gate_set = gate_set_digest(&manifest);
    let context = inputs.snapshot()?;
    let Some((position, arm)) = manifest.arms.iter().enumerate().find(|(_, arm)| {
        matches!(
            arm.disposition,
            ArmDisposition::Compiled { gate_id: candidate, .. } if candidate == gate_id
        )
    }) else {
        return Err(BatteryError::new(format!(
            "canonical battery omits selected gate {gate_id}"
        )));
    };
    let result = execute_arm(position, arm, inputs, executor);
    let report = FocusedArmReport {
        version: GATE_BATTERY_VERSION,
        gate_set,
        context,
        result,
    };
    report.validate()?;
    Ok(report)
}

fn execute_arm(
    position: usize,
    arm: &BatteryManifestArm,
    inputs: &BatteryInputs<'_>,
    executor: &mut impl BatteryExecutor,
) -> BatteryArmResult {
    let ArmDisposition::Compiled {
        gate_id,
        family,
        declaration,
    } = &arm.disposition;
    let (gate_id, family) = (*gate_id, *family);
    let compiled = CompiledArm {
        position,
        gate_id,
        family,
        declaration: declaration.clone(),
    };
    let execution = match executor.execute(&compiled, inputs) {
        Ok(execution) => admit_execution(execution, family, gate_id, declaration, inputs.source),
        Err(error) => failed_execution(
            gate_id,
            family,
            declaration,
            &format!("executor refused arm: {}", inline_evidence(error.detail())),
        ),
    };
    BatteryArmResult {
        position,
        gate_id,
        declaration: declaration.clone(),
        family,
        state: execution.state,
        reason: execution.reason,
        provenance: execution.provenance,
        evidence: execution.evidence,
    }
}

fn admit_execution(
    execution: ArmExecution,
    family: ArmFamily,
    gate_id: GateId,
    declaration: &str,
    source: &Digest,
) -> ArmExecution {
    match execution.validate_expected(family, gate_id, declaration, source) {
        Ok(()) => execution,
        Err(error) => failed_execution(
            gate_id,
            family,
            declaration,
            &format!(
                "executor returned invalid evidence: {}",
                inline_evidence(error.detail())
            ),
        ),
    }
}

fn failed_execution(
    gate_id: GateId,
    family: ArmFamily,
    declaration: &str,
    reason: &str,
) -> ArmExecution {
    ArmExecution {
        state: BatteryState::Failed,
        reason: reason.to_owned(),
        provenance: None,
        evidence: ArmEvidence::ExecutorFailure {
            gate_id,
            arm_family: family,
            declaration: declaration.to_owned(),
            reason: reason.to_owned(),
        },
    }
}

fn validate_result(
    position: usize,
    result: &BatteryArmResult,
    arm: &BatteryManifestArm,
    source: &Digest,
) -> Result<(), BatteryError> {
    if result.position != position {
        return Err(BatteryError::new(format!(
            "battery result {position} is missing or reordered"
        )));
    }
    let execution = ArmExecution {
        state: result.state,
        reason: result.reason.clone(),
        provenance: result.provenance.clone(),
        evidence: result.evidence.clone(),
    };
    let ArmDisposition::Compiled {
        gate_id,
        family,
        declaration,
    } = &arm.disposition;
    execution.validate_expected(*family, *gate_id, declaration, source)?;
    validate_compiled_result(position, result, *gate_id, *family, declaration)
}

fn validate_compiled_result(
    position: usize,
    result: &BatteryArmResult,
    gate_id: GateId,
    family: ArmFamily,
    declaration: &str,
) -> Result<(), BatteryError> {
    if result.gate_id != gate_id
        || result.family != family
        || result.declaration != declaration
        || result.evidence.family() != family
    {
        return Err(BatteryError::new(format!(
            "battery result {position} disagrees with its compiled declaration"
        )));
    }
    Ok(())
}

pub(super) fn gate_set_digest(manifest: &BatteryManifest) -> Digest {
    let mut bytes = CANONICAL_BATTERY_MANIFEST.as_bytes().to_vec();
    bytes.extend_from_slice(b"\0compiled-declaration-identities\0");
    for (position, arm) in manifest.arms.iter().enumerate() {
        bytes.extend_from_slice(position.to_string().as_bytes());
        bytes.push(0);
        let ArmDisposition::Compiled { declaration, .. } = &arm.disposition;
        bytes.extend_from_slice(declaration.as_bytes());
        bytes.push(b'\n');
    }
    Digest::compute(Domain::GateSet, &bytes)
}
