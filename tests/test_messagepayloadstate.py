"""Message payload cursor, state, renewal, attachment, and inbox tests."""

from pathlib import Path

import pytest

from spice.agent.driver import CLAUDE_DRIVER
from spice.agent.renewal import RENEWAL_HANDOFF_REQUEST_SUFFIX
from spice.mail.ackarchive import archive_ackd_inbox_items
from spice.mail.attachments import prepare_inbox_attachments
from spice.mail.inbox import compose_inbox_text, inbox_item_key, write_inbox_item
from spice.paths import shared_attachment_root
from spice.serve import lifecycle
from spice.serve import messages as message_reader
from spice.serve.agentapi import sent_steering_payload
from spice.serve.payload import identity, lane, message
from spice.serve.steering import submit_steering_message
from spice.serve.team.store import ServeTeamStore
from spice.serve.worktree import inventory
from spice.transcript.timestamps import parse_timestamp
from tests.test_messagepayload import (
    IMAGE_DATA_URL,
    _EmptyOpenTaskBoard,
    _State,
    _Status,
    _Target,
    _identity_status,
    _init_repo,
    _message,
    _message_read,
    _pending_identity,
    _record_identity,
    _stub_messages_payload,
    _task_board,
)


@pytest.fixture(autouse=True)
def _stub_open_task_board(monkeypatch):
    monkeypatch.setattr(
        message,
        "open_task_board_projection",
        lambda: _EmptyOpenTaskBoard(),
    )


def test_messages_payload_after_cursor_preserves_transcript_delta(
    tmp_path, monkeypatch
):
    actor = "a" * 32
    row = {
        "id": 43,
        "uuid": "newer-task",
        "incepted": "1k4Yh6Ps",
        "description": "Later CLI follow-up",
        "project": "serve.ui",
        "origin_thread": actor,
        "creation_surface": "cli",
        "status": "pending",
    }
    boundary_key = "2026-06-10T12:00:01.001000Z#task-card:older-task"
    seen: dict[str, object] = {}

    def fake_messages(_thread_id: str, **kwargs) -> message_reader.AssistantMessageRead:
        seen["reader_kwargs"] = kwargs
        return _message_read([_message("2026-06-10T12:00:03.000000Z")])

    projection = _task_board([row])
    monkeypatch.setattr(message, "open_task_board_projection", lambda: projection)
    monkeypatch.setattr(
        message,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    monkeypatch.setattr(
        inventory,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_available_work",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        message, "resolve_thread_id_for_target", lambda _state, _target: actor
    )
    monkeypatch.setattr(
        message,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id=actor,
        ),
    )
    monkeypatch.setattr(
        identity,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id=actor,
        ),
    )
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id=actor,
        ),
    )
    monkeypatch.setattr(message, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(lane, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        fake_messages,
    )

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
        after=boundary_key,
    )

    assert seen["reader_kwargs"]["after"] == boundary_key
    assert [item["display_text"] for item in payload["messages"]] == [
        "hello",
        "Task capture: Later CLI follow-up (serve.ui)",
    ]
    assert payload["messages"][0]["timestamp"] == "2026-06-10T12:00:03.000000Z"
    assert payload["messages"][1]["source_kind"] == "cli_task_created"


def test_cli_review_followup_row_renders_standalone_task_card(monkeypatch):
    actor = "a" * 32
    row = {
        "id": 43,
        "uuid": "review-followup-43",
        "incepted": "1k4Yh6n5",
        "description": "CLI review follow-up",
        "project": "serve.ui",
        "acceptance": "Review follow-up appears as a card",
        "origin_thread": actor,
        "creation_surface": "cli",
        "depends": ["reviewed-task-uuid"],
        "status": "pending",
    }
    cards = message._task_card_messages_for_thread(
        actor,
        after=None,
        before=None,
        task_board=_task_board([row]),
    )
    assert len(cards) == 1
    card = cards[0]
    assert card.kind == "task_card"
    assert card.source_kind == "cli_task_created"
    assert card.display_text == "Task capture: CLI review follow-up (serve.ui)"
    assert '<blockquote class="task-directive-quote">' in card.display_html
    assert "<dt>title</dt><dd>CLI review follow-up</dd>" in card.display_html
    assert (
        "<dt>acceptance</dt><dd>Review follow-up appears as a card</dd>"
        in card.display_html
    )


