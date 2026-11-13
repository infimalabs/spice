use super::*;
use crate::browser::clock::{Age, Countdown, local_full_millis};

pub(super) fn render_sidebar(runtime: Option<&AppRuntime>) -> Html {
    render(
        runtime,
        "spice",
        classes!("app-sheet-section", "app-spice-quota"),
    )
}

fn render(runtime: Option<&AppRuntime>, scope: &str, class: Classes) -> Html {
    let heading = format!("{scope}-providers-heading");
    let Some(generation) = runtime.and_then(AppRuntime::quota) else {
        return section_unavailable(class, heading);
    };
    let presentation = crate::quota::present(generation);
    let snapshot = generation.last_good.as_deref();
    html! {
        <section
            {class}
            aria-labelledby={heading.clone()}
            data-quota-state={presentation.condition.key()}
        >
            <div class="section-heading">
                <h2 id={heading}>{"Providers"}</h2>
            </div>
            if !presentation.fresh {
                <div class="quota-status" role="status" aria-live="polite">
                    <strong>{presentation.condition.label()}</strong>
                    <span>{presentation.detail}</span>
                    if let Some(sampled_at) = presentation.sampled_at_ms {
                        <span>{"Sample age "}<Age authored={sampled_at} class={classes!("app-quota-age")} title="Age of retained successful quota data" /></span>
                    }
                </div>
            }
            {render_attempt(generation.latest_attempt.as_ref())}
            if let Some(snapshot) = snapshot {
                {render_provider_policy_issue(snapshot.disabled_provider_issue)}
                {render_reports(scope, snapshot)}
                {render_accounts_without_usage(scope, &snapshot.accounts_without_usage)}
                {render_disabled(scope, &snapshot.disabled_credentials)}
            }
        </section>
    }
}

fn section_unavailable(class: Classes, heading: String) -> Html {
    html! {
        <section {class} aria-labelledby={heading.clone()} data-quota-state="unavailable">
            <div class="section-heading"><h2 id={heading}>{"Providers"}</h2></div>
            <p class="quota-status" role="status">{"No admitted quota generation is available."}</p>
        </section>
    }
}

fn render_attempt(attempt: Option<&wire::QuotaAttempt>) -> Html {
    let Some(wire::QuotaAttempt::Failed {
        kind,
        exit_code,
        finished_at_ms,
    }) = attempt
    else {
        return Html::default();
    };
    let exit = exit_code
        .map(|code| format!("; exit {code}"))
        .unwrap_or_default();
    html! {
        <p class="quota-attempt" role="alert">
            {format!("Latest attempt: {}{exit}. ", crate::quota::failure_label(*kind))}
            <Age authored={i64::try_from(*finished_at_ms).unwrap_or(i64::MAX)} class={classes!("app-quota-age")} title="Age of latest failed quota attempt" />
            {" ago"}
        </p>
    }
}
fn render_provider_policy_issue(issue: Option<wire::QuotaFailureKind>) -> Html {
    let Some(issue) = issue else {
        return Html::default();
    };
    html! {
        <p class="quota-attempt" role="alert">
            {"Provider availability could not be refreshed: "}
            {crate::quota::failure_label(issue)}
            {". Usage remains current; provider availability is last known."}
        </p>
    }
}

fn render_reports(scope: &str, snapshot: &wire::QuotaSnapshot) -> Html {
    let heading = format!("{scope}-quota-accounts-heading");
    let accents = crate::quota::weekly_quota_stack(snapshot)
        .map(|stack| stack.report_accents(snapshot.reports.len()));
    let cards = crate::quota::provider_cards(snapshot);
    html! {
        <section class="quota-detail-group" aria-labelledby={heading.clone()}>
            <h3 id={heading}>{"Accounts"}</h3>
            if cards.is_empty() {
                <p class="quota-empty quota-account-empty">{"No account usage reports were returned."}</p>
            } else {
                <div class="quota-account-list">
                    {for cards.into_iter().enumerate().map(|(card_index, card)| {
                        match card {
                            crate::quota::ProviderCard::Active { index, report } => render_report(
                                scope,
                                index,
                                report,
                                accents
                                    .as_deref()
                                    .and_then(|accents| accents[index]),
                            ),
                            crate::quota::ProviderCard::Disabled { provider } => {
                                render_disabled_provider(scope, card_index, provider)
                            }
                        }
                    })}
                </div>
            }
        </section>
    }
}

