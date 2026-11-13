//! Pure presentation rules for redacted provider quota generations.
use std::collections::BTreeSet;

use wire::{QuotaAttempt, QuotaFailureKind, QuotaGeneration};

/// Operator-visible sampler condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Condition {
    NeverSampled,
    Current,
    StaleLastGood,
    Unsupported,
    Failed,
}

impl Condition {
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::NeverSampled => "never-sampled",
            Self::Current => "current",
            Self::StaleLastGood => "stale-last-good",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed-attempt",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NeverSampled => "Not sampled yet",
            Self::Current => "Current",
            Self::StaleLastGood => "Last good sample; refresh failed",
            Self::Unsupported => "Quota sampling unsupported",
            Self::Failed => "Quota sample failed",
        }
    }
}

/// Distinguishable words and chronology for one generation.
///
/// A sample in flight is routine and says nothing: the presentation follows
/// only finished attempts, so a refresh never changes what the operator reads.
pub(crate) struct Presentation {
    pub(crate) condition: Condition,
    pub(crate) detail: &'static str,
    pub(crate) sampled_at_ms: Option<i64>,
    /// Retained data whose latest attempt did not fail; its age then carries
    /// nothing worth reading.
    pub(crate) fresh: bool,
}

pub(crate) fn present(generation: &QuotaGeneration) -> Presentation {
    let sampled_at_ms = generation
        .last_good
        .as_ref()
        .map(|snapshot| snapshot.generated_at_ms);
    let failed = matches!(generation.latest_attempt, Some(QuotaAttempt::Failed { .. }));
    let condition = if sampled_at_ms.is_some() && failed {
        Condition::StaleLastGood
    } else if sampled_at_ms.is_some() {
        Condition::Current
    } else if matches!(
        generation.latest_attempt,
        Some(QuotaAttempt::Failed {
            kind: QuotaFailureKind::ExecutableUnavailable,
            ..
        })
    ) {
        Condition::Unsupported
    } else if failed {
        Condition::Failed
    } else {
        Condition::NeverSampled
    };
    let detail = match condition {
        Condition::Current => "The most recent sample completed successfully.",
        Condition::StaleLastGood => {
            "The latest refresh failed; retained successful data remains visible and continues to age."
        }
        Condition::Unsupported => "The configured OMP executable cannot provide quota usage.",
        Condition::Failed => "No successful provider quota sample is available.",
        Condition::NeverSampled => "The resident has not completed a provider quota sample.",
    };
    Presentation {
        condition,
        detail,
        sampled_at_ms,
        fresh: sampled_at_ms.is_some() && !failed,
    }
}
/// One account-limit card after provider policy is applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ProviderCard<'a> {
    Active {
        index: usize,
        report: &'a wire::QuotaReport,
    },
    Disabled {
        provider: &'a str,
    },
}

/// Apply provider-level availability before rendering account cards.
///
/// Disabled providers collapse to one placeholder even if a retained usage
/// sample still carries several of their account reports.
pub(crate) fn provider_cards(snapshot: &wire::QuotaSnapshot) -> Vec<ProviderCard<'_>> {
    let disabled: BTreeSet<&str> = snapshot
        .disabled_providers
        .iter()
        .map(String::as_str)
        .collect();
    let mut cards = snapshot
        .reports
        .iter()
        .enumerate()
        .filter(|(_, report)| !disabled.contains(report.provider.as_str()))
        .map(|(index, report)| ProviderCard::Active { index, report })
        .collect::<Vec<_>>();
    cards.extend(
        snapshot
            .disabled_providers
            .iter()
            .map(|provider| ProviderCard::Disabled { provider }),
    );
    cards
}

pub(crate) fn failure_label(kind: QuotaFailureKind) -> &'static str {
    match kind {
        QuotaFailureKind::ExecutableUnavailable => "executable unavailable",
        QuotaFailureKind::Spawn => "process launch failed",
        QuotaFailureKind::Timeout => "sample timed out",
        QuotaFailureKind::StdoutOverflow => "usage output exceeded its bound",
        QuotaFailureKind::StderrOverflow => "diagnostic output exceeded its bound",
        QuotaFailureKind::NonzeroExit => "OMP returned a nonzero exit",
        QuotaFailureKind::MalformedOutput => "OMP returned malformed usage data",
        QuotaFailureKind::ExtensionUnloaded => "OMP did not load the credit-balance extension",
        QuotaFailureKind::OutputIncomplete => "OMP output did not drain completely",
        QuotaFailureKind::Supervisor => "process supervision failed",
        QuotaFailureKind::WorkerStart => "sampler worker could not start",
        QuotaFailureKind::WorkerLost => "sampler worker ended without a result",
    }
}

