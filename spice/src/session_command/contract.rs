//! Session command contracts, driven through the parser, `run`, and the rim
//! renderer exactly as the binary dispatches them, with an explicit ambient
//! environment instead of the test process's own.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::render::{self, Form, Stream};

mod authority;
mod compaction_authority;
mod explicit_files;
mod message_shape;
mod recovery;
mod window_planner;

const REFUSAL_EXIT: i32 = 2;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "spice-session-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("scratch root is exclusively created");
        Self(root)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn remove(self) {
        fs::remove_dir_all(&self.0).expect("scratch root is explicitly removed");
        assert!(!self.0.exists(), "scratch root outlived removal");
        std::mem::forget(self);
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One rendered outcome: its exit code and the single stream it spoke on.
struct Spoken {
    code: i32,
    out: Vec<u8>,
    err: Vec<u8>,
}

fn invoke(argv: &[String], environment: &[(&str, String)]) -> Spoken {
    invoke_in(None, argv, environment)
}

/// Parse `argv` as the binary does, run the selected session leaf against an
/// environment holding only `environment`, and render its outcome.
///
/// Without `current_dir` the reader has no working directory, so no case
/// reaches the repository the test process happens to run in.
fn invoke_in(
    current_dir: Option<&Path>,
    argv: &[String],
    environment: &[(&str, String)],
) -> Spoken {
    let matches = crate::cli::command()
        .try_get_matches_from(std::iter::once("spice").chain(argv.iter().map(String::as_str)))
        .expect("session argv parses");
    let contract =
        crate::cli::selected_contract(&matches).expect("the parser selects a registered leaf");
    assert_eq!(
        contract.path[0], "session",
        "the harness runs session leaves"
    );
    let form = if crate::cli::json_selected(&matches) {
        Form::Json
    } else {
        Form::Text
    };
    let ambient = ambient(current_dir, environment);
    let outcome = super::run(
        crate::cli::selected_matches(&matches),
        contract.path,
        &ambient,
    );
    let rendering = render::render_with_reply_text(&outcome, form, super::Report::text)
        .expect("the session outcome renders");
    let document = rendering.document().to_vec();
    let (out, err) = match rendering.stream() {
        Stream::Out => (document, Vec::new()),
        Stream::Err => (Vec::new(), document),
    };
    Spoken {
        code: crate::main_tail::exit_status(&outcome),
        out,
        err,
    }
}

/// The session environment the rim would capture from exactly `variables`,
/// with briefings leaving the checkout's working state unread.
fn ambient(current_dir: Option<&Path>, variables: &[(&str, String)]) -> super::Environment {
    let variable = |name: &str| {
        variables
            .iter()
            .rev()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| std::ffi::OsString::from(value))
    };
    crate::main_tail::session_environment_from(variable, current_dir.map(Path::to_path_buf))
        .unglanced()
}

fn reply_forms(argv: &[String], environment: &[(&str, String)], expected: &str) -> Value {
    reply_forms_in(None, argv, environment, expected)
}

fn reply_forms_in(
    current_dir: Option<&Path>,
    argv: &[String],
    environment: &[(&str, String)],
    expected: &str,
) -> Value {
    let text = invoke_in(current_dir, argv, environment);
    assert_eq!(text.code, 0, "text stderr: {}", utf8(&text.err));
    assert!(text.err.is_empty(), "successful text reply has no stderr");

    let mut structured_argv = vec!["--json".to_owned()];
    structured_argv.extend_from_slice(argv);
    let structured = invoke_in(current_dir, &structured_argv, environment);
    assert_eq!(
        structured.code,
        0,
        "structured stderr: {}",
        utf8(&structured.err)
    );
    assert!(
        structured.err.is_empty(),
        "successful structured reply has no stderr"
    );
    assert_eq!(
        structured.out.iter().filter(|byte| **byte == b'\n').count(),
        1,
        "structured reply is one DTO record"
    );
    let envelope: Value =
        serde_json::from_slice(&structured.out).expect("structured form is an outcome DTO");
    assert_eq!(envelope["kind"], "reply");
    let body = envelope["body"].clone();
    assert_eq!(body["session"], expected);
    if expected == "recall" {
        // The briefing speaks its folded page; JSON keeps the typed rows.
        assert!(
            text.out.len() <= BRIEFING_BYTES,
            "the folded briefing stays under its ceiling: {}",
            text.out.len()
        );
        assert!(serde_json::from_slice::<Value>(&text.out).is_err());
    } else {
        let typed: Value = serde_json::from_slice(&text.out).expect("text form is the typed body");
        assert_eq!(typed, body, "both forms cross the same DTO");
    }
    body
}

/// The folded briefing's byte ceiling.
const BRIEFING_BYTES: usize = 9_600;

