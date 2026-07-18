"""ACK grammar: the tuned header parser is a core product surface."""

import io
import json
import subprocess

from spice.agent.driver import DRIVER
from spice.agent import sidechannelnotify, watchdog
from spice.sqliteconnection import sqlite_connection
from spice.mail.feedback import supervisor_feedback_line
from spice.mail.ackarchive import (
    AckArchivalSummary,
    NackArchivalSummary,
    archive_ackd_inbox_items,
    summarize_ack_archival,
    summarize_nack_archival,
)
from spice.mail.ackgrammar import (
    ack_content_by_key,
    extract_ack_keys_from_text,
    extract_ack_segments_from_text,
    extract_nack_segments_from_text,
    extract_task_batch_lines_from_text,
    split_ack_message,
    split_keyed_response,
)
from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    ACK_DISPOSITION_REFUSED,
    AckStateWrite,
    ack_state_database_path,
    ack_state_records,
    record_acked_inbox_items,
)
from spice.mail.inbox import (
    InboxResendAttempt,
    collect_acked_inbox_items,
    collect_inbox_items,
    collect_refused_inbox_items,
    compose_inbox_text,
    inbox_ack_state_context_rows,
    parse_inbox_payload,
    pending_inbox_count,
)
from spice.mail.inbox import write_inbox_item

KEY_A = "1jyG6kGq"
KEY_B = "1jyG6kSc"
KEY_C = "1jyG6kqr"
KEY_D = "1jyG6lC4"
THREAD_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


def _init_repo(path):
    # The ACK-state db is centralized under the shared git common dir, so
    # archiving needs repo_root to be a real worktree.
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)


def test_backticked_key_with_colon_body():
    text = f"ACK `{KEY_A}`: captured, proceeding with the refactor."
    assert list(extract_ack_keys_from_text(text)) == [KEY_A]


def test_plain_key_with_space_body():
    text = f"ACK {KEY_A} captured."
    assert list(extract_ack_keys_from_text(text)) == [KEY_A]


def test_multiple_keys_one_header():
    text = f"ACK {KEY_A} {KEY_B}: both items handled."
    assert list(extract_ack_keys_from_text(text)) == [KEY_A, KEY_B]


def test_filler_words_between_ack_and_key():
    text = f"ACK inbox key `{KEY_A}`: done."
    assert list(extract_ack_keys_from_text(text)) == [KEY_A]


def test_collision_suffixed_key_is_extracted_verbatim():
    suffixed = f"{KEY_A}-2"
    text = f"ACK {suffixed}: retried send handled."
    assert list(extract_ack_keys_from_text(text)) == [suffixed]


def test_keys_only_extracted_from_valid_headers():
    text = f"The key {KEY_A} appears here without any marker.\nACK {KEY_B}: real."
    assert list(extract_ack_keys_from_text(text)) == [KEY_B]


def test_hyphen_prefixed_task_handle_remains_preamble_before_valid_ack():
    text = f"ACK-{KEY_A}: task handle prose.\nACK {KEY_B}: real acknowledgment."

    preamble, responses = split_keyed_response(text)

    assert preamble == f"ACK-{KEY_A}: task handle prose."
    assert [
        (response.keys, response.content, response.disposition)
        for response in responses
    ] == [((KEY_B,), "real acknowledgment.", ACK_DISPOSITION_ACKED)]
    assert list(extract_ack_keys_from_text(text)) == [KEY_B]


def test_negated_ack_mentions_do_not_extract_keys():
    guarded = [
        f"I will not ACK {KEY_A}: this steering conflicts.",
        f"I will-not ACK {KEY_A}: this steering conflicts.",
        f"I refuse to ACK {KEY_A}: this steering conflicts.",
        f"I cannot ACK {KEY_A}: this steering conflicts.",
        f"Use the alternative instead of ACK {KEY_A}: this steering conflicts.",
        f"Use the alternative instead-of ACK {KEY_A}: this steering conflicts.",
        f"I could not reproduce it, so I still refuse to ACK {KEY_A}: no.",
        f"I could not reproduce it, but I will not ACK {KEY_A}: no.",
    ]

    for text in guarded:
        assert list(extract_ack_keys_from_text(text)) == []
        assert extract_ack_segments_from_text(text) == []


