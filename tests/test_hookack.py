"""The hook-side ACK backstop: retirement when the stdout stream carried nothing.

Every test here runs with no supervisor at all -- nothing tees an agent's stdout,
nothing calls `spice agent reply` -- because that is exactly the failure being
covered: the supervisor's pipe went quiet mid-session and the durable transcript
is the only place the acknowledgment survives.
"""

import io
import json
import subprocess

from spice.agent import hookack, sidechannel, watchdog
from spice.agent.driver import SPICE_AGENT_DRIVER_ENV, dashed_uuid
from spice.agent.hookack import sweep_transcript_acks
from spice.agent.paths import write_agent_thread_pointer
from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    ACK_DISPOSITION_REFUSED,
    ack_state_records,
)
from spice.mail.inbox import (
    collect_refused_inbox_items,
    compose_inbox_text,
    pending_inbox_count,
    write_inbox_item,
)

KEY_A = "1jyG6kGq"
KEY_B = "1jyG6kSc"
THREAD_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"


def _init_worktree(tmp_path, monkeypatch):
    """Make `tmp_path` a Claude-driven worktree seating no thread yet.

    The ACK-state db is centralized under the shared git common dir, so
    archiving needs repo_root to be a real worktree.
    """
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    monkeypatch.setenv(SPICE_AGENT_DRIVER_ENV, "claude")
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path / "claude"))


def _seat_agent(tmp_path, monkeypatch):
    """Seat a Claude thread in `tmp_path` and return its transcript path."""
    _init_worktree(tmp_path, monkeypatch)
    write_agent_thread_pointer(tmp_path, THREAD_A)
    transcript = (
        tmp_path
        / "claude"
        / "projects"
        / "-tmp-worktree"
        / f"{dashed_uuid(THREAD_A)}.jsonl"
    )
    transcript.parent.mkdir(parents=True)
    transcript.write_text("", encoding="utf-8")
    return transcript


def _append_assistant_text(transcript, text, *, at="2026-08-05T20:38:41.774Z"):
    """Append one assistant record in the Claude transcript dialect."""
    record = {
        "type": "assistant",
        "timestamp": at,
        "message": {
            "role": "assistant",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": text}],
        },
    }
    with transcript.open("a", encoding="utf-8") as handle:
        handle.write(f"{json.dumps(record)}\n")


def _pend(tmp_path, key, body):
    write_inbox_item(
        tmp_path,
        f"{key}.txt",
        compose_inbox_text(body=body, priority=None, stop=False),
    )


def test_hook_retires_ack_the_stdout_stream_never_carried(tmp_path, monkeypatch):
    transcript = _seat_agent(tmp_path, monkeypatch)
    _pend(tmp_path, KEY_A, "clear this from the hook, not the supervisor")
    sweep_transcript_acks(tmp_path, stderr=io.StringIO())
    _append_assistant_text(transcript, f"ACK {KEY_A}: caught by the hook.")

    sweep = sweep_transcript_acks(tmp_path, stderr=io.StringIO())

    acked = [
        (record.key, record.ack_content)
        for record in ack_state_records(tmp_path)
        if record.disposition == ACK_DISPOSITION_ACKED
    ]
    assert sweep.archived == (KEY_A,)
    assert sweep.messages == 1
    assert pending_inbox_count(tmp_path) == 0
    assert acked == [(KEY_A, "caught by the hook.")]


def test_sweep_retires_every_message_in_a_multi_record_span(tmp_path, monkeypatch):
    transcript = _seat_agent(tmp_path, monkeypatch)
    _pend(tmp_path, KEY_A, "acknowledged two commands ago")
    _pend(tmp_path, KEY_B, "acknowledged one command ago")
    sweep_transcript_acks(tmp_path, stderr=io.StringIO())
    _append_assistant_text(
        transcript, f"ACK {KEY_A}: older.", at="2026-08-05T20:38:41.774Z"
    )
    _append_assistant_text(
        transcript, f"ACK {KEY_B}: newer.", at="2026-08-05T20:39:43.115Z"
    )

    sweep = sweep_transcript_acks(tmp_path, stderr=io.StringIO())

    assert sweep.archived == (KEY_A, KEY_B)
    assert sweep.messages == 2
    assert pending_inbox_count(tmp_path) == 0


def test_hook_refuses_the_key_a_transcript_nack_refused(tmp_path, monkeypatch):
    transcript = _seat_agent(tmp_path, monkeypatch)
    _pend(tmp_path, KEY_A, "the lane refused this one and said why")
    sweep_transcript_acks(tmp_path, stderr=io.StringIO())
    _append_assistant_text(
        transcript, f"NACK {KEY_A}: refusing because it conflicts with policy."
    )
    stderr = io.StringIO()

    sweep = sweep_transcript_acks(tmp_path, stderr=stderr)

    refused = collect_refused_inbox_items(tmp_path)
    assert sweep.refused == (KEY_A,)
    assert sweep.archived == ()
    assert [(item.name, item.disposition) for item in refused] == [
        (f"{KEY_A}.txt", ACK_DISPOSITION_REFUSED)
    ]
    assert f"nack.refused-at-hook keys={KEY_A}" in stderr.getvalue()


