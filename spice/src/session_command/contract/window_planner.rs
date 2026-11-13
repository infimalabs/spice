use super::*;
use serde_json::{Value, json};

fn compacted(timestamp: &str, id: &str) -> Value {
    omp_compaction(timestamp, id, "", id)
}

fn planner_tail_rows() -> Vec<Value> {
    vec![
        omp_header(THREAD, "2026-02-01T00:09:59.000Z"),
        compacted("2026-02-01T00:10:00Z", "c1"),
        omp_user(
            "2026-02-01T00:19:10Z",
            "straddle",
            "This turn starts before the second boundary.",
        ),
        compacted("2026-02-01T00:20:00Z", "c2"),
        omp_text(
            "2026-02-01T00:20:10Z",
            "a1",
            "The same turn continues after the boundary.",
            "toolUse",
        ),
        omp_assistant("2026-02-01T00:21:00Z", "a2", json!([]), "stop"),
        omp_user(
            "2026-02-01T00:22:10Z",
            "after",
            "This turn belongs only to the open tail window.",
        ),
        omp_assistant("2026-02-01T00:23:00Z", "a3", json!([]), "stop"),
    ]
}

fn planner_surrounding_rows() -> Vec<Value> {
    let mut tail = planner_tail_rows();
    let mut rows = vec![
        tail.remove(0),
        omp_user("2026-02-01T00:00:00Z", "ignored-before", "An older prompt."),
        omp_text(
            "2026-02-01T00:00:10Z",
            "a0",
            "Older assistant text must not become the selected compaction predecessor.",
            "stop",
        ),
    ];
    rows.extend(tail);
    rows
}

/// Strip what names the input file, so two inputs compare by content.
fn strip_inputs(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for key in ["source", "detail", "history"] {
                object.remove(key);
            }
            for value in object.values_mut() {
                strip_inputs(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                strip_inputs(value);
            }
        }
        _ => {}
    }
}

fn without_inputs(mut value: Value) -> Value {
    strip_inputs(&mut value);
    value
}

/// The prompts opening each turn a sweep window's own detail command reads,
/// oldest first.
fn window_prompts(window: &Value) -> Vec<String> {
    let detail: Vec<String> =
        serde_json::from_value(window["detail"].clone()).expect("detail is one command");
    let turns = reply_forms(&detail, &[], "turns");
    let mut prompts = turns["rows"]
        .as_array()
        .expect("turn rows are typed")
        .iter()
        .map(|turn| {
            turn["dialogue"][0]["text"]["text"]
                .as_str()
                .expect("fixture turns open on a prompt")
                .to_owned()
        })
        .collect::<Vec<_>>();
    prompts.reverse();
    prompts
}

fn sweep_rows(input: &str, limit: &str) -> Vec<Value> {
    reply_forms(
        &strings(&["session", "sweep", input, "--limit", limit]),
        &[],
        "sweep",
    )["rows"]
        .as_array()
        .expect("windows are typed")
        .clone()
}

#[test]
fn sweep_windows_share_boundary_straddling_turns() {
    let scratch = Scratch::new("session-window-straddle");
    let transcript = scratch.path().join("planner.jsonl");
    write_jsonl(&transcript, &planner_tail_rows());
    let input = transcript.to_string_lossy().into_owned();

    let windows = sweep_rows(&input, "2");

    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0]["start"], "2026-02-01T00:20:00.000Z");
    assert_eq!(windows[1]["start"], "2026-02-01T00:10:00.000Z");
    let straddle = "This turn starts before the second boundary.";
    assert_eq!(windows[1]["turns"], 1);
    assert_eq!(window_prompts(&windows[1]), [straddle]);
    assert_eq!(windows[0]["turns"], 2);
    assert_eq!(
        window_prompts(&windows[0]),
        [straddle, "This turn belongs only to the open tail window."]
    );

    scratch.remove();
}