pub(crate) fn account_label(account: &wire::QuotaAccount) -> String {
    account
        .email
        .as_ref()
        .or(account.account_id.as_ref())
        .or(account.org_name.as_ref())
        .or(account.org_id.as_ref())
        .or(account.project_id.as_ref())
        .cloned()
        .unwrap_or_else(|| "Redacted account".to_owned())
}

/// Whether retained quota data outlived a failed refresh. An in-flight refresh
/// keeps the retained figure steady instead of flashing on every sample.
pub(crate) fn last_good_stale(generation: &QuotaGeneration) -> bool {
    generation.last_good.is_some()
        && matches!(generation.latest_attempt, Some(QuotaAttempt::Failed { .. }))
}

/// Plan-weighted weekly quota grouped into provider and account layers.
///
/// Providers and their accounts are ordered by allocated capacity, heaviest
/// first; equal capacities retain report order. Reports without a shared weekly
/// limit do not contribute to the global weekly quota.
pub(crate) struct WeeklyQuotaStack<'a> {
    pub bands: Vec<WeeklyQuotaBand<'a>>,
    pub capacity: f64,
    pub remaining: f64,
}

pub(crate) struct WeeklyQuotaBand<'a> {
    pub provider: &'a str,
    pub capacity: f64,
    pub available: f64,
    pub accounts: Vec<WeeklyQuotaAccount>,
}

pub(crate) struct WeeklyQuotaAccount {
    pub capacity: f64,
    pub available: f64,
    report_index: usize,
}

/// One account's layer in the drawn stack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct WeeklyQuotaLayer {
    /// The account's place in capacity order, which picks its accent; it
    /// follows the account through any reorder.
    pub accent: usize,
    pub available: f64,
}

impl WeeklyQuotaStack<'_> {
    /// Account layers bottom to top: the most available capacity lowest and
    /// the account nearest to running out highest. Equal availability keeps
    /// capacity order, so equal refreshes draw the same stack.
    pub(crate) fn layers(&self) -> Vec<WeeklyQuotaLayer> {
        let mut layers = self
            .bands
            .iter()
            .flat_map(|band| &band.accounts)
            .enumerate()
            .map(|(accent, account)| WeeklyQuotaLayer {
                accent,
                available: account.available,
            })
            .collect::<Vec<_>>();
        layers.sort_by(|lower, upper| upper.available.total_cmp(&lower.available));
        layers
    }

    /// Stable fill accent for each source report. Accounts without a shared
    /// weekly limit have no layer in the Spice button and remain unaccented.
    pub(crate) fn report_accents(&self, report_count: usize) -> Vec<Option<usize>> {
        let mut accents = vec![None; report_count];
        for (accent, account) in self
            .bands
            .iter()
            .flat_map(|band| &band.accounts)
            .enumerate()
        {
            accents[account.report_index] = Some(accent);
        }
        accents
    }
}

/// Build the account layers behind the global weekly-quota presentation.
pub(crate) fn weekly_quota_stack(snapshot: &wire::QuotaSnapshot) -> Option<WeeklyQuotaStack<'_>> {
    let mut bands: Vec<WeeklyQuotaBand<'_>> = Vec::new();
    let mut capacity = 0.0;
    let mut weighted_remaining = 0.0;
    for (report_index, report) in snapshot.reports.iter().enumerate() {
        let Some(remaining) = binding_remaining_fraction(report) else {
            continue;
        };
        let account_capacity = plan_capacity_units(report);
        let account_available = remaining * account_capacity;
        capacity += account_capacity;
        weighted_remaining += account_available;
        let band = bands
            .iter_mut()
            .find(|band| band.provider.eq_ignore_ascii_case(&report.provider));
        if let Some(band) = band {
            band.capacity += account_capacity;
            band.available += account_available;
            band.accounts.push(WeeklyQuotaAccount {
                capacity: account_capacity,
                available: account_available,
                report_index,
            });
        } else {
            bands.push(WeeklyQuotaBand {
                provider: &report.provider,
                capacity: account_capacity,
                available: account_available,
                accounts: vec![WeeklyQuotaAccount {
                    capacity: account_capacity,
                    available: account_available,
                    report_index,
                }],
            });
        }
    }
    for band in &mut bands {
        band.accounts
            .sort_by(|left, right| right.capacity.total_cmp(&left.capacity));
    }
    bands.sort_by(|left, right| right.capacity.total_cmp(&left.capacity));
    (capacity > 0.0).then(|| WeeklyQuotaStack {
        bands,
        capacity,
        remaining: (weighted_remaining / capacity).clamp(0.0, 1.0),
    })
}

