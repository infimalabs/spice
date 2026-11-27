use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;

const VALID_JSON: &str = r#"{
      "generatedAt": 1700000000000,
      "unknownTop": "ignored",
      "reports": [{
        "provider": "openai-codex",
        "fetchedAt": 1700000000100,
        "unknownReport": true,
        "metadata": {
          "email": "us*",
          "accountId": "ac*",
          "projectId": "raw-project",
          "orgId": "or*",
          "orgName": "te*",
          "planType": "pro",
          "spiceExtension": "openai-credit-balance",
          "allowed": true,
          "limitReached": false,
          "endpoint": "https://secret.invalid",
          "token": "secret"
        },
        "limits": [{
          "id": "openai-codex:primary",
          "label": "7 days",
          "scope": {
            "provider": "openai-codex",
            "accountId": "sc*",
            "projectId": "raw-scope-project",
            "windowId": "7d",
            "tier": "pro",
            "shared": true,
            "unknownScope": "ignored"
          },
          "window": {
            "id": "7d",
            "label": "7 days",
            "durationMs": 604800000,
            "resetsAt": 1700600000000
          },
          "amount": {
            "used": 9,
            "limit": 100,
            "remaining": 91,
            "usedFraction": 0.09,
            "remainingFraction": 0.91,
            "unit": "percent"
          },
          "status": "ok"
        }, {
          "id": "openai-codex:credits",
          "label": "Credit balance",
          "scope": { "provider": "openai-codex", "windowId": "balance" },
          "amount": { "remaining": 62000, "unit": "credits" }
        }],
        "resetCredits": {
          "availableCount": 1,
          "redeemableCount": 1,
          "eligible": true,
          "credits": [{
            "id": "credit-1",
            "remainingCount": 1,
            "usable": true,
            "grantedAt": "2026-09-22T20:27:08Z",
            "expiresAt": "2026-10-22T20:27:08Z",
            "status": "available"
          }]
        }
      }, {
        "provider": "openai-codex",
        "fetchedAt": 1700000000200,
        "metadata": {
          "email": "ot*",
          "accountId": "o2*",
          "planType": "pro",
          "spiceExtension": "openai-credit-balance"
        },
        "limits": [{
          "id": "openai-codex:credits",
          "label": "Credit balance",
          "scope": { "provider": "openai-codex", "windowId": "balance" },
          "amount": { "remaining": 61500.25, "unit": "credits" }
        }]
      }, {
        "provider": "openai-codex",
        "fetchedAt": 1700000000300,
        "metadata": {
          "email": "mi*",
          "accountId": "m3*",
          "planType": "pro",
          "spiceExtension": "openai-credit-balance"
        },
        "limits": [{
          "id": "openai-codex:credits:1w",
          "label": "Weekly credits",
          "scope": { "provider": "openai-codex", "windowId": "1w" },
          "window": { "id": "1w", "label": "1 week", "durationMs": 604800000 },
          "amount": { "used": 40, "limit": 100, "remaining": 60, "unit": "credits" }
        }, {
          "id": "openai-codex:credits:reserve",
          "label": "Credit reserve",
          "scope": { "provider": "openai-codex", "windowId": "reserve" },
          "amount": { "remaining": 25, "unit": "credits" }
        }]
      }],
      "accountsWithoutUsage": [{
        "provider": "xai",
        "type": "oauth",
        "email": "no*",
        "accountId": "raw-account",
        "enterpriseUrl": "https://secret.invalid",
        "authorizedAt": 1690000000000
      }],
      "disabledCredentials": [{
        "id": 42,
        "provider": "anthropic",
        "type": "oauth",
        "email": "de*",
        "accountId": "di*",
        "cause": "token secret for raw@example.com",
        "disabledAtMs": 1680000000000
      }],
      "capacity": {
        "openai-codex": [{
          "window": "7d",
          "durationMs": 604800000,
          "meter": "chat",
          "accounts": 2,
          "usedAccounts": 0.38,
          "remainingAccounts": 1.62
        }]
      }
    }"#;

/// A sampler that answers each launch with the next scripted result.
fn scripted(results: Vec<Result<Snapshot, SampleFailure>>) -> Arc<Sampler> {
    let results = Mutex::new(VecDeque::from(results));
    Arc::new(move |_| {
        results
            .lock()
            .expect("script lock")
            .pop_front()
            .expect("one scripted result per launch")
    })
}

fn finished(stdout: &[u8], ending: Ending) -> Finished<'_> {
    Finished {
        stdout,
        stdout_truncated: false,
        stderr_truncated: false,
        complete: true,
        ending,
    }
}

fn valid() -> Result<Snapshot, SampleFailure> {
    judge(&finished(VALID_JSON.as_bytes(), Ending::Succeeded))
}

async fn settle(runtime: &mut Runtime) {
    let completion = runtime.recv().await;
    runtime.accept(completion);
}

fn latest_failure(state: &State) -> &Failure {
    match state.latest_attempt.as_ref() {
        Some(Attempt::Failed(failure)) => failure,
        other => panic!("expected failed attempt, got {other:?}"),
    }
}

