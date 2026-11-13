use super::*;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::fs;

/// One page of turns over `file`, the history view that carries residues.
fn turns(file: &str) -> Value {
    reply_forms(&strings(&["session", "turns", file]), &[], "turns")
}

/// Authored dialogue lines as a turns page renders them.
fn dialogue(lines: &[(&str, &str)]) -> Value {
    lines
        .iter()
        .map(|(side, text)| json!({"side": side, "text": {"text": text}}))
        .collect()
}

fn damaged_fixture(scratch: &Scratch) -> String {
    let damaged_path = scratch.path().join("damaged.jsonl");
    let mut before = serde_json::to_vec(&omp_header(THREAD, "2026-01-03T00:00:00.000Z"))
        .expect("valid header serializes");
    before.push(b'\n');
    before.extend(
        serde_json::to_vec(&omp_user(
            "2026-01-03T00:00:00Z",
            "u1",
            "Recover the readable prefix.",
        ))
        .expect("valid prefix serializes"),
    );
    let after = serde_json::to_vec(&omp_text(
        "2026-01-03T00:00:02Z",
        "a1",
        "history after unsupported evidence remains available",
        "stop",
    ))
    .expect("valid suffix serializes");
    let mut damaged = before;
    damaged.extend_from_slice(
        b"\n{\"timestamp\":\"2026-01-03T00:00:01Z\",\"type\":\"future_authority_mutation\"}\n{malformed}\n",
    );
    damaged.extend_from_slice(&after);
    damaged.push(b'\n');
    fs::write(&damaged_path, damaged).expect("damaged transcript is written");
    damaged_path.to_string_lossy().into_owned()
}