/// Weekly remaining capacity across every account as one fraction in
/// `0..=1`, each account weighted by its plan capacity; `None` when no
/// account reports a weekly window.
pub(crate) fn weekly_remaining(snapshot: &wire::QuotaSnapshot) -> Option<f64> {
    let (remaining, capacity) = snapshot
        .reports
        .iter()
        .filter_map(|report| {
            let remaining = binding_remaining_fraction(report)?;
            let capacity = plan_capacity_units(report);
            Some((remaining * capacity, capacity))
        })
        .fold((0.0, 0.0), |(remaining, capacity), account| {
            (remaining + account.0, capacity + account.1)
        });
    (capacity > 0.0).then(|| (remaining / capacity).clamp(0.0, 1.0))
}

fn binding_remaining_fraction(report: &wire::QuotaReport) -> Option<f64> {
    report
        .limits
        .iter()
        .filter(|limit| limit.scope.shared == Some(true) && is_weekly_limit(limit))
        .filter_map(limit_remaining_fraction)
        .reduce(f64::min)
}

fn is_weekly_limit(limit: &wire::QuotaLimit) -> bool {
    const WEEK_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

    limit.window.as_ref().is_some_and(|window| {
        window.duration_ms == Some(WEEK_MS) || is_weekly_id(window.id.as_str())
    }) || limit.scope.window_id.as_deref().is_some_and(is_weekly_id)
}

fn is_weekly_id(id: &str) -> bool {
    ["7d", "7-day", "7-days", "weekly"]
        .iter()
        .any(|candidate| id.eq_ignore_ascii_case(candidate))
}

pub(crate) fn limit_remaining_fraction(limit: &wire::QuotaLimit) -> Option<f64> {
    let amount = &limit.amount;
    parse_quantity(amount.remaining_fraction.as_ref())
        .or_else(|| parse_quantity(amount.used_fraction.as_ref()).map(|used| 1.0 - used))
        .or_else(|| {
            let remaining = parse_quantity(amount.remaining.as_ref())?;
            let limit = parse_quantity(amount.limit.as_ref())?;
            (limit > 0.0).then_some(remaining / limit)
        })
        .filter(|fraction| fraction.is_finite())
        .map(|fraction| fraction.clamp(0.0, 1.0))
}

fn parse_quantity(value: Option<&String>) -> Option<f64> {
    value?.parse::<f64>().ok().filter(|value| value.is_finite())
}

/// The amount beside a limit's meter, leading with remaining capacity: a
/// percent limit is always 100, so it reads `N% left`; a counted limit reads
/// `R / L unit left`. A remainder the provider omits is derived from the limit
/// and the amount used; only when neither is possible does the label fall back
/// to the amount used.
pub(crate) fn amount_text(amount: &wire::QuotaAmount) -> String {
    let percent = amount.unit.eq_ignore_ascii_case("percent");
    let quantity = |value: &str| {
        if percent {
            format!("{}%", whole_percent(value))
        } else {
            format!("{value} {}", amount.unit)
        }
    };
    let limit = amount.limit.as_deref();
    match (
        remaining_amount(amount).as_deref(),
        amount.used.as_deref(),
        limit,
    ) {
        (Some(remaining), _, _) if percent => format!("{} left", quantity(remaining)),
        (Some(remaining), _, Some(limit)) => format!("{remaining} / {} left", quantity(limit)),
        (Some(remaining), _, None) => format!("{} left", quantity(remaining)),
        (None, Some(used), Some(limit)) if !percent => format!("{used} / {}", quantity(limit)),
        (None, Some(used), _) => format!("{} used", quantity(used)),
        (None, None, _) => "usage unknown".to_owned(),
    }
}

