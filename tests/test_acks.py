"""ACK grammar: the tuned header parser is a core product surface."""

import io
import json
import sqlite3
import subprocess

from spice.agent import sidechannelnotify, watchdog
from spice.mail.feedback import supervisor_feedback_line
from spice.mail.acks import (
    ack_content_by_key,
    archive_ackd_inbox_items,
    extract_ack_keys_from_text,
    extract_ack_segments_from_text,
    extract_nack_segments_from_text,
    extract_task_batch_lines_from_text,
    iter_ack_state_keys,
    summarize_ack_archival,
    summarize_nack_archival,
    split_ack_message,
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
    collect_acked_inbox_items,
    collect_inbox_items,
    collect_refused_inbox_items,
    compose_inbox_text,
    inbox_ack_state_context_rows,
    inbox_dir,
    parse_inbox_payload,
    pending_inbox_count,
)
from spice.mail.inbox import write_inbox_item
from spice.mail.watch import (
    AckWatchOutcome,
    AckWatchState,
    extract_owned_ack_utterance,
    extract_owned_nack_utterance,
)

KEY_A = "20260513T184251491561Z"
KEY_B = "20260513T184252000000Z"
KEY_C = "20260513T184253000000Z"
KEY_D = "20260513T184254000000Z"


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


def test_dropped_z_key_is_extracted_verbatim():
    bare = KEY_A[:-1]
    text = f"ACK {bare}: transcribed without the Z."
    assert list(extract_ack_keys_from_text(text)) == [bare]


def test_keys_only_extracted_from_valid_headers():
    text = f"The key {KEY_A} appears here without any marker.\nACK {KEY_B}: real."
    assert list(extract_ack_keys_from_text(text)) == [KEY_B]


def test_negated_ack_mentions_do_not_extract_keys():
    guarded = [
        f"I will not ACK {KEY_A}: this steering conflicts.",
        f"I will-not ACK {KEY_A}: this steering conflicts.",
        f"I refuse to ACK {KEY_A}: this steering conflicts.",
        f"I cannot ACK {KEY_A}: this steering conflicts.",
        f"Use the alternative instead of ACK {KEY_A}: this steering conflicts.",
        f"Use the alternative instead-of ACK {KEY_A}: this steering conflicts.",
    ]

    for text in guarded:
        assert list(extract_ack_keys_from_text(text)) == []
        assert extract_ack_segments_from_text(text) == []


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

    # Sibling of the task backend db under the shared common dir, not .spice.
    assert path == common / "spice" / "data" / "spiceacks.sqlite3"
    assert ".spice" not in path.parts


def test_ack_state_migrates_existing_rows_to_store_operator_text(tmp_path):
    _init_repo(tmp_path)
    path = ack_state_database_path(tmp_path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(path) as connection:
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
    with sqlite3.connect(path) as connection:
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


def test_summarize_nack_archival_records_refused_state(tmp_path):
    _init_repo(tmp_path)
    name = f"{KEY_A}.txt"
    text = compose_inbox_text(body="cannot do this", priority="urgent", stop=False)
    write_inbox_item(tmp_path, name, text)

    summary = summarize_nack_archival(
        tmp_path, f"NACK {KEY_A[:-1]}: refusing because it conflicts with policy."
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
    assert list(iter_ack_state_keys(tmp_path)) == []
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
            f"NACK {KEY_A[:-1]}: refusing because it conflicts with policy.",
            "refusing because it conflicts with policy.",
            ACK_DISPOSITION_REFUSED,
        )
    ]
    assert "status=already_consumed_operator_steering" in rows[0]
    assert f"refused_inbox key={KEY_A}" in rows[1]


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

    summary = summarize_ack_archival(tmp_path, f"ACK {KEY_A[:-1]}: handled fully.")

    records = ack_state_records(tmp_path)
    assert summary.archived == [KEY_A]
    assert summary.already_acked == []
    assert summary.unmatched == []
    assert [
        (record.key, record.ack_text, record.ack_content) for record in records
    ] == [(KEY_A, f"ACK {KEY_A[:-1]}: handled fully.", "handled fully.")]


