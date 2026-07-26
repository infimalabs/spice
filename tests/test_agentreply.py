"""spice agent reply: retire steering and synthesize one lane card per submission."""

import io

from spice.agent import cli as agent_cli
from spice.agent.paths import write_agent_thread_pointer
from spice.cli.parser import build_parser
from spice.mail.inbox import collect_inbox_items, write_inbox_item
from spice.mail.replies import append_reply_record, read_reply_records
from spice.serve import messages as message_reader
from tests.test_reposcaffolding import init_identified_repo as _init_git_repo

THREAD = "f2249a9fb99641e29e1854cb381cc634"
KEY = "1jNmXPHm"


def test_agent_reply_retires_key_and_logs_a_reply_card(tmp_path, monkeypatch):
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


def test_reply_record_synthesizes_a_chip_bearing_card():
    # The reply text runs through the ordinary ACK-message builder, so the card
    # carries the acknowledgment key as a chip -- exactly like a prose ACK.
    card = message_reader.reply_card_message(
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