def test_task_card_cursor_keeps_append_window_to_transcript_items(monkeypatch):
    actor = "a" * 32
    rows = [
        {
            "id": 1,
            "uuid": "older-task",
            "incepted": "1k4Yh62d",
            "description": "Older CLI follow-up",
            "project": "serve.ui",
            "origin_thread": actor,
            "creation_surface": "cli",
        },
        {
            "id": 2,
            "uuid": "newer-task",
            "incepted": "1k4Yh6Ps",
            "description": "Later CLI follow-up",
            "project": "serve.ui",
            "origin_thread": actor,
            "creation_surface": "cli",
        },
    ]
    boundary_key = "2026-06-10T12:00:01.001000Z#task-card:older-task"

    merged = message._merge_task_card_messages(
        actor,
        [_message("2026-06-10T12:00:03.000000Z")],
        limit=5,
        after=boundary_key,
        task_board=_task_board(rows),
    )

    assert [item.display_text for item in merged] == [
        "hello",
        "Task capture: Later CLI follow-up (serve.ui)",
    ]
    boundary = parse_timestamp("2026-06-10T12:00:01.001000Z")
    assert boundary is not None
    assert all(
        (timestamp := parse_timestamp(item.timestamp)) is not None
        and timestamp > boundary
        for item in merged
    )


def test_task_card_tail_merge_drops_cards_older_than_visible_window(monkeypatch):
    actor = "a" * 32
    rows = [
        {
            "id": 1,
            "uuid": "stale-task",
            "incepted": "1k4VkTVn",
            "description": "Stale CLI follow-up",
            "project": "serve.docs",
            "origin_thread": actor,
            "creation_surface": "cli",
        },
        {
            "id": 2,
            "uuid": "fresh-task",
            "incepted": "1k4Yh62d",
            "description": "Fresh CLI follow-up",
            "project": "serve.ui",
            "origin_thread": actor,
            "creation_surface": "cli",
        },
    ]
    merged = message._merge_task_card_messages(
        actor,
        [_message("2026-06-10T12:00:00.000000Z")],
        limit=5,
        task_board=_task_board(rows),
    )

    assert [item.display_text for item in merged] == [
        "Task capture: Fresh CLI follow-up (serve.ui)",
        "hello",
    ]
    assert all("Stale CLI follow-up" not in item.display_text for item in merged)


