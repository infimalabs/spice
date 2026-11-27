use super::*;
#[test]
fn live_changes_use_exact_requests_and_only_matching_provider_responses_apply() {
    let sink = Sink::default();
    let driver = driver(&sink);
    let reply = door_directory().join(format!("spice-retune-test-{}.sock", std::process::id()));
    let callback = door::Door::bind(&reply, CALLBACK_REPLY_BYTES).expect("callback door binds");

    driver.retune(
        "live-effort".to_owned(),
        LiveRetune::Effort {
            effort: "xhigh".to_owned(),
        },
        reply.clone(),
    );
    assert_eq!(
        sink.answered(1),
        [serde_json::json!({
            "id": "live-effort",
            "type": "set_thinking_level",
            "level": "xhigh",
        })],
    );
    driver.observe(&decoded(
        br#"{"id":"another-request","type":"response","command":"set_thinking_level","success":true}"#,
    ));
    assert!(
        driver
            .retunes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key("live-effort"),
        "an unrelated response cannot settle the live change",
    );
    driver.observe(&decoded(
        br#"{"id":"live-effort","type":"response","command":"set_thinking_level","success":true}"#,
    ));
    let bytes = callback
        .recv(Some(test_support::DEADLOCK_GUARD))
        .expect("the matching response answers");
    let answer: RetuneReply = serde_json::from_slice(&bytes).expect("the callback is typed");
    assert_eq!(answer.request, "live-effort");
    assert!(matches!(answer.result, LiveRetuneResult::Applied));

    driver.retune(
        "live-model".to_owned(),
        LiveRetune::Model {
            provider: "openai".to_owned(),
            model: "gpt-5.4".to_owned(),
        },
        reply.clone(),
    );
    assert_eq!(
        sink.answered(2)[1],
        serde_json::json!({
            "id": "live-model",
            "type": "set_model",
            "provider": "openai",
            "modelId": "gpt-5.4",
        }),
    );
    driver.observe(&decoded(
        br#"{"id":"live-model","type":"response","command":"set_thinking_level","success":true}"#,
    ));
    let bytes = callback
        .recv(Some(test_support::DEADLOCK_GUARD))
        .expect("the mismatched command answers");
    let answer: RetuneReply =
        serde_json::from_slice(&bytes).expect("the refusal callback is typed");
    assert_eq!(answer.request, "live-model");
    assert!(matches!(answer.result, LiveRetuneResult::Refused { .. }));

    driver.retune(
        "live-model-success".to_owned(),
        LiveRetune::Model {
            provider: "openai".to_owned(),
            model: "gpt-5.4".to_owned(),
        },
        reply.clone(),
    );
    assert_eq!(
        sink.answered(3)[2],
        serde_json::json!({
            "id": "live-model-success",
            "type": "set_model",
            "provider": "openai",
            "modelId": "gpt-5.4",
        }),
    );
    driver.observe(&decoded(
        br#"{"id":"live-model-success","type":"response","command":"set_model","success":true}"#,
    ));
    let bytes = callback
        .recv(Some(test_support::DEADLOCK_GUARD))
        .expect("the matching model response answers");
    let answer: RetuneReply = serde_json::from_slice(&bytes).expect("the model callback is typed");
    assert_eq!(answer.request, "live-model-success");
    assert!(matches!(answer.result, LiveRetuneResult::Applied));
}

fn resolve(driver: &Driver<Sink>, spelling: &str, suffix: &str) -> ModelResolution {
    let reply = door_directory().join(format!(
        "spice-model-resolution-{}-{suffix}.sock",
        std::process::id()
    ));
    let callback = door::Door::bind(&reply, CALLBACK_REPLY_BYTES).expect("callback door binds");
    driver.resolve_model(spelling, &reply);
    let bytes = callback
        .recv(Some(test_support::DEADLOCK_GUARD))
        .expect("model resolution answers");
    serde_json::from_slice::<ModelResolutionReply>(&bytes)
        .expect("the callback is typed")
        .result
}

