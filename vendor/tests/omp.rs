//! oh-my-pi's RPC launch contract: the argument vector, the piped stdin it
//! opens, and the steering anchor its system prompt carries.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::time::Duration;

use listen::{Dialect, EventKind, Locus};
use vendor::{
    ContractError, Effort, Launch, OMP, RecordStream, Resume, SessionId, SteeringKey, omp,
};

const SESSION: &[u8] = br#"{"id":"spice-state","type":"response","command":"get_state","success":true,"data":{"model":{"id":"claude-opus-5","contextWindow":1000000},"sessionId":"01a0a807-666f-7524-be0d-2ee476b6c15c","isStreaming":false}}"#;
const PROSE: &[u8] = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"usage":{"input":7,"output":2,"cacheRead":5,"cacheWrite":3},"stopReason":"stop"}}"#;

const STEERING_TOKEN: &str = "1kJHh5bbx";
/// Width of the title slot omp rewrites in place at the top of a session file.
const TITLE_SLOT_BYTES: usize = 256;
const MODEL: &str = "claude-opus-5";
/// Stands in for the compact installed-skill resource bootstrap.
const BOOTSTRAP: &str = "Read `spice://SKILL.md` before any tool.";
/// Where the lane wrote this occupant's compaction threshold.
const CONFIG_OVERLAY: &str = "/supervisors/lane/occupant/omp.yml";
/// Where the lane wrote this occupant's steering extension.
const STEERING_EXTENSION: &str = "/supervisors/lane/occupant/omp-steering.mjs";

fn fixture(resume: Resume) -> Launch {
    Launch {
        cwd: "/worktree".into(),
        prompt: "start".to_owned(),
        bootstrap: BOOTSTRAP.to_owned(),
        model: MODEL.to_owned(),
        effort: Effort::XHigh,
        fast_mode: Some(false),
        auto_compact_window_tokens: Some(220_000),
        config_overlay: Some(CONFIG_OVERLAY.into()),
        steering_extension: Some(STEERING_EXTENSION.into()),
        lsp: Some(false),
        steering_key: Some(SteeringKey {
            token: STEERING_TOKEN.to_owned(),
        }),
        resume,
    }
}

#[test]
fn the_occupants_plan_is_the_task_it_holds_rather_than_a_list_only_it_can_read() {
    for roster in [omp::TOOLS, omp::LSP_TOOLS] {
        assert!(
            !roster.split(',').any(|tool| tool == "todo"),
            "a private plan is one no reviewer can check and one that drifts from the board"
        );
    }
    // omp's stop reminder counts the items a resumed session still carries
    // without asking whether the roster can close them, and wakes the turn
    // to say so; every other todo nudge checks the tool first.
    for lsp in [false, true] {
        assert!(
            omp::config_overlay(170_000, lsp).contains("todo:\n  reminders: false\n"),
            "an occupant with no todo tool must not be told to complete todo items"
        );
    }
}

#[test]
fn omp_cannot_delegate_around_the_lane_allocator() {
    for roster in [omp::TOOLS, omp::LSP_TOOLS] {
        assert!(
            !roster.split(',').any(|tool| tool == "task"),
            "one occupant cannot allocate another agent"
        );
    }
    let overlay = omp::config_overlay(170_000, false);
    assert!(
        overlay.contains("task:\n  maxRecursionDepth: 0\n"),
        "eval agent and workpool bridges share omp's task-depth gate"
    );
    assert!(
        overlay.contains("eval:\n  tools:\n    enabled: false\n"),
        "kernel-defined tools have no admitted subagent consumer"
    );
}

