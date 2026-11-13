//! Session message-shape cases split from the CLI suite.

use super::*;

#[test]
fn one_message_shape_fold_drives_session_reports_and_turn_dialogue() {
    let scratch = Scratch::new("message-shapes");
    let transcript = scratch.path().join("message-shapes.jsonl");
    write_jsonl(&transcript, &message_shape_rows());
    let file = transcript.to_string_lossy().into_owned();

    assert_message_shape_turn_dialogue(&file);
    assert_image_message_rows(&scratch);
    scratch.remove();
}

/// Every turn's dialogue in transcript order from one newest-first turns
/// page: an omp prompt opens a turn of its own, so a run of prompts spans
/// several.
fn turn_dialogue(file: &str) -> Vec<Value> {
    let page = reply_forms(&strings(&["session", "turns", file]), &[], "turns");
    let rows = page["rows"].as_array().expect("turn rows are typed");
    rows.iter()
        .rev()
        .flat_map(|turn| {
            turn["dialogue"]
                .as_array()
                .expect("turn dialogue is typed")
                .iter()
                .map(|line| json!({"side": line["side"], "text": line["text"]["text"]}))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn assert_image_message_rows(scratch: &Scratch) {
    let transcript = scratch.path().join("image-message-shapes.jsonl");
    write_jsonl(&transcript, &image_message_rows());
    let file = transcript.to_string_lossy().into_owned();

    assert_eq!(
        turn_dialogue(&file),
        [
            json!({"side": "user", "text": "Inspect this bootstrap capture."}),
            json!({"side": "assistant", "text": "The bootstrap capture is visible."}),
            json!({"side": "user", "text": "Compare the first capture"}),
            json!({"side": "user", "text": "with the second capture."}),
            json!({"side": "assistant", "text": "Both captures remain attached."}),
        ]
    );
}

fn image_message_rows() -> Vec<Value> {
    const PNG: &str = "iVBORw0KGgo=";
    vec![
        omp_header(THREAD, "2026-01-04T00:00:00.000Z"),
        omp_entry(
            "2026-01-04T00:00:01Z",
            "u1",
            json!({"role": "user", "content": [
                {"type": "text", "text": "Inspect this bootstrap capture."},
                {"type": "image", "data": PNG, "mimeType": "image/png"},
            ]}),
        ),
        omp_text(
            "2026-01-04T00:00:02Z",
            "a1",
            "The bootstrap capture is visible.",
            "stop",
        ),
        omp_entry(
            "2026-01-04T00:00:03Z",
            "u2",
            json!({"role": "user", "content": [
                {"type": "text", "text": "Compare the first capture"},
                {"type": "image", "data": PNG, "mimeType": "image/png"},
                {"type": "text", "text": "with the second capture."},
                {"type": "image", "data": "R0lGODlhAQABAAAAACw=", "mimeType": "image/gif"},
            ]}),
        ),
        omp_text(
            "2026-01-04T00:00:04Z",
            "a2",
            "Both captures remain attached.",
            "stop",
        ),
    ]
}

fn message_shape_rows() -> Vec<Value> {
    vec![
        omp_header(THREAD, "2026-01-03T00:00:00.000Z"),
        omp_user(
            "2026-01-03T00:00:01Z",
            "m1",
            "Please preserve the authored ask.",
        ),
        omp_user(
            "2026-01-03T00:00:02Z",
            "m2",
            "[$spice](.agents/skills/spice/SKILL.md)",
        ),
        omp_user(
            "2026-01-03T00:00:03Z",
            "m3",
            "1. Primary Request and Intent:\nContinue.\n2. Work Completed:\nNone.",
        ),
        omp_user(
            "2026-01-03T00:00:04Z",
            "m4",
            "<future-envelope>tool output</future-envelope>",
        ),
        omp_user(
            "2026-01-03T00:00:05Z",
            "m5",
            "[Please keep this bracketed ask.]",
        ),
        omp_user(
            "2026-01-03T00:00:06Z",
            "m6",
            "The malformed tool call was discussed before retrying with valid syntax.",
        ),
        omp_user(
            "2026-01-03T00:00:07Z",
            "m7",
            "Lead prose.\n[$spice](.agents/skills/spice/SKILL.md)\nTail prose.",
        ),
        omp_user(
            "2026-01-03T00:00:08Z",
            "m8",
            "Malformed <future-envelope>tool output",
        ),
        omp_user(
            "2026-01-03T00:00:09Z",
            "m9",
            "Same text at a distinct source position.",
        ),
        omp_user(
            "2026-01-03T00:00:10Z",
            "m10",
            "Same text at a distinct source position.",
        ),
        omp_text(
            "2026-01-03T00:00:11Z",
            "m11",
            "Authored commentary.\ndiff --git a/a.rs b/a.rs\n+generated body",
            "toolUse",
        ),
        omp_text(
            "2026-01-03T00:00:12Z",
            "m12",
            "*** Begin Patch\n+generated body\n*** End Patch",
            "toolUse",
        ),
    ]
}

fn assert_message_shape_turn_dialogue(file: &str) {
    assert_eq!(
        turn_dialogue(file),
        [
            json!({"side": "user", "text": "Please preserve the authored ask."}),
            json!({"side": "user", "text": "[Please keep this bracketed ask.]"}),
            json!({
                "side": "user",
                "text": "The malformed tool call was discussed before retrying with valid syntax."
            }),
            json!({"side": "user", "text": "Lead prose. Tail prose."}),
            json!({"side": "user", "text": "Malformed <future-envelope>tool output"}),
            json!({"side": "user", "text": "Same text at a distinct source position."}),
            json!({"side": "user", "text": "Same text at a distinct source position."}),
            json!({"side": "assistant", "text": "Authored commentary."}),
        ]
    );
}
