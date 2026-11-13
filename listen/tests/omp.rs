//! oh-my-pi's RPC frames and session records under the current decoder.
//!
//! The lane drives omp over `--mode rpc`, so its content frames are
//! `message_end` and the `get_state` reply rather than a per-turn envelope,
//! and the session file persists the same message shape under a `message`
//! envelope beside its own `session` record.

use listen::{
    Decoded, Dialect, Dialog, Event, EventKind, Locus, PromptStatus, ProseRole, RecordDecodeError,
    ToolMoment, ToolResult, TurnMoment, WITHHELD_REASONING, decode, decode_record, restated,
    withhold_reasoning,
};

const SESSION: &[u8] = br#"{"id":"spice-state","type":"response","command":"get_state","success":true,"data":{"model":{"id":"claude-opus-5","contextWindow":1000000},"sessionId":"01a0a807-666f-7524-be0d-2ee476b6c15c","sessionFile":"/tmp/sessions/x.jsonl","isStreaming":false}}"#;
const PROSE: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"api":"anthropic-messages","provider":"anthropic","model":"claude-opus-5","usage":{"input":7,"output":2,"cacheRead":5,"cacheWrite":3,"totalTokens":17},"stopReason":"stop","timestamp":1789525452596}}"#;
const REASONING: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"inspect"}],"usage":{"input":7,"output":2,"cacheRead":5,"cacheWrite":3},"stopReason":"stop"}}"#;
const TOOL: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"checking"},{"type":"toolCall","id":"call_1","name":"bash","arguments":{"command":"cargo test"},"partialArgs":"{}"}],"usage":{"input":120,"output":14,"cacheRead":0,"cacheWrite":0},"stopReason":"toolUse"}}"#;
const TOOL_RESULT: &[u8] = br#"{"type":"message_end","message":{"role":"toolResult","toolCallId":"call_1","toolName":"bash","content":[{"type":"text","text":"ok"}],"details":{"exitCode":0},"isError":false,"timestamp":1789525455120}}"#;
const OPERATOR: &[u8] = br#"{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"please run it"}],"attribution":"user","timestamp":1789525452570}}"#;
const PARTIAL: &[u8] = br#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"do","partial":{"role":"assistant","content":[{"type":"text","text":"do"}]}},"message":{"role":"assistant","content":[{"type":"text","text":"do"}]}}"#;
const TOOL_PARTIAL: &[u8] = br#"{"type":"tool_execution_update","toolCallId":"call_1","toolName":"bash","partialResult":{"content":[{"type":"text","text":"o"}]}}"#;
const JOB_PARTIAL: &[u8] = br#"{"type":"tool_execution_update","toolCallId":"call_2","toolName":"job","partialResult":{"content":[{"type":"text","text":"[job dW3w7qC4G] serve\nready on 8080"}]}}"#;
const TOOL_END: &[u8] = br#"{"type":"tool_execution_end","toolCallId":"call_1","toolName":"bash","result":{"content":[{"type":"text","text":"ok"}]},"isError":false}"#;
const COMPACTION: &[u8] = br#"{"type":"auto_compaction_end","action":"snapcompact","result":{"summary":"so far","firstKeptEntryId":"e1","tokensBefore":900000},"aborted":false,"willRetry":false}"#;
const FAILURE: &[u8] = br#"{"id":"spice-prompt","type":"response","command":"prompt","success":false,"error":"Agent is already processing"}"#;
const TRANSCRIPT_SESSION: &[u8] = br#"{"type":"session","version":3,"id":"01a0a807-666f-7524-be0d-2ee476b6c15c","timestamp":"2026-09-16T02:24:12.399Z","cwd":"/work/tree"}"#;
const TRANSCRIPT_MESSAGE: &[u8] = br#"{"type":"message","id":"93bc66b5","parentId":"1ded9227","timestamp":"2026-09-16T02:24:25.341Z","message":{"role":"assistant","content":[{"type":"text","text":"echo: second thing"}],"usage":{"input":120,"output":9,"cacheRead":0,"cacheWrite":0},"stopReason":"stop"}}"#;
const PROMPT_COMPLETED: &[u8] = br#"{"type":"prompt_result","id":"prompt-7","agentInvoked":true,"status":"completed","sessionSettled":false}"#;
const PROMPT_FAILED: &[u8] = br#"{"type":"prompt_result","id":"prompt-8","agentInvoked":true,"status":"error","sessionSettled":true,"error":{"message":"capacity exhausted","provider":"anthropic","model":"claude-opus-5-5","retryable":true,"httpStatus":529}}"#;
const SIGN_OUT: &[u8] = br#"{"type":"notice","level":"warning","message":"Anthropic account ops@example.com was signed out automatically. Run /login to sign in again.","source":"auth"}"#;