#[test]
fn a_job_call_names_its_op_and_the_job_every_op_but_a_listing_needs() {
    let arguments = |json: serde_json::Value| json.as_object().expect("an argument object").clone();
    assert_eq!(
        omp::JobCall::parse(&arguments(serde_json::json!({"op": "list"}))),
        Ok(omp::JobCall {
            op: omp::JobOp::List,
            id: None,
            timeout: Duration::from_secs(omp::SHELL_FOREGROUND),
            until: None,
        })
    );
    assert_eq!(
        omp::JobCall::parse(&arguments(
            serde_json::json!({"op": "wait", "id": "0badcafe", "timeout": 999_999})
        )),
        Ok(omp::JobCall {
            op: omp::JobOp::Wait,
            id: Some("0badcafe".to_owned()),
            timeout: Duration::from_secs(omp::SHELL_TIMEOUT_MAX),
            until: None,
        })
    );
    assert_eq!(
        omp::JobCall::parse(&arguments(serde_json::json!({"op": "stop"}))),
        Err("job stop needs an id".to_owned())
    );
    assert_eq!(
        omp::JobCall::parse(&arguments(serde_json::json!({"op": "restart", "id": "1"}))),
        Err("job has no op restart".to_owned())
    );
    assert_eq!(
        omp::JobCall::parse(&arguments(serde_json::json!({"id": "1"}))),
        Err("job needs an op".to_owned())
    );
    let until = |json| omp::JobCall::parse(&arguments(json)).map(|call| call.until);
    assert_eq!(
        until(serde_json::json!({"op": "wait", "id": "0badcafe", "until": "endpoint=\\S+"})),
        Ok(Some(
            omp::OutputPattern::new("endpoint=\\S+").expect("pattern compiles")
        ))
    );
    assert!(
        until(serde_json::json!({"op": "wait", "id": "0badcafe", "until": "endpoint=("})).is_err(),
        "a pattern that does not compile is refused before any wait"
    );
    assert!(
        until(serde_json::json!({"op": "logs", "id": "0badcafe", "until": "ready"})).is_err(),
        "only wait looks for a line"
    );
}

#[test]
fn a_history_call_admits_limits_within_its_surface_and_refuses_the_rest() {
    let arguments = |json: serde_json::Value| json.as_object().expect("an argument object").clone();
    let budget = |json| match omp::JobCall::parse(&arguments(json)) {
        Ok(omp::JobCall {
            op: omp::JobOp::History { budget },
            ..
        }) => Ok(budget.limits()),
        other => Err(format!("{other:?}")),
    };
    let (default, maximum) = (
        omp::JOB_HISTORY.default_limits(),
        omp::JOB_HISTORY.maximum(),
    );
    assert_eq!(budget(serde_json::json!({"op": "history"})), Ok(default));
    assert_eq!(
        budget(serde_json::json!({"op": "history", "id": "6ZbQk3mPx", "limit": maximum.rows})),
        Ok(wire::output::Limits {
            rows: maximum.rows,
            ..default
        }),
        "a cursor is optional and the maximum itself is admitted"
    );
    assert_eq!(
        omp::JobCall::parse(&arguments(
            serde_json::json!({"op": "history", "limit": maximum.rows + 1})
        )),
        Err(format!(
            "job history limit {} exceeds the maximum {}",
            maximum.rows + 1,
            maximum.rows
        ))
    );
    assert_eq!(
        omp::JobCall::parse(&arguments(serde_json::json!({"op": "history", "limit": 0}))),
        Err("job history limit must be at least 1".to_owned())
    );
}