def test_messages_payload_reports_transcript_owner_in_serve_identity(
    tmp_path, monkeypatch
):
    thread_id = "agent-a"
    transcript = message_reader.TranscriptResolution(
        thread_id=thread_id,
        path=tmp_path / "claude.jsonl",
        owner_driver=CLAUDE_DRIVER,
    )
    monkeypatch.setattr(
        identity,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "desired-model", "effort": "high"},
    )
    monkeypatch.setattr(
        message,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    monkeypatch.setattr(
        inventory,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_available_work",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        message,
        "resolve_thread_id_for_target",
        lambda _state, _target: thread_id,
    )
    monkeypatch.setattr(
        message,
        "agent_status",
        lambda _repo: _identity_status(
            tmp_path,
            driver="claude",
            thread_id=thread_id,
            model="actual-model",
            effort="low",
        ),
    )
    monkeypatch.setattr(
        identity,
        "agent_status",
        lambda _repo: _identity_status(
            tmp_path,
            driver="claude",
            thread_id=thread_id,
            model="actual-model",
            effort="low",
        ),
    )
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _identity_status(
            tmp_path,
            driver="claude",
            thread_id=thread_id,
            model="actual-model",
            effort="low",
        ),
    )
    monkeypatch.setattr(message, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(lane, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: _message_read(transcript=transcript),
    )

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    assert payload["serveAgentIdentity"]["driver"]["transcriptOwner"] == "claude"
    assert payload["serveAgentIdentity"]["driver"]["actual"] == "claude"
    assert payload["serveAgentIdentity"]["driver"]["desired"] == "codex"
    assert payload["laneInfo"]["summaryRows"][:7] == [
        {"key": "agent", "value": "-", "span": False},
        {"key": "driver actual", "value": "claude", "span": False},
        {"key": "driver desired", "value": "codex", "span": False},
        {"key": "model actual", "value": "actual-model", "span": False},
        {"key": "model desired", "value": "desired-model", "span": False},
        {"key": "effort actual", "value": "low", "span": False},
        {"key": "effort desired", "value": "high", "span": False},
    ]
    assert {"key": "session", "value": "claude", "span": False} in payload["laneInfo"][
        "summaryRows"
    ]


def test_messages_payload_reports_agent_renewal_intent(monkeypatch, tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.create_team(members=["thread:agent-a"])
    _record_identity(store, "thread:agent-a", thread_id="agent-a")
    store.set_agent_renewal_request("thread:agent-a", requested=True)
    monkeypatch.setattr(
        message,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    monkeypatch.setattr(
        inventory,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_available_work",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        message,
        "resolve_thread_id_for_target",
        lambda _state, _target: "agent-a",
    )
    monkeypatch.setattr(
        message,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id="agent-a",
        ),
    )
    monkeypatch.setattr(
        identity,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id="agent-a",
        ),
    )
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id="agent-a",
        ),
    )
    monkeypatch.setattr(lane, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(
        message.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: _message_read(),
    )

    payload = message.messages_payload_for_worktree(
        _State(team_store=store),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    assert payload["renewalIntent"]["agentId"] == "thread:agent-a"
    assert payload["renewalIntent"]["requested"] is True
    assert payload["renewalIntent"]["state"] == "requested"
    assert payload["renewalIntent"]["teamSlot"] == 0
    assert payload["renewalIntent"]["predecessorIdentity"]["threadId"] == "agent-a"
    assert payload["renewalIntent"]["successorIdentity"]["desiredModel"] == (
        "desired-model"
    )


def test_sent_steering_payload_includes_image_attachments(tmp_path):
    _init_repo(tmp_path)
    sent = submit_steering_message(
        text="inspect this",
        priority=None,
        stop=False,
        attachments=[
            {
                "name": "paste.png",
                "contentType": "image/png",
                "dataUrl": IMAGE_DATA_URL,
            }
        ],
        target_repo_root=tmp_path,
    )

    payload = sent_steering_payload(sent, target=_Target(id="wt", repo_root=tmp_path))

    assert payload["attachments"][0]["name"] == "paste.png"
    assert payload["attachments"][0]["contentType"] == "image/png"
    attachment_path = Path(payload["attachments"][0]["path"])
    assert attachment_path.is_absolute()
    assert shared_attachment_root(tmp_path) in attachment_path.parents
    assert payload["attachments"][0]["url"].startswith(
        "/api/work/trees/wt/files/image?path="
    )


def test_messages_payload_round_trips_ack_context_attachments(monkeypatch, tmp_path):
    _init_repo(tmp_path)
    name = "1jNmXPHm.txt"
    key = inbox_item_key(name)
    composed = compose_inbox_text(
        body=f"look here\n{RENEWAL_HANDOFF_REQUEST_SUFFIX}",
        priority=None,
        stop=False,
    )
    attachments = prepare_inbox_attachments(
        [
            {
                "name": "upload.png",
                "contentType": "image/png",
                "dataUrl": IMAGE_DATA_URL,
            }
        ]
    )
    write_inbox_item(tmp_path, name, composed, attachments=attachments)
    _stub_messages_payload(
        monkeypatch,
        [_message("2026-01-04T00:00:01.000000Z", ack_count=1, ack_keys=[key])],
    )

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    context = payload["ackContexts"][0]
    attachment = context["attachments"][0]
    assert context["key"] == key
    assert context["found"] is True
    assert context["text"] == "look here"
    assert context["html"] == "<p>look here</p>"
    assert attachment["name"] == "upload.png"
    assert attachment["contentType"] == "image/png"
    attachment_path = Path(attachment["path"])
    assert attachment_path.is_absolute()
    assert shared_attachment_root(tmp_path) in attachment_path.parents
    assert attachment["url"].startswith("/api/work/trees/wt/files/image?path=")


def test_messages_payload_reports_inbox_status_without_streaming_requests(
    monkeypatch, tmp_path
):
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    pending_name = "1jNmXPHp.txt"
    archived_name = "1jNmXPHq.txt"
    write_inbox_item(
        repo,
        pending_name,
        compose_inbox_text(body="pending request", priority="urgent", stop=False),
    )
    write_inbox_item(
        repo,
        archived_name,
        compose_inbox_text(body="archived request", priority=None, stop=False),
    )
    archive_ackd_inbox_items(repo, [inbox_item_key(archived_name)])
    monkeypatch.setattr(
        message, "resolve_thread_id_for_target", lambda _state, _target: ""
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        lifecycle,
        "ensure_agent_for_available_work",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        message,
        "agent_status",
        lambda _repo: _Status(running=False, started_at=""),
    )

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=repo),
        limit=5,
    )
    assert set(payload) == {
        "messages",
        "ackContexts",
        "targetWorktreeName",
        "targetBranch",
        "targetIdentity",
        "serveAgentIdentity",
        "taskFilters",
        "taskFilterEntries",
        "effectiveTaskFilters",
        "laneFilterVersion",
        "teamIdentity",
        "lifetime",
        "renewalIntent",
        "taskFilterInventory",
        "laneInfo",
        "agentProcessStatus",
        "error",
        "pendingInboxCount",
        "pendingInboxLabel",
        "pendingInboxKeys",
        "pendingInboxRevision",
        "pendingInboxVersion",
        "agentEnsure",
        "statusLine",
        "chrome",
    }
    assert payload["messages"] == []
    assert payload["targetIdentity"]["thread"] == {"state": "unbound"}
    assert payload["targetIdentity"]["agent"] == {"state": "unconfigured"}
    assert payload["serveAgentIdentity"]["actorId"] == "target:wt"
    assert payload["serveAgentIdentity"]["renewal"]["revision"] == 0
    assert payload["teamIdentity"] == {"state": "none"}
    assert payload["pendingInboxCount"] == 1
    assert payload["pendingInboxLabel"] == "1"
    assert payload["pendingInboxKeys"] == [inbox_item_key(pending_name)]
    assert payload["pendingInboxRevision"]
    assert payload["pendingInboxVersion"] > 0
    assert payload["statusLine"]["pendingInboxCount"] == 1
    assert payload["statusLine"]["pendingInboxLabel"] == "1"
    assert payload["statusLine"]["pendingInboxKeys"] == [inbox_item_key(pending_name)]
    assert (
        payload["statusLine"]["pendingInboxRevision"] == payload["pendingInboxRevision"]
    )
    assert (
        payload["statusLine"]["pendingInboxVersion"] == payload["pendingInboxVersion"]
    )