/// What is left of a limit as a decimal quantity in the limit's own unit:
/// the provider's remainder when reported, otherwise the limit (100 for a
/// percent limit) less the amount used, settled at a millionth and floored at
/// zero. `None` when neither can be known.
fn remaining_amount(amount: &wire::QuotaAmount) -> Option<String> {
    if let Some(remaining) = &amount.remaining {
        return Some(remaining.clone());
    }
    let used = parse_quantity(amount.used.as_ref())?;
    let limit = match parse_quantity(amount.limit.as_ref()) {
        Some(limit) => limit,
        None if amount.unit.eq_ignore_ascii_case("percent") => 100.0,
        None => return None,
    };
    let settled = (((limit - used) * 1e6).round() / 1e6).max(0.0);
    Some(format!("{}", settled + 0.0))
}

/// A provider's percent quantity as a whole number, truncated toward zero.
///
/// Providers derive percentages in binary floating point, so `7` may arrive as
/// `7.000000000000001` and `29` as `28.999999999999996`. The value is settled
/// at a millionth of a percent before truncation so such noise neither
/// surfaces nor drops a whole point. Text that is not a finite number stays
/// verbatim.
#[must_use]
pub fn whole_percent(value: &str) -> String {
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map_or_else(
            || value.to_owned(),
            |value| {
                let settled = (value * 1e6).round() / 1e6;
                let whole = if settled.is_finite() { settled } else { value }.trunc();
                format!("{}", whole + 0.0)
            },
        )
}

/// A provider's credit balance as whole credits, truncated toward zero so a
/// fractional balance never reads as more than can be spent. The value is
/// settled at a millionth first, as in [`whole_percent`], so floating-point
/// noise does not drop a whole credit. `None` when the balance is not a finite
/// number.
#[must_use]
pub fn whole_credits(value: &str) -> Option<f64> {
    let value = value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())?;
    let settled = (value * 1e6).round() / 1e6;
    Some(if settled.is_finite() { settled } else { value }.trunc() + 0.0)
}

fn plan_capacity_units(report: &wire::QuotaReport) -> f64 {
    const FREE: f64 = 0.2;
    const BASELINE: f64 = 1.0;
    const LOWER_PRO: f64 = 5.0;
    const HIGH_PRO: f64 = 20.0;

    let Some(plan) = report
        .account
        .plan_type
        .as_deref()
        .map(str::trim)
        .filter(|plan| !plan.is_empty())
    else {
        return HIGH_PRO;
    };
    // Claude Max is sold at 5x and 20x. A plan named only `max` carries no
    // tier, so it counts as the larger one, like any unrecognized plan.
    if plan_matches(
        plan,
        &[
            "20x",
            "max20",
            "max20x",
            "max_20x",
            "max-20x",
            "max 20x",
            "pro20",
            "pro20x",
            "pro_20x",
            "pro-20x",
            "pro 20x",
            "chatgpt_pro_20x",
            "default_claude_max_20x",
        ],
    ) {
        return HIGH_PRO;
    }
    if plan_matches(plan, &["free", "chatgpt_free", "go", "chatgpt_go"]) {
        return FREE;
    }
    if plan_matches(
        plan,
        &[
            "5x",
            "max5",
            "max5x",
            "max_5x",
            "max-5x",
            "max 5x",
            "pro5",
            "pro5x",
            "pro_5x",
            "pro-5x",
            "pro 5x",
            "chatgpt_pro_5x",
            "default_claude_max_5x",
            "prolite",
            "pro_lite",
            "pro-lite",
            "pro lite",
        ],
    ) {
        return LOWER_PRO;
    }
    if report.provider.eq_ignore_ascii_case("openai-codex")
        && plan_matches(plan, &["pro", "chatgpt_pro"])
    {
        return HIGH_PRO;
    }
    if plan_matches(
        plan,
        &[
            "1x",
            "plus",
            "chatgpt_plus",
            "claude_pro",
            "pro_1x",
            "pro-1x",
            "pro 1x",
        ],
    ) || (!report.provider.eq_ignore_ascii_case("openai-codex")
        && plan.eq_ignore_ascii_case("pro"))
    {
        return BASELINE;
    }
    HIGH_PRO
}