fn refusal(argv: &[String], environment: &[(&str, String)], expected_reason: &str) -> Value {
    let mut structured_argv = vec!["--json".to_owned()];
    structured_argv.extend_from_slice(argv);
    let spoken = invoke(&structured_argv, environment);
    assert_eq!(
        spoken.code,
        REFUSAL_EXIT,
        "refusal stdout: {} stderr: {}",
        utf8(&spoken.out),
        utf8(&spoken.err)
    );
    assert!(spoken.out.is_empty(), "refusals do not impersonate replies");
    let envelope: Value =
        serde_json::from_slice(&spoken.err).expect("refusal is a structured outcome DTO");
    let diagnostic = refusal_diagnostic(&envelope);
    assert_eq!(diagnostic["reason"], expected_reason);
    assert!(
        diagnostic["detail"]
            .as_str()
            .is_some_and(|value| !value.is_empty()),
        "every refusal carries explicit recovery guidance"
    );
    envelope
}

fn refusal_diagnostic(envelope: &Value) -> &Value {
    assert_eq!(envelope["kind"], "refusal");
    assert_eq!(envelope["form"], "bare");
    &envelope["diagnostic"]
}

fn utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn write_jsonl(path: &Path, rows: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row).expect("fixture row serializes");
        bytes.push(b'\n');
    }
    fs::write(path, &bytes).expect("fixture transcript is written");
    bytes
}

/// The omp thread the shared transcript fixture declares.
const THREAD: &str = "01a0a943-9fda-77b0-bf4e-10879cc619e1";

fn omp_header(id: &str, timestamp: &str) -> Value {
    json!({"type": "session", "version": 3, "id": id, "timestamp": timestamp, "cwd": "/repo"})
}

fn omp_entry(timestamp: &str, id: &str, message: Value) -> Value {
    json!({"type": "message", "id": id, "parentId": null, "timestamp": timestamp, "message": message})
}

fn omp_user(timestamp: &str, id: &str, text: &str) -> Value {
    omp_entry(
        timestamp,
        id,
        json!({"role": "user", "content": [{"type": "text", "text": text}]}),
    )
}

fn omp_assistant(timestamp: &str, id: &str, content: Value, stop: &str) -> Value {
    omp_entry(
        timestamp,
        id,
        json!({"role": "assistant", "content": content, "stopReason": stop}),
    )
}

fn omp_text(timestamp: &str, id: &str, text: &str, stop: &str) -> Value {
    omp_assistant(timestamp, id, json!([{"type": "text", "text": text}]), stop)
}

fn omp_call(timestamp: &str, id: &str, call: &str, tool: &str, arguments: Value) -> Value {
    omp_assistant(
        timestamp,
        id,
        json!([{"type": "toolCall", "id": call, "name": tool, "arguments": arguments}]),
        "toolUse",
    )
}

fn omp_result(timestamp: &str, id: &str, call: &str, tool: &str, exit_code: Option<i32>) -> Value {
    let mut message = json!({
        "role": "toolResult",
        "toolCallId": call,
        "toolName": tool,
        "content": [{"type": "text", "text": "done"}],
        "isError": exit_code.is_some_and(|code| code != 0),
    });
    if let Some(code) = exit_code {
        message["details"] = json!({"exitCode": code});
    }
    omp_entry(timestamp, id, message)
}

fn with_usage(mut entry: Value, input: u64, cached: u64, output: u64) -> Value {
    entry["message"]["usage"] =
        json!({"input": input, "output": output, "cacheRead": cached, "cacheWrite": 0});
    entry
}

/// One persisted omp compaction; `kept` names the entry it kept, which is
/// what distinguishes one compaction from another.
fn omp_compaction(timestamp: &str, id: &str, summary: &str, kept: &str) -> Value {
    json!({
        "type": "compaction",
        "id": id,
        "parentId": null,
        "timestamp": timestamp,
        "summary": summary,
        "firstKeptEntryId": kept,
        "tokensBefore": 200,
    })
}

const COMPACTION_SUMMARY: &str = concat!(
    "1. Primary Request and Intent\n",
    "- preserve session forensics\n",
    "2. Work Completed\n",
    "- first turn",
);

