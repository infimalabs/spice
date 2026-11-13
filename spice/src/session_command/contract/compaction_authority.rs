use std::fs::OpenOptions;
use std::io::Write as _;

use super::*;

const REHYDRATION: &str = concat!(
    "1. Primary Request and Intent\n",
    "Preserve the continuation context.\n",
    "2. Work Completed\n",
    "The boundary was recorded."
);
const MARKER_TS: &str = "2026-02-02T00:00:10.004Z";
const ASSISTANT_BEFORE: &str = "Summary-shaped compaction prose is still prose.";
const USER_AFTER: &str = "Continue after the advertised boundary.";

#[test]
fn a_persisted_boundary_counts_once_and_preserves_continuation_context() {
    let scratch = Scratch::new("omp-boundary-authority");
    let transcript = scratch.path().join("omp-boundary-authority.jsonl");
    write_jsonl(&transcript, &before_rows());
    assert_no_boundary(&transcript);

    append_rows(
        &transcript,
        &[
            omp_compaction(MARKER_TS, "c1", REHYDRATION, "after"),
            omp_user("2026-02-02T00:00:11Z", "after", USER_AFTER),
            omp_text(
                "2026-02-02T00:00:12Z",
                "a2",
                "The observed boundary now owns the horizon.",
                "stop",
            ),
        ],
    );
    assert_one_boundary(&transcript);
    scratch.remove();
}

fn assert_no_boundary(transcript: &Path) {
    let file = transcript.to_string_lossy();
    let briefing = reply_forms(
        &strings(&["session", "briefing", &file, "--deadline-seconds", "5"]),
        &[],
        "recall",
    );
    assert_eq!(briefing["live"]["compacted"], Value::Null);

    let sweep = reply_forms(
        &strings(&[
            "session",
            "sweep",
            &file,
            "--limit",
            "3",
            "--deadline-seconds",
            "5",
        ]),
        &[],
        "sweep",
    );
    assert_eq!(sweep["rows"].as_array().map(Vec::len), Some(1));
    assert_eq!(sweep["rows"][0]["compaction"], Value::Null);
}

fn assert_one_boundary(transcript: &Path) {
    let file = transcript.to_string_lossy();
    let briefing = reply_forms(
        &strings(&["session", "briefing", &file, "--deadline-seconds", "5"]),
        &[],
        "recall",
    );
    assert_eq!(briefing["live"]["compacted"], MARKER_TS);

    let sweep = reply_forms(
        &strings(&[
            "session",
            "sweep",
            &file,
            "--limit",
            "3",
            "--deadline-seconds",
            "5",
        ]),
        &[],
        "sweep",
    );
    assert_eq!(sweep["rows"].as_array().map(Vec::len), Some(1));
    let compaction = &sweep["rows"][0]["compaction"];
    assert_eq!(compaction["ts"], MARKER_TS);
    assert_summary(&compaction["summary_after"], REHYDRATION);
    assert_eq!(
        compaction["intent"]["text"],
        "Preserve the continuation context."
    );
    assert_eq!(compaction["user_after"]["text"], USER_AFTER);
    assert_eq!(sweep["residues"], json!([]));
}

/// A clipped field keeps a prefix of the whitespace-collapsed text and
/// accounts for every byte it cut.
fn assert_summary(actual: &Value, expected: &str) {
    let kept = actual["text"].as_str().expect("compaction summary is text");
    let withheld = actual["withheld_bytes"].as_u64().unwrap_or(0);
    let normalized = expected.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(normalized.starts_with(kept), "{kept:?}");
    assert_eq!(
        kept.len() as u64 + withheld,
        normalized.len() as u64,
        "kept and withheld bytes cover the recovered summary"
    );
}

fn append_rows(path: &Path, rows: &[Value]) {
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .expect("transcript opens for append");
    for row in rows {
        serde_json::to_writer(&mut file, row).expect("appended row serializes");
        file.write_all(b"\n").expect("appended row terminates");
    }
}

fn before_rows() -> Vec<Value> {
    vec![
        omp_header(THREAD, "2026-02-02T00:00:00.000Z"),
        omp_user(
            "2026-02-02T00:00:01Z",
            "u1",
            "[$spice] repeated bootstrap text",
        ),
        with_usage(
            omp_text("2026-02-02T00:00:02Z", "a1", ASSISTANT_BEFORE, "stop"),
            199_000,
            0,
            1,
        ),
        omp_user(
            "2026-02-02T00:00:03Z",
            "u2",
            "[$spice] this fresh user turn is not a compaction",
        ),
    ]
}