fn render_report(
    scope: &str,
    index: usize,
    report: &wire::QuotaReport,
    accent: Option<usize>,
) -> Html {
    let heading = format!("{scope}-quota-account-{index}");
    let label = crate::quota::account_label(&report.account);
    html! {
        <article
            class={classes!(
                "quota-account",
                accent.is_some().then_some("app-closed-edge"),
                accent.is_some().then_some("app-closed-edge--inline-start"),
            )}
            aria-labelledby={heading.clone()}
            data-provider={report.provider.clone()}
            data-provider-state="active"
            data-accent={accent.map(|accent| (accent % crate::watchband::ACCENTS).to_string())}
        >
            <header>
                <span class="quota-provider">{report.provider.clone()}</span>
                <h4 id={heading}>{label.clone()}</h4>
                {render_account_metadata(&report.account, &label)}
                <div class="quota-provider-tail">
                    <Age authored={report.fetched_at_ms} class={classes!("app-quota-age")} title="Age of this provider report" />
                </div>
            </header>
            <ul class="quota-limit-list">
                {for report.limits.iter().map(render_limit)}
            </ul>
            if report.reset_credits.is_none() {
                {render_credit_balance(report.credit_balance.as_ref())}
            }
            {render_reset_credits(report.reset_credits.as_ref(), report.credit_balance.as_ref())}
        </article>
    }
}

fn render_disabled_provider(scope: &str, index: usize, provider: &str) -> Html {
    let heading = format!("{scope}-quota-disabled-provider-{index}");
    html! {
        <article
            class={classes!("quota-account", "quota-account--provider-disabled")}
            aria-labelledby={heading.clone()}
            data-provider={provider.to_owned()}
            data-provider-state="disabled"
        >
            <header>
                <span class="quota-provider">{provider.to_owned()}</span>
                <h4 id={heading}>{"Provider disabled"}</h4>
                <p class="quota-provider-scope">
                    {"All accounts for this provider are unavailable until it is enabled."}
                </p>
            </header>
        </article>
    }
}

/// Identity facts beyond the one already spelled by the heading.
fn render_account_metadata(account: &wire::QuotaAccount, heading: &str) -> Html {
    let facts = [
        ("Email", account.email.as_deref()),
        ("Account", account.account_id.as_deref()),
        ("Project", account.project_id.as_deref()),
        ("Org", account.org_id.as_deref()),
        ("Org name", account.org_name.as_deref()),
        ("Plan", account.plan_type.as_deref()),
    ];
    html! {
        <dl class="quota-account-metadata">
            {for facts.into_iter().filter_map(|(label, value)| {
                let value = value.filter(|value| *value != heading)?;
                Some(html! { <div><dt>{label}</dt><dd>{value.to_owned()}</dd></div> })
            })}
        </dl>
    }
}

/// A balance on a line of its own, for an account with no reset credits to
/// carry it.
fn render_credit_balance(balance: Option<&wire::QuotaAmount>) -> Html {
    let Some(balance) = balance else {
        return Html::default();
    };
    html! {
        <dl class="quota-credit-balance">
            <dt>{"Credits"}</dt>
            <dd>{credit_balance_text(balance)}</dd>
        </dl>
    }
}

/// Whole credits grouped by the operator's locale, so a fractional balance
/// never reads as more than can be spent.
fn credit_balance_text(balance: &wire::QuotaAmount) -> String {
    balance
        .remaining
        .as_deref()
        .and_then(crate::quota::whole_credits)
        .map_or_else(
            || crate::quota::amount_text(balance),
            |whole| format!("{} {}", grouped_whole(whole), balance.unit),
        )
}

