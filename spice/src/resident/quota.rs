mod triggers;

pub(super) use triggers::Triggers;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use sill::{
    Argv, Cancellation, CancellationToken, Deadlines, Environment, Output, ProcessGroup, Redaction,
    RestartDisposition, Spec, StdinPolicy, StreamPolicy, WaitPolicy,
};
use spice::ambient::Ambient;
use tokio::sync::{mpsc, watch};

const RUNTIME_LIMIT: Duration = Duration::from_secs(30);
const OUTPUT_DRAIN_LIMIT: Duration = Duration::from_secs(1);
const REAP_LIMIT: Duration = Duration::from_secs(2);
const TERMINATION_GRACE: Duration = Duration::from_secs(1);
const STDOUT_LIMIT: usize = 64 * 1024;
const STDERR_LIMIT: usize = 16 * 1024;
/// Where the resident pins the credit-balance extension in its runtime
/// directory.
const CREDIT_BALANCE_EXTENSION: &str = "appliance/extensions/omp-credit-balance.mjs";
/// The provider whose reports the extension owns.
const CREDIT_BALANCE_PROVIDER: &str = "openai-codex";

/// Write the credit-balance extension this appliance carries into the
/// resident's runtime directory, where every quota sample names it.
pub(super) fn pin_extension(runtime: &Path) -> std::io::Result<PathBuf> {
    let path = runtime.join(CREDIT_BALANCE_EXTENSION);
    std::fs::create_dir_all(path.parent().expect("the extension has one parent"))?;
    std::fs::write(&path, vendor::omp::CREDIT_BALANCE_EXTENSION)?;
    Ok(path)
}