/// Every event kind the decoder produces, each from the record that carries it.
const KINDS: &[(&[u8], EventKind)] = &[
    (SESSION, EventKind::Session),
    (PROSE, EventKind::Prose),
    (REASONING, EventKind::Reasoning),
    (TOOL, EventKind::Tool),
    (TOOL_RESULT, EventKind::Tool),
    (PROSE, EventKind::Usage),
    (COMPACTION, EventKind::Compaction),
    (FAILURE, EventKind::Failure),
    (SIGN_OUT, EventKind::SignOut),
];

/// Frames the lane sees on every turn that carry nothing the tape types.
const SILENT: &[&[u8]] = &[
    br#"{"type":"ready","protocolVersion":1,"supportedProtocolVersions":[1,2],"maxFrameBytes":1048576}"#,
    br#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"do"},"message":{"role":"assistant","content":[{"type":"text","text":"do"}]}}"#,
    br#"{"type":"message_start","message":{"role":"assistant","content":[]}}"#,
    br#"{"type":"tool_execution_start","toolCallId":"call_1","toolName":"bash","args":{"command":"cargo test"}}"#,
    br#"{"type":"tool_execution_end","toolCallId":"call_1","toolName":"bash","result":{"content":[]},"isError":false}"#,
    br#"{"type":"turn_end","message":{"role":"assistant","content":[]},"toolResults":[]}"#,
    br#"{"id":"spice-prompt","type":"response","command":"prompt","success":true}"#,
    br#"{"type":"extension_ui_request","id":"1","method":"setWidget","widgetKey":"x"}"#,
    br#"{"type":"message_end","message":{"role":"custom","customType":"async-result","content":"<system-notice>done</system-notice>"}}"#,
    br#"{"type":"message_end","message":{"role":"developer","content":[{"type":"text","text":"<system-reminder>comply</system-reminder>"}]}}"#,
    // Current run-state and retry telemetry is either represented by the
    // settled message/prompt result or carries no authored transcript moment.
    br#"{"type":"session_settled"}"#,
    br#"{"type":"auto_retry_start","attempt":1,"maxAttempts":3,"delayMs":200,"errorMessage":"busy"}"#,
    br#"{"type":"auto_retry_end","success":true,"attempt":1}"#,
    br#"{"type":"retry_fallback_applied","from":"opus","to":"sonnet","role":"default"}"#,
    br#"{"type":"retry_fallback_succeeded","model":"sonnet","role":"default"}"#,
    br#"{"type":"model_changed"}"#,
    br#"{"type":"thinking_level_changed","thinkingLevel":"high"}"#,
    br#"{"type":"config_warnings_changed"}"#,
    br#"{"type":"advisor_yielded"}"#,
    br#"{"type":"ttsr_triggered","rules":[]}"#,
    br#"{"type":"todo_reminder","todos":[],"attempt":1,"maxAttempts":3}"#,
    br#"{"type":"todo_auto_clear"}"#,
    br#"{"type":"goal_updated","goal":null}"#,
    br#"{"type":"irc_message","message":{"role":"custom","content":"status"}}"#,
];

fn locus(bytes: &[u8]) -> Locus {
    Locus::new(4, 19, bytes.len() as u64)
}

fn decoded(bytes: &[u8]) -> Decoded {
    decode(Dialect::Omp, bytes, locus(bytes)).expect("matching locus")
}

#[test]
fn every_kind_is_produced_once_under_the_record_locus() {
    for (bytes, expected) in KINDS {
        let decoded = decoded(bytes);
        assert!(
            decoded
                .events()
                .iter()
                .any(|event| event.kind() == *expected),
            "omp did not produce {expected:?}: {decoded:?}"
        );
        assert!(
            decoded
                .events()
                .iter()
                .all(|event| event.locus() == locus(bytes))
        );
    }
}