def test_turning_connectives_reset_ack_negation_context():
    for turn in ("so", "therefore", "thus", "hence", "but"):
        text = f"I could not reproduce it, {turn} ACK {KEY_A}: handled."

        assert list(extract_ack_keys_from_text(text)) == [KEY_A]
        assert extract_ack_segments_from_text(text)[0].content == "handled."


def test_hypothetical_and_narrated_ack_mentions_do_not_extract_keys():
    guarded = [
        f"If I ACK {KEY_A}: the key would be retired.",
        f"Hypothetically ACK {KEY_A}: would retire the key.",
        f'The instruction says "ACK {KEY_A}: done" as an example.',
        f"The instruction says 'ACK {KEY_A}: done' as an example.",
        f"To acknowledge, write `ACK {KEY_A}: done` near the start.",
    ]

    for text in guarded:
        assert list(extract_ack_keys_from_text(text)) == []
        assert extract_ack_segments_from_text(text) == []


def test_ack_parser_ignores_markdown_examples_and_rendered_source_lines():
    real_key = "1jyG6lZJ"
    text = (
        "Example output:\n"
        "```text\n"
        f"ACK {KEY_A}: fenced example.\n"
        "```\n"
        f"> ACK {KEY_B}: quoted example.\n"
        f"docs/design/experimental/example.md:137:ACK {KEY_C}: rendered source output.\n"
        f"    ACK {KEY_D}: indented code output.\n"
        f"ACK {real_key}: actual acknowledgment."
    )

    assert list(extract_ack_keys_from_text(text)) == [real_key]
    assert [segment.keys for segment in extract_ack_segments_from_text(text)] == [
        (real_key,)
    ]


def test_nack_token_is_isolated_from_ack_parser():
    text = f"NACK {KEY_A}: refusing because the request is unsafe."
    segments = extract_nack_segments_from_text(text)

    assert list(extract_ack_keys_from_text(text)) == []
    assert [segment.keys for segment in segments] == [(KEY_A,)]
    assert segments[0].content == "refusing because the request is unsafe."


def test_split_preserves_preamble_and_segment_order():
    text = (
        "Some leading prose about the work.\n"
        f"ACK {KEY_A}: first answer.\n"
        "More detail for the first.\n"
        f"ACK {KEY_B}: second answer."
    )
    preamble, segments = split_ack_message(text)
    assert preamble == "Some leading prose about the work."
    assert [segment.keys for segment in segments] == [(KEY_A,), (KEY_B,)]
    assert segments[0].content == "first answer.\nMore detail for the first."
    assert segments[1].content == "second answer."


def test_ack_and_nack_segments_stop_at_next_keyed_marker():
    nack_then_ack = f"NACK {KEY_A}: cannot comply.\nACK {KEY_B}: done."
    ack_then_nack = f"ACK {KEY_A}: completed.\nNACK {KEY_B}: cannot comply."

    nack_segments = extract_nack_segments_from_text(nack_then_ack)
    ack_segments = extract_ack_segments_from_text(nack_then_ack)
    assert [segment.content for segment in nack_segments] == ["cannot comply."]
    assert [segment.content for segment in ack_segments] == ["done."]

    ack_segments = extract_ack_segments_from_text(ack_then_nack)
    nack_segments = extract_nack_segments_from_text(ack_then_nack)
    assert [segment.content for segment in ack_segments] == ["completed."]
    assert [segment.content for segment in nack_segments] == ["cannot comply."]