#[test]
fn a_shell_call_always_has_a_bounded_deadline_unless_it_is_asked_for_as_a_service() {
    let arguments = |json: serde_json::Value| json.as_object().expect("an argument object").clone();
    let parse = |json| omp::ShellCall::parse(&arguments(json));
    let deadline = |seconds| omp::Lifetime::Deadline(Duration::from_secs(seconds));
    let call = |lifetime, notices: Vec<&str>| {
        Ok(omp::ShellCall {
            command: "cargo test".to_owned(),
            lifetime,
            notices: notices.into_iter().map(str::to_owned).collect(),
            until: None,
        })
    };
    assert_eq!(omp::SHELL_TIMEOUT_DEFAULT, 15 * 60);
    assert_eq!(omp::SHELL_TIMEOUT_MAX, 60 * 60);
    assert_eq!(
        parse(serde_json::json!({"command": "cargo test"})),
        call(deadline(omp::SHELL_TIMEOUT_DEFAULT), vec![])
    );
    for bound in [omp::SHELL_TIMEOUT_MIN, omp::SHELL_TIMEOUT_MAX] {
        assert_eq!(
            parse(serde_json::json!({"command": "cargo test", "timeout": bound})),
            call(deadline(bound), vec![])
        );
    }
    assert_eq!(
        parse(serde_json::json!({"command": "cargo test", "timeout": 999_999})),
        call(
            deadline(omp::SHELL_TIMEOUT_MAX),
            vec!["Timeout clamped to 3600s (requested 999999s; allowed range 1-3600s)."]
        )
    );
    assert!(
        parse(serde_json::json!({"command": "cargo test", "timeout": 0})).is_err(),
        "zero never lifts an ordinary command's deadline"
    );
    assert_eq!(
        parse(serde_json::json!({"command": "cargo test", "service": true})),
        call(omp::Lifetime::Service, vec![])
    );
    assert_eq!(
        parse(serde_json::json!({"command": "cargo test", "service": false})),
        call(deadline(omp::SHELL_TIMEOUT_DEFAULT), vec![])
    );
    assert!(
        parse(serde_json::json!({"command": "cargo test", "service": true, "timeout": 60}))
            .is_err(),
        "a service has no deadline to name"
    );
    let until = |json| parse(json).map(|call| call.until);
    for json in [
        serde_json::json!({"command": "tail -f log", "timeout": 60, "until": "endpoint=\\S+"}),
        serde_json::json!({"command": "app-demo", "service": true, "until": "endpoint=\\S+"}),
    ] {
        assert_eq!(
            until(json),
            Ok(Some(
                omp::OutputPattern::new("endpoint=\\S+").expect("pattern compiles")
            )),
            "a command or a service may be watched for the pattern job wait takes"
        );
    }
    for json in [
        serde_json::json!({"command": "tail -f log", "until": "endpoint=("}),
        serde_json::json!({"command": "tail -f log", "until": true}),
    ] {
        assert!(
            until(json).is_err(),
            "a pattern that is not a compiling string is refused before launch"
        );
    }
    assert_eq!(
        omp::job_delivery("0badcafe", "done\n\nWall time: 0.10 seconds"),
        "<system-notice>\nBackground job 0badcafe has completed. Resume your work using the \
         result below.\ndone\n\nWall time: 0.10 seconds\n</system-notice>"
    );
    assert_eq!(
        omp::ShellCall::parse(&arguments(serde_json::json!({"timeout": 5}))),
        Err("shell needs a command".to_owned())
    );
}

#[test]
fn host_tool_progress_is_partial_and_only_answers_or_refusals_settle_the_call() {
    let frame = |bytes: Vec<u8>| -> serde_json::Value {
        serde_json::from_slice(bytes.strip_suffix(b"\n").expect("one line")).expect("JSON")
    };
    assert_eq!(
        frame(omp::host_tool_update(
            "call-0",
            "[job dW3w7qC4G] serve\nready"
        )),
        serde_json::json!({
            "type": "host_tool_update",
            "id": "call-0",
            "partialResult": {
                "content": [{"type": "text", "text": "[job dW3w7qC4G] serve\nready"}],
            },
        })
    );
    assert_eq!(
        frame(omp::host_tool_result("call-1", "ok\n", Some(0), false)),
        serde_json::json!({
            "type": "host_tool_result",
            "id": "call-1",
            "result": {
                "content": [{"type": "text", "text": "ok\n"}],
                "details": {"exitCode": 0},
            },
        })
    );
    assert_eq!(
        frame(omp::host_tool_result(
            "call-2",
            "boom\n\nCommand exited with code 3",
            Some(3),
            true
        )),
        serde_json::json!({
            "type": "host_tool_result",
            "id": "call-2",
            "result": {
                "content": [{"type": "text", "text": "boom\n\nCommand exited with code 3"}],
                "details": {"exitCode": 3},
                "isError": true,
            },
        })
    );
    assert_eq!(
        frame(omp::host_tool_refused("call-3", "shell needs a command")),
        serde_json::json!({
            "type": "host_tool_result",
            "id": "call-3",
            "result": {"content": [{"type": "text", "text": "shell needs a command"}]},
            "isError": true,
        })
    );
}