def test_summarize_ack_archival_reports_already_acked_key(tmp_path):
    _init_repo(tmp_path)
    name = f"{KEY_A}.txt"
    text = compose_inbox_text(body="already acked", priority=None, stop=False)
    write_inbox_item(tmp_path, name, text)
    assert archive_ackd_inbox_items(tmp_path, [KEY_A]) == [KEY_A]

    summary = summarize_ack_archival(tmp_path, f"ACK {KEY_A[:-1]}: repeated.")

    assert summary.archived == []
    assert summary.already_acked == [KEY_A[:-1]]
    assert summary.unmatched == []


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


def test_owned_ack_utterance_selects_matching_key_stem_and_bodyless_fallback():
    text = f"ACK {KEY_A}: other answer. ACK {KEY_B[:-1]}: owned answer."
    assert extract_owned_ack_utterance(text, KEY_B) == "owned answer."
    assert extract_owned_ack_utterance(f"ACK {KEY_B}", KEY_B) == "ACK"


def test_owned_nack_utterance_requires_reason_for_matching_key():
    text = f"NACK {KEY_A}: other refusal. NACK {KEY_B[:-1]}: owned refusal."
    assert extract_owned_nack_utterance(text, KEY_B) == "owned refusal."
    assert extract_owned_nack_utterance(f"NACK {KEY_B}", KEY_B) is None


def test_ack_watch_nack_halts_resend_escalation(tmp_path):
    original_key = "20260101T000000000001Z"
    original_text = compose_inbox_text(body="consider this", priority=None, stop=False)
    write_inbox_item(tmp_path, f"{original_key}.txt", original_text)
    state = AckWatchState(
        inbox_key=original_key,
        original_text=original_text,
        target_repo_root=tmp_path,
        quiet=True,
    )

    state.process_line(
        _assistant_line(f"NACK {original_key[:-1]}: refusing with a concrete reason.")
    )
    for index in range(3):
        state.process_line(_assistant_line(f"ordinary response {index}"))

    assert state.outcome() == AckWatchOutcome(
        acked=False, assistant_messages_seen=1, resends=0, refused=True
    )
    assert state.current_key == original_key


def test_ack_watch_resends_after_budget_and_escalates_stop_payload(
    tmp_path, monkeypatch
):
    original_key = "20260101T000000000001Z"
    original_text = compose_inbox_text(
        body="wind down after this", priority=None, stop=True
    )
    write_inbox_item(tmp_path, f"{original_key}.txt", original_text)
    stamps = iter(["20260101T000000000101Z", "20260101T000000000102Z"])
    observed_acks: list[tuple[str, str]] = []
    monkeypatch.setattr("spice.mail.inbox.inbox_timestamp", lambda: next(stamps))
    state = AckWatchState(
        inbox_key=original_key,
        original_text=original_text,
        target_repo_root=tmp_path,
        quiet=True,
        on_ack=lambda text, key: observed_acks.append((text, key)),
    )

    for index in range(3):
        state.process_line(_assistant_line(f"ordinary response {index}"))

    first_resend = inbox_dir(tmp_path) / "20260101T000000000101Z.txt"
    first_payload = parse_inbox_payload(first_resend.read_text(encoding="utf-8"))
    assert state.resends == 1
    assert state.current_key == "20260101T000000000101Z"
    assert first_payload.priority == "urgent"
    assert first_payload.body == "wind down after this"
    assert first_payload.is_stop is True

    for index in range(3, 6):
        state.process_line(_assistant_line(f"ordinary response {index}"))

    second_resend = inbox_dir(tmp_path) / "20260101T000000000102Z.txt"
    second_payload = parse_inbox_payload(second_resend.read_text(encoding="utf-8"))
    assert state.resends == 2
    assert state.current_key == "20260101T000000000102Z"
    assert second_payload.priority == "critical"
    assert second_payload.body == "wind down after this"
    assert second_payload.is_stop is True

    ack_text = f"ACK {state.current_key[:-1]}: received after retry."
    state.process_line(_assistant_line(ack_text))

    assert state.outcome() == AckWatchOutcome(
        acked=True, assistant_messages_seen=7, resends=2
    )
    assert observed_acks == [(ack_text, "20260101T000000000102Z")]


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