thread_local! {
    static WHOLE_NUMBER_FORMATTER: js_sys::Intl::NumberFormat = {
        let options = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            options.as_ref(),
            &wasm_bindgen::JsValue::from_str("maximumFractionDigits"),
            &wasm_bindgen::JsValue::from_f64(0.0),
        );
        js_sys::Intl::NumberFormat::new(&js_sys::Array::new(), &options)
    };
}

/// A whole number grouped by the operator's locale, plain digits when the
/// browser cannot format it.
fn grouped_whole(whole: f64) -> String {
    WHOLE_NUMBER_FORMATTER
        .with(|formatter| {
            formatter
                .format()
                .call1(
                    &wasm_bindgen::JsValue::UNDEFINED,
                    &wasm_bindgen::JsValue::from_f64(whole),
                )
                .ok()
                .and_then(|value| value.as_string())
        })
        .unwrap_or_else(|| format!("{whole}"))
}

fn render_limit(limit: &wire::QuotaLimit) -> Html {
    let status = limit
        .status
        .as_deref()
        .filter(|status| !status.eq_ignore_ascii_case("ok"));
    let remaining = crate::quota::limit_remaining_fraction(limit);
    html! {
        <li class="quota-limit" data-limit-status={status.map(str::to_owned)}>
            <strong>
                {limit.label.clone()}
                if let Some(status) = status {
                    <span class="quota-limit-status">{status.to_owned()}</span>
                }
            </strong>
            {render_meter(remaining)}
            <span class="quota-limit-amount">{crate::quota::amount_text(&limit.amount)}</span>
            <span class="quota-limit-reset">{render_reset(limit.window.as_ref())}</span>
        </li>
    }
}

/// Remaining share drawn as SVG geometry (the app CSP refuses inline styles),
/// anchored at the bar's start so usage empties it from the end beside the
/// amount. The amount follows the bar in row order.
fn render_meter(remaining: Option<f64>) -> Html {
    let Some(remaining) = remaining else {
        return html! { <span class="quota-meter" aria-hidden="true"></span> };
    };
    let width = remaining * 100.0;
    html! {
        <svg class="quota-meter" viewBox="0 0 100 6" preserveAspectRatio="none" aria-hidden="true">
            <rect class="quota-meter-track" width="100" height="6" rx="3" />
            <rect
                class="quota-meter-fill"
                x="0"
                width={format!("{width:.1}")}
                height="6"
                rx="3"
            />
        </svg>
    }
}

fn render_reset(window: Option<&wire::QuotaWindow>) -> Html {
    let label = window.and_then(|window| window.reset_label.clone());
    match window.and_then(|window| window.resets_at) {
        Some(at) => {
            let exact = local_full_millis(at);
            let title = match label {
                Some(label) => AttrValue::from(format!("{label} · {exact}")),
                None => exact,
            };
            html! { <><span class="visually-hidden">{"resets "}</span><Countdown expiry={at} class={classes!("app-quota-age")} title={title} /></> }
        }
        None => html! { {label.unwrap_or_default()} },
    }
}

fn iso_millis(value: &str) -> Option<i64> {
    let milliseconds = js_sys::Date::parse(value);
    milliseconds.is_finite().then_some(milliseconds as i64)
}

fn reset_credit_detail(credit: &wire::QuotaResetCredit) -> Option<String> {
    let title = credit.title.as_ref().or(credit.program.as_ref());
    match (title, credit.remaining_count) {
        (Some(title), Some(count)) => Some(format!("{title} ({count} remaining)")),
        (Some(title), None) => Some(title.clone()),
        (None, Some(count)) => Some(format!("{count} remaining")),
        (None, None) => None,
    }
}

