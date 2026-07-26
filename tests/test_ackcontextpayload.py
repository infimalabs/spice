"""ACK-context payload completeness and retirement-order contracts."""

from pathlib import Path

from spice.mail.ackarchive import archive_ackd_inbox_items
from spice.mail.ackstate import (
    AckStateWrite,
    ack_state_database_path,
    record_acked_inbox_items,
)
from spice.mail.inbox import compose_inbox_text, inbox_item_key, write_inbox_item
from spice.serve.payload import message
from spice.serve.team.store import ServeTeamStore
from tests.test_messagepayload import (
    _State,
    _Target,
    _init_repo,
    _message,
    _stub_messages_payload,
)

ACK_CONTEXT_BACKLOG_DEPTH = 60


def _isolated_state(tmp_path: Path) -> _State:
    return _State(
        team_store=ServeTeamStore(
            path=tmp_path / "teams.sqlite3",
            directive_state_path=ack_state_database_path(tmp_path),
        )
    )


def test_messages_payload_finds_ack_context_outside_recent_archive_window(
    monkeypatch, tmp_path
):
    _init_repo(tmp_path)
    oldest_key = "context-000"
    record_acked_inbox_items(
        tmp_path,
        [
            AckStateWrite(
                key=f"context-{index:03d}",
                inbox_name=f"context-{index:03d}.txt",
                text=compose_inbox_text(
                    body=f"operator context {index}",
                    priority=None,
                    stop=False,
                ),
            )
            for index in range(ACK_CONTEXT_BACKLOG_DEPTH)
        ],
        now=1_767_225_600.0,
    )
    _stub_messages_payload(
        monkeypatch,
        [
            _message(
                "2026-01-04T00:00:01.000000Z",
                ack_count=1,
                ack_keys=[oldest_key],
            )
        ],
    )

    payload = message.messages_payload_for_worktree(
        _isolated_state(tmp_path),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    assert payload["ackContexts"] == [
        {
            "key": oldest_key,
            "found": True,
            "text": "operator context 0",
            "html": "<p>operator context 0</p>",
            "priority": "",
            "disposition": "acked",
            "attachments": [],
        }
    ]


def test_messages_payload_closes_pending_to_consumed_hydration_gap(
    monkeypatch, tmp_path
):
    _init_repo(tmp_path)
    name = "1jNmXPHr.txt"
    key = inbox_item_key(name)
    write_inbox_item(
        tmp_path,
        name,
        compose_inbox_text(
            body="context crossing durable retirement",
            priority=None,
            stop=False,
        ),
    )
    _stub_messages_payload(
        monkeypatch,
        [_message("2026-01-04T00:00:01.000000Z", ack_count=1, ack_keys=[key])],
    )
    collect_pending = message.collect_inbox_items

    def archive_before_pending_snapshot(repo_root):
        archive_ackd_inbox_items(repo_root, [key])
        return collect_pending(repo_root)

    monkeypatch.setattr(
        message,
        "collect_inbox_items",
        archive_before_pending_snapshot,
    )

    payload = message.messages_payload_for_worktree(
        _isolated_state(tmp_path),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    assert payload["ackContexts"][0] == {
        "key": key,
        "found": True,
        "text": "context crossing durable retirement",
        "html": "<p>context crossing durable retirement</p>",
        "priority": "",
        "disposition": "acked",
        "attachments": [],
    }