#[tokio::test]
async fn a_sample_publishes_only_redacted_allowlisted_fields() {
    let mut runtime = Runtime::start_with(scripted(vec![valid()]));
    let mut updates = runtime.subscribe();

    runtime.request();
    assert!(updates.changed().await.is_ok());
    assert!(updates.borrow().refreshing);
    settle(&mut runtime).await;

    let state = runtime.current();
    assert_eq!(state.generation, 2);
    assert!(!state.refreshing);
    assert!(matches!(
        state.latest_attempt,
        Some(Attempt::Succeeded { .. })
    ));
    let snapshot = state.last_good.as_ref().expect("last good snapshot");
    assert_eq!(snapshot.generated_at_ms, 1_700_000_000_000);
    assert_eq!(snapshot.reports[0].account.email.as_deref(), Some("us*"));
    assert_eq!(snapshot.reports[0].account.project_id, None);
    assert_eq!(
        snapshot.reports[0].limits[0].scope.account_id.as_deref(),
        Some("sc*")
    );
    assert_eq!(snapshot.reports[0].limits[0].scope.project_id, None);
    assert_eq!(snapshot.reports[0].limits[0].amount.remaining, Some(91.0));
    assert_eq!(
        snapshot.reports[0]
            .credit_balance
            .as_ref()
            .and_then(|balance| balance.remaining),
        Some(62_000.0)
    );
    assert_eq!(
        snapshot.reports[1]
            .credit_balance
            .as_ref()
            .and_then(|balance| balance.remaining),
        Some(61_500.25)
    );
    assert_eq!(
        snapshot.reports[0].limits.len(),
        1,
        "the balance is no meter"
    );
    assert!(snapshot.reports[1].limits.is_empty());
    assert_eq!(snapshot.reports[2].credit_balance, None);
    assert_eq!(
        snapshot.reports[2]
            .limits
            .iter()
            .map(|limit| limit.id.as_str())
            .collect::<Vec<_>>(),
        ["openai-codex:credits:1w", "openai-codex:credits:reserve"],
        "windowed and differently scoped credit allowances stay quota meters"
    );
    assert_eq!(
        snapshot.reports[0]
            .reset_credits
            .as_ref()
            .expect("reset credits")
            .credits[0]
            .expires_at
            .as_deref(),
        Some("2026-10-22T20:27:08Z")
    );
    assert_eq!(
        snapshot.accounts_without_usage[0].email.as_deref(),
        Some("no*")
    );
    assert_eq!(snapshot.accounts_without_usage[0].account_id, None);
    assert_eq!(
        snapshot.disabled_credentials[0].account_id.as_deref(),
        Some("di*")
    );
    assert_eq!(
        snapshot.capacity["openai-codex"][0].remaining_accounts,
        1.62
    );
    let encoded = String::from_utf8(
        wire::encode(&wire_generation(&state)).expect("wire quota generation encodes"),
    )
    .expect("wire quota generation is UTF-8 JSON");
    for secret in [
        "raw-project",
        "raw-scope-project",
        "https://secret.invalid",
        "\"token\":\"secret\"",
    ] {
        assert!(!encoded.contains(secret), "{secret} crossed the quota wire");
    }
    for mask in ["us*", "sc*", "ot*", "o2*", "mi*", "m3*", "no*", "di*"] {
        assert!(encoded.contains(mask), "{mask} redaction was lost");
    }
}

#[test]
fn output_ceilings_outrank_how_the_usage_command_ended() {
    let overflowed = Finished {
        stdout_truncated: true,
        ..finished(b"", Ending::Failed(Some(1)))
    };
    let noisy = Finished {
        stderr_truncated: true,
        ..finished(VALID_JSON.as_bytes(), Ending::Succeeded)
    };
    let undrained = Finished {
        complete: false,
        ..finished(VALID_JSON.as_bytes(), Ending::Succeeded)
    };
    for (outcome, expected) in [
        (overflowed, SampleFailure::new(FailureKind::StdoutOverflow)),
        (noisy, SampleFailure::new(FailureKind::StderrOverflow)),
        (undrained, SampleFailure::new(FailureKind::OutputIncomplete)),
        (
            finished(b"", Ending::TimedOut),
            SampleFailure::new(FailureKind::Timeout),
        ),
        (
            finished(b"", Ending::Cancelled),
            SampleFailure::new(FailureKind::Supervisor),
        ),
        (
            finished(b"credential secret", Ending::Failed(Some(17))),
            SampleFailure::exit(Some(17)),
        ),
        (
            finished(b"{", Ending::Succeeded),
            SampleFailure::new(FailureKind::MalformedOutput),
        ),
    ] {
        assert_eq!(judge(&outcome).err(), Some(expected));
    }
}