fn omp_rows(thread: &str) -> Vec<Value> {
    vec![
        omp_header(thread, "2026-01-01T00:00:00.000Z"),
        omp_user(
            "2026-01-01T00:00:02Z",
            "u1",
            "Please implement parity; it must preserve evidence.",
        ),
        with_usage(
            omp_assistant(
                "2026-01-01T00:00:03Z",
                "a1",
                json!([
                    {"type": "text", "text": "I am inspecting the transcript."},
                    {"type": "toolCall", "id": "command-one", "name": "bash",
                        "arguments": {"command": "cargo check | rg error", "cwd": "/repo"}},
                ]),
                "toolUse",
            ),
            100,
            40,
            20,
        ),
        omp_result("2026-01-01T00:00:05Z", "r1", "command-one", "bash", Some(1)),
        omp_call(
            "2026-01-01T00:00:06Z",
            "a2",
            "patch-one",
            "write",
            json!({"path": "spice/src/session.rs", "content": "new"}),
        ),
        omp_result("2026-01-01T00:00:07Z", "r2", "patch-one", "write", None),
        with_usage(
            omp_text(
                "2026-01-01T00:00:08Z",
                "a3",
                "Committed abcdef1 after verification.",
                "stop",
            ),
            140,
            60,
            30,
        ),
        omp_compaction("2026-01-01T00:00:10Z", "c1", COMPACTION_SUMMARY, "u2"),
        omp_user(
            "2026-01-01T00:00:13Z",
            "u2",
            "Finish the second window without losing identity.",
        ),
        omp_call(
            "2026-01-01T00:00:14Z",
            "a4",
            "command-two",
            "shell",
            json!({"command": "cargo test | rg ok"}),
        ),
        omp_result(
            "2026-01-01T00:00:14.500Z",
            "r3",
            "command-two",
            "shell",
            Some(0),
        ),
        omp_call(
            "2026-01-01T00:00:15Z",
            "a5",
            "search-one",
            "web_search",
            json!({"query": "session parity"}),
        ),
        omp_result(
            "2026-01-01T00:00:15.500Z",
            "r4",
            "search-one",
            "web_search",
            None,
        ),
        omp_text(
            "2026-01-01T00:00:17Z",
            "a6",
            "Commit 1234abc completes the port.",
            "stop",
        ),
        omp_user("2026-01-01T00:00:20Z", "u3", "Interrupt this turn cleanly."),
        omp_assistant("2026-01-01T00:00:21Z", "a7", json!([]), "aborted"),
    ]
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn assert_recall(file: &str) {
    let recall = reply_forms(
        &strings(&["session", "briefing", file, "--deadline-seconds", "5"]),
        &[],
        "recall",
    );
    assert!(recall["live"]["turn"]["start"].is_string());
    assert!(
        recall.get("window").is_none(),
        "a briefing replays no turn window"
    );
}

fn assert_sweep(file: &str) {
    let sweep = reply_forms(
        &strings(&[
            "session",
            "sweep",
            file,
            "--limit",
            "2",
            "--deadline-seconds",
            "5",
        ]),
        &[],
        "sweep",
    );
    assert!(
        !sweep["rows"]
            .as_array()
            .expect("windows are typed")
            .is_empty()
    );
}

fn assert_turns(file: &str, start: &str, end: &str) {
    let turns = reply_forms(
        &strings(&[
            "session",
            "turns",
            file,
            "--start",
            start,
            "--end",
            end,
            "--contains",
            "preserve evidence",
            "--tool",
            "write",
            "--limit",
            "2",
            "--view",
            "full",
        ]),
        &[],
        "turns",
    );
    assert_eq!(turns["rows"].as_array().map(Vec::len), Some(1));
}

/// The shared omp transcript every query family reads, with its file operand.
fn query_transcript(label: &str) -> (Scratch, PathBuf, String) {
    let scratch = Scratch::new(label);
    let transcript = scratch.path().join("omp-session.jsonl");
    write_jsonl(&transcript, &omp_rows(THREAD));
    let file = transcript.to_string_lossy().into_owned();
    (scratch, transcript, file)
}

const QUERY_START: &str = "2026-01-01T00:00:00Z";
const QUERY_END: &str = "2026-01-01T00:00:18Z";

#[test]
fn recall_queries_cross_both_forms() {
    let (scratch, _, file) = query_transcript("query-recall");
    assert_recall(&file);
    scratch.remove();
}

#[test]
fn sweep_queries_accept_current_filters() {
    let (scratch, _, file) = query_transcript("query-sweep");
    assert_sweep(&file);
    scratch.remove();
}

#[test]
fn turn_queries_accept_current_filters() {
    let (scratch, _, file) = query_transcript("query-turns");
    assert_turns(&file, QUERY_START, QUERY_END);
    scratch.remove();
}

fn assert_final_rows(file: &str) {
    let turns = reply_forms(
        &strings(&["session", "turns", file, "--view", "full"]),
        &[],
        "turns",
    );
    let turns = turns["rows"].as_array().expect("turns are typed");
    assert_eq!(turns.len(), 3);
    assert_eq!(turns[0]["completed"], true, "an aborted turn still ends");
}

#[test]
fn turns_fold_finals_and_interrupted_turns() {
    let scratch = Scratch::new("turn-finals");
    let primary_path = scratch.path().join("primary.jsonl");
    write_jsonl(&primary_path, &omp_rows(THREAD));
    assert_final_rows(&primary_path.to_string_lossy());
    scratch.remove();
}