#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct State {
    pub(crate) generation: u64,
    pub(crate) refreshing: bool,
    pub(crate) last_good: Option<Arc<Snapshot>>,
    pub(crate) latest_attempt: Option<Attempt>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Attempt {
    Succeeded { finished_at_ms: u64 },
    Failed(Failure),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Failure {
    pub(crate) finished_at_ms: u64,
    pub(crate) kind: FailureKind,
    pub(crate) exit_code: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureKind {
    ExecutableUnavailable,
    Spawn,
    Timeout,
    StdoutOverflow,
    StderrOverflow,
    NonzeroExit,
    MalformedOutput,
    ExtensionUnloaded,
    OutputIncomplete,
    Supervisor,
    WorkerStart,
    WorkerLost,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Snapshot {
    pub(crate) generated_at_ms: i64,
    pub(crate) reports: Vec<Report>,
    pub(crate) accounts_without_usage: Vec<AccountWithoutUsage>,
    pub(crate) disabled_credentials: Vec<DisabledCredential>,
    pub(crate) capacity: BTreeMap<String, Vec<Capacity>>,
    pub(crate) disabled_providers: Vec<String>,
    pub(crate) disabled_provider_issue: Option<FailureKind>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Report {
    pub(crate) provider: String,
    pub(crate) fetched_at_ms: i64,
    pub(crate) account: ReportAccount,
    pub(crate) limits: Vec<Limit>,
    pub(crate) credit_balance: Option<Amount>,
    pub(crate) reset_credits: Option<ResetCredits>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReportAccount {
    pub(crate) email: Option<String>,
    pub(crate) account_id: Option<String>,
    pub(crate) project_id: Option<String>,
    pub(crate) org_id: Option<String>,
    pub(crate) org_name: Option<String>,
    pub(crate) plan_type: Option<String>,
    pub(crate) allowed: Option<bool>,
    pub(crate) limit_reached: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Limit {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) scope: Scope,
    pub(crate) window: Option<Window>,
    pub(crate) amount: Amount,
    pub(crate) status: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Scope {
    pub(crate) provider: String,
    pub(crate) account_id: Option<String>,
    pub(crate) project_id: Option<String>,
    pub(crate) org_id: Option<String>,
    pub(crate) model_id: Option<String>,
    pub(crate) tier: Option<String>,
    pub(crate) window_id: Option<String>,
    pub(crate) shared: Option<bool>,
    pub(crate) shared_group: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Window {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) resets_at: Option<i64>,
    pub(crate) reset_label: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Amount {
    pub(crate) used: Option<f64>,
    pub(crate) limit: Option<f64>,
    pub(crate) remaining: Option<f64>,
    pub(crate) used_fraction: Option<f64>,
    pub(crate) remaining_fraction: Option<f64>,
    pub(crate) unit: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResetCredits {
    pub(crate) available_count: u64,
    pub(crate) redeemable_count: Option<u64>,
    pub(crate) next_credit_id: Option<String>,
    pub(crate) eligible: Option<bool>,
    pub(crate) cooldown_until: Option<String>,
    #[serde(default)]
    pub(crate) credits: Vec<ResetCredit>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResetCredit {
    pub(crate) id: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) program: Option<String>,
    pub(crate) remaining_count: Option<u64>,
    pub(crate) usable: Option<bool>,
    pub(crate) requires_limit: Option<bool>,
    pub(crate) clears: Option<Vec<String>>,
    pub(crate) blocking: Option<Vec<String>>,
    pub(crate) used_fractions: Option<BTreeMap<String, f64>>,
    pub(crate) granted_at: Option<String>,
    pub(crate) expires_at: Option<String>,
    pub(crate) status: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountWithoutUsage {
    pub(crate) provider: String,
    #[serde(rename = "type")]
    pub(crate) credential_type: String,
    pub(crate) email: Option<String>,
    pub(crate) account_id: Option<String>,
    pub(crate) project_id: Option<String>,
    pub(crate) org_id: Option<String>,
    pub(crate) org_name: Option<String>,
    pub(crate) authorized_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DisabledCredential {
    pub(crate) provider: String,
    #[serde(rename = "type")]
    pub(crate) credential_type: String,
    pub(crate) email: Option<String>,
    pub(crate) account_id: Option<String>,
    pub(crate) org_id: Option<String>,
    pub(crate) org_name: Option<String>,
    pub(crate) disabled_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Capacity {
    pub(crate) window: String,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) meter: Option<String>,
    pub(crate) accounts: u64,
    pub(crate) used_accounts: f64,
    pub(crate) remaining_accounts: f64,
}

struct Settings {
    binary: OsString,
    extension: PathBuf,
    cwd: PathBuf,
    environment: Environment,
}

impl Settings {
    fn production(
        binary: impl Into<OsString>,
        extension: PathBuf,
        cwd: PathBuf,
        ambient: &Ambient,
    ) -> Self {
        Self {
            binary: binary.into(),
            extension,
            cwd,
            environment: ambient
                .environment()
                .expect("the inherited process environment has unique nonempty keys"),
        }
    }

    fn spec(
        &self,
        arguments: impl IntoIterator<Item = OsString>,
        wait_label: &'static str,
        cancellation: CancellationToken,
    ) -> Result<Spec, FailureKind> {
        Ok(Spec {
            argv: Argv::new(self.binary.clone(), arguments).map_err(|_| FailureKind::Spawn)?,
            cwd: self.cwd.clone(),
            environment: self.environment.clone(),
            group: ProcessGroup::New,
            deadlines: Deadlines::new(RUNTIME_LIMIT, OUTPUT_DRAIN_LIMIT, REAP_LIMIT)
                .map_err(|_| FailureKind::Supervisor)?,
            wait: WaitPolicy::new(
                wait_label,
                Cancellation::Token(cancellation),
                RestartDisposition::Never,
            )
            .map_err(|_| FailureKind::Supervisor)?,
            output: Output::new(
                StdinPolicy::Null,
                StreamPolicy::CaptureBounded {
                    limit: STDOUT_LIMIT,
                },
                StreamPolicy::CaptureBounded {
                    limit: STDERR_LIMIT,
                },
            ),
            redaction: Redaction::All,
            grace: TERMINATION_GRACE,
        })
    }
}

struct Active {
    cancellation: CancellationToken,
    worker: JoinHandle<()>,
}

/// One quota sample: launch OMP's usage command and judge what it returned.
type Sampler = dyn Fn(CancellationToken) -> Result<Snapshot, SampleFailure> + Send + Sync;

pub(super) struct Runtime {
    sampler: Arc<Sampler>,
    state: Arc<State>,
    updates: watch::Sender<Arc<State>>,
    feed: serve::stream::QuotaFeed,
    completions: mpsc::UnboundedReceiver<SampleCompletion>,
    completion_sender: mpsc::UnboundedSender<SampleCompletion>,
    active: Option<Active>,
    follow_up: bool,
}

pub(super) struct SampleCompletion {
    result: Result<Snapshot, SampleFailure>,
    finished_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SampleFailure {
    kind: FailureKind,
    exit_code: Option<i32>,
}

impl SampleFailure {
    const fn new(kind: FailureKind) -> Self {
        Self {
            kind,
            exit_code: None,
        }
    }

    const fn exit(code: Option<i32>) -> Self {
        Self {
            kind: FailureKind::NonzeroExit,
            exit_code: code,
        }
    }
}

impl Runtime {
    pub(super) fn start(
        binary: impl Into<OsString>,
        extension: PathBuf,
        cwd: PathBuf,
        ambient: &Ambient,
    ) -> Self {
        let settings = Settings::production(binary, extension, cwd, ambient);
        Self::start_with(Arc::new(move |cancellation| {
            sample(&settings, cancellation)
        }))
    }

    fn start_with(sampler: Arc<Sampler>) -> Self {
        let state = Arc::new(State::default());
        let feed = serve::stream::QuotaFeed::new(wire_generation(&state));
        let (updates, _) = watch::channel(Arc::clone(&state));
        let (completion_sender, completions) = mpsc::unbounded_channel();
        Self {
            sampler,
            state,
            updates,
            feed,
            completions,
            completion_sender,
            active: None,
            follow_up: false,
        }
    }

    pub(crate) fn request(&mut self) {
        if self.active.is_some() {
            self.follow_up = true;
            return;
        }
        self.launch();
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the dependent app task owns sampler state projection"
        )
    )]
    pub(crate) fn current(&self) -> Arc<State> {
        Arc::clone(&self.state)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the dependent app task owns live sampler subscriptions"
        )
    )]
    pub(crate) fn subscribe(&self) -> watch::Receiver<Arc<State>> {
        self.updates.subscribe()
    }

    pub(super) fn feed(&self) -> serve::stream::QuotaFeed {
        self.feed.clone()
    }

    pub(super) async fn recv(&mut self) -> Option<SampleCompletion> {
        if self.active.is_some() {
            self.completions.recv().await
        } else {
            future::pending().await
        }
    }

    pub(super) fn accept(&mut self, completion: Option<SampleCompletion>) {
        let Some(active) = self.active.take() else {
            return;
        };
        let joined = active.worker.join();
        let completion = match (joined, completion) {
            (Ok(()), Some(completion)) => completion,
            (Ok(()), None) | (Err(_), _) => SampleCompletion {
                result: Err(SampleFailure::new(FailureKind::WorkerLost)),
                finished_at_ms: now_ms(),
            },
        };
        let follow_up = std::mem::take(&mut self.follow_up);
        self.settle(completion);
        if follow_up {
            self.launch();
        }
    }

    fn launch(&mut self) {
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let sampler = Arc::clone(&self.sampler);
        let completed = self.completion_sender.clone();
        let worker = thread::Builder::new()
            .name("spice-omp-quota".to_owned())
            .spawn(move || {
                let result = sampler(worker_cancellation);
                let _sent = completed.send(SampleCompletion {
                    result,
                    finished_at_ms: now_ms(),
                });
            });
        match worker {
            Ok(worker) => {
                self.active = Some(Active {
                    cancellation,
                    worker,
                });
                self.publish_refreshing();
            }
            Err(_) => self.settle(SampleCompletion {
                result: Err(SampleFailure::new(FailureKind::WorkerStart)),
                finished_at_ms: now_ms(),
            }),
        }
    }

    fn publish_refreshing(&mut self) {
        let mut next = (*self.state).clone();
        next.generation = next
            .generation
            .checked_add(1)
            .expect("quota generations do not exhaust u64");
        next.refreshing = true;
        self.publish(next);
    }

    fn settle(&mut self, completion: SampleCompletion) {
        let mut next = (*self.state).clone();
        next.generation = next
            .generation
            .checked_add(1)
            .expect("quota generations do not exhaust u64");
        next.refreshing = false;
        match completion.result {
            Ok(mut snapshot) => {
                if snapshot.disabled_provider_issue.is_some()
                    && let Some(previous) = next.last_good.as_deref()
                {
                    snapshot
                        .disabled_providers
                        .clone_from(&previous.disabled_providers);
                }
                next.last_good = Some(Arc::new(snapshot));
                next.latest_attempt = Some(Attempt::Succeeded {
                    finished_at_ms: completion.finished_at_ms,
                });
            }
            Err(failure) => {
                next.latest_attempt = Some(Attempt::Failed(Failure {
                    finished_at_ms: completion.finished_at_ms,
                    kind: failure.kind,
                    exit_code: failure.exit_code,
                }));
            }
        }
        self.publish(next);
    }

    fn publish(&mut self, state: State) {
        let state = Arc::new(state);
        self.feed.publish(wire_generation(&state));
        self.updates.send_replace(Arc::clone(&state));
        self.state = state;
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            active.cancellation.cancel();
            let _joined = active.worker.join();
        }
    }
}

/// A resident test build samples the unavailable-provider result without
/// launching OMP. The production branch remains compiled in every build.
fn sample(settings: &Settings, cancellation: CancellationToken) -> Result<Snapshot, SampleFailure> {
    if cfg!(test) {
        Err(SampleFailure::new(FailureKind::ExecutableUnavailable))
    } else {
        let mut snapshot = sample_process(settings, cancellation.clone())?;
        match read_disabled_providers(settings, cancellation) {
            Ok(providers) => snapshot.disabled_providers = providers,
            Err(failure) => snapshot.disabled_provider_issue = Some(failure.kind),
        }
        Ok(snapshot)
    }
}

/// Launch OMP's bounded usage command and judge its complete output.
fn sample_process(
    settings: &Settings,
    cancellation: CancellationToken,
) -> Result<Snapshot, SampleFailure> {
    run_process(
        settings,
        [
            OsString::from("usage"),
            OsString::from("--json"),
            OsString::from("--redact"),
            OsString::from("--extension"),
            settings.extension.clone().into_os_string(),
        ],
        "resident OMP quota sample",
        cancellation,
        judge,
    )
}

fn read_disabled_providers(
    settings: &Settings,
    cancellation: CancellationToken,
) -> Result<Vec<String>, SampleFailure> {
    run_process(
        settings,
        [
            OsString::from("config"),
            OsString::from("get"),
            OsString::from("disabledProviders"),
        ],
        "resident OMP disabled provider read",
        cancellation,
        |finished| {
            let mut providers: Vec<String> = serde_json::from_slice(completed_stdout(finished)?)
                .map_err(|_| SampleFailure::new(FailureKind::MalformedOutput))?;
            providers.sort();
            providers.dedup();
            Ok(providers)
        },
    )
}

fn run_process<T>(
    settings: &Settings,
    arguments: impl IntoIterator<Item = OsString>,
    wait_label: &'static str,
    cancellation: CancellationToken,
    finish: impl FnOnce(&Finished<'_>) -> Result<T, SampleFailure>,
) -> Result<T, SampleFailure> {
    let spec = settings
        .spec(arguments, wait_label, cancellation)
        .map_err(SampleFailure::new)?;
    let running = sill::spawn(spec).map_err(|error| {
        let kind = if matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
        ) {
            FailureKind::ExecutableUnavailable
        } else {
            FailureKind::Spawn
        };
        SampleFailure::new(kind)
    })?;
    let outcome = running
        .wait()
        .map_err(|_| SampleFailure::new(FailureKind::Supervisor))?;
    let stdout = outcome
        .stdout()
        .ok_or_else(|| SampleFailure::new(FailureKind::Supervisor))?;
    let stderr = outcome
        .stderr()
        .ok_or_else(|| SampleFailure::new(FailureKind::Supervisor))?;
    finish(&Finished {
        stdout: stdout.bytes(),
        stdout_truncated: stdout.truncated(),
        stderr_truncated: stderr.truncated(),
        complete: outcome.outputs_drained() && stdout.complete() && stderr.complete(),
        ending: match outcome.completion() {
            sill::Completion::TimedOut(_) => Ending::TimedOut,
            sill::Completion::Cancelled(_) => Ending::Cancelled,
            sill::Completion::Exited(status) if !status.success() => Ending::Failed(status.code()),
            sill::Completion::Exited(_) => Ending::Succeeded,
        },
    })
}

/// What a finished usage command left behind, as the sample judges it.
struct Finished<'a> {
    stdout: &'a [u8],
    stdout_truncated: bool,
    stderr_truncated: bool,
    complete: bool,
    ending: Ending,
}

enum Ending {
    Succeeded,
    Failed(Option<i32>),
    TimedOut,
    Cancelled,
}

/// Output ceilings outrank how the command ended; only complete, successful
/// output is decoded.
fn completed_stdout<'a>(finished: &'a Finished<'_>) -> Result<&'a [u8], SampleFailure> {
    if finished.stdout_truncated {
        return Err(SampleFailure::new(FailureKind::StdoutOverflow));
    }
    if finished.stderr_truncated {
        return Err(SampleFailure::new(FailureKind::StderrOverflow));
    }
    if !finished.complete {
        return Err(SampleFailure::new(FailureKind::OutputIncomplete));
    }
    match finished.ending {
        Ending::TimedOut => Err(SampleFailure::new(FailureKind::Timeout)),
        Ending::Cancelled => Err(SampleFailure::new(FailureKind::Supervisor)),
        Ending::Failed(code) => Err(SampleFailure::exit(code)),
        Ending::Succeeded => Ok(finished.stdout),
    }
}

fn judge(finished: &Finished<'_>) -> Result<Snapshot, SampleFailure> {
    decode(completed_stdout(finished)?).map_err(SampleFailure::new)
}

fn decode(bytes: &[u8]) -> Result<Snapshot, FailureKind> {
    let input: UsageInput =
        serde_json::from_slice(bytes).map_err(|_| FailureKind::MalformedOutput)?;
    if input
        .reports
        .iter()
        .any(|report| report.provider == CREDIT_BALANCE_PROVIDER && !report.stamped())
    {
        return Err(FailureKind::ExtensionUnloaded);
    }
    Ok(input.normalize())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageInput {
    generated_at: i64,
    reports: Vec<ReportInput>,
    #[serde(default)]
    accounts_without_usage: Vec<AccountWithoutUsage>,
    #[serde(default)]
    disabled_credentials: Vec<DisabledCredential>,
    #[serde(default)]
    capacity: BTreeMap<String, Vec<Capacity>>,
}

impl UsageInput {
    fn normalize(mut self) -> Snapshot {
        for account in &mut self.accounts_without_usage {
            account.redact();
        }
        for disabled in &mut self.disabled_credentials {
            disabled.redact();
        }
        Snapshot {
            generated_at_ms: self.generated_at,
            reports: self
                .reports
                .into_iter()
                .map(ReportInput::normalize)
                .collect(),
            accounts_without_usage: self.accounts_without_usage,
            disabled_credentials: self.disabled_credentials,
            disabled_providers: Vec::new(),
            disabled_provider_issue: None,
            capacity: self.capacity,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportInput {
    provider: String,
    fetched_at: i64,
    #[serde(default)]
    limits: Vec<Limit>,
    reset_credits: Option<ResetCredits>,
    metadata: Option<ReportMetadata>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReportMetadata {
    #[serde(flatten)]
    account: ReportAccount,
    spice_extension: Option<String>,
}

impl ReportInput {
    fn stamped(&self) -> bool {
        self.metadata
            .as_ref()
            .and_then(|metadata| metadata.spice_extension.as_deref())
            == Some(vendor::omp::CREDIT_BALANCE_STAMP)
    }

    fn normalize(mut self) -> Report {
        let mut account = self
            .metadata
            .take()
            .map(|metadata| metadata.account)
            .unwrap_or_default();
        account.redact();
        for limit in &mut self.limits {
            limit.scope.redact();
        }
        let credit_balance = self
            .limits
            .iter()
            .position(Limit::is_credit_balance)
            .map(|index| self.limits.remove(index).amount);
        Report {
            provider: self.provider,
            fetched_at_ms: self.fetched_at,
            account,
            limits: self.limits,
            credit_balance,
            reset_credits: self.reset_credits,
        }
    }
}

impl Limit {
    /// OMP's normalized raw balance: a windowless credits amount that reports
    /// what remains and no allowance to measure it against. Windowed or capped
    /// credit allowances stay quota meters.
    fn is_credit_balance(&self) -> bool {
        self.window.is_none()
            && self.scope.window_id.as_deref() == Some("balance")
            && self.amount.unit.eq_ignore_ascii_case("credits")
            && self.amount.remaining.is_some()
            && self.amount.limit.is_none()
    }
}

impl ReportAccount {
    fn redact(&mut self) {
        retain_mask(&mut self.email);
        retain_mask(&mut self.account_id);
        retain_mask(&mut self.project_id);
        retain_mask(&mut self.org_id);
        retain_mask(&mut self.org_name);
    }
}

impl Scope {
    fn redact(&mut self) {
        retain_mask(&mut self.account_id);
        retain_mask(&mut self.project_id);
        retain_mask(&mut self.org_id);
    }
}

impl AccountWithoutUsage {
    fn redact(&mut self) {
        retain_mask(&mut self.email);
        retain_mask(&mut self.account_id);
        retain_mask(&mut self.project_id);
        retain_mask(&mut self.org_id);
        retain_mask(&mut self.org_name);
    }
}

impl DisabledCredential {
    fn redact(&mut self) {
        retain_mask(&mut self.email);
        retain_mask(&mut self.account_id);
        retain_mask(&mut self.org_id);
        retain_mask(&mut self.org_name);
    }
}

fn retain_mask(value: &mut Option<String>) {
    if value.as_ref().is_some_and(|value| !value.contains('*')) {
        *value = None;
    }
}

fn wire_generation(state: &State) -> wire::QuotaGeneration {
    wire::QuotaGeneration {
        generation: state.generation,
        refreshing: state.refreshing,
        last_good: state.last_good.as_deref().map(wire_snapshot).map(Arc::new),
        latest_attempt: state.latest_attempt.as_ref().map(|attempt| match attempt {
            Attempt::Succeeded { finished_at_ms } => wire::QuotaAttempt::Succeeded {
                finished_at_ms: *finished_at_ms,
            },
            Attempt::Failed(failure) => wire::QuotaAttempt::Failed {
                finished_at_ms: failure.finished_at_ms,
                kind: wire_failure_kind(failure.kind),
                exit_code: failure.exit_code,
            },
        }),
    }
}

fn wire_failure_kind(kind: FailureKind) -> wire::QuotaFailureKind {
    match kind {
        FailureKind::ExecutableUnavailable => wire::QuotaFailureKind::ExecutableUnavailable,
        FailureKind::Spawn => wire::QuotaFailureKind::Spawn,
        FailureKind::Timeout => wire::QuotaFailureKind::Timeout,
        FailureKind::StdoutOverflow => wire::QuotaFailureKind::StdoutOverflow,
        FailureKind::StderrOverflow => wire::QuotaFailureKind::StderrOverflow,
        FailureKind::NonzeroExit => wire::QuotaFailureKind::NonzeroExit,
        FailureKind::MalformedOutput => wire::QuotaFailureKind::MalformedOutput,
        FailureKind::ExtensionUnloaded => wire::QuotaFailureKind::ExtensionUnloaded,
        FailureKind::OutputIncomplete => wire::QuotaFailureKind::OutputIncomplete,
        FailureKind::Supervisor => wire::QuotaFailureKind::Supervisor,
        FailureKind::WorkerStart => wire::QuotaFailureKind::WorkerStart,
        FailureKind::WorkerLost => wire::QuotaFailureKind::WorkerLost,
    }
}

fn wire_snapshot(snapshot: &Snapshot) -> wire::QuotaSnapshot {
    wire::QuotaSnapshot {
        generated_at_ms: snapshot.generated_at_ms,
        reports: snapshot.reports.iter().map(wire_report).collect(),
        accounts_without_usage: snapshot
            .accounts_without_usage
            .iter()
            .map(|account| wire::QuotaAccountWithoutUsage {
                provider: account.provider.clone(),
                credential_type: account.credential_type.clone(),
                account: wire_account_fields(
                    &account.email,
                    &account.account_id,
                    &account.project_id,
                    &account.org_id,
                    &account.org_name,
                ),
                authorized_at_ms: account.authorized_at,
            })
            .collect(),
        disabled_credentials: snapshot
            .disabled_credentials
            .iter()
            .map(|credential| wire::QuotaDisabledCredential {
                provider: credential.provider.clone(),
                credential_type: credential.credential_type.clone(),
                account: wire_account_fields(
                    &credential.email,
                    &credential.account_id,
                    &None,
                    &credential.org_id,
                    &credential.org_name,
                ),
                disabled_at_ms: credential.disabled_at_ms,
            })
            .collect(),
        capacity: snapshot
            .capacity
            .iter()
            .map(|(provider, windows)| wire::QuotaProviderCapacity {
                provider: provider.clone(),
                windows: windows
                    .iter()
                    .map(|capacity| wire::QuotaCapacity {
                        window: capacity.window.clone(),
                        duration_ms: capacity.duration_ms,
                        meter: capacity.meter.clone(),
                        accounts: capacity.accounts,
                        used_accounts: quantity(capacity.used_accounts),
                        remaining_accounts: quantity(capacity.remaining_accounts),
                    })
                    .collect(),
            })
            .collect(),
        disabled_providers: snapshot.disabled_providers.clone(),
        disabled_provider_issue: snapshot.disabled_provider_issue.map(wire_failure_kind),
    }
}

fn wire_report(report: &Report) -> wire::QuotaReport {
    wire::QuotaReport {
        provider: report.provider.clone(),
        fetched_at_ms: report.fetched_at_ms,
        account: wire::QuotaAccount {
            email: report.account.email.clone(),
            account_id: report.account.account_id.clone(),
            project_id: report.account.project_id.clone(),
            org_id: report.account.org_id.clone(),
            org_name: report.account.org_name.clone(),
            plan_type: report.account.plan_type.clone(),
            allowed: report.account.allowed,
            limit_reached: report.account.limit_reached,
        },
        limits: report
            .limits
            .iter()
            .map(|limit| wire::QuotaLimit {
                id: limit.id.clone(),
                label: limit.label.clone(),
                scope: wire::QuotaScope {
                    provider: limit.scope.provider.clone(),
                    account_id: limit.scope.account_id.clone(),
                    project_id: limit.scope.project_id.clone(),
                    org_id: limit.scope.org_id.clone(),
                    model_id: limit.scope.model_id.clone(),
                    tier: limit.scope.tier.clone(),
                    window_id: limit.scope.window_id.clone(),
                    shared: limit.scope.shared,
                    shared_group: limit.scope.shared_group.clone(),
                },
                window: limit.window.as_ref().map(|window| wire::QuotaWindow {
                    id: window.id.clone(),
                    label: window.label.clone(),
                    duration_ms: window.duration_ms,
                    resets_at: window.resets_at,
                    reset_label: window.reset_label.clone(),
                }),
                amount: wire_amount(&limit.amount),
                status: limit.status.clone(),
            })
            .collect(),
        credit_balance: report.credit_balance.as_ref().map(wire_amount),
        reset_credits: report
            .reset_credits
            .as_ref()
            .map(|credits| wire::QuotaResetCredits {
                available_count: credits.available_count,
                redeemable_count: credits.redeemable_count,
                eligible: credits.eligible,
                cooldown_until: credits.cooldown_until.clone(),
                credits: credits
                    .credits
                    .iter()
                    .map(|credit| wire::QuotaResetCredit {
                        title: credit.title.clone(),
                        program: credit.program.clone(),
                        remaining_count: credit.remaining_count,
                        usable: credit.usable,
                        requires_limit: credit.requires_limit,
                        granted_at: credit.granted_at.clone(),
                        expires_at: credit.expires_at.clone(),
                        status: credit.status.clone(),
                    })
                    .collect(),
            }),
    }
}

fn wire_account_fields(
    email: &Option<String>,
    account_id: &Option<String>,
    project_id: &Option<String>,
    org_id: &Option<String>,
    org_name: &Option<String>,
) -> wire::QuotaAccount {
    wire::QuotaAccount {
        email: email.clone(),
        account_id: account_id.clone(),
        project_id: project_id.clone(),
        org_id: org_id.clone(),
        org_name: org_name.clone(),
        ..wire::QuotaAccount::default()
    }
}

fn wire_amount(amount: &Amount) -> wire::QuotaAmount {
    wire::QuotaAmount {
        used: amount.used.map(quantity),
        limit: amount.limit.map(quantity),
        remaining: amount.remaining.map(quantity),
        used_fraction: amount.used_fraction.map(quantity),
        remaining_fraction: amount.remaining_fraction.map(quantity),
        unit: amount.unit.clone(),
    }
}

fn quantity(value: f64) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests;