#[test]
fn the_get_state_reply_declares_the_session_and_its_context_window() {
    let session = decoded(SESSION);
    assert!(matches!(
        session.events(),
        [Event::Session(declaration)]
            if declaration.identity == "01a0a807-666f-7524-be0d-2ee476b6c15c"
    ));
    assert_eq!(
        session
            .context()
            .and_then(|context| context.model_context_window),
        Some(1_000_000)
    );
    assert!(matches!(
        session.context().and_then(|context| context.response.as_ref()),
        Some(response)
            if response.id.as_deref() == Some("spice-state")
                && response.command == "get_state"
                && response.success
                && response.error.is_none()
    ));
    let refused =
        decode_record(None, PROSE, locus(PROSE)).expect("an omp envelope is uniquely owned");
    assert_eq!(refused.dialect, Dialect::Omp);
}

#[test]
fn assistant_messages_type_prose_reasoning_tool_calls_and_usage() {
    let prose = decoded(PROSE);
    assert!(matches!(
        prose.events(),
        [Event::Prose(prose), Event::Usage(usage)]
            if prose.role == ProseRole::Assistant
                && prose.text == "done"
                && prose.phase.as_deref() == Some("final_answer")
                && usage.input_tokens == 7
                && usage.cached_input_tokens == 5
                && usage.cache_write_input_tokens == 3
                && usage.output_tokens == 2
                && usage.reasoning_output_tokens.is_none()
    ));
    let tool = decoded(TOOL);
    assert!(matches!(
        tool.events(),
        [Event::Prose(prose), Event::Tool(call), Event::Usage(_)]
            if prose.text == "checking"
                && prose.phase.is_none()
                && call.call_id == "call_1"
                && call.moment == ToolMoment::Call {
                    name: "bash".to_owned(),
                    arguments: r#"{"command":"cargo test"}"#.to_owned(),
                    intent: None,
                }
    ));
    let result = decoded(TOOL_RESULT);
    assert!(matches!(
        result.events(),
        [Event::Tool(tool)]
            if tool.call_id == "call_1"
                && matches!(
                    &tool.moment,
                    ToolMoment::Result(outcome)
                        if outcome.result == ToolResult::Succeeded
                            && outcome.output.as_deref() == Some("ok")
                            && outcome.exit_code == Some(0)
                )
    ));
    let operator = decoded(OPERATOR);
    assert!(matches!(
        operator.events(),
        [Event::Prose(prose)]
            if prose.role == ProseRole::Operator
                && prose.text == "please run it"
                && prose.phase.as_deref() == Some("prompt")
    ));
}

#[test]
fn compaction_and_refusals_are_typed_while_lifecycle_frames_stay_silent() {
    let compaction = decoded(COMPACTION);
    assert!(matches!(
        compaction.events(),
        [Event::Compaction(compaction)]
            if compaction.summary.as_deref() == Some("so far")
                && compaction.method.as_deref() == Some("snapcompact")
    ));
    let failure = decoded(FAILURE);
    assert!(matches!(
        failure.events(),
        [Event::Failure(failure)]
            if failure.message
                == "prompt not accepted: agent is already processing; queue it with \
                    steer/followUp or retry after completion"
    ));
    assert!(matches!(
        failure.context().and_then(|context| context.response.as_ref()),
        Some(response)
            if response.id.as_deref() == Some("spice-prompt")
                && response.command == "prompt"
                && !response.success
                && response.error.as_deref() == Some("Agent is already processing")
    ));
    let unsupported = br#"{"id":"spice-protocol","type":"response","command":"negotiate_protocol","success":false,"error":"Unsupported RPC protocol version: 2"}"#;
    let refused = decoded(unsupported);
    assert!(
        matches!(
            refused.events(),
            [Event::Failure(failure)]
                if failure.message
                    == format!(
                        "negotiate_protocol refused: Unsupported RPC protocol version: 2; \
                         this lane requires RPC protocol version {}",
                        listen::OMP_PROTOCOL_VERSION
                    )
        ),
        "a refused wire must name the version the lane requires: {:?}",
        refused.events()
    );
    for silent in SILENT {
        let decoded = decoded(silent);
        assert!(
            matches!(decoded, Decoded::Events { ref events, .. } if events.is_empty()),
            "a silent omp frame produced something: {decoded:?}"
        );
    }
    let started = br#"{"type":"agent_start"}"#;
    let ended = br#"{"type":"agent_end","messages":[]}"#;
    assert_eq!(
        decoded(started).context().and_then(|context| context.turn),
        Some(TurnMoment::Started)
    );
    assert_eq!(
        decoded(ended).context().and_then(|context| context.turn),
        Some(TurnMoment::Completed)
    );
}

