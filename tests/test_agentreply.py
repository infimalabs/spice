"""spice agent reply retirement and independent reply-card behavior."""

import io
import json

import pytest

from spice.agent import cli as agent_cli
from spice.agent.paths import write_agent_thread_pointer
from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.mail import ackarchive
from spice.mail.ackarchive import summarize_ack_archival
from spice.mail.ackstate import (
    ACK_DISPOSITION_ACKED,
    ACK_DISPOSITION_REFUSED,
    ack_state_records,
)
from spice.mail.inbox import collect_inbox_items, write_inbox_item
from spice.mail.replies import append_reply_record, read_reply_records, reply_log_path
from spice.serve.messagepresentation import reply_card_message
from tests.test_reposcaffolding import init_identified_repo as _init_git_repo

THREAD = "f2249a9fb99641e29e1854cb381cc634"
KEY = "1jNmXPHm"
KEY_B = "1jyG6kSc"


def test_agent_reply_retires_key_and_logs_a_reply_card(tmp_path, monkeypatch, capsys):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_agent_thread_pointer(repo, THREAD)
    write_inbox_item(repo, f"{KEY}.txt", "do X")
    monkeypatch.setattr(agent_cli.sys, "stdin", io.StringIO(f"ACK {KEY}: shipped X\n"))

    args = build_parser().parse_args(["agent", "reply"])
    assert agent_cli.handle_agent(args) == 0

    assert not collect_inbox_items(str(repo))  # key retired
    records = read_reply_records(repo, THREAD)
    assert len(records) == 1
    assert records[0]["ackKeys"] == [KEY]
    assert "shipped X" in records[0]["text"]
    assert capsys.readouterr().out == f"ack {KEY}: retired\n"


def test_agent_reply_after_inline_ack_reports_already_without_duplicate_card(
    tmp_path, monkeypatch, capsys
):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_agent_thread_pointer(repo, THREAD)
    text = f"ACK {KEY}: shipped once"
    write_inbox_item(repo, f"{KEY}.txt", "do X")
    assert summarize_ack_archival(repo, text).archived == [KEY]

    args = build_parser().parse_args(["agent", "reply", text])
    assert agent_cli.handle_agent(args) == 0

    assert read_reply_records(repo, THREAD) == []
    assert capsys.readouterr().out == f"ack {KEY}: already acknowledged\n"


def test_repeated_agent_reply_logs_only_the_newly_consumed_submission(
    tmp_path, monkeypatch, capsys
):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_agent_thread_pointer(repo, THREAD)
    write_inbox_item(repo, f"{KEY}.txt", "do X")
    text = f"ACK {KEY}: shipped once"
    args = build_parser().parse_args(["agent", "reply", text])

    assert agent_cli.handle_agent(args) == 0
    assert agent_cli.handle_agent(args) == 0

    assert len(read_reply_records(repo, THREAD)) == 1
    assert capsys.readouterr().out.splitlines() == [
        f"ack {KEY}: retired",
        f"ack {KEY}: already acknowledged",
    ]