fn plan_matches(plan: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| plan.eq_ignore_ascii_case(candidate))
}
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn sampler_conditions_keep_retained_failure_distinct_and_ignore_refreshes() {
        let mut generation = QuotaGeneration::default();
        let unchanged_by_refresh = |generation: &mut QuotaGeneration| {
            let settled = present(generation);
            generation.refreshing = true;
            let refreshing = present(generation);
            generation.refreshing = false;
            assert_eq!(
                (settled.condition, settled.detail, settled.fresh),
                (refreshing.condition, refreshing.detail, refreshing.fresh),
                "a sample in flight changes nothing the operator reads"
            );
            settled.condition
        };
        assert_eq!(
            unchanged_by_refresh(&mut generation),
            Condition::NeverSampled
        );

        generation.latest_attempt = Some(QuotaAttempt::Failed {
            finished_at_ms: 20,
            kind: QuotaFailureKind::ExecutableUnavailable,
            exit_code: None,
        });
        assert_eq!(
            unchanged_by_refresh(&mut generation),
            Condition::Unsupported
        );
        generation.latest_attempt = Some(QuotaAttempt::Failed {
            finished_at_ms: 21,
            kind: QuotaFailureKind::Timeout,
            exit_code: None,
        });
        assert_eq!(unchanged_by_refresh(&mut generation), Condition::Failed);

        generation.last_good = Some(Arc::new(snapshot()));
        assert_eq!(
            unchanged_by_refresh(&mut generation),
            Condition::StaleLastGood
        );
        assert_eq!(present(&generation).sampled_at_ms, Some(10));
        assert!(!present(&generation).fresh);

        generation.latest_attempt = Some(QuotaAttempt::Succeeded { finished_at_ms: 30 });
        assert_eq!(unchanged_by_refresh(&mut generation), Condition::Current);
        assert!(present(&generation).fresh);
    }

    #[test]
    fn weekly_stack_preserves_provider_bands_and_plan_weighted_accounts() {
        let snapshot = snapshot();
        let stack = weekly_quota_stack(&snapshot).expect("weekly quota");

        assert_eq!(stack.bands.len(), 2);
        assert_eq!(stack.bands[0].provider, "openai-codex");
        assert_eq!(stack.bands[0].accounts.len(), 3);
        assert_eq!(
            stack.bands[0]
                .accounts
                .iter()
                .map(|account| account.capacity)
                .collect::<Vec<_>>(),
            vec![20.0, 20.0, 0.2],
        );
        assert_eq!(stack.bands[1].provider, "anthropic");
        assert_eq!(stack.bands[1].accounts.len(), 1);
        assert!((stack.bands[0].capacity / stack.capacity - 40.2 / 60.2).abs() < f64::EPSILON);
        assert!((stack.bands[0].available / stack.capacity - 10.2 / 60.2).abs() < f64::EPSILON);
        assert!((stack.bands[1].capacity / stack.capacity - 20.0 / 60.2).abs() < f64::EPSILON);
        assert!((stack.bands[1].available / stack.capacity - 20.0 / 60.2).abs() < f64::EPSILON);
        assert!((stack.remaining - 30.2 / 60.2).abs() < f64::EPSILON);
    }

    #[test]
    fn high_capacity_is_the_default_without_lower_tier_evidence() {
        let weekly = &[("7d", None, "1")];
        assert_eq!(
            plan_capacity_units(&report("openai-codex", None, weekly)),
            20.0
        );
        assert_eq!(
            plan_capacity_units(&report("other", Some(""), weekly)),
            20.0
        );
        assert_eq!(
            plan_capacity_units(&report("other", Some("unrecognized"), weekly)),
            20.0
        );
        assert_eq!(
            plan_capacity_units(&report("openai-codex", Some("free"), weekly)),
            0.2
        );
        assert_eq!(
            plan_capacity_units(&report("openai-codex", Some("plus"), weekly)),
            1.0
        );
        assert_eq!(
            plan_capacity_units(&report("openai-codex", Some("pro_5x"), weekly)),
            5.0
        );
        assert_eq!(
            plan_capacity_units(&report("openai-codex", Some("prolite"), weekly)),
            5.0
        );
        assert_eq!(
            plan_capacity_units(&report("anthropic", Some("max"), weekly)),
            20.0
        );
        assert_eq!(
            plan_capacity_units(&report("anthropic", Some("default_claude_max_5x"), weekly)),
            5.0
        );

        let snapshot = wire::QuotaSnapshot {
            generated_at_ms: 10,
            reports: vec![
                report("openai-codex", Some("pro"), &[("7d", None, "0.5")]),
                report("openai-codex", Some("chatgpt_pro"), &[("7d", None, "0.5")]),
                report("anthropic", None, &[("7d", None, "0.95")]),
            ],
            accounts_without_usage: Vec::new(),
            disabled_credentials: Vec::new(),
            disabled_providers: Vec::new(),
            disabled_provider_issue: None,
            capacity: Vec::new(),
        };
        let stack = weekly_quota_stack(&snapshot).expect("weekly quota");
        let openai = stack
            .bands
            .iter()
            .find(|band| band.provider == "openai-codex")
            .expect("OpenAI band");
        let anthropic = stack
            .bands
            .iter()
            .find(|band| band.provider == "anthropic")
            .expect("Anthropic band");

        assert!((openai.available - 20.0).abs() < f64::EPSILON);
        assert!((anthropic.available - 19.0).abs() < f64::EPSILON);
        assert!((stack.remaining - 39.0 / 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn layers_stack_by_remaining_capacity_and_keep_capacity_order_accents() {
        let layers = |snapshot: &wire::QuotaSnapshot| {
            weekly_quota_stack(snapshot)
                .expect("weekly quota")
                .layers()
                .iter()
                .map(|layer| (layer.accent, layer.available))
                .collect::<Vec<_>>()
        };
        // Capacity order assigns accents: pro 0, pro_20x 1, free 2, anthropic 3.
        let mut snapshot = snapshot();
        assert_eq!(
            layers(&snapshot),
            vec![(3, 20.0), (0, 10.0), (2, 0.2), (1, 0.0)],
            "fullest lowest, nearest to empty highest"
        );

        // Pro refills to tie Anthropic: it moves with its color, and the tie
        // keeps capacity order.
        snapshot.reports[2].limits[1].amount.remaining_fraction = Some("1".to_owned());
        assert_eq!(
            layers(&snapshot),
            vec![(0, 20.0), (3, 20.0), (2, 0.2), (1, 0.0)],
        );

        let stack = weekly_quota_stack(&snapshot).expect("weekly quota");
        assert_eq!(
            stack.report_accents(snapshot.reports.len()),
            vec![Some(3), Some(2), Some(0), Some(1)],
            "quota cards keep the accents assigned to the same Spice-button layers"
        );
    }

    #[test]
    fn disabled_provider_collapses_retained_accounts_into_one_placeholder() {
        let mut disabled = snapshot();
        disabled.disabled_providers = vec!["openai-codex".to_owned()];
        let cards = provider_cards(&disabled);
        assert_eq!(cards.len(), 2);
        assert!(matches!(
            cards[0],
            ProviderCard::Active { report, .. } if report.provider == "anthropic"
        ));
        assert_eq!(
            cards[1],
            ProviderCard::Disabled {
                provider: "openai-codex"
            }
        );
    }

    fn snapshot() -> wire::QuotaSnapshot {
        const FIVE_HOURS_MS: u64 = 5 * 60 * 60 * 1_000;
        const WEEK_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

        wire::QuotaSnapshot {
            generated_at_ms: 10,
            reports: vec![
                report("anthropic", None, &[("provider-week", Some(WEEK_MS), "1")]),
                report("openai-codex", Some("free"), &[("weekly", None, "1")]),
                report(
                    "openai-codex",
                    Some("pro"),
                    &[
                        ("5h", Some(FIVE_HOURS_MS), "0"),
                        ("7d", Some(WEEK_MS), "0.5"),
                    ],
                ),
                report("openai-codex", Some("pro_20x"), &[("7-days", None, "0")]),
            ],
            accounts_without_usage: Vec::new(),
            disabled_credentials: Vec::new(),
            disabled_providers: Vec::new(),
            disabled_provider_issue: None,
            capacity: Vec::new(),
        }
    }

    fn report(
        provider: &str,
        plan_type: Option<&str>,
        limits: &[(&str, Option<u64>, &str)],
    ) -> wire::QuotaReport {
        let account = wire::QuotaAccount {
            plan_type: plan_type.map(ToOwned::to_owned),
            ..wire::QuotaAccount::default()
        };
        wire::QuotaReport {
            provider: provider.to_owned(),
            fetched_at_ms: 10,
            account,
            limits: limits
                .iter()
                .enumerate()
                .map(
                    |(index, (window_id, duration_ms, remaining))| wire::QuotaLimit {
                        id: format!("{provider}:{index}"),
                        label: format!("Window {index}"),
                        scope: wire::QuotaScope {
                            provider: provider.to_owned(),
                            account_id: None,
                            project_id: None,
                            org_id: None,
                            model_id: None,
                            tier: None,
                            window_id: Some((*window_id).to_owned()),
                            shared: Some(true),
                            shared_group: None,
                        },
                        window: Some(wire::QuotaWindow {
                            id: (*window_id).to_owned(),
                            label: format!("Window {index}"),
                            duration_ms: *duration_ms,
                            resets_at: None,
                            reset_label: None,
                        }),
                        amount: wire::QuotaAmount {
                            used: None,
                            limit: None,
                            remaining: None,
                            used_fraction: None,
                            remaining_fraction: Some((*remaining).to_owned()),
                            unit: "percent".to_owned(),
                        },
                        status: None,
                    },
                )
                .collect(),
            credit_balance: None,
            reset_credits: None,
        }
    }

    #[test]
    fn percentages_truncate_to_whole_points_without_float_noise() {
        for (reported, shown) in [
            ("7.000000000000001", "7"),
            ("28.999999999999996", "29"),
            ("4.9", "4"),
            ("99.99", "99"),
            ("0", "0"),
            ("-0.4", "0"),
            ("100", "100"),
            ("n/a", "n/a"),
        ] {
            assert_eq!(whole_percent(reported), shown, "{reported}");
        }
    }

    #[test]
    fn credit_balances_truncate_to_whole_credits_past_float_noise() {
        for (reported, shown) in [
            ("61500.25", Some(61_500.0)),
            ("62000", Some(62_000.0)),
            ("61499.99999999999", Some(61_500.0)),
            ("0.9", Some(0.0)),
            ("-0.4", Some(0.0)),
            ("n/a", None),
        ] {
            assert_eq!(whole_credits(reported), shown, "{reported}");
        }
    }

    fn amount(
        used: Option<&str>,
        limit: Option<&str>,
        remaining: Option<&str>,
        unit: &str,
    ) -> wire::QuotaAmount {
        wire::QuotaAmount {
            used: used.map(str::to_owned),
            limit: limit.map(str::to_owned),
            remaining: remaining.map(str::to_owned),
            used_fraction: None,
            remaining_fraction: None,
            unit: unit.to_owned(),
        }
    }

    #[test]
    fn remaining_amount_prefers_the_report_and_otherwise_derives_it_from_usage() {
        for (case, left) in [
            // A percent limit is out of 100 even when the limit is omitted.
            (amount(Some("28"), None, None, "percent"), Some("72")),
            (
                amount(Some("28.999999999999996"), None, None, "percent"),
                Some("71"),
            ),
            // The provider's own remainder wins over derivation.
            (
                amount(Some("40"), Some("100"), Some("55"), "requests"),
                Some("55"),
            ),
            // Counted: limit less used, without float noise, floored at zero.
            (
                amount(Some("40"), Some("100"), None, "requests"),
                Some("60"),
            ),
            (amount(Some("0.1"), Some("0.3"), None, "usd"), Some("0.2")),
            (
                amount(Some("120"), Some("100"), None, "requests"),
                Some("0"),
            ),
            // Underivable: no limit for a counted unit, unparsable, or no usage.
            (amount(Some("40"), None, None, "requests"), None),
            (amount(Some("40"), Some("n/a"), None, "requests"), None),
            (amount(None, None, None, "percent"), None),
        ] {
            assert_eq!(remaining_amount(&case).as_deref(), left, "{case:?}");
        }
    }
}