#[test]
fn omp_resumes_by_the_exact_session_and_the_vendor_value_renders_the_same_record() {
    let launch = fixture(Resume::Session(
        SessionId::new("01a0a807-666f-7524-be0d-2ee476b6c15c").expect("session identity"),
    ));
    let record = omp::record("/opt/omp", &launch).expect("valid resumed record");
    let arguments: Vec<_> = record
        .arguments()
        .iter()
        .map(|a| a.to_string_lossy())
        .collect();
    assert_eq!(
        &arguments[arguments.len() - 2..],
        ["--resume", "01a0a807-666f-7524-be0d-2ee476b6c15c"]
    );
    assert_eq!(
        record.resume().session_id(),
        Some("01a0a807-666f-7524-be0d-2ee476b6c15c")
    );
    assert_eq!(
        record.environment(),
        [(
            OsString::from(omp::SESSION_ENVIRONMENT),
            OsString::from("01a0a807-666f-7524-be0d-2ee476b6c15c"),
        )],
        "the identity rides the child environment because omp exports none"
    );
    assert_eq!(OMP.thread_id_environment(), omp::SESSION_ENVIRONMENT);
    assert_eq!(
        OMP.record("/opt/omp", &launch)
            .expect("vendor value renders"),
        record
    );
}

#[test]
fn omp_refuses_an_unspellable_binary() {
    assert!(matches!(
        omp::record("", &fixture(Resume::Fresh)),
        Err(ContractError::Spec(_))
    ));
}

#[test]
fn fast_mode_rides_only_the_opening_and_must_be_explicit() {
    let standard = omp::record("/opt/omp", &fixture(Resume::Fresh)).expect("valid record");
    let mut fast = fixture(Resume::Fresh);
    fast.fast_mode = Some(true);
    let fast = omp::record("/opt/omp", &fast).expect("valid fast record");
    assert_eq!(
        fast.arguments(),
        standard.arguments(),
        "fast mode states no launch tier; the opening states it for the selected model"
    );

    let mut implicit = fixture(Resume::Fresh);
    implicit.fast_mode = None;
    assert_eq!(
        omp::record("/opt/omp", &implicit).expect_err("refused"),
        ContractError::Missing {
            path: "vendor.omp.fast_mode"
        }
    );
}

#[test]
fn the_compaction_threshold_rides_an_overlay_the_lane_must_have_written() {
    // The exact document omp 18.2.11 resolves to a 170,000-token limit through
    // its `--config` overlay; the layout is omp's own config.yml.
    assert_eq!(
        omp::config_overlay(170_000, false),
        "compaction:\n  thresholdTokens: 170000\nlsp:\n  enabled: false\neval:\n  tools:\n    enabled: false\ntask:\n  maxRecursionDepth: 0\ntodo:\n  reminders: false\nproviders:\n  openaiLiveSteering: false\n"
    );

    let mut unwritten = fixture(Resume::Fresh);
    unwritten.config_overlay = None;
    assert_eq!(
        omp::record("/opt/omp", &unwritten).expect_err("refused"),
        ContractError::Missing {
            path: "vendor.omp.config_overlay"
        }
    );
}

#[test]
fn the_language_server_disposition_names_the_tool_and_keeps_its_servers_in_the_lane() {
    let declined = omp::record("/opt/omp", &fixture(Resume::Fresh)).expect("valid record");
    assert_eq!(roster(&declined), Some(OsStr::new(omp::TOOLS)));
    assert!(!omp::TOOLS.split(',').any(|tool| tool == "lsp"));
    assert!(omp::config_overlay(170_000, false).contains("lsp:\n  enabled: false\n"));

    let mut admitted = fixture(Resume::Fresh);
    admitted.lsp = Some(true);
    let record = omp::record("/opt/omp", &admitted).expect("valid record");
    assert_eq!(roster(&record), Some(OsStr::new(omp::LSP_TOOLS)));
    assert_eq!(
        omp::LSP_TOOLS,
        format!("{},lsp", omp::TOOLS),
        "the admitted roster is the declined roster plus the tool"
    );
    // Shared servers are brokered by a daemon built to outlive one session,
    // which is exactly the process the lane could neither see nor reap.
    assert_eq!(
        omp::config_overlay(170_000, true),
        "compaction:\n  thresholdTokens: 170000\nlsp:\n  enabled: true\n  shared: false\neval:\n  tools:\n    enabled: false\ntask:\n  maxRecursionDepth: 0\ntodo:\n  reminders: false\nproviders:\n  openaiLiveSteering: false\n"
    );

    let mut implicit = fixture(Resume::Fresh);
    implicit.lsp = None;
    assert_eq!(
        omp::record("/opt/omp", &implicit).expect_err("refused"),
        ContractError::Missing {
            path: "vendor.omp.lsp"
        }
    );
}