#[test]
fn live_model_spellings_resolve_only_against_the_running_occupants_catalog() {
    let sink = Sink::default();
    let driver = driver(&sink);
    driver
        .registration
        .models
        .set(vec![
            listen::ServedModel {
                provider: "anthropic".to_owned(),
                id: "claude-opus-5".to_owned(),
            },
            listen::ServedModel {
                provider: "openai".to_owned(),
                id: "gpt-5.6-sol".to_owned(),
            },
            listen::ServedModel {
                provider: "openrouter".to_owned(),
                id: "gpt-5.6-sol".to_owned(),
            },
        ])
        .expect("catalog is installed once");

    assert!(matches!(
        resolve(&driver, "Claude-Opus-5", "bare"),
        ModelResolution::Resolved { provider, model }
            if provider == "anthropic" && model == "claude-opus-5"
    ));
    assert!(matches!(
        resolve(&driver, "OpenRouter/GPT-5.6-Sol", "qualified"),
        ModelResolution::Resolved { provider, model }
            if provider == "openrouter" && model == "gpt-5.6-sol"
    ));

    let ModelResolution::Refused { detail } = resolve(&driver, "gpt-5.6-sol", "ambiguous") else {
        panic!("a shared bare identity must refuse");
    };
    assert!(
        detail.contains("openai/gpt-5.6-sol") && detail.contains("openrouter/gpt-5.6-sol"),
        "{detail}"
    );

    let ModelResolution::Refused { detail } = resolve(&driver, "missing-model", "absent") else {
        panic!("an absent identity must refuse");
    };
    assert!(detail.contains("missing-model"), "{detail}");
}

#[test]
fn fast_mode_requests_name_the_disposition_and_a_failed_response_refuses() {
    let sink = Sink::default();
    let driver = driver(&sink);
    let reply = door_directory().join(format!("spice-fast-test-{}.sock", std::process::id()));
    let callback = door::Door::bind(&reply, CALLBACK_REPLY_BYTES).expect("callback door binds");

    driver.retune(
        "live-fast".to_owned(),
        LiveRetune::Fast { enabled: true },
        reply.clone(),
    );
    assert_eq!(
        sink.answered(1)[0],
        serde_json::json!({
            "id": "live-fast",
            "type": "set_fast_mode",
            "enabled": true,
        }),
    );
    // OMP refuses to enable fast mode on a model without a priority tier.
    driver.observe(&decoded(
        br#"{"id":"live-fast","type":"response","command":"set_fast_mode","success":false,"error":"Fast mode is unavailable for the current model."}"#,
    ));
    let bytes = callback
        .recv(Some(test_support::DEADLOCK_GUARD))
        .expect("the failed fast-mode response answers");
    let answer: RetuneReply = serde_json::from_slice(&bytes).expect("the fast callback is typed");
    assert_eq!(answer.request, "live-fast");
    assert!(matches!(answer.result, LiveRetuneResult::Refused { .. }));
}

#[test]
fn a_vendor_refusing_fast_mode_continues_the_turn_without_it_once_until_fast_returns() {
    const REFUSED: &[u8] = br#"{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"error","sessionSettled":true,"error":{"message":"400 'priority' is not supported for 'service_tier' on this model (type=invalid_request_error param=validation_error)","provider":"openai","model":"gpt-5.4","httpStatus":400,"retryable":false}}"#;
    let open = |driver: &Driver<Sink>| {
        driver
            .stdin
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    };
    let sink = Sink::default();
    let driver = driver(&sink);

    let raised = driver.observe(&decoded(REFUSED));
    assert!(
        matches!(&raised, Some(Raised::FastRefused(refusal)) if refusal.contains("'service_tier'")),
        "the vendor's words reach the resident"
    );
    let frames = sink.answered(2);
    assert_eq!(
        frames[0],
        serde_json::json!({"id": FAST_REFUSED_REQUEST_ID, "type": "set_fast_mode", "enabled": false}),
    );
    assert_eq!(
        frames[1]["type"], "prompt",
        "the same session resumes the turn"
    );
    assert!(open(&driver), "the occupant keeps running");

    // The operator turning fast mode back on lets one more refusal recover.
    driver.retune(
        "live-fast".to_owned(),
        LiveRetune::Fast { enabled: true },
        door_directory().join("spice-fast-refusal-unanswered.sock"),
    );
    assert!(matches!(
        driver.observe(&decoded(REFUSED)),
        Some(Raised::FastRefused(_))
    ));
    assert!(open(&driver));

    // Refused again without fast mode back on: an ordinary vendor error.
    assert!(driver.observe(&decoded(REFUSED)).is_none());
    assert!(!open(&driver), "the turn ends as any vendor failure does");

    let other = Sink::default();
    let filtered = self::driver(&other);
    assert!(
        filtered
            .observe(&decoded(
                br#"{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"error","sessionSettled":true,"error":{"message":"Refusal: blocked by content filtering policy.","provider":"anthropic","model":"claude-opus-4-6","httpStatus":400,"retryable":false}}"#,
            ))
            .is_none()
    );
    assert!(
        !open(&filtered),
        "any other vendor error still ends the turn"
    );
}