def test_agent_reply_rejects_reasonless_nack_before_retirement(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_agent_thread_pointer(repo, THREAD)
    write_inbox_item(repo, f"{KEY}.txt", "do X")
    args = build_parser().parse_args(["agent", "reply", f"NACK {KEY}"])

    with pytest.raises(SpiceError, match="NACK requires a reason"):
        agent_cli.handle_agent(args)

    assert [item.name for item in collect_inbox_items(str(repo))] == [f"{KEY}.txt"]
    assert ack_state_records(repo) == []
    assert read_reply_records(repo, THREAD) == []


def test_agent_reply_rejects_text_without_a_keyed_response(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_agent_thread_pointer(repo, THREAD)
    args = build_parser().parse_args(["agent", "reply", "shipped without a header"])

    with pytest.raises(SpiceError, match="no ACK or NACK header"):
        agent_cli.handle_agent(args)

    assert ack_state_records(repo) == []
    assert read_reply_records(repo, THREAD) == []


def test_agent_reply_rejects_conflicting_polarity_before_retirement(
    tmp_path, monkeypatch
):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_agent_thread_pointer(repo, THREAD)
    write_inbox_item(repo, f"{KEY}.txt", "do X")
    text = f"ACK {KEY}: done\nNACK {KEY}: cannot do it"
    args = build_parser().parse_args(["agent", "reply", text])

    with pytest.raises(SpiceError, match="cannot ACK and NACK the same"):
        agent_cli.handle_agent(args)

    assert [item.name for item in collect_inbox_items(str(repo))] == [f"{KEY}.txt"]
    assert ack_state_records(repo) == []
    assert read_reply_records(repo, THREAD) == []


def test_agent_reply_honors_reasoned_nack_and_records_refusal(
    tmp_path, monkeypatch, capsys
):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_agent_thread_pointer(repo, THREAD)
    write_inbox_item(repo, f"{KEY}.txt", "do X")
    text = f"NACK {KEY}: unsafe request"
    args = build_parser().parse_args(["agent", "reply", text])

    assert agent_cli.handle_agent(args) == 0

    assert not collect_inbox_items(str(repo))
    record = ack_state_records(repo)[0]
    assert record.disposition == ACK_DISPOSITION_REFUSED
    assert record.ack_content == "unsafe request"
    assert read_reply_records(repo, THREAD)[0]["nackKeys"] == [KEY]
    assert capsys.readouterr().out == f"nack {KEY}: refused\n"


def test_agent_reply_parses_mixed_polarities_once(tmp_path, monkeypatch, capsys):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_agent_thread_pointer(repo, THREAD)
    write_inbox_item(repo, f"{KEY}.txt", "do X")
    write_inbox_item(repo, f"{KEY_B}.txt", "do Y")
    real_parser = ackarchive.split_keyed_response
    parse_calls = 0

    def counted_parser(text):
        nonlocal parse_calls
        parse_calls += 1
        return real_parser(text)

    monkeypatch.setattr(ackarchive, "split_keyed_response", counted_parser)
    text = f"ACK {KEY}: shipped X\nNACK {KEY_B}: unsafe Y"
    args = build_parser().parse_args(["agent", "reply", text])

    assert agent_cli.handle_agent(args) == 0

    assert parse_calls == 1
    dispositions = {
        record.key: record.disposition for record in ack_state_records(repo)
    }
    assert dispositions == {
        KEY: ACK_DISPOSITION_ACKED,
        KEY_B: ACK_DISPOSITION_REFUSED,
    }
    record = read_reply_records(repo, THREAD)[0]
    assert record["ackKeys"] == [KEY]
    assert record["nackKeys"] == [KEY_B]
    assert capsys.readouterr().out.splitlines() == [
        f"ack {KEY}: retired",
        f"nack {KEY_B}: refused",
    ]


def test_agent_reply_canonicalizes_redundant_positional_key(
    tmp_path, monkeypatch, capsys
):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_agent_thread_pointer(repo, THREAD)
    write_inbox_item(repo, f"{KEY}.txt", "do X")
    response = f"ACK {KEY}: shipped without a visible prefix"
    args = build_parser().parse_args(["agent", "reply", KEY, response])

    assert agent_cli.handle_agent(args) == 0

    state = ack_state_records(repo)[0]
    assert state.ack_text == response
    record = read_reply_records(repo, THREAD)[0]
    assert record["text"] == response
    card = reply_card_message(
        "2026-07-06T01:00:00.000000Z#reply-card:0",
        0,
        "2026-07-06T01:00:00.000000Z",
        record["text"],
    )
    assert card.preamble_html == ""
    assert card.display_text == "Shipped without a visible prefix"
    assert capsys.readouterr().out == f"ack {KEY}: retired\n"


def test_agent_reply_preserves_non_key_preamble(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_agent_thread_pointer(repo, THREAD)
    write_inbox_item(repo, f"{KEY}.txt", "do X")
    response = f"Status update.\nACK {KEY}: shipped with context"
    args = build_parser().parse_args(["agent", "reply", response])

    assert agent_cli.handle_agent(args) == 0

    assert ack_state_records(repo)[0].ack_text == response
    assert read_reply_records(repo, THREAD)[0]["text"] == response


def test_reply_record_synthesizes_a_chip_bearing_card():
    # The reply text runs through the ordinary ACK-message builder, so the card
    # carries the acknowledgment key as a chip -- exactly like a prose ACK.
    card = reply_card_message(
        "2026-07-06T01:00:00.000000Z#reply-card:0",
        0,
        "2026-07-06T01:00:00.000000Z",
        f"ACK {KEY}: shipped the reply card",
    )
    assert card.kind == "reply"
    assert card.ack_keys == [KEY]
    assert card.ack_count == 1
    assert "reply card" in card.display_html.lower()


def test_reply_log_round_trips(tmp_path):
    _init_git_repo(tmp_path)
    append_reply_record(
        tmp_path,
        THREAD,
        timestamp="2026-07-06T01:00:00.000000Z",
        text=f"ACK {KEY}: done",
        ack_keys=[KEY],
        nack_keys=[],
    )
    records = read_reply_records(tmp_path, THREAD)
    assert records[0]["ackKeys"] == [KEY]
    assert records[0]["nackKeys"] == []


def test_reply_log_normalizes_legacy_redundant_key_prefix(tmp_path):
    _init_git_repo(tmp_path)
    path = reply_log_path(tmp_path, THREAD)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(
            {
                "timestamp": "2026-07-06T01:00:00.000000Z",
                "text": f"{KEY} ACK {KEY}: legacy reply",
                "ackKeys": [KEY],
                "nackKeys": [],
            }
        )
        + "\n",
        encoding="utf-8",
    )

    canonical = f"ACK {KEY}: legacy reply"
    assert read_reply_records(tmp_path, THREAD)[0]["text"] == canonical
    assert json.loads(path.read_text(encoding="utf-8"))["text"] == canonical


def test_agent_reply_ack_state_is_recorded_as_acknowledged(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    _init_git_repo(repo)
    monkeypatch.setattr(agent_cli, "require_repo_root", lambda: repo)
    write_inbox_item(repo, f"{KEY}.txt", "do X")
    args = build_parser().parse_args(
        ["agent", "reply", f"ACK {KEY}: durable acknowledgment"]
    )

    assert agent_cli.handle_agent(args) == 0

    record = ack_state_records(repo)[0]
    assert record.disposition == ACK_DISPOSITION_ACKED
    assert record.ack_content == "durable acknowledgment"