def test_messages_payload_finds_ack_context_by_collision_suffixed_key(
    monkeypatch, tmp_path
):
    _init_repo(tmp_path)
    name = "1jNmXPHn-2.txt"
    suffixed_key = "1jNmXPHn-2"
    composed = compose_inbox_text(body="operator original", priority=None, stop=False)
    write_inbox_item(tmp_path, name, composed)
    archive_ackd_inbox_items(tmp_path, [suffixed_key])
    _stub_messages_payload(
        monkeypatch,
        [
            _message(
                "2026-01-04T00:00:01.000000Z",
                ack_count=1,
                ack_keys=[suffixed_key],
            )
        ],
    )

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    assert payload["ackContexts"][0]["key"] == suffixed_key
    assert payload["ackContexts"][0]["found"] is True
    assert payload["ackContexts"][0]["text"] == "operator original"


def test_messages_payload_does_not_quote_assistant_ack_when_inbox_missing(
    monkeypatch, tmp_path
):
    _init_repo(tmp_path)
    key = "1jNmXPHn"
    _stub_messages_payload(
        monkeypatch,
        [_message("2026-01-04T00:00:01.000000Z", ack_count=1, ack_keys=[key])],
    )

    payload = message.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    assert payload["ackContexts"] == [{"key": key, "found": False}]