/// The live retune driver answers its own refusals and the occupant keeps
/// running; the same command refused while a launch opens is a failure.
#[test]
fn only_a_live_retune_refusal_is_left_to_its_driver() {
    let opening = br#"{"id":"spice-effort","type":"response","command":"set_thinking_level","success":false,"error":"unsupported"}"#;
    assert!(
        matches!(
            decoded(opening).events(),
            [Event::Failure(failure)] if failure.message == "set_thinking_level refused: unsupported"
        ),
        "a refused opening effort must fail the launch"
    );
    for command in ["set_thinking_level", "set_model"] {
        let live = format!(
            r#"{{"id":"{}0","type":"response","command":"{command}","success":false,"error":"unsupported"}}"#,
            listen::OMP_RETUNE_REQUEST_ID_PREFIX
        );
        assert!(
            decoded(live.as_bytes()).events().is_empty(),
            "a refused live {command} belongs to the retune driver"
        );
    }
}

/// An omp without host-owned schemes refuses the `spice://` registration;
/// the lane runs on without the scheme rather than failing.
#[test]
fn a_refused_uri_scheme_registration_is_no_failure() {
    let refused = format!(
        r#"{{"id":"{}","type":"response","command":"set_host_uri_schemes","success":false,"error":"Unknown command: set_host_uri_schemes"}}"#,
        listen::OMP_URI_SCHEMES_REQUEST_ID
    );
    assert!(decoded(refused.as_bytes()).events().is_empty());
    let other = br#"{"id":"spice-tools","type":"response","command":"set_host_uri_schemes","success":false,"error":"reserved"}"#;
    assert!(
        matches!(decoded(other).events(), [Event::Failure(_)]),
        "only the registration's own identity is exempt"
    );
}

/// omp refuses the opening fast mode only for a model without a priority
/// tier; the driver tells the lane row and the turn runs without it.
#[test]
fn a_refused_opening_fast_mode_is_no_failure() {
    let refused = format!(
        r#"{{"id":"{}","type":"response","command":"set_fast_mode","success":false,"error":"Fast mode is unavailable for the current model."}}"#,
        listen::OMP_FAST_REQUEST_ID
    );
    assert!(decoded(refused.as_bytes()).events().is_empty());
}