fn assert_damaged_recovery(damaged: &str) {
    let first = turns(damaged);
    let second = turns(damaged);
    assert_eq!(first, second, "mixed-record recovery is deterministic");
    assert_eq!(first["rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(first["rows"][0]["completed"], true);
    assert_eq!(
        first["rows"][0]["dialogue"][1]["text"]["text"],
        "history after unsupported evidence remains available"
    );
    assert_eq!(first["residues"].as_array().map(Vec::len), Some(2));
    let canonical = Path::new(damaged)
        .canonicalize()
        .expect("damaged fixture canonicalizes");
    assert_eq!(
        first["residues"][0]["source"],
        canonical.to_string_lossy().as_ref()
    );
    assert_eq!(first["residues"][0]["line"], 3);
    assert_eq!(first["residues"][0]["kind"], "unrecognized-record");
    assert_eq!(first["residues"][0]["detail"], "future_authority_mutation");
    assert_eq!(
        first["residues"][1]["source"],
        canonical.to_string_lossy().as_ref()
    );
    assert_eq!(first["residues"][1]["line"], 4);
    assert_eq!(first["residues"][1]["kind"], "record-field-malformed");
    assert_eq!(
        first["residues"][1]["detail"],
        "field \"$\" must be JSON record"
    );
    assert!(
        first["residues"][1]["recovery"].as_str().is_some_and(
            |recovery| recovery.contains("surrounding typed history remains available")
        )
    );
}

fn assert_invalid_query_arguments(damaged: &str) {
    refusal(
        &strings(&["session", "briefing", damaged, "--deadline-seconds", "0"]),
        &[],
        "session::deadline-invalid",
    );
    refusal(
        &strings(&[
            "session",
            "turns",
            damaged,
            "--start",
            "2026-01-04T00:00:00Z",
            "--end",
            "2026-01-03T00:00:00Z",
        ]),
        &[],
        "session::bounds-reversed",
    );
    refusal(
        &strings(&["session", "turns", damaged, "--start", "not-a-timestamp"]),
        &[],
        "session::timestamp-invalid",
    );
    refusal(
        &strings(&["session", "turns", damaged, "--limit", "many"]),
        &[],
        "session::bound-invalid",
    );
}

fn assert_unavailable_files(scratch: &Scratch) {
    let corrupt_gzip_path = scratch.path().join("corrupt.jsonl.gz");
    fs::write(&corrupt_gzip_path, b"not a gzip stream")
        .expect("corrupt compressed transcript is written");
    let corrupt_gzip = corrupt_gzip_path.to_string_lossy().into_owned();
    let interrupted = turns(&corrupt_gzip);
    assert_eq!(interrupted["residues"][0]["kind"], "read-interrupted");
    assert!(interrupted["residues"][0]["recovery"].is_string());

    let missing_path = scratch.path().join("missing.jsonl");
    let missing = missing_path.to_string_lossy().into_owned();
    let refusal_one = refusal(
        &strings(&["session", "turns", &missing]),
        &[],
        "session::transcript-missing",
    );
    let refusal_two = refusal(
        &strings(&["session", "turns", &missing]),
        &[],
        "session::transcript-missing",
    );
    assert_eq!(refusal_one, refusal_two, "missing-state refusal is stable");

    let directory = scratch.path().join("directory.jsonl");
    fs::create_dir(&directory).expect("non-regular transcript fixture exists");
    refusal(
        &strings(&["session", "turns", &directory.to_string_lossy()]),
        &[],
        "session::transcript-not-regular",
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let real = scratch.path().join("real.jsonl");
        write_jsonl(&real, &omp_rows(THREAD));
        let link = scratch.path().join("linked.jsonl");
        symlink(&real, &link).expect("transcript symlink fixture exists");
        refusal(
            &strings(&["session", "turns", &link.to_string_lossy()]),
            &[],
            "session::transcript-not-regular",
        );
    }
}

fn assert_unavailable_turn_identity(scratch: &Scratch) {
    let path = scratch.path().join("turns.jsonl");
    write_jsonl(&path, &omp_rows(THREAD));
    let file = path.to_string_lossy().into_owned();
    let unavailable_turn = refusal(
        &strings(&["session", "turns", &file, "--turn-id", "unnamed-turn"]),
        &[],
        "session::turn-id-unavailable",
    );
    assert!(
        refusal_diagnostic(&unavailable_turn)["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("--start"))
    );
}

#[test]
fn turns_retain_an_end_only_tool_outcome_as_unattributed_residue() {
    let scratch = Scratch::new("end-only-tool");
    let transcript = scratch.path().join("omp-session.jsonl");
    write_jsonl(
        &transcript,
        &[
            omp_header(THREAD, "2026-01-03T00:00:00.000Z"),
            omp_user(
                "2026-01-03T00:00:01Z",
                "u1",
                "Preserve the independent dialogue.",
            ),
            omp_result("2026-01-03T00:00:02Z", "r1", "exec-end-only", "read", None),
            omp_text(
                "2026-01-03T00:00:03Z",
                "a1",
                "Independent dialogue preserved.",
                "stop",
            ),
        ],
    );
    let file = transcript.to_string_lossy().into_owned();

    let page = turns(&file);
    assert_eq!(page["rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        page["rows"][0]["dialogue"][1]["text"]["text"],
        "Independent dialogue preserved."
    );
    assert_eq!(page["residues"].as_array().map(Vec::len), Some(1));
    assert_eq!(page["residues"][0]["kind"], "tool-invocation-missing");
    assert_eq!(page["residues"][0]["line"], 3);
    assert!(
        page["residues"][0]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("call_id=exec-end-only"))
    );
    assert!(page["residues"][0]["recovery"].is_string());
    let recall = reply_forms(&strings(&["session", "briefing", &file]), &[], "recall");
    assert_eq!(recall["residues"], 1, "the briefing counts the residue");
    scratch.remove();
}

#[test]
fn explicit_forensic_briefing_accounts_final_truncated_record_without_blocking() {
    let scratch = Scratch::new("live-tail");
    let omp_home = scratch.path().join("omp-home");
    let folder = omp_home.join("sessions/-fixture");
    fs::create_dir_all(&folder).expect("omp session root exists");
    let transcript = folder.join(format!("2026-01-01T00-00-00-000Z_{THREAD}.jsonl"));
    let mut bytes = write_jsonl(&transcript, &omp_rows(THREAD));
    bytes.extend_from_slice(b"{\"timestamp\":");
    fs::write(&transcript, bytes).expect("live tail fixture is written");

    let file = transcript.to_string_lossy().into_owned();
    let recall = reply_forms(
        &strings(&["session", "briefing", &file, "--deadline-seconds", "5"]),
        &[],
        "recall",
    );
    assert_eq!(recall["residues"], 1, "the briefing counts the live tail");
    let page = reply_forms(&strings(&["session", "turns", &file]), &[], "turns");
    assert!(
        page["residues"].as_array().is_some_and(|residues| {
            residues
                .iter()
                .any(|residue| residue["kind"] == "record-truncated")
        }),
        "forensic truncation is retained as residue: {page}"
    );
    scratch.remove();
}

#[test]
fn recall_and_sweep_deadlines_name_inputs_without_partial_report_data() {
    let scratch = Scratch::new("deadline");
    let transcript = scratch.path().join("private-session.jsonl");
    write_jsonl(&transcript, &omp_rows(THREAD));
    let input = transcript.to_string_lossy().into_owned();
    for argv in [
        strings(&[
            "session",
            "briefing",
            &input,
            "--deadline-seconds",
            "0.000000001",
        ]),
        strings(&[
            "session",
            "sweep",
            &input,
            "--deadline-seconds",
            "0.000000001",
        ]),
    ] {
        let action = &argv[1];
        let envelope = refusal(&argv, &[], "session::deadline-exceeded");
        let expected_observed = format!(
            "action={action} inputs=requested[{:?}] budget=0.000000001s",
            input
        );
        assert_eq!(
            refusal_diagnostic(&envelope)["observed"].as_str(),
            Some(expected_observed.as_str())
        );
        assert!(
            !envelope
                .to_string()
                .contains("Please implement parity; it must preserve evidence."),
            "a deadline refusal must not carry partial transcript prose"
        );
    }
    scratch.remove();
}

#[test]
fn malformed_truncated_missing_and_unavailable_state_is_accounted_with_recovery() {
    let scratch = Scratch::new("residue");
    let damaged = damaged_fixture(&scratch);
    assert_damaged_recovery(&damaged);
    let valid_path = scratch.path().join("valid.jsonl");
    write_jsonl(&valid_path, &omp_rows(THREAD));
    assert_invalid_query_arguments(&valid_path.to_string_lossy());
    assert_unavailable_files(&scratch);
    assert_unavailable_turn_identity(&scratch);
    scratch.remove();
}

/// omp names its tools in lower case and keys their arguments as `command`
/// and `path`, which is how a session file's shell calls and file edits count.
#[test]
fn turns_count_omp_shell_calls_and_file_edits() {
    let scratch = Scratch::new("omp-tools");
    let transcript = scratch
        .path()
        .join("2026-09-16T08-09-36-474Z_01a0a943-9fda-77b0-bf4e-10879cc619e1.jsonl");
    write_jsonl(
        &transcript,
        &[
            json!({"type": "session", "version": 3, "id": "01a0a943-9fda-77b0-bf4e-10879cc619e1", "timestamp": "2026-09-16T08:09:36.474Z", "cwd": "/work/tree"}),
            omp_entry(
                "2026-09-16T08:09:37.189Z",
                "u1",
                json!({"role": "user", "content": [{"type": "text", "text": "run the tests, then fix the seam"}], "attribution": "user"}),
            ),
            omp_entry(
                "2026-09-16T08:09:40.340Z",
                "a1",
                json!({"role": "assistant", "content": [{"type": "toolCall", "id": "call_1", "name": "bash", "arguments": {"command": "cargo test -p spice"}}], "usage": {"input": 4, "output": 12, "cacheRead": 0, "cacheWrite": 0}, "stopReason": "toolUse"}),
            ),
            omp_entry(
                "2026-09-16T08:09:41.000Z",
                "r1",
                json!({"role": "toolResult", "toolCallId": "call_1", "toolName": "bash", "content": [{"type": "text", "text": "test result: ok"}], "details": {"exitCode": 0}, "isError": false}),
            ),
            omp_entry(
                "2026-09-16T08:09:45.000Z",
                "a2",
                json!({"role": "assistant", "content": [{"type": "toolCall", "id": "call_2", "name": "edit", "arguments": {"path": "spice/src/session.rs", "oldText": "a", "newText": "b"}}], "usage": {"input": 4, "output": 12, "cacheRead": 0, "cacheWrite": 0}, "stopReason": "toolUse"}),
            ),
            omp_entry(
                "2026-09-16T08:09:46.000Z",
                "r2",
                json!({"role": "toolResult", "toolCallId": "call_2", "toolName": "edit", "content": [{"type": "text", "text": "edited"}], "isError": false}),
            ),
            omp_entry(
                "2026-09-16T08:09:50.000Z",
                "a3",
                json!({"role": "assistant", "content": [{"type": "text", "text": "The seam is fixed and the tests pass."}], "usage": {"input": 4, "output": 12, "cacheRead": 0, "cacheWrite": 0}, "stopReason": "stop"}),
            ),
        ],
    );
    let file = transcript.to_string_lossy().into_owned();

    let page = turns(&file);
    let turn = &page["rows"][0];
    assert_eq!(turn["vendor"], "omp");
    assert_eq!(turn["completed"], true, "{turn}");
    assert_eq!(turn["commands"], 1, "{turn}");
    assert_eq!(turn["patches"], 1, "{turn}");
    assert_eq!(turn["touched_paths"], json!(["spice/src/session.rs"]));
    assert_eq!(page["residues"], json!([]));

    scratch.remove();
}

#[test]
fn turns_complete_an_omp_command_with_surplus_screenshot_media() {
    let scratch = Scratch::new("omp-surplus-media");
    let transcript = scratch
        .path()
        .join("2026-09-26T20-00-00-000Z_019f0000-0000-7000-8000-000000000001.jsonl");
    let mut result = vec![json!({"type": "text", "text": "nine screenshots retained"})];
    result.extend(
        (0..=wire::media::MAX_MEDIA_ITEMS)
            .map(|_| json!({"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"})),
    );
    write_jsonl(
        &transcript,
        &[
            json!({"type": "session", "version": 3, "id": "019f0000-0000-7000-8000-000000000001", "timestamp": "2026-09-26T20:00:00Z", "cwd": "/work/tree"}),
            omp_entry(
                "2026-09-26T20:00:01Z",
                "u1",
                json!({"role": "user", "content": "inspect every screenshot"}),
            ),
            omp_entry(
                "2026-09-26T20:00:02Z",
                "a1",
                json!({"role": "assistant", "content": [{"type": "toolCall", "id": "call-nine", "name": "bash", "arguments": {"command": "capture --all"}}], "stopReason": "toolUse"}),
            ),
            omp_entry(
                "2026-09-26T20:00:03Z",
                "r1",
                json!({"role": "toolResult", "toolCallId": "call-nine", "toolName": "bash", "content": result, "details": {"exitCode": 0}, "isError": false}),
            ),
            omp_entry(
                "2026-09-26T20:00:04Z",
                "a2",
                json!({"role": "assistant", "content": [{"type": "text", "text": "all captures inspected"}], "stopReason": "stop"}),
            ),
        ],
    );
    let file = transcript.to_string_lossy().into_owned();

    let page = turns(&file);
    assert_eq!(page["residues"], json!([]));
    assert_eq!(
        page["rows"][0]["dialogue"],
        dialogue(&[
            ("user", "inspect every screenshot"),
            ("assistant", "all captures inspected"),
        ])
    );

    scratch.remove();
}

#[test]
fn turns_collapse_repeated_residues_without_losing_dialogue() {
    let scratch = Scratch::new("briefing-residue-groups");
    let transcript = scratch
        .path()
        .join("2026-09-26T21-00-00-000Z_019f0000-0000-7000-8000-000000000002.jsonl");
    let mut records = vec![json!({
        "type": "session",
        "version": 3,
        "id": "019f0000-0000-7000-8000-000000000002",
        "timestamp": "2026-09-26T21:00:00Z",
        "cwd": "/work/tree",
    })];
    for index in 0..12 {
        records.push(omp_entry(
            "2026-09-26T21:00:01Z",
            &format!("malformed-{index}"),
            json!({
                "role": "user",
                "content": [{
                    "type": "image",
                    "data": "not-base64",
                    "mimeType": "image/png",
                }],
            }),
        ));
    }
    records.extend([
        omp_entry(
            "2026-09-26T21:00:02Z",
            "u1",
            json!({"role": "user", "content": "retain the current ask"}),
        ),
        omp_entry(
            "2026-09-26T21:00:03Z",
            "a1",
            json!({
                "role": "assistant",
                "content": [{"type": "text", "text": "window remains intact"}],
                "stopReason": "stop",
            }),
        ),
    ]);
    write_jsonl(&transcript, &records);
    let file = transcript.to_string_lossy().into_owned();

    let page = turns(&file);
    let residues = page["residues"]
        .as_array()
        .expect("turn residues are an array");
    assert_eq!(residues.len(), 1, "{residues:?}");
    let residue = &residues[0];
    assert_eq!(
        residue["source"],
        transcript
            .canonicalize()
            .expect("transcript canonicalizes")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(residue["line"], 2);
    assert_eq!(residue["kind"], "record-field-malformed");
    let detail = residue["detail"]
        .as_str()
        .expect("grouped residue detail is text");
    for evidence in ["12 records", "line 2", "line 13", "field \"media\""] {
        assert!(detail.contains(evidence), "{detail:?}");
    }
    assert!(
        residue["recovery"].as_str().is_some_and(
            |recovery| recovery.contains("surrounding typed history remains available")
        )
    );
    assert_eq!(
        page["rows"][0]["dialogue"],
        dialogue(&[
            ("user", "retain the current ask"),
            ("assistant", "window remains intact"),
        ])
    );

    scratch.remove();
}

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

fn external_image_records(data: &str) -> Vec<serde_json::Value> {
    vec![
        omp_header(THREAD, "2026-09-29T01:00:00.000Z"),
        omp_user("2026-09-29T01:00:01Z", "u1", "Inspect the captured image."),
        omp_call(
            "2026-09-29T01:00:02Z",
            "a1",
            "call-image",
            "read",
            json!({"path": "capture.png"}),
        ),
        omp_entry(
            "2026-09-29T01:00:03Z",
            "r1",
            json!({
                "role": "toolResult",
                "toolCallId": "call-image",
                "toolName": "read",
                "content": [
                    {"type": "text", "text": "Read image [image/png]"},
                    {
                        "type": "image",
                        "data": data,
                        "mimeType": "image/png",
                        "name": "capture.png",
                    },
                ],
                "isError": false,
            }),
        ),
        omp_text("2026-09-29T01:00:04Z", "a2", "The image is intact.", "stop"),
    ]
}

#[test]
fn turns_resolve_omp_external_image_blobs_from_the_session_state_root() {
    let scratch = Scratch::new("briefing-external-media");
    let sessions = scratch.path().join("sessions/team");
    let blobs = scratch.path().join("blobs");
    fs::create_dir_all(&sessions).expect("session directory creates");
    fs::create_dir_all(&blobs).expect("blob directory creates");
    let digest = format!("{:x}", Sha256::digest(PNG_SIGNATURE));
    fs::write(blobs.join(&digest), PNG_SIGNATURE).expect("OMP blob writes");
    let transcript = sessions.join("thread.jsonl");
    write_jsonl(
        &transcript,
        &external_image_records(&format!("blob:sha256:{digest}")),
    );

    let file = transcript.to_string_lossy().into_owned();
    let page = turns(&file);
    assert_eq!(page["residues"], json!([]));
    assert_eq!(
        page["rows"][0]["dialogue"],
        dialogue(&[
            ("user", "Inspect the captured image."),
            ("assistant", "The image is intact."),
        ])
    );

    scratch.remove();
}

#[test]
fn turns_distinguish_missing_and_mismatched_omp_external_image_blobs() {
    let scratch = Scratch::new("briefing-invalid-external-media");
    let sessions = scratch.path().join("sessions/team");
    let blobs = scratch.path().join("blobs");
    fs::create_dir_all(&sessions).expect("session directory creates");
    fs::create_dir_all(&blobs).expect("blob directory creates");
    let digest = format!("{:x}", Sha256::digest(PNG_SIGNATURE));
    let transcript = sessions.join("thread.jsonl");
    write_jsonl(
        &transcript,
        &external_image_records(&format!("blob:sha256:{digest}")),
    );
    let file = transcript.to_string_lossy().into_owned();

    let missing = turns(&file);
    assert_eq!(missing["residues"][0]["kind"], "media-blob-missing");
    assert_eq!(
        missing["rows"][0]["dialogue"],
        dialogue(&[
            ("user", "Inspect the captured image."),
            ("assistant", "The image is intact."),
        ])
    );

    fs::write(blobs.join(&digest), b"not the referenced image").expect("bad OMP blob writes");
    let mismatched = turns(&file);
    assert_eq!(
        mismatched["residues"][0]["kind"],
        "media-blob-digest-mismatch"
    );
    assert_eq!(
        mismatched["rows"][0]["dialogue"],
        missing["rows"][0]["dialogue"],
    );

    scratch.remove();
}