def test_split_keyed_response_tags_disposition_in_source_order():
    text = (
        "Leading prose.\n"
        f"ACK {KEY_A}: done the first.\n"
        f"NACK {KEY_B}: cannot do the second.\n"
        f"ACK {KEY_C}: done the third."
    )
    preamble, responses = split_keyed_response(text)

    assert preamble == "Leading prose."
    assert [(r.keys, r.content, r.disposition) for r in responses] == [
        ((KEY_A,), "done the first.", ACK_DISPOSITION_ACKED),
        ((KEY_B,), "cannot do the second.", ACK_DISPOSITION_REFUSED),
        ((KEY_C,), "done the third.", ACK_DISPOSITION_ACKED),
    ]


def test_split_keyed_response_keeps_a_nack_out_of_the_preamble():
    # split_ack_message alone would spill this refusal into the preamble because
    # it never sees the NACK marker; the unified split must not.
    text = f"NACK {KEY_A}: refusing because it would weaken the gate."
    preamble, responses = split_keyed_response(text)

    assert preamble == ""
    assert [(r.disposition, r.content) for r in responses] == [
        (ACK_DISPOSITION_REFUSED, "refusing because it would weaken the gate.")
    ]


def test_split_keyed_response_without_markers_returns_only_preamble():
    preamble, responses = split_keyed_response("just some prose, no markers.")
    assert preamble == "just some prose, no markers."
    assert responses == []


def test_bold_wrapped_ack_header_leaves_no_stray_markers():
    # Claude routinely bolds the header: `**ACK k:** body`. The wrapper must be
    # fully consumed — no stray `**` in the preamble or the segment body.
    text = f"**ACK {KEY_A}:** here is the full breakdown."
    preamble, segments = split_ack_message(text)
    assert preamble == ""
    assert segments[0].keys == (KEY_A,)
    assert segments[0].content == "here is the full breakdown."


def test_bold_wrapper_closing_at_body_end_is_stripped():
    # The wrapper can instead close after the body: `**ACK k: body**`.
    text = f"**ACK {KEY_A}: here is the full breakdown**"
    preamble, segments = split_ack_message(text)
    assert preamble == ""
    assert segments[0].content == "here is the full breakdown"


def test_underscore_wrapped_nack_header_is_consumed():
    text = f"__NACK {KEY_A}:__ cannot comply with this one."
    segments = extract_nack_segments_from_text(text)
    assert segments[0].keys == (KEY_A,)
    assert segments[0].content == "cannot comply with this one."


def test_single_asterisk_wrapped_ack_header_is_consumed():
    text = f"*ACK {KEY_A}:* italic-wrapped body."
    preamble, segments = split_ack_message(text)
    assert preamble == ""
    assert segments[0].content == "italic-wrapped body."


def test_bold_header_preserves_inner_bold_in_the_body():
    # Only the header wrapper is consumed; legitimate bold inside the body stays.
    text = f"**ACK {KEY_A}:** see **this detail** kept intact."
    segments = extract_ack_segments_from_text(text)
    assert segments[0].content == "see **this detail** kept intact."


def test_bold_wrapper_before_marker_stays_out_of_the_preamble():
    text = f"Leading prose about the work.\n**ACK {KEY_A}:** the answer."
    preamble, segments = split_ack_message(text)
    assert preamble == "Leading prose about the work."
    assert segments[0].content == "the answer."


def test_split_keyed_response_consumes_bold_wrappers_on_both_polarities():
    text = f"**ACK {KEY_A}:** did the first.\n**NACK {KEY_B}:** cannot do the second."
    preamble, responses = split_keyed_response(text)
    assert preamble == ""
    assert [(r.keys, r.content, r.disposition) for r in responses] == [
        ((KEY_A,), "did the first.", ACK_DISPOSITION_ACKED),
        ((KEY_B,), "cannot do the second.", ACK_DISPOSITION_REFUSED),
    ]


