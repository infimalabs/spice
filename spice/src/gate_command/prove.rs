//! Proof-aware execution of selected reusable gate actions.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant as MonotonicInstant};

use gate::{
    ActionFingerprintAuthority, ActionFingerprintAuthorityFields, ActionFingerprintManifest,
    ActionFingerprintManifestFields, ActionInputClosure, ActionInputInventory, ActionKey,
    ActionKeyAuthority, ActionNonFileInput, ActionNonFileKind, ActionPolicyAuthority,
    ActionReuseClass, ActionSpec, ActionSpecFields, ArmDisposition, BatteryManifest,
    CargoExternalSourceAuthority, CargoGateShard, CargoGateShardKind, CargoGateShardPlanning,
    EvidenceProducerRecord, EvidenceStore, FocusedArmReport, GateId, PassReceipt,
    PassReceiptFields,
};
use serde::Serialize;
use sill::Environment;
use wire::output::Export;
use wire::{Digest, Domain, Outcome, Refusal, Reply, ReplyPosition};

use spice::ambient::Ambient;
use spice::change_gate::{ChangeGateError, StagedActionSelection};

use super::output;
use spice::repository::Repository;

mod action;
mod cargo;
#[cfg(test)]
#[path = "prove/cargo_authority_tests.rs"]
mod cargo_authority_tests;
#[path = "prove/fingerprint.rs"]
mod fingerprint;
mod inspect;
#[path = "prove/single_flight.rs"]
mod single_flight;
#[path = "prove/tools.rs"]
mod tools;

use cargo::{
    authority as cargo_authority, request as cargo_request,
    source_authority as cargo_source_authority,
};
use fingerprint::{PreparedActions, evidence_lookup_failure};
pub(super) use inspect::{fingerprint_generation_detail, missing_proof_detail};
use tools::action_requirements as action_tools;

const RUNNER_VERSION: u64 = 1;

#[derive(Clone, Copy)]
struct Request<'a> {
    action: Option<&'a str>,
    missing: bool,
}