def test_first_sweep_primes_past_history_the_supervisor_already_handled(
    tmp_path, monkeypatch
):
    transcript = _seat_agent(tmp_path, monkeypatch)
    _pend(tmp_path, KEY_A, "an unrelated key that must stay pending")
    _append_assistant_text(transcript, f"ACK {KEY_A}: handled long before the hook.")

    sweep = sweep_transcript_acks(tmp_path, stderr=io.StringIO())

    assert sweep.primed is True
    assert sweep.archived == ()
    assert sweep.records == 0
    assert pending_inbox_count(tmp_path) == 1


def test_sweep_that_read_nothing_differs_from_one_that_read_records(
    tmp_path, monkeypatch
):
    transcript = _seat_agent(tmp_path, monkeypatch)
    sweep_transcript_acks(tmp_path, stderr=io.StringIO())

    quiet = sweep_transcript_acks(tmp_path, stderr=io.StringIO())
    _append_assistant_text(transcript, "Reading the config now, no keys in hand.")
    busy = sweep_transcript_acks(tmp_path, stderr=io.StringIO())

    assert (quiet.records, quiet.messages) == (0, 0)
    assert (busy.records, busy.messages) == (1, 1)
    assert quiet.records != busy.records
    assert quiet.archived == busy.archived == ()


def test_both_retirement_roads_consult_one_archival_authority(tmp_path, monkeypatch):
    transcript = _seat_agent(tmp_path, monkeypatch)
    _pend(tmp_path, KEY_A, "retired down the supervisor road")
    _pend(tmp_path, KEY_B, "retired down the hook road")
    sweep_transcript_acks(tmp_path, stderr=io.StringIO())
    consultations: list[str] = []
    real_archival = watchdog.summarize_ack_archival

    def counted_archival(repo_root, message_text):
        consultations.append(message_text)
        return real_archival(repo_root, message_text)

    monkeypatch.setattr(watchdog, "summarize_ack_archival", counted_archival)
    supervisor_header = f"ACK {KEY_A}: down the supervisor road."
    hook_header = f"ACK {KEY_B}: down the hook road."

    watchdog._publish_ack_feedback(tmp_path, supervisor_header, io.StringIO())
    _append_assistant_text(transcript, hook_header)
    sweep_transcript_acks(tmp_path, stderr=io.StringIO())

    # Counting consultations, not comparing answers: two independent copies of
    # the archival logic would agree on these keys and register one call.
    assert consultations == [supervisor_header, hook_header]
    assert pending_inbox_count(tmp_path) == 0


def test_post_tool_hook_payload_retires_and_narrates_the_catch(tmp_path, monkeypatch):
    transcript = _seat_agent(tmp_path, monkeypatch)
    _pend(tmp_path, KEY_A, "the operator is watching this key sit pending")
    sidechannel.render_post_tool_hook_payload(tmp_path)
    _append_assistant_text(transcript, f"ACK {KEY_A}: on it.")

    payload = sidechannel.render_post_tool_hook_payload(tmp_path)

    assert pending_inbox_count(tmp_path) == 0
    assert f"ack.archived-at-hook keys={KEY_A}" in payload


def test_replaced_transcript_restarts_rather_than_resuming_into_new_bytes(
    tmp_path, monkeypatch
):
    transcript = _seat_agent(tmp_path, monkeypatch)
    _pend(tmp_path, KEY_A, "acknowledged in the session that replaced the file")
    _append_assistant_text(transcript, "A long first session with plenty of prose.")
    sweep_transcript_acks(tmp_path, stderr=io.StringIO())
    replacement = transcript.with_name("replacement.jsonl")
    replacement.write_text("", encoding="utf-8")
    _append_assistant_text(replacement, f"ACK {KEY_A}: fresh session.")
    replacement.replace(transcript)

    sweep = sweep_transcript_acks(tmp_path, stderr=io.StringIO())

    assert sweep.archived == (KEY_A,)
    assert pending_inbox_count(tmp_path) == 0


def test_sweep_without_a_seated_thread_reads_no_transcript(tmp_path, monkeypatch):
    _init_worktree(tmp_path, monkeypatch)
    _pend(tmp_path, KEY_A, "nothing may retire this without a transcript")

    sweep = sweep_transcript_acks(tmp_path, stderr=io.StringIO())

    assert sweep == hookack.HookAckSweep()
    assert pending_inbox_count(tmp_path) == 1