#[test]
fn sweep_does_not_copy_exact_boundary_starts_backward() {
    let scratch = Scratch::new("session-window-boundary-start");
    let transcript = scratch.path().join("boundary.jsonl");
    write_jsonl(
        &transcript,
        &[
            omp_header(THREAD, "2026-02-01T00:00:00.000Z"),
            compacted("2026-02-01T00:10:00Z", "c1"),
            omp_user(
                "2026-02-01T00:19:10Z",
                "before",
                "This turn stays before the boundary.",
            ),
            omp_assistant("2026-02-01T00:19:30Z", "a1", json!([]), "stop"),
            compacted("2026-02-01T00:20:00Z", "c2"),
            omp_user(
                "2026-02-01T00:20:00Z",
                "boundary-start",
                "This turn starts exactly at the boundary.",
            ),
            omp_assistant("2026-02-01T00:20:30Z", "a2", json!([]), "stop"),
        ],
    );
    let input = transcript.to_string_lossy().into_owned();

    let windows = sweep_rows(&input, "2");

    assert_eq!(windows.len(), 2);
    assert_eq!(
        window_prompts(&windows[1]),
        ["This turn stays before the boundary."]
    );
    assert_eq!(
        window_prompts(&windows[0]),
        ["This turn starts exactly at the boundary."]
    );

    write_jsonl(
        &transcript,
        &[
            omp_header(THREAD, "2026-02-01T00:00:00.000Z"),
            omp_user("2026-02-01T00:30:00Z", "later-first", "Later first."),
            omp_assistant("2026-02-01T00:31:00Z", "a1", json!([]), "stop"),
            omp_user("2026-02-01T00:20:00Z", "earlier-second", "Earlier second."),
            omp_assistant("2026-02-01T00:21:00Z", "a2", json!([]), "stop"),
        ],
    );
    for action in ["briefing", "sweep"] {
        refusal(
            &strings(&["session", action, &input]),
            &[],
            "session::source-order-regressed",
        );
    }

    write_jsonl(
        &transcript,
        &[
            omp_header(THREAD, "2026-02-01T00:00:00.000Z"),
            compacted("2026-02-01T00:30:00Z", "c1"),
            compacted("2026-02-01T00:20:00Z", "c2"),
        ],
    );
    refusal(
        &strings(&["session", "sweep", &input, "--limit", "2"]),
        &[],
        "session::source-order-regressed",
    );

    scratch.remove();
}

#[test]
fn recall_and_sweep_ignore_surrounding_input_before_the_recent_horizon() {
    let scratch = Scratch::new("session-window-horizon");
    let base = scratch.path().join("base.jsonl");
    let surrounded = scratch.path().join("surrounded.jsonl");
    write_jsonl(&base, &planner_tail_rows());
    write_jsonl(&surrounded, &planner_surrounding_rows());
    let base = base.to_string_lossy().into_owned();
    let surrounded = surrounded.to_string_lossy().into_owned();

    let base_recall = reply_forms(&strings(&["session", "briefing", &base]), &[], "recall");
    let surrounded_recall = reply_forms(
        &strings(&["session", "briefing", &surrounded]),
        &[],
        "recall",
    );
    assert_eq!(
        without_inputs(base_recall["live"].clone()),
        without_inputs(surrounded_recall["live"].clone())
    );
    assert_eq!(surrounded_recall["residues"], 0);

    let base_sweep = sweep_rows(&base, "2");
    let surrounded_sweep = reply_forms(
        &strings(&["session", "sweep", &surrounded, "--limit", "2"]),
        &[],
        "sweep",
    );
    assert_eq!(
        without_inputs(Value::Array(base_sweep)),
        without_inputs(surrounded_sweep["rows"].clone())
    );
    assert_eq!(
        surrounded_sweep["residues"].as_array().map(Vec::len),
        Some(0)
    );

    scratch.remove();
}

#[test]
fn sweep_keeps_one_labelled_window_without_severing_compactions() {
    let scratch = Scratch::new("session-window-full-history");
    let transcript = scratch.path().join("plain.jsonl");
    write_jsonl(
        &transcript,
        &[
            omp_header(THREAD, "2026-02-02T00:00:00.000Z"),
            omp_user("2026-02-02T00:00:10Z", "first", "First prompt."),
            omp_assistant("2026-02-02T00:01:00Z", "a1", json!([]), "stop"),
            omp_user("2026-02-02T00:02:10Z", "second", "Second prompt."),
            omp_assistant("2026-02-02T00:03:00Z", "a2", json!([]), "stop"),
        ],
    );
    let input = transcript.to_string_lossy().into_owned();

    let windows = sweep_rows(&input, "3");

    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["compaction"], Value::Null);
    assert_eq!(windows[0]["turns"], 2);
    assert_eq!(
        window_prompts(&windows[0]),
        ["First prompt.", "Second prompt."]
    );

    scratch.remove();
}
