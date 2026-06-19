"""ACK grammar: the tuned header parser is a core product surface."""

import json
from types import SimpleNamespace

from spice.agent.driver import DRIVER
from spice.mail.acks import (
    ack_content_by_key,
    collect_ack_segments,
    collect_unique_ack_keys,
    extract_ack_keys_from_text,
    extract_ack_segments_from_text,
    split_ack_message,
)
from spice.mail.inbox import compose_inbox_text, inbox_dir, parse_inbox_payload
from spice.mail.inbox import write_inbox_item
from spice.mail.watch import (
    AckWatchOutcome,
    _AckWatchState,
    extract_owned_ack_utterance,
    resolve_target_thread_id,
)

KEY_A = "20260513T184251491561Z"
KEY_B = "20260513T184252000000Z"
KEY_C = "20260513T184253000000Z"
KEY_D = "20260513T184254000000Z"


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


def test_collect_ack_keys_respects_time_window_short_circuit(tmp_path):
    transcript = tmp_path / "rollout.jsonl"
    _write_jsonl(
        transcript,
        [
            _assistant("2026-01-01T00:00:00.000Z", f"ACK {KEY_A}: before."),
            _assistant("2026-01-01T00:00:01.000Z", f"ACK {KEY_B}: inside."),
            _assistant("2026-01-01T00:00:02.000Z", f"ACK {KEY_C}: after."),
            _assistant("2026-01-01T00:00:01.500Z", f"ACK {KEY_D}: out of order."),
        ],
    )

    keys = collect_unique_ack_keys(
        [transcript],
        start_ts="2026-01-01T00:00:01.000Z",
        end_ts="2026-01-01T00:00:01.999Z",
    )

    assert keys == [KEY_B]


def test_collect_ack_segments_filters_to_active_turn_id(tmp_path):
    transcript = tmp_path / "rollout.jsonl"
    _write_jsonl(
        transcript,
        [
            _event(
                "2026-01-01T00:00:00.000Z",
                "event_msg",
                {"type": "task_started", "turn_id": "turn-a"},
            ),
            _assistant("2026-01-01T00:00:01.000Z", f"ACK {KEY_A}: turn a."),
            _event(
                "2026-01-01T00:00:02.000Z",
                "event_msg",
                {"type": "task_complete"},
            ),
            _assistant("2026-01-01T00:00:03.000Z", f"ACK {KEY_B}: outside."),
            _event(
                "2026-01-01T00:00:04.000Z",
                "event_msg",
                {"type": "task_started", "turn_id": "turn-b"},
            ),
            _assistant("2026-01-01T00:00:05.000Z", f"ACK {KEY_C}: turn b."),
        ],
    )

    segments = collect_ack_segments([transcript], turn_ids=["turn-b"])

    assert [(segment.keys, segment.content) for segment in segments] == [
        ((KEY_C,), "turn b.")
    ]


def test_owned_ack_utterance_selects_matching_key_stem_and_bodyless_fallback():
    text = f"ACK {KEY_A}: other answer. ACK {KEY_B[:-1]}: owned answer."
    assert extract_owned_ack_utterance(text, KEY_B) == "owned answer."
    assert extract_owned_ack_utterance(f"ACK {KEY_B}", KEY_B) == "ACK"


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
    state = _AckWatchState(
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


def test_resolve_target_thread_id_prefers_explicit_then_state_then_ambient(
    tmp_path, monkeypatch
):
    explicit = "AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA"
    state_thread = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    ambient = "cccccccccccccccccccccccccccccccc"
    monkeypatch.setenv(DRIVER.thread_id_env, ambient)
    monkeypatch.setattr(
        "spice.agent.lifecycle.agent_status",
        lambda repo_root: SimpleNamespace(thread_id=state_thread),
    )

    assert (
        resolve_target_thread_id(tmp_path, explicit_thread_id=explicit)
        == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    )
    assert resolve_target_thread_id(tmp_path, explicit_thread_id=None) == state_thread
    assert resolve_target_thread_id(None, explicit_thread_id=None) == ambient


def _event(timestamp: str, record_type: str, payload: dict):
    return {"timestamp": timestamp, "type": record_type, "payload": payload}


def _assistant(timestamp: str, text: str):
    return _event(
        timestamp,
        "response_item",
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )


def _assistant_line(text: str) -> str:
    return f"{json.dumps(_assistant('2026-01-01T00:00:00.000Z', text), separators=(',', ':'))}\n"


def _write_jsonl(path, events):
    path.write_text(
        "".join(f"{json.dumps(event, separators=(',', ':'))}\n" for event in events),
        encoding="utf-8",
    )