def test_segment_content_drops_app_directive_lines():
    text = f'ACK {KEY_A}: shipped.\n::git-commit{{"sha":"abc"}}\ntrailing prose.'
    segments = extract_ack_segments_from_text(text)
    assert segments[0].content == "shipped.\ntrailing prose."


def test_segment_content_drops_inline_task_directive_lines():
    text = (
        f"ACK {KEY_A}: captured.\n"
        "TASK title=Follow up | project=task.unit | acceptance=Tracked\n"
        "continuing."
    )
    segments = extract_ack_segments_from_text(text)

    assert list(extract_ack_keys_from_text(text)) == [KEY_A]
    assert segments[0].content == "captured.\ncontinuing."


def test_task_directives_are_extracted_from_any_message_line():
    text = (
        "TASK title=Standalone | project=task.unit | acceptance=Outside ACK\n"
        f"ACK {KEY_A}: captured.\n"
        "TASK: title=Captured | project=task.unit | acceptance=Inside ACK\n"
        f"ACK {KEY_B}: second."
    )
    preamble, segments = split_ack_message(text)

    assert extract_task_batch_lines_from_text(text) == [
        "TASK title=Standalone | project=task.unit | acceptance=Outside ACK",
        "TASK: title=Captured | project=task.unit | acceptance=Inside ACK",
    ]
    assert preamble == ""
    assert segments[0].content == "captured."


def test_task_directives_ignore_markdown_examples():
    text = (
        "```text\n"
        "TASK title=Fenced | project=task.unit | acceptance=Should not create\n"
        "```\n"
        "> TASK title=Quoted | project=task.unit | acceptance=Should not create\n"
        "    TASK title=Indented | project=task.unit | acceptance=Should not create\n"
        "TASK title=Real | project=task.unit | acceptance=Should create"
    )

    assert extract_task_batch_lines_from_text(text) == [
        "TASK title=Real | project=task.unit | acceptance=Should create"
    ]


def test_standalone_task_directive_is_stripped_from_display_text():
    text = "TASK title=Standalone | project=task.unit | acceptance=Tracked\nDone."

    preamble, segments = split_ack_message(text)

    assert extract_task_batch_lines_from_text(text) == [
        "TASK title=Standalone | project=task.unit | acceptance=Tracked"
    ]
    assert preamble == "Done."
    assert segments == []


def test_ack_state_database_is_centralized_under_git_common_dir(tmp_path):
    _init_repo(tmp_path)
    from spice.paths import git_common_dir

    path = ack_state_database_path(tmp_path)
    common = git_common_dir(tmp_path)

    # Sibling of the task backend db under the shared hidden common-dir root.
    assert path == common / ".spice" / "data" / "spiceacks.sqlite3"