fn served(models: &[(&str, &str)]) -> Vec<listen::ServedModel> {
    models
        .iter()
        .map(|(provider, id)| listen::ServedModel {
            provider: (*provider).to_owned(),
            id: (*id).to_owned(),
        })
        .collect()
}

fn requests(bytes: &[u8]) -> Vec<serde_json::Value> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("a request line"))
        .collect()
}

#[test]
fn the_process_line_carries_only_settings_no_rpc_command_states() {
    for resumed in [false, true] {
        let resume = if resumed {
            Resume::Session(
                SessionId::new("01a0a807-666f-7524-be0d-2ee476b6c15c").expect("session identity"),
            )
        } else {
            Resume::Fresh
        };
        let record = omp::record("/opt/omp", &fixture(resume)).expect("valid record");
        let flags: Vec<_> = record
            .arguments()
            .iter()
            .map(|argument| argument.to_string_lossy())
            .filter(|argument| argument.starts_with("--"))
            .collect();
        let mut expected = vec![
            "--mode",
            "--config",
            "--extension",
            "--approval-mode",
            "--tools",
            "--append-system-prompt",
        ];
        if resumed {
            expected.push("--resume");
        }
        assert_eq!(
            flags, expected,
            "only bootstrap, paths, and settings without an RPC command ride the process line"
        );
        assert!(
            !record
                .arguments()
                .iter()
                .any(|argument| argument.to_string_lossy().contains(MODEL)),
            "no process line names the model"
        );
    }
}

#[test]
fn the_model_rides_rpc_and_selects_exactly_one_served_model_before_the_effort() {
    let record = omp::record("/opt/omp", &fixture(Resume::Fresh)).expect("valid record");
    let selection = record
        .model_selection()
        .expect("the launch selects its model");
    assert_eq!(selection.spelling(), MODEL);

    let listing = served(&[
        ("openai-codex", "gpt-5.6-sol"),
        ("anthropic", "claude-opus-5"),
        ("anthropic", "claude-sonnet-5"),
    ]);
    for (spelling, fast_mode) in [
        (MODEL, false),
        ("Claude-Opus-5", true),
        ("anthropic/claude-opus-5", false),
    ] {
        let requests = requests(
            &omp::ModelSelection::new(spelling, Effort::XHigh, fast_mode)
                .request(&listing)
                .expect("one served model answers"),
        );
        assert_eq!(
            requests,
            [
                serde_json::json!({
                    "id": omp::MODEL_REQUEST_ID,
                    "type": "set_model",
                    "provider": "anthropic",
                    "modelId": "claude-opus-5",
                }),
                serde_json::json!({
                    "id": omp::EFFORT_REQUEST_ID,
                    "type": "set_thinking_level",
                    "level": "xhigh",
                }),
                serde_json::json!({
                    "id": listen::OMP_FAST_REQUEST_ID,
                    "type": "set_fast_mode",
                    "enabled": fast_mode,
                }),
            ],
            "{spelling}: omp's set_model resets the level, and a resumed session's recorded \
             tier outranks the overlay, so the effort and fast mode follow it"
        );
    }
    let mut fast = fixture(Resume::Fresh);
    fast.fast_mode = Some(true);
    let fast_request = requests(
        &omp::record("/opt/omp", &fast)
            .expect("valid fast record")
            .model_selection()
            .expect("the launch selects its model")
            .request(&listing)
            .expect("one served model answers"),
    );
    assert_eq!(
        fast_request[2]["enabled"], true,
        "the launch's own fast mode rides the opening"
    );

    assert_eq!(
        omp::ModelSelection::new("openai/claude-opus-5", Effort::XHigh, false).request(&listing),
        Err(omp::SelectionError::Unserved {
            spelling: "openai/claude-opus-5".to_owned(),
        }),
        "a named provider must be the one serving the model"
    );
    let shared = served(&[("openai", "gpt-5.6-sol"), ("openai-codex", "gpt-5.6-sol")]);
    assert_eq!(
        omp::ModelSelection::new("gpt-5.6-sol", Effort::XHigh, false).request(&shared),
        Err(omp::SelectionError::Ambiguous {
            spelling: "gpt-5.6-sol".to_owned(),
            candidates: vec![
                "openai/gpt-5.6-sol".to_owned(),
                "openai-codex/gpt-5.6-sol".to_owned(),
            ],
        })
    );
    assert!(
        omp::ModelSelection::new("openai-codex/gpt-5.6-sol", Effort::XHigh, false)
            .request(&shared)
            .is_ok(),
        "naming the provider settles a shared identity"
    );
}