#[tokio::test]
async fn failed_attempts_keep_the_last_good_snapshot() {
    let mut runtime = Runtime::start_with(scripted(vec![
        valid(),
        Err(SampleFailure::new(FailureKind::MalformedOutput)),
        Err(SampleFailure::exit(Some(17))),
    ]));
    runtime.request();
    settle(&mut runtime).await;
    let good = Arc::clone(
        runtime
            .current()
            .last_good
            .as_ref()
            .expect("first sample succeeds"),
    );

    runtime.request();
    settle(&mut runtime).await;
    let state = runtime.current();
    assert_eq!(state.generation, 4);
    assert!(Arc::ptr_eq(
        state.last_good.as_ref().expect("last good remains"),
        &good
    ));
    assert_eq!(latest_failure(&state).kind, FailureKind::MalformedOutput);

    runtime.request();
    settle(&mut runtime).await;
    let state = runtime.current();
    assert!(Arc::ptr_eq(
        state.last_good.as_ref().expect("last good remains"),
        &good
    ));
    let failure = latest_failure(&state);
    assert_eq!(failure.kind, FailureKind::NonzeroExit);
    assert_eq!(failure.exit_code, Some(17));
}
#[tokio::test]
async fn provider_policy_failure_keeps_usage_and_last_known_disabled_set() {
    let mut first = valid().expect("valid first sample");
    first.disabled_providers = vec!["anthropic".to_owned()];
    let mut second = valid().expect("valid second sample");
    second.generated_at_ms += 1;
    second.disabled_provider_issue = Some(FailureKind::MalformedOutput);
    let recovered = valid().expect("valid recovered sample");
    let mut runtime = Runtime::start_with(scripted(vec![Ok(first), Ok(second), Ok(recovered)]));

    runtime.request();
    settle(&mut runtime).await;
    runtime.request();
    settle(&mut runtime).await;

    let state = runtime.current();
    assert!(matches!(
        state.latest_attempt,
        Some(Attempt::Succeeded { .. })
    ));
    let snapshot = state.last_good.as_ref().expect("usage remains available");
    assert_eq!(snapshot.disabled_providers, ["anthropic"]);
    assert_eq!(
        snapshot.disabled_provider_issue,
        Some(FailureKind::MalformedOutput)
    );
    assert_eq!(snapshot.generated_at_ms, 1_700_000_000_001);
    assert_eq!(
        wire_generation(&state)
            .last_good
            .as_ref()
            .expect("published usage")
            .disabled_provider_issue,
        Some(wire::QuotaFailureKind::MalformedOutput)
    );

    runtime.request();
    settle(&mut runtime).await;
    let recovered = runtime.current();
    let snapshot = recovered.last_good.as_ref().expect("recovered usage");
    assert_eq!(snapshot.disabled_providers, Vec::<String>::new());
    assert_eq!(snapshot.disabled_provider_issue, None);
}

#[test]
fn only_an_unstamped_codex_report_refuses_the_sample() {
    // OMP serves its built-in provider when it cannot load the named
    // extension, so one unstamped Codex report means the balance source
    // reverted.
    let unstamped = VALID_JSON.replacen("\"spiceExtension\": \"openai-credit-balance\",", "", 1);
    assert_eq!(
        decode(unstamped.as_bytes()),
        Err(FailureKind::ExtensionUnloaded)
    );
    let restamped = VALID_JSON.replacen("\"openai-credit-balance\"", "\"another-extension\"", 1);
    assert_eq!(
        decode(restamped.as_bytes()),
        Err(FailureKind::ExtensionUnloaded)
    );
    let other_provider =
        br#"{"generatedAt": 1, "reports": [{"provider": "anthropic", "fetchedAt": 2, "limits": []}]}"#;
    assert!(decode(other_provider).is_ok());
}

#[tokio::test]
async fn requests_during_a_run_and_its_settlement_coalesce_to_one_follow_up() {
    let launches = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&launches);
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let mut runtime = Runtime::start_with(Arc::new(move |_| {
        counted.fetch_add(1, Ordering::SeqCst);
        gate.lock()
            .expect("gate lock")
            .recv()
            .expect("the test releases every launch");
        valid()
    }));

    runtime.request();
    runtime.request();
    runtime.request();
    release.send(()).expect("first launch waits");
    let first = runtime.recv().await;
    runtime.request();
    runtime.accept(first);
    assert!(runtime.current().refreshing);
    release.send(()).expect("follow-up launch waits");
    settle(&mut runtime).await;

    assert_eq!(runtime.current().generation, 4);
    assert!(!runtime.current().refreshing);
    assert_eq!(launches.load(Ordering::SeqCst), 2);
}

#[test]
fn shutdown_cancels_the_running_sample_and_waits_for_it() {
    let (started, running) = std::sync::mpsc::channel();
    let started = Mutex::new(started);
    let cancelled = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&cancelled);
    let mut runtime = Runtime::start_with(Arc::new(move |cancellation: CancellationToken| {
        started
            .lock()
            .expect("start lock")
            .send(())
            .expect("the test awaits the start");
        let (woken, wake) = std::sync::mpsc::channel();
        let _registration = cancellation.register_waker(Arc::new(move || {
            let _ = woken.send(());
        }));
        let _ = wake.recv();
        observed.store(cancellation.is_cancelled(), Ordering::SeqCst);
        Err(SampleFailure::new(FailureKind::Supervisor))
    }));
    runtime.request();
    running.recv().expect("the sample starts");

    drop(runtime);

    assert!(cancelled.load(Ordering::SeqCst));
}