#[test]
fn prompt_results_preserve_identity_lifecycle_and_structured_provider_failure() {
    let completed = decoded(PROMPT_COMPLETED);
    assert!(completed.events().is_empty());
    let context = completed.context().expect("prompt result has context");
    assert_eq!(context.turn_id.as_deref(), Some("prompt-7"));
    assert!(matches!(
        context.prompt_outcome,
        Some(ref outcome)
            if outcome.agent_invoked
                && outcome.status == PromptStatus::Completed
                && !outcome.session_settled
    ));

    let failed = decoded(PROMPT_FAILED);
    assert!(matches!(
        failed.events(),
        [Event::Failure(failure)]
            if failure.message == "capacity exhausted"
                && matches!(
                    failure.provider,
                    Some(ref provider)
                        if provider.provider.as_deref() == Some("anthropic")
                            && provider.model.as_deref() == Some("claude-opus-5-5")
                            && provider.http_status == Some(529)
                            && provider.retryable
                )
    ));
    let context = failed.context().expect("failed prompt result has context");
    assert_eq!(context.turn_id.as_deref(), Some("prompt-8"));
    assert!(matches!(
        context.prompt_outcome,
        Some(ref outcome)
            if outcome.agent_invoked
                && outcome.status == PromptStatus::Error
                && outcome.session_settled
    ));

    let failed_without_detail = decoded(
        br#"{"type":"prompt_result","id":"prompt-plain-error","agentInvoked":false,"status":"error","sessionSettled":true}"#,
    );
    assert!(failed_without_detail.events().is_empty());
    assert!(matches!(
        failed_without_detail
            .context()
            .and_then(|context| context.prompt_outcome.as_ref()),
        Some(outcome)
            if !outcome.agent_invoked
                && outcome.status == PromptStatus::Error
                && outcome.session_settled
    ));

    let aborted = decoded(
        br#"{"type":"prompt_result","id":"prompt-9","agentInvoked":false,"status":"aborted","sessionSettled":true}"#,
    );
    assert!(aborted.events().is_empty());
    assert!(matches!(
        aborted
            .context()
            .and_then(|context| context.prompt_outcome.as_ref()),
        Some(outcome)
            if !outcome.agent_invoked
                && outcome.status == PromptStatus::Aborted
                && outcome.session_settled
    ));

    let settled = decoded(br#"{"type":"session_settled"}"#);
    assert!(settled.events().is_empty());
    assert!(
        settled
            .context()
            .is_some_and(|context| context.session_settled)
    );
}

#[test]
fn dialogs_that_wait_on_an_answer_are_named_while_notices_are_not() {
    let dialog = |bytes: &[u8]| {
        let decoded = decoded(bytes);
        assert!(decoded.events().is_empty(), "a dialog frame is silent");
        decoded.context().and_then(|context| context.dialog.clone())
    };
    assert_eq!(
        dialog(br#"{"type":"extension_ui_request","id":"8","method":"select","title":"Which?","options":["a"]}"#),
        Some(Dialog::Prompt { id: "8".to_owned() })
    );
    assert_eq!(
        dialog(br#"{"type":"extension_ui_request","id":"9","method":"confirm","title":"Deploy","message":"Deploy to prod?"}"#),
        Some(Dialog::Confirm {
            id: "9".to_owned(),
            title: "Deploy".to_owned(),
            message: "Deploy to prod?".to_owned(),
        })
    );
    assert_eq!(
        dialog(br#"{"type":"extension_ui_request","id":"10","method":"notify","message":"hi"}"#),
        None
    );
    assert_eq!(
        dialog(
            br#"{"type":"extension_ui_request","id":"11","method":"open_url","url":"https://x"}"#
        ),
        None
    );
    assert_eq!(dialog(br#"{"type":"agent_start"}"#), None);
}

#[test]
fn an_auth_notice_is_a_sign_out_naming_the_account_while_other_warnings_stay_silent() {
    let record = decoded(SIGN_OUT);
    let [Event::SignOut(sign_out)] = record.events() else {
        panic!("an auth notice decodes to one sign-out");
    };
    assert_eq!(
        sign_out.message,
        "Anthropic account ops@example.com was signed out automatically. Run /login to sign in again."
    );
    assert_eq!(sign_out.login, "omp login");
    let warning = br#"{"type":"notice","level":"warning","message":"Plan autosave failed: disk full Continuing with implementation.","source":"plan-yolo"}"#;
    assert!(decoded(warning).events().is_empty());
}

#[test]
fn a_host_tool_call_is_a_dialog_and_its_cancellation_withdraws_it() {
    let call = decoded(
        br#"{"type":"host_tool_call","id":"h1","toolCallId":"call_9","toolName":"shell","arguments":{"command":"cargo test","timeout":30}}"#,
    );
    assert!(call.events().is_empty(), "a host tool call is silent");
    let context = call.context().expect("a frame has context");
    assert_eq!(
        context.dialog,
        Some(Dialog::HostTool {
            id: "h1".to_owned(),
            tool: "shell".to_owned(),
            arguments: serde_json::json!({"command": "cargo test", "timeout": 30})
                .as_object()
                .expect("object")
                .clone(),
        })
    );
    assert_eq!(context.withdrawn, None);

    let cancel = decoded(br#"{"type":"host_tool_cancel","id":"h2","targetId":"h1"}"#);
    assert!(cancel.events().is_empty(), "a withdrawal is silent");
    let context = cancel.context().expect("a frame has context");
    assert_eq!(context.dialog, None);
    assert_eq!(context.withdrawn, Some("h1".to_owned()));
    assert_eq!(
        decoded(br#"{"type":"agent_start"}"#)
            .context()
            .and_then(|context| context.withdrawn.clone()),
        None
    );
}

#[test]
fn a_host_uri_request_is_a_dialog_and_its_cancellation_withdraws_it() {
    let request = decoded(
        br#"{"type":"host_uri_request","id":"u1","operation":"read","url":"spice://quote/v1/steer/1kQnrzNZx/00"}"#,
    );
    assert!(request.events().is_empty(), "a host URI request is silent");
    let context = request.context().expect("a frame has context");
    assert_eq!(
        context.dialog,
        Some(Dialog::HostUri {
            id: "u1".to_owned(),
            operation: "read".to_owned(),
            url: "spice://quote/v1/steer/1kQnrzNZx/00".to_owned(),
            content: None,
        })
    );
    let write = decoded(
        br#"{"type":"host_uri_request","id":"u3","operation":"write","url":"spice://task/X/description","content":"new text\n"}"#,
    );
    assert_eq!(
        write.context().and_then(|context| context.dialog.clone()),
        Some(Dialog::HostUri {
            id: "u3".to_owned(),
            operation: "write".to_owned(),
            url: "spice://task/X/description".to_owned(),
            content: Some("new text\n".to_owned()),
        })
    );

    let cancel = decoded(br#"{"type":"host_uri_cancel","id":"u2","targetId":"u1"}"#);
    assert!(cancel.events().is_empty(), "a withdrawal is silent");
    let context = cancel.context().expect("a frame has context");
    assert_eq!(context.dialog, None);
    assert_eq!(context.withdrawn, Some("u1".to_owned()));
}

#[test]
fn the_session_file_reads_under_the_same_dialect() {
    let session = decoded(TRANSCRIPT_SESSION);
    assert!(matches!(
        session.events(),
        [Event::Session(declaration)]
            if declaration.identity == "01a0a807-666f-7524-be0d-2ee476b6c15c"
    ));
    let message = decoded(TRANSCRIPT_MESSAGE);
    assert!(matches!(
        message.events(),
        [Event::Prose(prose), Event::Usage(_)] if prose.text == "echo: second thing"
    ));
    assert_eq!(
        message
            .context()
            .and_then(|context| context.timestamp.clone()),
        Some("2026-09-16T02:24:25.341Z".to_owned())
    );
}

/// A session-file message marks the turn its lifecycle frames would have.
#[test]
fn session_file_messages_mark_the_turns_their_frames_are_absent_for() {
    let turn = |bytes: &[u8]| decoded(bytes).context().and_then(|context| context.turn);
    assert_eq!(
        turn(br#"{"type":"message","id":"u1","parentId":null,"timestamp":"2026-09-16T02:24:20.000Z","message":{"role":"user","content":[{"type":"text","text":"second thing"}],"timestamp":1789546460000}}"#),
        Some(TurnMoment::Started)
    );
    assert_eq!(
        turn(br#"{"type":"message","id":"s1","parentId":"u1","timestamp":"2026-09-16T02:24:21.000Z","message":{"role":"user","content":[{"type":"text","text":"stop"}],"steering":true,"attribution":"user","timestamp":1789546461000}}"#),
        None,
        "a steer lands inside the turn it addresses"
    );
    assert_eq!(
        turn(br#"{"type":"message","id":"a1","parentId":"s1","timestamp":"2026-09-16T02:24:22.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"c1","name":"bash","arguments":{"command":"true"}}],"usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0},"stopReason":"toolUse"}}"#),
        None,
        "stopping for a tool call is mid-turn"
    );
    assert_eq!(
        turn(br#"{"type":"message","id":"r1","parentId":"a1","timestamp":"2026-09-16T02:24:23.000Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"bash","content":[{"type":"text","text":""}],"isError":false,"timestamp":1789546463000}}"#),
        None
    );
    assert_eq!(turn(TRANSCRIPT_MESSAGE), Some(TurnMoment::Completed));
    assert_eq!(
        turn(br#"{"type":"message","id":"x1","parentId":"r1","timestamp":"2026-09-16T02:24:24.000Z","message":{"role":"assistant","content":[],"stopReason":"aborted"}}"#),
        Some(TurnMoment::Completed)
    );
    // On the wire the lifecycle frames speak and the restated message stays silent.
    assert_eq!(
        turn(br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"stopReason":"stop"}}"#),
        None
    );
}

/// Omp bookkeeping and lifecycle records that carry no listening moment.
const AMBIENTLY_OWNED_BOOKKEEPING: &[&[u8]] = &[
    br#"{"type":"title","v":1,"title":"","updatedAt":"2026-09-16T06:43:21.805Z","pad":"   "}"#,
    br#"{"type":"title_change","id":"n1","parentId":null,"timestamp":"2026-09-16T06:43:21.900Z","title":"Current title","source":"auto"}"#,
    br#"{"type":"credential_pin","id":"c1","parentId":null,"timestamp":"2026-09-16T06:43:22.000Z","provider":"anthropic","hash":"deadbeef"}"#,
    br#"{"type":"model_change","id":"m1","parentId":"c1","timestamp":"2026-09-16T06:43:22.100Z","provider":"anthropic","modelId":"claude-opus-5"}"#,
    br#"{"type":"thinking_level_change","id":"t1","parentId":"m1","timestamp":"2026-09-16T06:43:22.200Z","thinkingLevel":"high"}"#,
    br#"{"type":"custom","id":"x1","parentId":"t1","timestamp":"2026-09-16T06:43:22.300Z","customType":"spice","data":{}}"#,
    br#"{"type":"label","id":"l1","parentId":"x1","timestamp":"2026-09-16T06:43:22.400Z","targetId":"x1","label":"here"}"#,
    br#"{"type":"session_init","id":"i1","parentId":null,"timestamp":"2026-09-16T06:43:22.500Z","systemPrompt":"","tools":[]}"#,
    br#"{"type":"mode_change","id":"d1","parentId":"i1","timestamp":"2026-09-16T06:43:22.600Z","mode":"build"}"#,
    br#"{"type":"manual_compaction_start","mode":"snapcompact"}"#,
    br#"{"type":"extension_error","message":"widget unavailable"}"#,
];

#[test]
fn known_bookkeeping_is_owned_ambiently_and_says_nothing() {
    for bytes in AMBIENTLY_OWNED_BOOKKEEPING {
        let detected =
            decode_record(None, bytes, locus(bytes)).expect("omp owns its known bookkeeping");
        assert_eq!(detected.dialect, Dialect::Omp, "{bytes:?}");
        assert!(
            !matches!(detected.decoded, Decoded::Residue(_))
                && detected.decoded.events().is_empty(),
            "bookkeeping is neither residue nor typed: {:?}",
            detected.decoded
        );
    }
    // The session's own compaction record types the same event the RPC frame does.
    let compaction = decoded(
        br#"{"type":"compaction","id":"k1","parentId":"d1","timestamp":"2026-09-16T06:43:23.000Z","summary":"so far","firstKeptEntryId":"e1","tokensBefore":900000}"#,
    );
    assert!(matches!(compaction.events(), [Event::Compaction(record)]
            if record.summary.as_deref() == Some("so far") && record.method.is_none()));
}

/// A compaction is one boundary however omp started it, and only when it
/// committed.
///
/// omp announces an automatic and a manual compaction with one shape: a
/// committed `result`, or an `aborted`, skipped, or result-less end that
/// closed an attempt without committing. A provider method can fail before
/// omp falls through to another method; that end closes the attempt, not the
/// compaction. A manual start is lifecycle, not a boundary.
#[test]
fn a_compaction_is_one_boundary_however_started_and_only_when_committed() {
    const MANUAL: &[u8] = br#"{"type":"manual_compaction_end","result":{"summary":"so far","shortSummary":"Archived 38,080 chars","firstKeptEntryId":"e1","tokensBefore":56112,"tokensAfter":33642,"method":"snapcompact"},"aborted":false}"#;
    const MANUAL_START: &[u8] = br#"{"type":"manual_compaction_start","mode":"snapcompact"}"#;
    const MANUAL_CANCELLED: &[u8] = br#"{"type":"manual_compaction_end","aborted":true}"#;
    const MANUAL_NOTHING: &[u8] = br#"{"type":"manual_compaction_end","aborted":false,"errorMessage":"Nothing to compact (session too small)"}"#;
    const FAILED_REMOTE: &[u8] = br#"{"type":"auto_compaction_end","action":"remote","aborted":false,"willRetry":false,"errorMessage":"Auto-compaction failed: remote input exceeds the context window; trying the next preferred compaction method."}"#;
    const SKIPPED: &[u8] =
        br#"{"type":"auto_compaction_end","action":"context-full","skipped":true,"aborted":false}"#;

    let automatic = decoded(COMPACTION);
    let manual = decoded(MANUAL);
    let [Event::Compaction(automatic)] = automatic.events() else {
        panic!("an automatic end with a result is one compaction");
    };
    let [Event::Compaction(manual)] = manual.events() else {
        panic!("a manual end with a result is one compaction");
    };
    assert_eq!(automatic.summary.as_deref(), Some("so far"));
    assert_eq!(automatic.method.as_deref(), Some("snapcompact"));
    assert_eq!(manual.summary.as_deref(), Some("so far"));
    assert_eq!(manual.method.as_deref(), Some("snapcompact"));

    const REMOTE: &[u8] = br#"{"type":"auto_compaction_end","action":"remote","result":{"shortSummary":"Remote compaction","firstKeptEntryId":"e2","tokensBefore":260848,"preserveData":{"anthropicCompaction":{"content":"Goal: finish"}}},"aborted":false,"willRetry":false}"#;
    let remote = decoded(REMOTE);
    let [Event::Compaction(remote)] = remote.events() else {
        panic!("a remote end with a result is one compaction");
    };
    assert_eq!(remote.summary.as_deref(), Some("Remote compaction"));
    assert_eq!(remote.method.as_deref(), Some("remote"));

    for attempt in [
        MANUAL_START,
        MANUAL_CANCELLED,
        MANUAL_NOTHING,
        FAILED_REMOTE,
        SKIPPED,
    ] {
        assert!(
            decoded(attempt).events().is_empty(),
            "an attempt that committed nothing is no boundary: {}",
            String::from_utf8_lossy(attempt),
        );
    }
}

#[test]
fn cumulative_partials_are_restated_while_a_blocked_job_wait_is_current_activity() {
    for partial in [PARTIAL, TOOL_PARTIAL] {
        assert!(restated(Dialect::Omp, partial));
        let decoded = decode(Dialect::Omp, partial, locus(partial)).expect("locus matches");
        assert!(decoded.events().is_empty(), "{decoded:?}");
        assert!(!matches!(decoded, Decoded::Residue(_)), "{decoded:?}");
    }
    assert!(!restated(Dialect::Omp, JOB_PARTIAL));
    assert!(matches!(
        decoded(JOB_PARTIAL).events(),
        [Event::Tool(tool)]
            if tool.call_id == "call_2"
                && matches!(
                    &tool.moment,
                    ToolMoment::Update { name, arguments }
                        if name == "job"
                            && serde_json::from_str::<serde_json::Value>(arguments)
                                .ok()
                                .and_then(|result| result["content"][0]["text"].as_str().map(str::to_owned))
                                .as_deref()
                                == Some("[job dW3w7qC4G] serve\nready on 8080")
                )
    ));
    for whole in [PROSE, TOOL_END, SESSION, COMPACTION] {
        assert!(!restated(Dialect::Omp, whole));
    }
    assert!(
        !restated(Dialect::Omp, b"{\"type\":\"message_update\""),
        "a torn record is kept"
    );
}

#[test]
fn an_unreassembled_transport_envelope_is_residue_not_silent_content() {
    let record = br#"{"type":"rpc_chunk","chunkId":"one","index":0,"count":5,"byteLength":1048577,"data":"eA=="}"#;
    assert!(matches!(decoded(record), Decoded::Residue(_)));
    assert!(!restated(Dialect::Omp, record));
}

#[test]
fn a_record_omp_does_not_own_is_refused_without_an_explicit_dialect() {
    let future = br#"{"type":"future_record"}"#;
    let bare_message = br#"{"type":"message","role":"assistant","content":"done"}"#;
    for record in [&future[..], &bare_message[..]] {
        assert_eq!(
            decode_record(None, record, locus(record)),
            Err(RecordDecodeError::Unowned)
        );
    }
}

#[test]
fn withheld_records_carry_no_reasoning_in_any_omp_shape() {
    let carriers = [
        r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"SECRET plan","thinkingSignature":"SECRET-sig"},{"type":"text","text":"visible"}]}}"#,
        r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","contentIndex":0,"delta":"SECRET step","partial":{"role":"assistant","content":[{"type":"thinking","thinking":"SECRET step"}]}},"message":{"role":"assistant","content":[{"type":"thinking","thinking":"SECRET step"}]}}"#,
        r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_end","contentIndex":0,"content":"SECRET whole"}}"#,
        r#"{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"redactedThinking","data":"SECRET-opaque"},{"type":"text","text":"visible"}]}]}"#,
    ];
    for record in carriers {
        let withheld =
            withhold_reasoning(Dialect::Omp, record).expect("an object record is rewritten");
        assert!(!withheld.contains("SECRET"), "{withheld}");
        assert!(
            withheld.contains("visible") || !record.contains("visible"),
            "{withheld}"
        );
        assert!(
            withheld.contains(WITHHELD_REASONING) || record.contains("redactedThinking"),
            "{withheld}"
        );
    }

    let thinking_level: &[u8] = br#"{"type":"thinking_level_changed","thinkingLevel":"high"}"#;
    for unchanged in [
        PROSE,
        TOOL,
        OPERATOR,
        PARTIAL,
        PROMPT_FAILED,
        thinking_level,
    ] {
        let record = std::str::from_utf8(unchanged).expect("fixture is UTF-8");
        assert_eq!(
            withhold_reasoning(Dialect::Omp, record).as_deref(),
            Some(record),
            "a record without reasoning is returned exactly as written"
        );
    }

    assert_eq!(withhold_reasoning(Dialect::Omp, "not json"), None);
    assert_eq!(withhold_reasoning(Dialect::Omp, r#"["thinking"]"#), None);
}