#[test]
fn qualified_model_selection_does_not_fall_through_to_a_slash_bearing_raw_id() {
    let selection = omp::ModelSelection::new("OpenAI-Codex/GPT-5.6-Sol", Effort::High, false);
    let collision = served(&[
        ("openai-codex", "gpt-5.6-sol"),
        ("openrouter", "openai-codex/gpt-5.6-sol"),
    ]);
    let chosen = requests(
        &selection
            .request(&collision)
            .expect("the explicit native pair wins"),
    );
    assert_eq!(chosen[0]["provider"], "openai-codex");
    assert_eq!(chosen[0]["modelId"], "gpt-5.6-sol");
    assert_eq!(chosen[1]["type"], "set_thinking_level");

    assert_eq!(
        selection.request(&served(&[("openrouter", "openai-codex/gpt-5.6-sol")])),
        Err(omp::SelectionError::Unserved {
            spelling: "OpenAI-Codex/GPT-5.6-Sol".to_owned(),
        }),
        "an absent explicit provider/id never falls through to a raw identity"
    );

    let aggregator = omp::ModelSelection::new("OpenRouter/OpenAI/GPT-5.6-Sol", Effort::High, false);
    let chosen = requests(
        &aggregator
            .request(&served(&[("openrouter", "openai/gpt-5.6-sol")]))
            .expect("the provider splits from the slash-bearing identity once"),
    );
    assert_eq!(chosen[0]["provider"], "openrouter");
    assert_eq!(chosen[0]["modelId"], "openai/gpt-5.6-sol");
}

fn roster(record: &vendor::Record) -> Option<&OsStr> {
    record
        .arguments()
        .windows(2)
        .find(|pair| pair[0] == OsStr::new("--tools"))
        .map(|pair| pair[1].as_os_str())
}

#[test]
fn the_resource_bootstrap_rides_the_system_prompt_and_the_turn_stays_compact() {
    let launch = fixture(Resume::Fresh);
    let system_prompt = omp::system_prompt(&launch);
    assert!(system_prompt.contains(BOOTSTRAP));
    assert!(system_prompt.contains(&format!("<{STEERING_TOKEN}>")));

    // Every turn re-anchors on the key and requires the resource read without
    // copying even the bootstrap again.
    let turn_prompt = omp::turn_prompt(&launch);
    assert!(turn_prompt.contains("\n\nstart\n\n"));
    assert!(turn_prompt.contains(&format!("<{STEERING_TOKEN}>")));
    assert!(!turn_prompt.contains(BOOTSTRAP));

    let mut anonymous = launch.clone();
    anonymous.steering_key = None;
    assert!(!omp::system_prompt(&anonymous).contains(STEERING_TOKEN));
    assert!(!omp::turn_prompt(&anonymous).contains(STEERING_TOKEN));
}

#[test]
fn steer_requests_carry_the_steering_block() {
    let steer = omp::steer_request(3, "<1kJHh5bbx>\ndirective=d1\nstop\n</1kJHh5bbx>\n");
    let frame: serde_json::Value =
        serde_json::from_slice(steer.strip_suffix(b"\n").expect("one line")).expect("JSON");
    assert_eq!(
        frame,
        serde_json::json!({
            "id": "spice-steer-3",
            "type": "prompt",
            "message": "<1kJHh5bbx>\ndirective=d1\nstop\n</1kJHh5bbx>\n",
            "streamingBehavior": "steer",
        })
    );
}