fn render_reset_credit(credit: &wire::QuotaResetCredit) -> Html {
    let detail = reset_credit_detail(credit);
    let granted = credit
        .granted_at
        .as_deref()
        .and_then(|exact| iso_millis(exact).map(|granted| (granted, exact)));
    let (grant, grant_title) = match granted {
        Some((granted, exact)) => {
            let exact = format!("Granted {exact}");
            let grant = html! {
                <>
                    {"Granted "}
                    <Age
                        authored={granted}
                        class={classes!("app-quota-age")}
                        title={exact.clone()}
                    />
                    {" ago"}
                </>
            };
            (grant, exact)
        }
        None => (
            html! { "Granted at unknown time" },
            "Grant time unavailable".to_owned(),
        ),
    };
    let label_title = detail.as_ref().map_or_else(
        || grant_title.clone(),
        |detail| format!("{detail} · {grant_title}"),
    );
    let expiry = credit
        .expires_at
        .as_deref()
        .and_then(|exact| iso_millis(exact).map(|expiry| (expiry, exact)))
        .map(|(expiry, exact)| {
            let exact = format!("Expires {exact}");
            html! {
                <span class="quota-reset-credit-expiry" title={exact.clone()}>
                    {"expires "}
                    <Countdown
                        {expiry}
                        class={classes!("app-quota-age")}
                        title={exact}
                    />
                </span>
            }
        });
    html! {
        <span class="quota-reset-credit">
            <span class="quota-reset-credit-label" title={label_title}>
                {grant}
            </span>
            if credit.usable == Some(false) {
                <span
                    class="quota-reset-credit-usability"
                    title="This credit is available but cannot currently be redeemed."
                >
                    {"not redeemable"}
                </span>
            }
            {expiry}
        </span>
    }
}

/// Reset credits, with the account's credit balance at the right end of the
/// heading line when there is one.
fn render_reset_credits(
    credits: Option<&wire::QuotaResetCredits>,
    balance: Option<&wire::QuotaAmount>,
) -> Html {
    let Some(credits) = credits else {
        return Html::default();
    };
    let redeemable = credits.redeemable_count.unwrap_or(credits.available_count);
    html! {
        <div class="quota-reset-credits">
            <strong>{"Reset credits"}</strong>
            <span>{format!("{} available", credits.available_count)}</span>
            <span>{format!("{redeemable} redeemable")}</span>
            {for balance.map(|balance| html! {
                <span class="quota-reset-credits-balance" title="Credit balance">
                    {credit_balance_text(balance)}
                </span>
            })}
            {for credits.credits.iter().map(render_reset_credit)}
        </div>
    }
}

fn render_accounts_without_usage(scope: &str, accounts: &[wire::QuotaAccountWithoutUsage]) -> Html {
    if accounts.is_empty() {
        return Html::default();
    }
    let heading = format!("{scope}-quota-without-usage-heading");
    html! {
        <section class="quota-detail-group" aria-labelledby={heading.clone()}>
            <h3 id={heading}>{"Accounts lacking usage"}</h3>
            <ul class="quota-simple-list">{for accounts.iter().map(|entry| {
                render_simple_row(&entry.provider, &entry.account, &entry.credential_type)
            })}</ul>
        </section>
    }
}

fn render_disabled(scope: &str, credentials: &[wire::QuotaDisabledCredential]) -> Html {
    if credentials.is_empty() {
        return Html::default();
    }
    let heading = format!("{scope}-quota-disabled-heading");
    html! {
        <section class="quota-detail-group" aria-labelledby={heading.clone()}>
            <h3 id={heading}>{"Disabled credentials"}</h3>
            <ul class="quota-simple-list">{for credentials.iter().map(|entry| {
                render_simple_row(&entry.provider, &entry.account, &entry.credential_type)
            })}</ul>
        </section>
    }
}

/// One single-line row; the title spells what the line may ellipsize.
fn render_simple_row(provider: &str, account: &wire::QuotaAccount, credential: &str) -> Html {
    let label = crate::quota::account_label(account);
    html! {
        <li title={format!("{provider} · {label} · {credential}")}>
            <strong>{provider.to_owned()}</strong>
            <span>{label}</span>
            <span>{credential.to_owned()}</span>
        </li>
    }
}