impl Request<'_> {
    const fn inspect(self) -> bool {
        self.action.is_none() && !self.missing
    }

    const fn uses_fingerprint(self) -> bool {
        self.action.is_none()
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProveReport {
    candidate_tree: String,
    source: Digest,
    /// Which private snapshot ran and which worktree paths it left out.
    snapshot: gate::tree::WorktreeDivergence,
    actions: Vec<ProveActionReport>,
    /// The decision-first terminal projection; `--json` carries the fields.
    #[serde(skip)]
    text: String,
}

impl ProveReport {
    const fn new(
        candidate_tree: String,
        source: Digest,
        snapshot: gate::tree::WorktreeDivergence,
        actions: Vec<ProveActionReport>,
    ) -> Self {
        Self {
            candidate_tree,
            source,
            snapshot,
            actions,
            text: String::new(),
        }
    }

    /// The terminal reply text, set once the report is complete.
    pub(crate) fn text(&self) -> Option<&str> {
        Some(&self.text)
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ProveActionReport {
    action: String,
    action_key: ActionKey,
    disposition: ProveDisposition,
    receipt: gate::ReceiptId,
    /// How long this invocation spent producing the receipt.
    #[serde(skip)]
    elapsed: Option<Duration>,
    /// The retained complete subprocess capture of that production.
    #[serde(skip)]
    capture: Option<Export>,
    /// How often this receipt has been reused and admitted, once counted.
    #[serde(skip)]
    stats: Option<Box<gate::ReceiptStats>>,
}

impl ProveActionReport {
    const fn present(action: String, action_key: ActionKey, receipt: gate::ReceiptId) -> Self {
        Self {
            action,
            action_key,
            disposition: ProveDisposition::Present,
            receipt,
            elapsed: None,
            capture: None,
            stats: None,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ProveDisposition {
    Present,
    Published,
}

struct PreparedAction {
    action: String,
    action_key: ActionKey,
    action_spec: Digest,
    input_manifest: Digest,
    execution: PreparedExecution,
}

impl PreparedAction {
    fn expectation(&self) -> Result<gate::ReceiptExpectation, ProveFailure> {
        gate::ReceiptExpectation::new(
            self.action.clone(),
            self.action_key.clone(),
            self.action_spec.clone(),
            self.input_manifest.clone(),
        )
        .map_err(ProveFailure::store)
    }
}

pub(super) struct AdmissionPreparation {
    candidate_tree: String,
    staged: StagedActionSelection,
    selection: Digest,
    policy: Digest,
    gate_set: Digest,
    authority: ActionFingerprintAuthority,
}

pub(super) struct AdmissionLookup {
    candidate_tree: String,
    anchor: spice::gate::PreparedArm<'static>,
    authority: ActionFingerprintAuthority,
    policy: Digest,
    gate_set: Digest,
}

impl AdmissionLookup {
    pub(super) const fn authority(&self) -> &ActionFingerprintAuthority {
        &self.authority
    }

    pub(super) fn reject_state(&self) -> Result<(), ProveFailure> {
        self.anchor.reject_state().map_err(ProveFailure::run)
    }

    pub(super) fn execute_live_checks(
        &self,
    ) -> Result<Vec<gate::AdmissionLiveCheck>, ProveFailure> {
        [GateId::PlanningPhase, GateId::Staging]
            .into_iter()
            .map(|gate_id| {
                let arm = self
                    .anchor
                    .derive_live_arm(gate_id)
                    .map_err(ProveFailure::run)?;
                execute_live_arm(arm, gate_id)
            })
            .collect()
    }

    pub(super) fn restore(
        self,
        manifest: &ActionFingerprintManifest,
    ) -> Result<AdmissionPreparation, ProveFailure> {
        let selection = manifest.selection().clone();
        let selection_digest = digest_encoded(Domain::GateSelection, &selection)?;
        let staged = spice::change_gate::restore_staged_actions(
            self.anchor,
            selection,
            manifest.cargo_tests(),
        )
        .map_err(ProveFailure::selection)?;
        Ok(AdmissionPreparation {
            candidate_tree: self.candidate_tree,
            staged,
            selection: selection_digest,
            policy: self.policy,
            gate_set: self.gate_set,
            authority: self.authority,
        })
    }
}

impl AdmissionPreparation {
    pub(super) fn candidate_tree(&self) -> &str {
        &self.candidate_tree
    }

    pub(super) fn source(&self) -> &Digest {
        self.staged.selection().source()
    }

    pub(super) fn selection(&self) -> &Digest {
        &self.selection
    }

    pub(super) fn policy(&self) -> &Digest {
        &self.policy
    }

    pub(super) fn gate_set(&self) -> &Digest {
        &self.gate_set
    }

    pub(super) fn head(&self) -> Option<String> {
        self.staged
            .anchor()
            .staged_authority()
            .and_then(gate::tree::StagedAuthority::baseline_identity)
    }

    pub(super) fn verify_source(&self) -> Result<(), ProveFailure> {
        self.staged
            .anchor()
            .verify_source()
            .map_err(ProveFailure::run)
    }

    pub(super) fn verify_fingerprint_authority(
        &self,
        repository: &Repository,
    ) -> Result<(), ProveFailure> {
        let observed = fingerprint::admission_authority(
            repository,
            self.staged.anchor(),
            &self.candidate_tree,
            self.policy.clone(),
            self.gate_set.clone(),
        )?;
        if observed != self.authority {
            return Err(ProveFailure::refusal(
                "gate::prove-authority-moved",
                "proof fingerprint authority changed after lookup",
            ));
        }
        Ok(())
    }

    pub(super) fn verify_current(
        &mut self,
        repository: &Repository,
        ambient: &Ambient,
        claim: &gate::ClaimCapture,
    ) -> Result<bool, ProveFailure> {
        let refreshed = if let Some(staged) =
            refresh_staged_source(&self.staged, repository, ambient, claim)?
        {
            self.staged = staged;
            true
        } else {
            false
        };
        self.verify_source()?;
        self.verify_fingerprint_authority(repository)?;
        Ok(refreshed)
    }

    pub(super) fn execute_live_checks(
        &self,
    ) -> Result<Vec<gate::AdmissionLiveCheck>, ProveFailure> {
        let mut checks = Vec::new();
        for gate_id in self.staged.selection().gate_ids().iter().copied() {
            if gate::gate_action_reuse_class(gate_id) != Some(ActionReuseClass::LiveOnly) {
                continue;
            }
            let arm = self
                .staged
                .anchor()
                .derive_reached_arm(self.staged.selection(), gate_id)
                .map_err(ProveFailure::run)?;
            checks.push(execute_live_arm(arm, gate_id)?);
        }
        checks.sort_by(|left, right| left.action().cmp(right.action()));
        Ok(checks)
    }

    pub(super) fn accept_state(&self) -> Result<(), ProveFailure> {
        self.staged
            .anchor()
            .accept_state(gate::StateRunScope::Partial)
            .map_err(ProveFailure::run)
    }

    pub(super) fn reject_state(&self) -> Result<(), ProveFailure> {
        self.staged.reject_state().map_err(ProveFailure::run)
    }

    pub(super) fn into_staged(self) -> StagedActionSelection {
        self.staged
    }
}

fn refresh_staged_source(
    staged: &StagedActionSelection,
    repository: &Repository,
    ambient: &Ambient,
    claim: &gate::ClaimCapture,
) -> Result<Option<StagedActionSelection>, ProveFailure> {
    match staged
        .anchor()
        .verify_refreshable_source()
        .map_err(ProveFailure::run)?
    {
        gate::tree::RefreshableSourceState::Current => Ok(None),
        gate::tree::RefreshableSourceState::IndexChanged => {
            let expected = staged
                .anchor()
                .staged_authority()
                .expect("admission preparation retains staged authority");
            let anchor = spice::gate::prepare_arm_at_with_claim(
                ambient,
                repository.worktree_root(),
                gate::Selector::staged(),
                GateId::Staging,
                claim.clone(),
            )
            .map_err(ProveFailure::run)?;
            let observed = anchor
                .staged_authority()
                .expect("a recaptured staged anchor retains staged authority");
            let differences = expected.differences(observed);
            if !differences.is_empty() {
                return Err(ProveFailure::refusal(
                    "gate::source-moved",
                    format!(
                        "staged semantic authority changed after evidence lookup: {differences:?}"
                    ),
                ));
            }
            let restored = spice::change_gate::restore_staged_actions(
                anchor,
                staged.selection().clone(),
                staged.cargo_tests(),
            )
            .map_err(ProveFailure::selection)?;
            Ok(Some(restored))
        }
    }
}

fn execute_live_arm(
    mut arm: spice::gate::PreparedArm<'static>,
    gate_id: GateId,
) -> Result<gate::AdmissionLiveCheck, ProveFailure> {
    let report = arm.execute().map_err(ProveFailure::run)?;
    if !report.admits() {
        return Err(ProveFailure::refusal(
            "gate::pre-commit-live-check-regressed",
            refusal_detail(&report),
        ));
    }
    let result = Digest::compute(
        Domain::GateFocusedReport,
        &report.encode().map_err(|error| {
            ProveFailure::fault("gate::pre-commit-live-result-unencodable", error)
        })?,
    );
    gate::AdmissionLiveCheck::new(gate_id.to_string(), result).map_err(ProveFailure::store)
}

pub(super) fn prepare_admission_from_anchor(
    repository: &Repository,
    anchor: spice::gate::PreparedArm<'static>,
) -> Result<AdmissionLookup, ProveFailure> {
    let staged_authority = anchor
        .staged_authority()
        .expect("staged action preparation retains staged authority");
    let candidate_tree = staged_authority.candidate_tree_identity().to_owned();
    let context = anchor.context().map_err(ProveFailure::run)?;
    let policy = context.policy().clone();
    let gate_set = gate::canonical_gate_set_digest()
        .map_err(|error| ProveFailure::fault("gate::prove-manifest-invalid", error))?;
    let authority = fingerprint::admission_authority(
        repository,
        &anchor,
        &candidate_tree,
        policy.clone(),
        gate_set.clone(),
    )?;
    Ok(AdmissionLookup {
        candidate_tree,
        anchor,
        authority,
        policy,
        gate_set,
    })
}

enum PreparedExecution {
    Gate {
        arm: Box<spice::gate::PreparedArm<'static>>,
        tools: Vec<gate::ToolEvidence>,
        /// Home admitted for the staged lock, held so its generation pin
        /// outlives the arm's Cargo runs.
        cargo_home: Option<std::sync::Arc<spice::cargo_isolation::PreparedHome>>,
    },
    Cargo(Box<cargo::Execution>),
}

struct PendingReceipt {
    action: String,
    action_key: ActionKey,
    receipt: PassReceipt,
    capture: Option<Export>,
}

pub(crate) fn prove(
    matches: &clap::ArgMatches,
    live: bool,
    ambient: &Ambient,
) -> Outcome<ProveReport> {
    let request = Request {
        action: matches.get_one::<String>("ACTION").map(String::as_str),
        missing: matches.get_flag("missing"),
    };
    if let Some(action) = request.action
        && !registered_request(action)
    {
        return refusal(
            "gate::prove-action-unregistered",
            format!("{action:?} is not a registered gate action or Cargo shard"),
        );
    }
    match run(request, live, ambient) {
        Ok(mut report) => {
            report.text = summary(&report, request, matches.get_flag("verbose"));
            Outcome::Reply(Reply {
                position: ReplyPosition::Direct {},
                body: report,
            })
        }
        Err(failure) => failure.outcome(),
    }
}

/// Counts first, then each action this run proved, then the next command.
/// Already-proven actions are counted; `--verbose` lists them with identities.
fn summary(report: &ProveReport, request: Request<'_>, verbose: bool) -> String {
    let published =
        |action: &&ProveActionReport| matches!(action.disposition, ProveDisposition::Published);
    let proved = report.actions.iter().filter(published).count();
    let present = report.actions.len() - proved;
    let tree = report
        .candidate_tree
        .get(..12)
        .unwrap_or(&report.candidate_tree);
    let mut summary = output::Summary::new(format_args!(
        "gate prove: {} actions current for tree {tree} ({proved} proved now, {present} already)",
        report.actions.len(),
    ));
    let already = report
        .actions
        .iter()
        .filter(|action| verbose && !published(action));
    for action in report.actions.iter().filter(published).chain(already) {
        let verdict = match action.disposition {
            ProveDisposition::Published => "proved",
            ProveDisposition::Present => "already",
        };
        summary.row(verdict, &action.action, action.elapsed);
        if verbose {
            summary.detail(format_args!(
                "key {} receipt {}",
                action.action_key, action.receipt
            ));
            if let Some(stats) = &action.stats {
                summary.detail(format_args!("{}", UsageDetail(stats)));
            }
            if let Some(capture) = &action.capture {
                summary.detail(format_args!(
                    "output {} ({} bytes)",
                    capture.path, capture.bytes
                ));
            }
        }
    }
    if !verbose && present > 0 {
        summary.closing("detail", &prove_command(request, "--verbose"));
    }
    summary.snapshot(&report.snapshot);
    let next = if request.action.is_some() {
        prove_command(
            Request {
                action: None,
                missing: false,
            },
            "",
        )
    } else {
        "git commit".to_owned()
    };
    summary.closing("next", &next);
    summary.finish()
}

fn prove_command(request: Request<'_>, flag: &str) -> String {
    let mut command = "spice gate prove".to_owned();
    if let Some(action) = request.action {
        command.push(' ');
        command.push_str(action);
    }
    if request.missing {
        command.push_str(" --missing");
    }
    if !flag.is_empty() {
        command.push(' ');
        command.push_str(flag);
    }
    command
}

fn run(request: Request<'_>, live: bool, ambient: &Ambient) -> Result<ProveReport, ProveFailure> {
    let current = std::env::current_dir()
        .map_err(|error| ProveFailure::fault("gate::prove-working-directory-unavailable", error))?;
    let repository = Repository::discover(&current)
        .map_err(|error| ProveFailure::fault(spice::repository::ResolveError::REASON, error))?;
    let lease = crate::cargo_command::output_lease(&repository)
        .map_err(|error| ProveFailure::fault("cargo-output::lease-unavailable", error))?;
    let report = prove_leased(&repository, request, live, ambient);
    crate::cargo_command::complete_output_lease(lease, &repository);
    report
}

/// Prove `request` under the output lease the caller holds.
fn prove_leased(
    repository: &Repository,
    request: Request<'_>,
    live: bool,
    ambient: &Ambient,
) -> Result<ProveReport, ProveFailure> {
    let candidate_tree =
        spice::gate::capture_candidate_tree(repository, ambient).map_err(ProveFailure::run)?;
    let anchor =
        spice::change_gate::prepare_staged_anchor(ambient).map_err(ProveFailure::selection)?;
    let snapshot = anchor.worktree_divergence().map_err(ProveFailure::run)?;
    let store = EvidenceStore::open(repository.common_dir()).map_err(ProveFailure::store)?;
    let fingerprint = request
        .uses_fingerprint()
        .then(|| fingerprint::current_authority(repository, &anchor, &candidate_tree))
        .transpose()?;
    if request.inspect() {
        return inspect::current(
            repository,
            ambient,
            &store,
            &candidate_tree,
            &anchor,
            fingerprint
                .as_ref()
                .expect("inspection always derives fingerprint authority"),
            snapshot,
        );
    }
    let staged = if let Some(authority) = fingerprint.as_ref() {
        match fingerprint::lookup_complete(&store, authority) {
            Ok(Some(mut actions)) => {
                validate_source(repository, ambient, &anchor, &candidate_tree)?;
                anchor.reject_state().map_err(ProveFailure::run)?;
                account_reuse(&store, &mut actions);
                return Ok(ProveReport::new(
                    candidate_tree.to_string(),
                    anchor.source().clone(),
                    snapshot,
                    actions,
                ));
            }
            Ok(None) => {}
            Err(error) => {
                let _ = anchor.reject_state();
                return Err(error);
            }
        }
        match store
            .lookup_action_fingerprints(authority)
            .map_err(ProveFailure::store)
        {
            Ok(Some(manifest)) => spice::change_gate::restore_staged_actions(
                anchor,
                manifest.selection().clone(),
                manifest.cargo_tests(),
            )
            .map_err(ProveFailure::selection)?,
            Ok(None) => spice::change_gate::prepare_staged_actions_from_anchor(ambient, anchor)
                .map_err(ProveFailure::selection)?,
            Err(error) => {
                let _ = anchor.reject_state();
                return Err(error);
            }
        }
    } else {
        spice::change_gate::prepare_staged_actions_from_anchor(ambient, anchor)
            .map_err(ProveFailure::selection)?
    };
    let (mut actions, pending, expectations) =
        match prepare_actions(&staged, &store, request, repository.worktree_root()) {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = staged.reject_state();
                return Err(error);
            }
        };
    if let Some(authority) = fingerprint.as_ref()
        && let Err(error) = ActionFingerprintManifest::new(ActionFingerprintManifestFields {
            authority: authority.clone(),
            selection: staged.selection().clone(),
            cargo_tests: staged.cargo_tests(),
            actions: expectations.clone(),
        })
        .map_err(ProveFailure::store)
        .and_then(|manifest| {
            store
                .publish_action_fingerprints(&manifest)
                .map(|_| ())
                .map_err(ProveFailure::store)
        })
    {
        let _ = staged.reject_state();
        return Err(error);
    }
    let published = single_flight::run(
        &store,
        pending,
        || validate_source(repository, ambient, staged.anchor(), &candidate_tree),
        || staged.reject_state().map_err(ProveFailure::run),
        &output::Progress::stderr("gate prove", live),
        &output::captures(repository.common_dir()),
    )?;
    record_arm_times(repository, &published)?;
    actions.extend(published);
    actions.sort_by(|left, right| proof_action_order(&left.action, &right.action));
    account_reuse(&store, &mut actions);
    Ok(ProveReport::new(
        candidate_tree.to_string(),
        staged.selection().source().clone(),
        snapshot,
        actions,
    ))
}

/// Count every receipt this run reused, then attach each action's usage counts.
/// Statistics never fail a proof; a miss is reported on stderr and skipped.
fn account_reuse(store: &EvidenceStore, actions: &mut [ProveActionReport]) {
    let reused = actions
        .iter()
        .filter(|action| matches!(action.disposition, ProveDisposition::Present))
        .map(|action| action.receipt.clone())
        .collect::<Vec<_>>();
    if !reused.is_empty() {
        let counted = now()
            .map_err(|failure| failure.detail().to_owned())
            .and_then(|at| {
                store
                    .record_receipt_uses(&reused, at)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = counted {
            eprintln!("gate prove: receipt uses not counted: {error}");
        }
    }
    let receipts = actions
        .iter()
        .map(|action| action.receipt.clone())
        .collect::<Vec<_>>();
    match store.receipt_stats(&receipts) {
        Ok(stats) => {
            for (action, stats) in actions.iter_mut().zip(stats) {
                action.stats = stats.map(Box::new);
            }
        }
        Err(error) => eprintln!("gate prove: receipt stats unreadable: {error}"),
    }
}

/// `used N, last AT; admitted M, last AT` for one receipt's counts.
struct UsageDetail<'a>(&'a gate::ReceiptStats);

impl std::fmt::Display for UsageDetail<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stats = self.0;
        write!(formatter, "used {}", stats.uses())?;
        if let Some(at) = stats.last_used_at() {
            write!(formatter, ", last {at}")?;
        }
        write!(formatter, "; admitted {}", stats.admissions())?;
        if let Some(at) = stats.last_admitted_at() {
            write!(formatter, ", last {at}")?;
        }
        Ok(())
    }
}

/// Retain the wall time of every arm this run proved; a reused receipt timed
/// nothing. The sweep ranks slow arms from this record.
fn record_arm_times(
    repository: &Repository,
    published: &[ProveActionReport],
) -> Result<(), ProveFailure> {
    let observed = published
        .iter()
        .filter(|action| matches!(action.disposition, ProveDisposition::Published))
        .filter_map(|action| {
            let elapsed = action.elapsed?;
            Some((
                action.action.clone(),
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            ))
        })
        .collect::<Vec<_>>();
    if observed.is_empty() {
        return Ok(());
    }
    let state = repository
        .admit_state()
        .map_err(|error| ProveFailure::fault("gate::arm-times-unavailable", error))?;
    spice::gate::arms::record_arms(state.gate_arms(), state.gate_arms_lock(), observed)
        .map_err(|error| ProveFailure::fault("gate::arm-times-unrecorded", error))
}

#[cfg(test)]
fn complete_actions<A, R, O, E>(
    actions: Vec<A>,
    mut execute: impl FnMut(A) -> Result<R, E>,
    mut validate: impl FnMut() -> Result<(), E>,
    cleanup: impl FnOnce() -> Result<(), E>,
    mut publish: impl FnMut(R) -> Result<O, E>,
) -> Result<Vec<O>, E> {
    let completed = actions
        .into_iter()
        .try_fold(Vec::new(), |mut reports, action| {
            let receipt = execute(action)?;
            validate()?;
            reports.push(publish(receipt)?);
            Ok(reports)
        });
    let cleaned = cleanup();
    let reports = completed?;
    cleaned?;
    Ok(reports)
}

fn validate_source(
    repository: &Repository,
    ambient: &Ambient,
    anchor: &spice::gate::PreparedArm<'_>,
    candidate_tree: &spice::gate::CandidateTree,
) -> Result<(), ProveFailure> {
    anchor
        .verify_refreshable_source()
        .map_err(ProveFailure::run)?;
    let observed =
        spice::gate::capture_candidate_tree(repository, ambient).map_err(ProveFailure::run)?;
    if &observed != candidate_tree {
        return Err(ProveFailure::refusal(
            "gate::prove-source-moved",
            format!("before={candidate_tree}; after={observed}"),
        ));
    }
    Ok(())
}

fn prepare_actions(
    staged: &StagedActionSelection,
    store: &EvidenceStore,
    request: Request<'_>,
    writable_root: &Path,
) -> Result<PreparedActions, ProveFailure> {
    let prepared_actions = prepare_action_authorities(staged, request, writable_root)?;
    let mut expectations = prepared_actions
        .iter()
        .map(PreparedAction::expectation)
        .collect::<Result<Vec<_>, _>>()?;
    expectations.sort_by(|left, right| left.action().cmp(right.action()));
    let mut present = Vec::new();
    let mut pending = Vec::new();
    for prepared in prepared_actions {
        classify_prepared(store, request.missing, prepared, &mut present, &mut pending)?;
    }

    if let Some(action) = request.action
        && !request_matches_any(action, &present, &pending)
    {
        return Err(ProveFailure::refusal(
            "gate::prove-action-unregistered",
            format!("{action:?} is not a selected registered gate or Cargo shard"),
        ));
    }
    present.sort_by(|left, right| proof_action_order(&left.action, &right.action));
    pending.sort_by(|left, right| proof_action_order(&left.action, &right.action));
    Ok((present, pending, expectations))
}

fn prepare_action_authorities(
    staged: &StagedActionSelection,
    request: Request<'_>,
    writable_root: &Path,
) -> Result<Vec<PreparedAction>, ProveFailure> {
    let requested = requested_actions(staged.selection(), request)?;
    let manifest = BatteryManifest::canonical()
        .map_err(|error| ProveFailure::fault("gate::prove-manifest-invalid", error))?;
    let gate_set = gate::canonical_gate_set_digest()
        .map_err(|error| ProveFailure::fault("gate::prove-manifest-invalid", error))?;
    let baseline = staged
        .anchor()
        .staged_authority()
        .and_then(gate::tree::StagedAuthority::baseline_identity);
    let mut prepared = Vec::new();
    for gate_id in requested {
        prepared.push(action::prepare(
            staged,
            &manifest,
            &gate_set,
            baseline.as_deref(),
            gate_id,
            writable_root,
        )?);
    }

    if cargo_request(request) {
        prepared.extend(cargo::prepare(
            staged,
            &manifest,
            &gate_set,
            request,
            writable_root,
        )?);
    }

    Ok(prepared)
}

fn classify_prepared(
    store: &EvidenceStore,
    missing: bool,
    prepared: PreparedAction,
    present: &mut Vec<ProveActionReport>,
    pending: &mut Vec<PreparedAction>,
) -> Result<(), ProveFailure> {
    let current = if missing {
        store
            .lookup(&prepared.action_key)
            .map_err(ProveFailure::store)?
    } else {
        None
    };
    if action_needed(missing, current.is_some()) {
        pending.push(prepared);
    } else {
        let receipt = current.expect("a filtered action has a current receipt");
        present.push(ProveActionReport::present(
            prepared.action,
            prepared.action_key,
            receipt.id().map_err(ProveFailure::store)?,
        ));
    }
    Ok(())
}

fn normalized_action_environment(values: &[(OsString, OsString)]) -> Vec<(OsString, OsString)> {
    const LOCATION_ONLY: [&str; 4] = [
        "PWD",
        "CARGO_HOME",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_BUILD_DIR",
    ];
    const DISABLED_WRAPPERS: [&str; 2] = ["RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"];
    let mut normalized = values
        .iter()
        .filter(|(name, _)| {
            !LOCATION_ONLY
                .iter()
                .any(|candidate| name == OsStr::new(candidate))
                // Windows carries per-drive working directories as hidden
                // `=C:` entries, not semantic compiler inputs.
                && !matches!(name.as_encoded_bytes(), [b'=', b'A'..=b'Z' | b'a'..=b'z', b':'])
                && !DISABLED_WRAPPERS
                    .iter()
                    .any(|candidate| name == OsStr::new(candidate))
                && !spice::gate::operational_routing(name)
        })
        .map(|(name, value)| {
            let value = if name == OsStr::new("PATH") {
                normalized_search_path(value)
            } else {
                value.clone()
            };
            (name.clone(), value)
        })
        .collect::<Vec<_>>();
    normalized.extend(DISABLED_WRAPPERS.map(|name| (OsString::from(name), OsString::new())));
    normalized.sort();
    normalized
}

/// The search path as lookup sees it: a repeated entry never resolves
/// anything its first occurrence did not, so a shell that prepended a prefix
/// twice describes the same toolchain as one that prepended it once.
fn normalized_search_path(value: &OsStr) -> OsString {
    let mut seen = Vec::new();
    let retained = std::env::split_paths(value).filter(|path| {
        if seen.contains(path) {
            return false;
        }
        seen.push(path.clone());
        true
    });
    let retained = retained.collect::<Vec<_>>();
    let retained = if cfg!(windows) {
        normalized_windows_git_search_path(retained)
    } else {
        retained
    };
    std::env::join_paths(retained).unwrap_or_else(|_| value.to_owned())
}

fn normalized_windows_git_search_path(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    normalized_windows_git_search_path_with(paths, |path| {
        path.file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("cmd"))
            && path.join("git.exe").is_file()
    })
}

fn normalized_windows_git_search_path_with(
    mut paths: Vec<PathBuf>,
    is_public_git: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let public = paths.iter().find(|path| is_public_git(path)).cloned();
    if let Some(public) = public.as_ref() {
        paths.retain(|path| !windows_path_eq(path, public));
    }
    let routed = paths
        .iter()
        .take_while(|path| windows_git_routing(path))
        .count();
    let shell_prefix = routed >= 2;
    if public.is_none() && !shell_prefix {
        return paths;
    }
    let mut routed = if shell_prefix { routed } else { 0 };
    if paths.get(routed).is_some_and(|path| windows_user_bin(path)) {
        routed += 1;
    }
    paths.drain(..routed);
    paths
}

fn windows_git_routing(path: &Path) -> bool {
    let spelling = path.to_string_lossy().replace('\\', "/");
    let spelling = spelling.to_ascii_lowercase();
    spelling.ends_with("/mingw64/libexec/git-core")
        || spelling.ends_with("/mingw64/bin")
        || spelling.ends_with("/usr/bin")
}

fn windows_user_bin(path: &Path) -> bool {
    let spelling = path.to_string_lossy().replace('\\', "/");
    let spelling = spelling.to_ascii_lowercase();
    spelling.ends_with("/bin") && spelling.contains("/users/")
}

fn windows_path_eq(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

/// Produce one receipt. Subprocess output is retained under `captures`, never
/// streamed; a failure carries its bounded tail.
///
/// `queued` is the lease wait before execution, recorded beside the execution
/// time rather than inside it.
fn execute(
    action: PreparedAction,
    captures: &output::Captures,
    run: wire::CanonicalUuid,
    queued: Duration,
) -> Result<PendingReceipt, ProveFailure> {
    let started_at = now()?;
    let started = MonotonicInstant::now();
    let (executed, mut diagnostics) = gate::observe_execution(|| match action.execution {
        PreparedExecution::Gate {
            arm,
            tools: expected_tools,
            cargo_home,
        } => {
            let result = tools::execute(*arm, expected_tools, captures)?;
            drop(cargo_home);
            Ok((result, None))
        }
        PreparedExecution::Cargo(execution) => cargo::execute(*execution, captures),
    });
    let (typed_result, capture) = executed?;
    let duration_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let finished_at = now()?;
    diagnostics.queue_millis = Some(u64::try_from(queued.as_millis()).unwrap_or(u64::MAX));
    let receipt = PassReceipt::new(PassReceiptFields {
        action_key: action.action_key.clone(),
        action_spec: action.action_spec,
        input_manifest: action.input_manifest,
        typed_result,
        started_at,
        finished_at,
        duration_millis,
        artifacts: Vec::new(),
        producer: producer(run, action.action.clone()),
        diagnostics: Some(diagnostics),
    })
    .map_err(ProveFailure::store)?;
    Ok(PendingReceipt {
        action: action.action,
        action_key: action.action_key,
        receipt,
        capture,
    })
}

/// The executing appliance, the proving run it serves, and the arm it proved.
fn producer(run: wire::CanonicalUuid, action: String) -> EvidenceProducerRecord {
    EvidenceProducerRecord {
        appliance_generation: spice::version::running()
            .ok()
            .map(|running| running.binary.clone()),
        target: Some(format!(
            "{}-{}",
            std::env::consts::ARCH,
            std::env::consts::OS
        )),
        profile: Some(
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_owned(),
        ),
        run: Some(run),
        action: Some(action),
        ..EvidenceProducerRecord::default()
    }
}

fn requested_actions(
    selection: &gate::ChangeSelectionReport,
    request: Request<'_>,
) -> Result<Vec<GateId>, ProveFailure> {
    let named = request.action.and_then(gate_id);
    if request.action.is_some() && named.is_none() {
        return Ok(Vec::new());
    }
    if let Some(gate_id) = named
        && !selection.gate_ids().contains(&gate_id)
    {
        return Err(ProveFailure::refusal(
            "gate::prove-action-not-selected",
            format!("action {gate_id} is not selected by the staged change"),
        ));
    }
    let mut requested = Vec::new();
    for gate_id in selection.gate_ids().iter().copied() {
        if gate_id == GateId::RustTypecheck {
            continue;
        }
        if named.is_some_and(|named| named != gate_id) {
            continue;
        }
        match gate::gate_action_reuse_class(gate_id) {
            Some(ActionReuseClass::Cacheable | ActionReuseClass::AncestrySensitive) => {
                requested.push(gate_id);
            }
            Some(ActionReuseClass::LiveOnly) | None if named == Some(gate_id) => {
                return Err(ProveFailure::refusal(
                    "gate::prove-action-not-reusable",
                    format!("action {gate_id} is live-only and cannot publish reusable proof"),
                ));
            }
            Some(ActionReuseClass::LiveOnly) | None => {}
        }
    }
    Ok(requested)
}

fn registered_request(action: &str) -> bool {
    gate_id(action).is_some()
        || action == "cargo-tests"
        || action.starts_with("cargo:typecheck:")
        || action.starts_with("cargo:test:")
}

fn request_matches_any(
    requested: &str,
    present: &[ProveActionReport],
    pending: &[PreparedAction],
) -> bool {
    let exact = present.iter().any(|row| row.action == requested)
        || pending.iter().any(|row| row.action == requested);
    exact
        || (requested == "rust-typecheck"
            && present
                .iter()
                .map(|row| row.action.as_str())
                .chain(pending.iter().map(|row| row.action.as_str()))
                .any(|action| action.starts_with("cargo:typecheck:")))
        || (requested == "cargo-tests"
            && present
                .iter()
                .map(|row| row.action.as_str())
                .chain(pending.iter().map(|row| row.action.as_str()))
                .any(|action| action.starts_with("cargo:test:")))
}

fn declaration(manifest: &BatteryManifest, gate_id: GateId) -> Result<&str, ProveFailure> {
    let position = gate_position(gate_id);
    match manifest
        .arms()
        .get(position)
        .map(gate::BatteryManifestArm::disposition)
    {
        Some(ArmDisposition::Compiled {
            gate_id: observed,
            declaration,
            ..
        }) if *observed == gate_id => Ok(declaration),
        _ => Err(ProveFailure::fault(
            "gate::prove-manifest-invalid",
            format!("canonical manifest has no row for action {gate_id}"),
        )),
    }
}

fn environment(
    values: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<(Vec<String>, Digest), ProveFailure> {
    let mut values = values
        .iter()
        .map(|(name, value)| (name.as_os_str(), value.as_os_str()))
        .collect::<Vec<_>>();
    values.sort();
    let mut bytes = Vec::new();
    let mut names = Vec::with_capacity(values.len());
    for (name, value) in values {
        let text = name.to_str().ok_or_else(|| {
            ProveFailure::refusal(
                "gate::prove-environment-invalid",
                "compiler environment contains a non-UTF-8 name",
            )
        })?;
        names.push(text.to_owned());
        append_bytes(&mut bytes, name.as_encoded_bytes());
        append_bytes(&mut bytes, value.as_encoded_bytes());
    }
    names.dedup();
    Ok((names, Digest::compute(Domain::GateConfiguration, &bytes)))
}

fn append_bytes(target: &mut Vec<u8>, value: &[u8]) {
    target.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    target.extend_from_slice(value);
}

fn digest_encoded<T: Serialize>(domain: Domain, value: &T) -> Result<Digest, ProveFailure> {
    let bytes = wire::encode(value)
        .map_err(|error| ProveFailure::fault("gate::prove-authority-unencodable", error))?;
    Ok(Digest::compute(domain, &bytes))
}

fn non_file(
    kind: ActionNonFileKind,
    name: &str,
    digest: Digest,
) -> Result<ActionNonFileInput, ProveFailure> {
    ActionNonFileInput::new(kind, name, digest).map_err(ProveFailure::action_input)
}

fn gate_id(action: &str) -> Option<GateId> {
    GateId::BATTERY
        .into_iter()
        .find(|candidate| candidate.as_str() == action)
}

const fn action_needed(missing: bool, receipt_present: bool) -> bool {
    !missing || !receipt_present
}

fn gate_position(gate_id: GateId) -> usize {
    GateId::BATTERY
        .into_iter()
        .position(|candidate| candidate == gate_id)
        .expect("registered gate ids have canonical positions")
}

fn proof_action_order(left: &str, right: &str) -> std::cmp::Ordering {
    proof_action_position(left)
        .cmp(&proof_action_position(right))
        .then_with(|| left.cmp(right))
}

fn proof_action_position(action: &str) -> usize {
    if action.starts_with("cargo:typecheck:") {
        return gate_position(GateId::RustTypecheck);
    }
    if action.starts_with("cargo:test:") {
        return GateId::BATTERY.len();
    }
    gate_id(action).map_or(GateId::BATTERY.len() + 1, gate_position)
}

fn now() -> Result<wire::Instant, ProveFailure> {
    wire::Instant::new(jiff::Timestamp::now())
        .map_err(|error| ProveFailure::fault("gate::prove-clock-invalid", error))
}

fn refusal_detail(report: &FocusedArmReport) -> String {
    format!(
        "action={} state={} reason={}",
        report.gate_id(),
        report.result().state().as_str(),
        report.result().reason(),
    )
}

fn refusal<B>(reason: &str, detail: impl Into<String>) -> Outcome<B> {
    let remedy = if super::proof_absent(reason) {
        "the staged tree is not proved yet; run each named command to prove it"
    } else {
        "repair the named proof request or staged action inputs, then rerun it"
    };
    Outcome::Refusal(Refusal::evidence(reason, detail, remedy))
}

pub(super) enum ProveFailure {
    Refusal { reason: String, detail: String },
    Fault { reason: String, detail: String },
}

impl ProveFailure {
    pub(super) fn reason(&self) -> &str {
        match self {
            Self::Refusal { reason, .. } | Self::Fault { reason, .. } => reason,
        }
    }

    pub(super) fn detail(&self) -> &str {
        match self {
            Self::Refusal { detail, .. } | Self::Fault { detail, .. } => detail,
        }
    }

    pub(super) const fn is_fault(&self) -> bool {
        matches!(self, Self::Fault { .. })
    }

    pub(super) fn refusal(reason: impl Into<String>, detail: impl ToString) -> Self {
        Self::Refusal {
            reason: reason.into(),
            detail: detail.to_string(),
        }
    }

    fn fault(reason: impl Into<String>, detail: impl ToString) -> Self {
        Self::Fault {
            reason: reason.into(),
            detail: detail.to_string(),
        }
    }

    fn run(error: spice::gate::RunError) -> Self {
        if error.is_fault() {
            Self::fault(error.reason(), error.detail())
        } else {
            Self::refusal(error.reason(), error.detail())
        }
    }

    fn selection(error: ChangeGateError) -> Self {
        if error.is_fault() {
            Self::fault(error.reason(), error.detail())
        } else {
            Self::refusal(error.reason(), error.detail())
        }
    }

    fn store(error: gate::EvidenceStoreError) -> Self {
        Self::fault(error.reason(), error.detail())
    }

    fn action_input(error: gate::ActionInputError) -> Self {
        Self::refusal("gate::prove-action-input-refused", error)
    }

    fn cargo(error: gate::WorkspaceTestError) -> Self {
        Self::refusal("gate::prove-cargo-shard-refused", error)
    }

    fn cargo_isolation(error: spice::cargo_isolation::IsolationError) -> Self {
        Self::refusal(error.reason(), error.detail())
    }

    fn outcome<B>(self) -> Outcome<B> {
        match self {
            Self::Refusal { reason, detail } => refusal(&reason, detail),
            Self::Fault { reason, detail } => crate::environment_fault(reason, detail),
        }
    }
}

#[cfg(test)]
mod tests;