#[test]
fn a_drain_interrupts_the_turn_with_one_abort_request() {
    let abort = omp::abort_request();
    let frame: serde_json::Value =
        serde_json::from_slice(abort.strip_suffix(b"\n").expect("one line")).expect("JSON");
    assert_eq!(
        frame,
        serde_json::json!({"id": "spice-abort", "type": "abort"})
    );
}

#[test]
fn the_state_reply_declares_the_session_and_content_rides_the_selected_stream() {
    let record = omp::record("/opt/omp", &fixture(Resume::Fresh)).expect("valid record");
    let locus = Locus::new(2, 17, SESSION.len() as u64);
    let decoded = record
        .decode(RecordStream::Stdout, SESSION, locus)
        .expect("matching locus")
        .expect("selected stream");
    let declaration = vendor::SessionDeclaration::admit(&decoded)
        .expect("nonempty identity")
        .expect("the state reply declares the session");
    assert_eq!(
        declaration.session.as_str(),
        "01a0a807-666f-7524-be0d-2ee476b6c15c"
    );
    assert_eq!(declaration.locus, locus);

    let prose = record
        .decode(
            RecordStream::Stdout,
            PROSE,
            Locus::new(2, 17, PROSE.len() as u64),
        )
        .expect("matching locus")
        .expect("selected stream");
    assert!(prose.events().iter().any(|e| e.kind() == EventKind::Prose));
    assert_eq!(
        record
            .decode(RecordStream::Stderr, b"diag", Locus::new(2, 17, 4))
            .expect("diagnostics are not decoded"),
        None
    );
}

#[test]
fn a_pinned_session_lands_where_omp_keeps_this_worktree_and_reads_back_as_its_own() {
    let at = "2026-09-16T06:43:21.805Z";
    let identity = "01a0a8f4-aa4d-7581-a92c-d7ddf0405263";
    let sessions = Path::new("/home/lane/.omp/agent/sessions");
    let home = Path::new("/home/lane");
    let temp = Path::new("/private/tmp");
    let (path, contents) = omp::session_file(
        sessions,
        home,
        temp,
        Path::new("/home/lane/devel/tree"),
        identity,
        at,
    );
    assert_eq!(
        path,
        sessions
            .join("-devel-tree")
            .join(format!("2026-09-16T06-43-21-805Z_{identity}.jsonl"))
    );
    let (slot, header) = contents.split_once('\n').expect("a slot, then the header");
    assert_eq!(
        slot.len() + 1,
        TITLE_SLOT_BYTES,
        "the title slot keeps omp's fixed width"
    );
    let slot: serde_json::Value = serde_json::from_str(slot).expect("the slot is JSON");
    assert_eq!(slot["type"], "title");
    assert_eq!(slot["title"], "");
    assert_eq!(slot["updatedAt"], at);
    let header: serde_json::Value = serde_json::from_str(header).expect("the header is JSON");
    assert_eq!(header["type"], "session");
    assert_eq!(header["version"], 3);
    assert_eq!(header["id"], identity);
    assert_eq!(header["timestamp"], at);
    assert_eq!(header["cwd"], "/home/lane/devel/tree");
    assert!(contents.ends_with('\n'));

    // The file already reads as omp's own: the slot is silent, the header declares.
    let lines: Vec<_> = contents.lines().collect();
    let slot = listen::decode(
        Dialect::Omp,
        lines[0].as_bytes(),
        Locus::new(1, 0, lines[0].len() as u64),
    )
    .expect("locus");
    assert!(slot.events().is_empty() && !matches!(slot, listen::Decoded::Residue(_)));
    let header = listen::decode(
        Dialect::Omp,
        lines[1].as_bytes(),
        Locus::new(1, 0, lines[1].len() as u64),
    )
    .expect("locus");
    assert!(
        matches!(header.events(), [listen::Event::Session(declaration)] if declaration.identity == identity)
    );

    // The three roots omp tells apart: home itself, the temporary tree, anywhere else.
    for (cwd, directory) in [
        ("/home/lane", "-"),
        ("/private/tmp/scratch/tree", "-tmp-scratch-tree"),
        ("/srv/tree", "--srv-tree--"),
    ] {
        let (path, _) = omp::session_file(sessions, home, temp, Path::new(cwd), identity, at);
        assert_eq!(
            path.parent(),
            Some(sessions.join(directory).as_path()),
            "{cwd}"
        );
    }
}