def test_ack_state_migrates_existing_rows_to_store_operator_text(tmp_path):
    _init_repo(tmp_path)
    path = ack_state_database_path(tmp_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite_connection(path) as connection:
        connection.execute(
            """
            CREATE TABLE acked_inbox_items (
              key TEXT PRIMARY KEY,
              inbox_name TEXT NOT NULL,
              archived_at REAL NOT NULL
            )
            """
        )
        connection.execute(
            """
            INSERT INTO acked_inbox_items (key, inbox_name, archived_at)
            VALUES (?, ?, ?)
            """,
            (KEY_A, f"{KEY_A}.txt", 100.0),
        )

    text = compose_inbox_text(
        body="operator text from ack db", priority=None, stop=False
    )
    written = record_acked_inbox_items(
        tmp_path,
        [
            AckStateWrite(
                key=KEY_A,
                inbox_name=f"{KEY_A}.txt",
                text=text,
                attachments=(
                    {"path": "/tmp/attachment.png", "name": "attachment.png"},
                ),
            )
        ],
        now=200.0,
    )

    records = ack_state_records(tmp_path)
    with sqlite_connection(path) as connection:
        columns = {
            str(row[1])
            for row in connection.execute("PRAGMA table_info(acked_inbox_items)")
        }
    assert written == [KEY_A]
    assert {
        "key",
        "inbox_name",
        "text",
        "attachments_json",
        "lineage_json",
        "ack_text",
        "ack_content",
        "disposition",
        "archived_at",
    } <= columns
    assert [
        (
            record.key,
            record.inbox_name,
            record.text,
            record.ack_text,
            record.ack_content,
            record.disposition,
            record.archived_at,
        )
        for record in records
    ] == [(KEY_A, f"{KEY_A}.txt", text, "", "", ACK_DISPOSITION_ACKED, 200.0)]
    assert records[0].attachments == (
        {"name": "attachment.png", "path": "/tmp/attachment.png"},
    )
    assert records[0].lineage == {}


def test_archive_ackd_inbox_items_records_durable_ack_state(tmp_path):
    _init_repo(tmp_path)
    name = f"{KEY_A}.txt"
    text = compose_inbox_text(body="durable ack state", priority=None, stop=False)
    write_inbox_item(
        tmp_path,
        name,
        text,
    )

    assert archive_ackd_inbox_items(tmp_path, [KEY_A]) == [KEY_A]

    archived = collect_acked_inbox_items(tmp_path)
    records = ack_state_records(tmp_path)
    assert [(item.name, item.text) for item in archived] == [(name, text)]
    assert [
        (record.key, record.inbox_name, record.text, record.disposition)
        for record in records
    ] == [(KEY_A, name, text, ACK_DISPOSITION_ACKED)]


def test_summarize_ack_archival_retires_lineage_record_by_exact_key(
    tmp_path,
    monkeypatch,
):
    _init_repo(tmp_path)
    monkeypatch.setenv(DRIVER.thread_id_env, THREAD_A)
    name = f"{KEY_A}.txt"
    text = compose_inbox_text(
        body="lineage ack state",
        priority="critical",
        stop=False,
        resend_attempts=(
            InboxResendAttempt(
                attempt=1,
                at="2026-01-01T00:00:00Z",
                messages_elapsed=3,
            ),
            InboxResendAttempt(
                attempt=2,
                at="2026-01-01T00:01:00Z",
                messages_elapsed=4,
            ),
        ),
    )
    write_inbox_item(tmp_path, name, text)

    summary = summarize_ack_archival(
        tmp_path,
        f"ACK {KEY_A}: handled after retry.",
    )

    archived = collect_acked_inbox_items(tmp_path)
    records = ack_state_records(tmp_path)
    assert summary.archived == [KEY_A]
    assert pending_inbox_count(tmp_path) == 0
    assert [
        (item.name, parse_inbox_payload(item.text).resend_count) for item in archived
    ] == [(name, 2)]
    assert [
        (record.key, record.inbox_name, record.ack_content, record.lineage)
        for record in records
    ] == [
        (
            KEY_A,
            name,
            "handled after retry.",
            {
                "thread_id": THREAD_A,
                "resend_count": 2,
                "resend_attempts": [
                    {
                        "attempt": 1,
                        "at": "2026-01-01T00:00:00Z",
                        "messages_elapsed": 3,
                    },
                    {
                        "attempt": 2,
                        "at": "2026-01-01T00:01:00Z",
                        "messages_elapsed": 4,
                    },
                ],
            },
        )
    ]


def test_summarize_nack_archival_records_refused_state(tmp_path):
    _init_repo(tmp_path)
    name = f"{KEY_A}.txt"
    text = compose_inbox_text(body="cannot do this", priority="urgent", stop=False)
    write_inbox_item(tmp_path, name, text)

    summary = summarize_nack_archival(
        tmp_path, f"NACK {KEY_A}: refusing because it conflicts with policy."
    )

    refused = collect_refused_inbox_items(tmp_path)
    records = ack_state_records(tmp_path)
    rows = inbox_ack_state_context_rows(refused)
    assert summary.refused == [KEY_A]
    assert summary.already_refused == []
    assert summary.already_acked == []
    assert summary.unmatched == []
    assert summary.reasonless == []
    assert collect_acked_inbox_items(tmp_path) == []
    assert [
        record.key for record in records if record.disposition == ACK_DISPOSITION_ACKED
    ] == []
    assert pending_inbox_count(tmp_path) == 0
    assert [(item.name, item.text, item.disposition) for item in refused] == [
        (name, text, ACK_DISPOSITION_REFUSED)
    ]
    assert [
        (record.key, record.ack_text, record.ack_content, record.disposition)
        for record in records
    ] == [
        (
            KEY_A,
            f"NACK {KEY_A}: refusing because it conflicts with policy.",
            "refusing because it conflicts with policy.",
            ACK_DISPOSITION_REFUSED,
        )
    ]
    assert "status=already_consumed_operator_steering" in rows[0]
    assert f"refused_inbox key={KEY_A}" in rows[1]


def test_summarize_nack_archival_retires_lineage_record_with_stable_key(
    tmp_path,
    monkeypatch,
):
    _init_repo(tmp_path)
    monkeypatch.setenv(DRIVER.thread_id_env, THREAD_A)
    name = f"{KEY_B}.txt"
    text = compose_inbox_text(
        body="lineage refusal state",
        priority="urgent",
        stop=False,
        resend_attempts=(
            InboxResendAttempt(
                attempt=1,
                at="2026-01-01T00:00:00Z",
                messages_elapsed=3,
            ),
        ),
    )
    write_inbox_item(tmp_path, name, text)

    summary = summarize_nack_archival(
        tmp_path,
        f"NACK {KEY_B}: refusing after retry.",
    )

    refused = collect_refused_inbox_items(tmp_path)
    records = ack_state_records(tmp_path)
    assert summary.refused == [KEY_B]
    assert pending_inbox_count(tmp_path) == 0
    assert [
        (item.name, parse_inbox_payload(item.text).resend_count) for item in refused
    ] == [(name, 1)]
    assert [
        (
            record.key,
            record.inbox_name,
            record.ack_content,
            record.disposition,
            record.lineage,
        )
        for record in records
    ] == [
        (
            KEY_B,
            name,
            "refusing after retry.",
            ACK_DISPOSITION_REFUSED,
            {
                "thread_id": THREAD_A,
                "resend_count": 1,
                "resend_attempts": [
                    {
                        "attempt": 1,
                        "at": "2026-01-01T00:00:00Z",
                        "messages_elapsed": 3,
                    },
                ],
            },
        )
    ]


def test_reasonless_nack_does_not_retire_pending_item(tmp_path):
    _init_repo(tmp_path)
    name = f"{KEY_B}.txt"
    write_inbox_item(
        tmp_path,
        name,
        compose_inbox_text(body="needs a reasoned refusal", priority=None, stop=False),
    )

    summary = summarize_nack_archival(tmp_path, f"NACK {KEY_B}")

    assert summary.refused == []
    assert summary.reasonless == [KEY_B]
    assert pending_inbox_count(tmp_path) == 1
    assert [item.name for item in collect_inbox_items(tmp_path)] == [name]
    assert collect_refused_inbox_items(tmp_path) == []


def test_reasonless_nack_before_ack_does_not_refuse(tmp_path):
    _init_repo(tmp_path)
    name = f"{KEY_A}.txt"
    write_inbox_item(
        tmp_path,
        name,
        compose_inbox_text(body="needs a reasoned refusal", priority=None, stop=False),
    )

    summary = summarize_nack_archival(tmp_path, f"NACK {KEY_A}\nACK {KEY_B}: done.")

    assert summary.refused == []
    assert summary.reasonless == [KEY_A]
    assert [item.name for item in collect_inbox_items(tmp_path)] == [name]
    assert collect_refused_inbox_items(tmp_path) == []


def test_refused_key_does_not_block_operator_resend_under_fresh_key(tmp_path):
    _init_repo(tmp_path)
    first_name = f"{KEY_A}.txt"
    second_name = f"{KEY_B}.txt"
    text = compose_inbox_text(body="same operator steering", priority=None, stop=False)
    write_inbox_item(tmp_path, first_name, text)
    summarize_nack_archival(tmp_path, f"NACK {KEY_A}: cannot take this one.")

    write_inbox_item(tmp_path, second_name, text)
    second_summary = summarize_nack_archival(
        tmp_path, f"NACK {KEY_B}: still cannot take this fresh send."
    )

    assert second_summary.refused == [KEY_B]
    assert [item.name for item in collect_refused_inbox_items(tmp_path)] == [
        second_name,
        first_name,
    ]


def test_summarize_ack_archival_records_ack_content_in_ack_state(tmp_path):
    _init_repo(tmp_path)
    name = f"{KEY_A}.txt"
    text = compose_inbox_text(body="durable ack content", priority=None, stop=False)
    write_inbox_item(tmp_path, name, text)

    summary = summarize_ack_archival(tmp_path, f"ACK {KEY_A}: handled fully.")

    records = ack_state_records(tmp_path)
    assert summary.archived == [KEY_A]
    assert summary.already_acked == []
    assert summary.unmatched == []
    assert [
        (record.key, record.ack_text, record.ack_content) for record in records
    ] == [(KEY_A, f"ACK {KEY_A}: handled fully.", "handled fully.")]


def test_summarize_ack_archival_keeps_task_handle_and_retires_valid_header(tmp_path):
    _init_repo(tmp_path)
    for key, body in ((KEY_A, "task-handle lookalike"), (KEY_B, "real ack")):
        write_inbox_item(
            tmp_path,
            f"{key}.txt",
            compose_inbox_text(body=body, priority=None, stop=False),
        )

    summary = summarize_ack_archival(
        tmp_path,
        f"ACK-{KEY_A}: task handle prose.\nACK {KEY_B}: handled the real steering.",
    )

    assert summary.archived == [KEY_B]
    assert [item.name for item in collect_inbox_items(tmp_path)] == [f"{KEY_A}.txt"]
    assert [
        (record.key, record.ack_content) for record in ack_state_records(tmp_path)
    ] == [(KEY_B, "handled the real steering.")]
    assert summarize_ack_archival(
        tmp_path, f"ACK-{KEY_A}: task handle without a real acknowledgment."
    ) == AckArchivalSummary(archived=[], already_acked=[], unmatched=[])


def test_summarize_ack_archival_reports_already_acked_key(tmp_path):
    _init_repo(tmp_path)
    name = f"{KEY_A}.txt"
    text = compose_inbox_text(body="already acked", priority=None, stop=False)
    write_inbox_item(tmp_path, name, text)
    assert archive_ackd_inbox_items(tmp_path, [KEY_A]) == [KEY_A]

    summary = summarize_ack_archival(tmp_path, f"ACK {KEY_A}: repeated.")

    assert summary.archived == []
    assert summary.already_acked == [KEY_A]
    assert summary.unmatched == []
    assert not summary.noop


def test_summarize_ack_archival_reports_noop_ack_without_key(tmp_path):
    _init_repo(tmp_path)

    summary = summarize_ack_archival(tmp_path, "ACK: moving on.")

    assert summary.archived == []
    assert summary.already_acked == []
    assert summary.unmatched == []
    assert summary.noop


def test_summarize_ack_archival_ignores_narrated_noop_ack(tmp_path):
    _init_repo(tmp_path)

    summary = summarize_ack_archival(tmp_path, "To acknowledge, write ACK plainly.")

    assert summary == AckArchivalSummary(
        archived=[],
        already_acked=[],
        unmatched=[],
    )


def test_summarize_ack_archival_ignores_scanner_narration(tmp_path):
    _init_repo(tmp_path)

    summary = summarize_ack_archival(tmp_path, "I am checking the ACK scanner code.")

    assert summary == AckArchivalSummary(
        archived=[],
        already_acked=[],
        unmatched=[],
    )


def test_archival_summaries_degrade_outside_git_worktree(tmp_path):
    outside_worktree = tmp_path / "outside"
    outside_worktree.mkdir()

    assert summarize_ack_archival(
        outside_worktree, f"ACK {KEY_A}: handled outside a worktree."
    ) == AckArchivalSummary(archived=[], already_acked=[], unmatched=[])
    assert summarize_nack_archival(
        outside_worktree, f"NACK {KEY_B}: refusing outside a worktree."
    ) == NackArchivalSummary(
        refused=[],
        already_refused=[],
        already_acked=[],
        unmatched=[],
        reasonless=[],
    )


def test_ack_state_supplies_archive_context_without_archive_files(tmp_path):
    _init_repo(tmp_path)
    name = f"{KEY_B}.txt"
    text = compose_inbox_text(
        body="ack state outlives archive", priority=None, stop=False
    )
    write_inbox_item(
        tmp_path,
        name,
        text,
    )

    archive_ackd_inbox_items(tmp_path, [KEY_B])

    archived = collect_acked_inbox_items(tmp_path)
    records = ack_state_records(tmp_path)
    assert [(item.name, item.text) for item in archived] == [(name, text)]
    assert [(record.key, record.inbox_name, record.text) for record in records] == [
        (KEY_B, name, text)
    ]


def test_content_by_key_latest_ack_wins():
    early = extract_ack_segments_from_text(f"ACK {KEY_A}: early answer.")
    late = extract_ack_segments_from_text(f"ACK {KEY_A}: revised answer.")
    mapping = ack_content_by_key([*early, *late])
    assert mapping == {KEY_A: "revised answer."}


def test_cross_line_ack_header_extracts_key_and_body():
    text = f"ACK\n`{KEY_A}`:\nhandled across lines."
    segments = extract_ack_segments_from_text(text)
    assert list(extract_ack_keys_from_text(text)) == [KEY_A]
    assert [segment.keys for segment in segments] == [(KEY_A,)]
    assert segments[0].content == "handled across lines."


def test_inline_multi_ack_splitting_keeps_each_body_with_its_key():
    text = f"ACK {KEY_A}: first handled. ACK {KEY_B}: second handled."
    preamble, segments = split_ack_message(text)
    assert preamble == ""
    assert [segment.keys for segment in segments] == [(KEY_A,), (KEY_B,)]
    assert [segment.content for segment in segments] == [
        "first handled.",
        "second handled.",
    ]


def test_supervised_nack_reports_refused_key(tmp_path, monkeypatch):
    _init_repo(tmp_path)
    monkeypatch.setattr(watchdog, "record_supervised_lane_metrics", lambda _repo: None)
    monkeypatch.setattr(
        watchdog,
        "publish_maxim_hits_as_inbox",
        lambda _repo, _text, **_kwargs: [],
    )
    write_inbox_item(
        tmp_path,
        f"{KEY_C}.txt",
        compose_inbox_text(body="operator asks", priority=None, stop=False),
    )
    log = io.StringIO()

    watchdog.process_supervised_assistant_message(
        tmp_path,
        f"NACK {KEY_C}: refusing with operator-visible rationale.",
        log,
        watchdog.MaximReminderGate(),
    )

    feedback = sidechannelnotify.consume_side_channel_notices(tmp_path)
    assert feedback == [supervisor_feedback_line("nack.refused", keys=[KEY_C])]
    assert [item.name for item in collect_refused_inbox_items(tmp_path)] == [
        f"{KEY_C}.txt"
    ]


def _assistant_line(text: str) -> str:
    event = {
        "timestamp": "2026-01-01T00:00:00.000Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    }
    return f"{json.dumps(event, separators=(',', ':'))}\n"
