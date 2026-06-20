"""Lane metrics: sparkline buckets, uptime, and counter assembly."""

from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
import json
from pathlib import Path
import subprocess
from types import SimpleNamespace

import pytest

from spice.agent.driver import CLAUDE_DRIVER
from spice.errors import SpiceError
from spice.agent.renewal import RENEWAL_HANDOFF_REQUEST_SUFFIX
from spice.mail.acks import archive_ackd_inbox_items
from spice.mail.attachments import prepare_inbox_attachments
from spice.mail.inbox import compose_inbox_text, inbox_item_key, write_inbox_item
from spice.paths import shared_attachment_root
from spice.serve.agentapi import sent_steering_payload
from spice.serve.messages import AssistantMessage
from spice.serve import messages as message_reader
from spice.serve import payloads
from spice.serve.payloads import (
    LANE_METRIC_SPARKLINE_BUCKET_SECONDS,
    LANE_METRIC_SPARKLINE_BUCKETS,
    _agent_uptime_seconds,
    ack_context_payload_for_worktree,
    _message_sparkline,
    lane_metrics_payload,
    task_filter_inventory,
)
from spice.serve.steering import submit_steering_message
from spice.serve.teams import ServeTeamStore
from spice.tasks import tw

IMAGE_DATA_URL = "data:image/png;base64,aW1hZ2UtYnl0ZXM="


def _record_identity(
    store: ServeTeamStore,
    actor_id: str,
    *,
    target_id: str = "wt",
    thread_id: str = "",
) -> None:
    store.record_agent_identity(
        actor_id=actor_id,
        target_id=target_id,
        thread_id=thread_id or actor_id.removeprefix("thread:"),
        actual_driver="codex",
        actual_model="actual-model",
        actual_effort="low",
        actual_service_tier="fast",
        desired_driver="codex",
        desired_model="desired-model",
        desired_effort="high",
        transcript_owner="codex",
    )


def _message(
    timestamp: str,
    *,
    kind: str = "assistant",
    ack_count: int = 0,
    preview: str = "",
):
    return AssistantMessage(
        key=f"{timestamp}#0",
        index=0,
        timestamp=timestamp,
        text="hello",
        display_text="hello",
        display_html="<p>hello</p>",
        ack_count=ack_count,
        ack_keys=[],
        ack_utterances=[],
        kind=kind,
        preview=preview,
    )


def _message_read(
    items: list[AssistantMessage] | None = None,
    *,
    error: str | None = None,
    transcript: message_reader.TranscriptResolution | None = None,
) -> message_reader.AssistantMessageRead:
    return message_reader.AssistantMessageRead(
        items=items or [],
        error=error,
        transcript=transcript,
    )


@dataclass(frozen=True)
class _Status:
    running: bool
    started_at: str
    process_status: str = "idle"
    thread_id: str = ""
    model: str = ""
    reasoning_effort: str = ""
    service_tier: str = ""
    state_path: Path | None = None


@dataclass(frozen=True)
class _Target:
    id: str
    repo_root: Path | None = None
    name: str = "repo"
    display_name: str = "repo"
    branch: str = "main"


class _State:
    def __init__(
        self, sends: int = 0, team_store: ServeTeamStore | None = None
    ) -> None:
        self._sends = sends
        self.team_store = team_store or ServeTeamStore()
        self.pending_agent_ensure_attempts: dict[str, float] = {}

    def lane_send_count(self, target_id: str) -> int:
        return self._sends

    def rollout_cursor(self, thread_id: str):
        return None


class _InventoryState(_State):
    def __init__(self, target: _Target) -> None:
        super().__init__()
        self._target = target

    def worktree_targets(self) -> list[_Target]:
        return [self._target]


def test_target_identity_payload_rejects_blank_bound_thread_id():
    with pytest.raises(SpiceError, match="thread id must be non-empty"):
        payloads.target_identity_payload(
            _Target(id="wt"),
            "",
            binding_status="bound",
        )


def test_target_identity_payload_reports_configured_driver(tmp_path, monkeypatch):
    from spice.agent.driver import SPICE_AGENT_DRIVER_ENV
    from spice.config import update_section

    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    update_section(
        repo,
        "agent",
        {"driver": "claude", "model": "claude-sonnet-4-5", "effort": "medium"},
    )
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)

    identity = payloads.target_identity_payload(
        _Target(id="wt", repo_root=repo),
        "",
        binding_status="unbound",
    )

    assert identity["driver"] == {
        "name": "claude",
        "model": "claude-sonnet-4-5",
        "effort": "medium",
    }
    target = _Target(id="wt", repo_root=repo)
    serve_identity = payloads.serve_agent_identity_payload(
        target,
        "",
        binding_status="unbound",
    )
    rows = {
        row["key"]: row["value"]
        for row in payloads._lane_info_payload(target, serve_identity)["summaryRows"]
    }
    assert rows["driver"] == "claude"
    assert rows["model"] == "claude-sonnet-4-5"
    assert rows["effort"] == "medium"


def test_serve_agent_identity_reports_unbound_target_identity(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    target = _Target(id="wt", repo_root=repo, name="repo", branch="main")
    monkeypatch.setattr(
        payloads,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "gpt-5.5", "effort": "xhigh"},
    )
    monkeypatch.setattr(
        payloads,
        "agent_status",
        lambda _repo: _identity_status(repo),
    )

    identity = payloads.serve_agent_identity_payload(target)

    assert identity["actorId"] == "target:wt"
    assert identity["target"] == {
        "id": "wt",
        "worktreeName": "repo",
        "repoRoot": str(repo),
        "branch": "main",
    }
    assert identity["thread"] == {"state": "unbound"}
    assert identity["driver"] == {
        "desired": "codex",
        "actual": "",
        "transcriptOwner": "",
    }
    assert identity["launch"]["desired"] == {
        "model": "gpt-5.5",
        "effort": "xhigh",
        "source": "effective agent config",
    }
    assert identity["launch"]["actual"] == {
        "model": "",
        "effort": "",
        "serviceTier": "",
        "source": "",
    }
    assert identity["renewal"] == {
        "state": "none",
        "teamIndex": None,
        "ancestorThreadId": "",
        "successorThreadId": "",
        "revision": 0,
    }


def test_serve_agent_identity_splits_actual_and_desired_launch(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    target = _Target(id="wt", repo_root=repo, name="repo", branch="main")
    monkeypatch.setattr(
        payloads,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "desired-model", "effort": "high"},
    )
    monkeypatch.setattr(
        payloads,
        "agent_status",
        lambda _repo: _identity_status(
            repo,
            driver="claude",
            thread_id="thread-a",
            model="actual-model",
            effort="low",
            service_tier="fast",
            started_at="2026-06-20T04:00:00Z",
        ),
    )

    store = ServeTeamStore(tmp_path / "teams.sqlite")

    identity = payloads.serve_agent_identity_payload(
        target,
        transcript_owner="claude",
        store=store,
    )
    stored = store.agent_identity_for_actor("thread:thread-a")

    assert identity["actorId"] == "thread:thread-a"
    assert identity["thread"] == {"state": "bound", "threadId": "thread-a"}
    assert identity["driver"] == {
        "desired": "codex",
        "actual": "claude",
        "transcriptOwner": "claude",
    }
    assert identity["launch"]["desired"]["model"] == "desired-model"
    assert identity["launch"]["actual"] == {
        "model": "actual-model",
        "effort": "low",
        "serviceTier": "fast",
        "source": "agent state",
    }
    assert stored is not None
    assert stored.actor_id == "thread:thread-a"
    assert stored.target_id == "wt"
    assert stored.thread_id == "thread-a"
    assert stored.actual_driver == "claude"
    assert stored.actual_model == "actual-model"
    assert stored.actual_effort == "low"
    assert stored.actual_service_tier == "fast"
    assert stored.desired_driver == "codex"
    assert stored.desired_model == "desired-model"
    assert stored.desired_effort == "high"
    assert stored.transcript_owner == "claude"


def test_serve_agent_identity_normalizes_legacy_bare_actor_and_renewal(
    tmp_path, monkeypatch
):
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    store = ServeTeamStore(tmp_path / "teams.sqlite")
    store.create_team(members=["thread-a", "thread-b"])
    _record_identity(store, "thread-a", thread_id="thread-a")
    renewal = store.record_pending_renewal(
        agent_id="thread-a", ancestor_thread_id="thread-a"
    )
    target = _Target(id="wt", repo_root=repo, name="repo", branch="main")
    monkeypatch.setattr(
        payloads,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "gpt-5.5", "effort": "xhigh"},
    )
    monkeypatch.setattr(
        payloads,
        "agent_status",
        lambda _repo: _identity_status(repo, thread_id="thread-a"),
    )

    identity = payloads.serve_agent_identity_payload(
        target,
        actor_id="thread-a",
        store=store,
    )

    assert identity["actorId"] == "thread:thread-a"
    assert identity["renewal"] == {
        "state": "pending",
        "teamIndex": 0,
        "ancestorThreadId": "thread-a",
        "successorThreadId": "",
        "revision": renewal.revision,
    }
    assert store.current_team_for_agent("thread-a") is None
    assert store.current_team_for_agent("thread:thread-a") is not None
    assert (
        payloads.serve_agent_identity_payload(target, actor_id="wt")["actorId"]
        == "target:wt"
    )


def test_team_identity_payload_rejects_missing_member_revisions():
    with pytest.raises(SpiceError, match="team revision is required"):
        payloads.team_identity_payload({"teamId": "team-1"})


def _stamp(when: datetime) -> str:
    return when.isoformat(timespec="microseconds").replace("+00:00", "Z")


def _write_response_item(
    path: Path, timestamp: str, payload: dict[str, object]
) -> None:
    path.write_text(
        json.dumps(
            {"timestamp": timestamp, "type": "response_item", "payload": payload},
            separators=(",", ":"),
        )
        + "\n",
        encoding="utf-8",
    )


def _pending_identity(count: int = 0) -> dict[str, object]:
    return {
        "pendingInboxCount": count,
        "pendingInboxLabel": str(count),
        "pendingInboxKeys": [],
        "pendingInboxRevision": f"test-revision-{count}",
    }


def _init_repo(path: Path) -> None:
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=path, check=True)


def _identity_status(
    repo: Path,
    *,
    driver: str = "codex",
    thread_id: str = "",
    model: str = "",
    effort: str = "",
    service_tier: str = "",
    started_at: str = "",
) -> SimpleNamespace:
    return SimpleNamespace(
        running=bool(thread_id),
        process_status="running" if thread_id else "idle",
        thread_id=thread_id,
        model=model,
        reasoning_effort=effort,
        service_tier=service_tier,
        started_at=started_at,
        state_path=repo / ".git" / "spice" / "agents" / driver / "state.json",
    )


def test_sparkline_buckets_messages_by_minute():
    latest = datetime(2026, 6, 10, 12, 0, tzinfo=UTC)
    one_bucket_ago = latest - timedelta(seconds=LANE_METRIC_SPARKLINE_BUCKET_SECONDS)
    items = [_message(_stamp(latest)), _message(_stamp(one_bucket_ago))]
    sparkline = _message_sparkline(items)
    assert len(sparkline) == LANE_METRIC_SPARKLINE_BUCKETS
    assert sparkline[-1] == 1
    assert sparkline[-2] == 1
    assert sum(sparkline) == len(items)


def test_sparkline_clamps_old_messages_into_first_bucket():
    latest = datetime(2026, 6, 10, 12, 0, tzinfo=UTC)
    ancient = latest - timedelta(hours=2)
    sparkline = _message_sparkline(
        [_message(_stamp(latest)), _message(_stamp(ancient))]
    )
    assert sparkline[0] == 1
    assert sparkline[-1] == 1


FIVE_MINUTES_SECONDS = 300


def test_uptime_measures_started_at_to_latest_message():
    started = datetime(2026, 6, 10, 12, 0, tzinfo=UTC)
    latest = started + timedelta(minutes=5)
    status = _Status(running=True, started_at=_stamp(started))
    uptime = _agent_uptime_seconds(status, [_message(_stamp(latest))])
    assert uptime == FIVE_MINUTES_SECONDS


def test_uptime_reads_zero_while_agent_is_off():
    status = _Status(running=False, started_at="2026-06-10T12:00:00.000000Z")
    assert _agent_uptime_seconds(status, []) == 0


def test_status_line_pairs_activity_preview_with_activity_timestamp(
    tmp_path, monkeypatch
):
    latest = _stamp(datetime(2026, 6, 10, 12, 0, tzinfo=UTC))
    target = _Target(id="wt", repo_root=tmp_path)
    items = [_message(latest, kind="presence:reasoning", preview="thinking")]
    monkeypatch.setattr(
        payloads,
        "agent_status",
        lambda _repo: _Status(running=True, started_at="", process_status="running"),
    )
    monkeypatch.setattr(
        payloads, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )

    line = payloads.status_line_payload(_State(), target, items=items, error=None)

    assert line["lastAssistantAt"] == latest
    assert line["preview"] == "thinking"
    assert line["latestActivityPreview"] == "thinking"
    assert line["latestMessagePreview"] == ""


def test_inline_task_directive_renders_quote_like_block_in_message(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": (
                        "Queued the follow-up.\n"
                        "TASK title=Inline follow-up | project=task.unit | "
                        "acceptance=Tracked from UI\n"
                        "Continuing."
                    ),
                }
            ],
        },
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)

    assert len(items) == 1
    item = items[0]
    assert item.kind == "assistant"
    assert item.task_card_count == 1
    assert item.to_payload()["task_card_count"] == 1
    assert item.display_text == (
        "Queued the follow-up.\nTask capture: Inline follow-up (task.unit)\nContinuing."
    )
    assert "TASK title" not in item.display_text
    assert "TASK title" not in item.display_html
    assert '<blockquote class="task-directive-quote">' in item.display_html
    assert '<div class="task-directive-kicker">Task capture</div>' in item.display_html
    assert "<dt>title</dt><dd>Inline follow-up</dd>" in item.display_html
    assert "<dt>project</dt><dd>task.unit</dd>" in item.display_html
    assert "<dt>acceptance</dt><dd>Tracked from UI</dd>" in item.display_html


def test_malformed_task_like_progress_update_remains_plain_message(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    text = (
        "TASK badges now use the plum task accent with the count after the label. "
        "I am validating the focused tests next."
    )
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        },
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)

    assert len(items) == 1
    item = items[0]
    payload = item.to_payload()
    expected_html = f"<p>{text}</p>"
    assert item.task_card_count == 0
    assert payload["task_card_count"] == item.task_card_count
    assert item.display_text == text
    assert item.display_html == expected_html
    assert payload["display_text"] == text
    assert payload["display_html"] == expected_html
    assert payload["preview"] == text
    assert payload["text"] == text


def test_inline_task_directive_renders_inside_ack_segment_at_written_position(
    tmp_path,
):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": (
                        "ACK 20260610T115900000000Z: captured.\n"
                        "TASK: title=ACK follow-up | project=serve.ui | "
                        "acceptance=Inline block appears\n"
                        "Continuing."
                    ),
                }
            ],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]
    segment_html = item.ack_segments[0]["html"]

    assert item.ack_count == 1
    assert item.task_card_count == 1
    assert item.ack_utterances == ["captured.\nContinuing."]
    assert item.display_text == (
        "Captured.\nTask capture: ACK follow-up (serve.ui)\nContinuing."
    )
    assert "TASK:" not in segment_html
    assert segment_html.index("<p>Captured.</p>") < segment_html.index(
        '<blockquote class="task-directive-quote">'
    )
    assert segment_html.index('<blockquote class="task-directive-quote">') < (
        segment_html.index("<p>Continuing.</p>")
    )
    assert "<dt>title</dt><dd>ACK follow-up</dd>" in segment_html
    assert "<dt>project</dt><dd>serve.ui</dd>" in segment_html


def test_inline_task_directive_counts_multiple_task_cards(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 11, 59, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": (
                        "TASK title=First follow-up | project=serve.ui | "
                        "acceptance=First card\n"
                        "TASK title=Second follow-up | project=task.unit | "
                        "acceptance=Second card"
                    ),
                }
            ],
        },
    )

    item = message_reader.read_assistant_messages(transcript, limit=5)[0]

    assert item.task_card_count == 2
    assert item.to_payload()["task_card_count"] == 2
    assert item.display_html.count('class="task-directive-quote"') == 2


def test_cli_created_task_row_renders_standalone_task_card(tmp_path, monkeypatch):
    actor = "a" * 32
    row = {
        "id": 42,
        "uuid": "task-uuid-42",
        "incepted": "20260610T120001000001Z",
        "description": "CLI follow-up",
        "project": "serve.ui",
        "acceptance": "Task card comes from the backend",
        "origin_thread": actor,
        "creation_surface": "cli",
        "status": "pending",
    }
    seen: dict[str, object] = {}

    def fake_export(filters: list[str] | None = None) -> list[dict[str, object]]:
        if filters and "creation_surface.is:cli" in filters:
            seen["filters"] = filters
            return [row]
        return []

    monkeypatch.setattr(payloads.tw, "export", fake_export)
    monkeypatch.setattr(payloads, "task_filter_inventory", lambda: {})
    monkeypatch.setattr(
        payloads, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )
    monkeypatch.setattr(
        payloads,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        payloads, "resolve_thread_id_for_target", lambda _state, _target: actor
    )
    monkeypatch.setattr(
        payloads,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id=actor,
        ),
    )
    monkeypatch.setattr(payloads, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(
        payloads.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: _message_read(),
    )

    payload = payloads.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=tmp_path),
        limit=5,
    )

    assert seen["filters"] == [
        "status.any:",
        "creation_surface.is:cli",
        f"origin_thread.is:{actor}",
    ]
    item = payload["messages"][0]
    assert item["kind"] == "task_card"
    assert item["source_kind"] == "cli_task_created"
    assert item["task_card_count"] == 1
    assert item["timestamp"] == "2026-06-10T12:00:01.000001Z"
    assert item["display_text"] == "Task capture: CLI follow-up (serve.ui)"
    assert item["preview"] == "Task capture: CLI follow-up (serve.ui)"
    assert '<blockquote class="task-directive-quote">' in item["display_html"]
    assert (
        '<div class="task-directive-kicker">Task capture</div>' in item["display_html"]
    )
    assert "<dt>title</dt><dd>CLI follow-up</dd>" in item["display_html"]
    assert "<dt>project</dt><dd>serve.ui</dd>" in item["display_html"]
    assert (
        "<dt>acceptance</dt><dd>Task card comes from the backend</dd>"
        in item["display_html"]
    )
    assert "<dt>handle</dt><dd>UI-20260610T120001000001Z</dd>" in item["display_html"]


def test_cli_review_followup_row_renders_standalone_task_card(monkeypatch):
    actor = "a" * 32
    row = {
        "id": 43,
        "uuid": "review-followup-43",
        "incepted": "20260610T120003000001Z",
        "description": "CLI review follow-up",
        "project": "serve.ui",
        "acceptance": "Review follow-up appears as a card",
        "origin_thread": actor,
        "creation_surface": "cli",
        "depends": ["reviewed-task-uuid"],
        "status": "pending",
    }
    seen: dict[str, object] = {}

    def fake_export(filters: list[str] | None = None) -> list[dict[str, object]]:
        seen["filters"] = filters
        return [row]

    monkeypatch.setattr(payloads.tw, "export", fake_export)

    cards = payloads._task_card_messages_for_thread(actor, after=None, before=None)

    assert seen["filters"] == [
        "status.any:",
        "creation_surface.is:cli",
        f"origin_thread.is:{actor}",
    ]
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


def test_task_card_cursor_merges_newer_backend_and_transcript_items(monkeypatch):
    actor = "a" * 32
    rows = [
        {
            "id": 1,
            "uuid": "older-task",
            "incepted": "20260610T120001000001Z",
            "description": "Older CLI follow-up",
            "project": "serve.ui",
            "origin_thread": actor,
            "creation_surface": "cli",
        },
        {
            "id": 2,
            "uuid": "newer-task",
            "incepted": "20260610T120002000001Z",
            "description": "Later CLI follow-up",
            "project": "serve.ui",
            "origin_thread": actor,
            "creation_surface": "cli",
        },
    ]
    boundary_key = "2026-06-10T12:00:01.000001Z#task-card:older-task"

    monkeypatch.setattr(payloads.tw, "export", lambda _filters: rows)

    merged = payloads._merge_task_card_messages(
        actor,
        [_message("2026-06-10T12:00:03.000000Z")],
        limit=5,
        after=boundary_key,
    )

    assert [item.display_text for item in merged] == [
        "hello",
        "Task capture: Later CLI follow-up (serve.ui)",
    ]
    boundary = message_reader.parse_timestamp("2026-06-10T12:00:01.000001Z")
    assert boundary is not None
    assert all(
        (timestamp := message_reader.parse_timestamp(item.timestamp)) is not None
        and timestamp > boundary
        for item in merged
    )


def test_task_card_tail_merge_drops_cards_older_than_visible_window(monkeypatch):
    actor = "a" * 32
    rows = [
        {
            "id": 1,
            "uuid": "stale-task",
            "incepted": "20260610T060000000001Z",
            "description": "Stale CLI follow-up",
            "project": "serve.docs",
            "origin_thread": actor,
            "creation_surface": "cli",
        },
        {
            "id": 2,
            "uuid": "fresh-task",
            "incepted": "20260610T120001000001Z",
            "description": "Fresh CLI follow-up",
            "project": "serve.ui",
            "origin_thread": actor,
            "creation_surface": "cli",
        },
    ]
    monkeypatch.setattr(payloads.tw, "export", lambda _filters: rows)

    merged = payloads._merge_task_card_messages(
        actor,
        [_message("2026-06-10T12:00:00.000000Z")],
        limit=5,
    )

    assert [item.display_text for item in merged] == [
        "Task capture: Fresh CLI follow-up (serve.ui)",
        "hello",
    ]
    assert all("Stale CLI follow-up" not in item.display_text for item in merged)


def test_inline_task_supervisor_success_updates_presence_preview(tmp_path, monkeypatch):
    latest = _stamp(datetime(2026, 6, 10, 12, 0, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "function_call_output",
            "call_id": "call-inline-task",
            "output": (
                "Chunk ID: 123\n"
                "Output:\n"
                "Supervisor Feedback\n"
                "  ack_archived=20260610T120000000000Z\n"
                "Supervisor Feedback\n"
                "  inline_task_created=FILTERS-20260610T120000000001Z "
                "UI-20260610T120000000002Z\n"
                "next task:\n"
            ),
        },
    )
    monkeypatch.setattr(
        payloads,
        "agent_status",
        lambda _repo: _Status(running=True, started_at="", process_status="running"),
    )
    monkeypatch.setattr(
        payloads, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)
    line = payloads.status_line_payload(
        _State(), _Target(id="wt", repo_root=tmp_path), items=items, error=None
    )

    assert len(items) == 1
    item = items[0]
    assert item.kind == "presence:function_call_output"
    assert item.preview == (
        "Tasks captured: FILTERS-20260610T120000000001Z, UI-20260610T120000000002Z"
    )
    assert line["preview"] == item.preview
    assert line["latestActivityPreview"] == item.preview
    assert line["latestMessagePreview"] == ""


def test_inline_task_supervisor_error_updates_presence_preview(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 12, 1, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "function_call_output",
            "call_id": "call-inline-task-error",
            "output": (
                "Output:\n"
                "Supervisor Feedback\n"
                "  inline_task_error=batch add rejected: line 2 project depth\n"
            ),
        },
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)

    assert len(items) == 1
    item = items[0]
    assert item.kind == "presence:function_call_output"
    assert item.preview == (
        "Task capture failed: batch add rejected: line 2 project depth"
    )


def test_status_line_prefers_latest_claude_presence_over_visible_message(
    tmp_path, monkeypatch
):
    claude_home = tmp_path / "claude"
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(claude_home))
    older = "2026-06-10T12:00:00.000Z"
    latest = "2026-06-10T12:01:00.000Z"
    transcript = (
        claude_home
        / "projects"
        / "-private-tmp-spice-sup"
        / "11111111-2222-3333-4444-555555555555.jsonl"
    )
    transcript.parent.mkdir(parents=True)
    transcript.write_text(
        "\n".join(
            [
                json.dumps(
                    {
                        "type": "assistant",
                        "timestamp": older,
                        "message": {
                            "role": "assistant",
                            "stop_reason": "end_turn",
                            "content": [{"type": "text", "text": "older answer"}],
                        },
                    },
                    separators=(",", ":"),
                ),
                json.dumps(
                    {
                        "type": "assistant",
                        "timestamp": latest,
                        "message": {
                            "role": "assistant",
                            "stop_reason": "tool_use",
                            "content": [
                                {
                                    "type": "tool_use",
                                    "name": "Bash",
                                    "input": {"command": "ls"},
                                }
                            ],
                        },
                    },
                    separators=(",", ":"),
                ),
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    target = _Target(id="wt", repo_root=tmp_path)
    monkeypatch.setattr(
        payloads,
        "agent_status",
        lambda _repo: _Status(running=True, started_at="", process_status="running"),
    )
    monkeypatch.setattr(
        payloads, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)
    line = payloads.status_line_payload(_State(), target, items=items, error=None)

    assert items[0].kind == "presence:function_call"
    assert line["lastAssistantAt"] == latest
    assert line["preview"] == "Bash: ls"
    assert line["latestActivityPreview"] == "Bash: ls"
    assert line["latestMessagePreview"] == "older answer"


def test_work_trees_payload_includes_latest_activity_for_global_menu(
    tmp_path, monkeypatch
):
    latest = _stamp(datetime(2026, 6, 10, 12, 1, tzinfo=UTC))
    target = _Target(id="wt", repo_root=tmp_path)
    calls: list[dict[str, object]] = []

    def fake_assistant_messages_for_thread_id(
        thread_id: str, **kwargs: object
    ) -> message_reader.AssistantMessageRead:
        calls.append({"thread_id": thread_id, **kwargs})
        return _message_read(
            [_message(latest, kind="presence:reasoning", preview="thinking")]
        )

    monkeypatch.setattr(payloads, "task_filter_inventory", lambda: {})
    monkeypatch.setattr(
        payloads, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )
    monkeypatch.setattr(
        payloads,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        payloads,
        "resolve_thread_id_for_target",
        lambda _state, _target: "agent-a",
    )
    monkeypatch.setattr(
        payloads,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id="agent-a",
        ),
    )
    monkeypatch.setattr(payloads, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(payloads, "configured_say_voice", lambda _repo: "")
    monkeypatch.setattr(
        payloads.message_reader,
        "assistant_messages_for_thread_id",
        fake_assistant_messages_for_thread_id,
    )

    payload = payloads.work_trees_payload(_InventoryState(target))

    work_tree = payload["workTrees"][0]
    assert work_tree["lastAssistantAt"] == latest
    assert work_tree["serveAgentIdentity"]["actorId"] == "thread:agent-a"
    assert work_tree["statusLine"]["lastAssistantAt"] == latest
    assert work_tree["statusLine"]["preview"] == "thinking"
    assert calls == [
        {
            "thread_id": "agent-a",
            "limit": 1,
            "worktree_id": "wt",
            "repo_root": tmp_path,
        }
    ]


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
        payloads,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "desired-model", "effort": "high"},
    )
    monkeypatch.setattr(payloads, "task_filter_inventory", lambda: {})
    monkeypatch.setattr(
        payloads, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )
    monkeypatch.setattr(
        payloads,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        payloads,
        "resolve_thread_id_for_target",
        lambda _state, _target: thread_id,
    )
    monkeypatch.setattr(
        payloads,
        "agent_status",
        lambda _repo: _identity_status(
            tmp_path,
            driver="claude",
            thread_id=thread_id,
            model="actual-model",
            effort="low",
        ),
    )
    monkeypatch.setattr(payloads, "agent_binding_error", lambda _repo, _status: "")
    monkeypatch.setattr(
        payloads.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: _message_read(transcript=transcript),
    )

    payload = payloads.messages_payload_for_worktree(
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


def test_lane_metrics_payload_reads_durable_agent_metrics(tmp_path):
    latest = datetime.now(UTC)
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.create_team(members=["thread:agent-a"])
    store.record_agent_metric_delta(
        "thread:agent-a",
        acked=2,
        sends=3,
        tool_calls=2,
        message_timestamps=[latest.timestamp()] * 4,
    )
    items = [
        _message(_stamp(latest), ack_count=2),
        _message(_stamp(latest), kind="presence:function_call"),
        _message(_stamp(latest), kind="presence:web_search_call"),
        _message(_stamp(latest), kind="presence:reasoning"),
    ]
    status = _Status(running=False, started_at="")
    metrics = lane_metrics_payload(
        _State(team_store=store),
        _Target(id="wt"),
        thread_id="agent-a",
        items=items,
        status=status,
    )
    assert metrics["acked"] == 2
    assert metrics["sends"] == 3
    assert metrics["toolCalls"] == 2
    assert metrics["drained"] == 0
    assert metrics["uptimeSeconds"] == 0
    assert sum(metrics["sparkline"]) == len(items)


def test_messages_payload_reports_agent_renewal_intent(monkeypatch, tmp_path):
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.create_team(members=["thread:agent-a"])
    _record_identity(store, "thread:agent-a", thread_id="agent-a")
    store.set_agent_renewal_request("thread:agent-a", requested=True)
    monkeypatch.setattr(payloads, "task_filter_inventory", lambda: {})
    monkeypatch.setattr(
        payloads, "pending_inbox_identity_payload", lambda _repo: _pending_identity()
    )
    monkeypatch.setattr(
        payloads,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        payloads,
        "resolve_thread_id_for_target",
        lambda _state, _target: "agent-a",
    )
    monkeypatch.setattr(
        payloads,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id="agent-a",
        ),
    )
    monkeypatch.setattr(
        payloads.message_reader,
        "assistant_messages_for_thread_id",
        lambda *_args, **_kwargs: _message_read(),
    )

    payload = payloads.messages_payload_for_worktree(
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


def test_ack_context_payload_round_trips_inbox_attachments(tmp_path):
    _init_repo(tmp_path)
    name = "20260104T000000000004Z.txt"
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

    payload = ack_context_payload_for_worktree(
        _State(sends=0),
        _Target(id="wt", repo_root=tmp_path),
        keys=[inbox_item_key(name)],
    )

    attachment = payload["acks"][0]["attachments"][0]
    assert payload["acks"][0]["text"] == "look here"
    assert payload["acks"][0]["html"] == "<p>look here</p>"
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
    pending_name = "20260104T000000000006Z.txt"
    archived_name = "20260104T000000000007Z.txt"
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
        payloads, "resolve_thread_id_for_target", lambda _state, _target: ""
    )
    monkeypatch.setattr(
        payloads,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        payloads,
        "agent_status",
        lambda _repo: _Status(running=False, started_at=""),
    )
    monkeypatch.setattr(payloads, "task_filter_inventory", lambda: {})

    payload = payloads.messages_payload_for_worktree(
        _State(),
        _Target(id="wt", repo_root=repo),
        limit=5,
    )
    assert set(payload) == {
        "messages",
        "targetWorktreeName",
        "targetBranch",
        "targetIdentity",
        "serveAgentIdentity",
        "taskFilters",
        "laneFilterVersion",
        "teamIdentity",
        "lifetime",
        "renewalIntent",
        "taskFilterInventory",
        "laneMetrics",
        "laneInfo",
        "agentProcessStatus",
        "error",
        "pendingInboxCount",
        "pendingInboxLabel",
        "pendingInboxKeys",
        "pendingInboxRevision",
        "agentEnsure",
        "statusLine",
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
    assert payload["statusLine"]["pendingInboxCount"] == 1
    assert payload["statusLine"]["pendingInboxLabel"] == "1"
    assert payload["statusLine"]["pendingInboxKeys"] == [inbox_item_key(pending_name)]
    assert (
        payload["statusLine"]["pendingInboxRevision"] == payload["pendingInboxRevision"]
    )


def test_ack_context_payload_finds_acked_inbox_item_by_dropped_z_alias(tmp_path):
    _init_repo(tmp_path)
    name = "20260104T000000000005Z.txt"
    bare_key = "20260104T000000000005"
    composed = compose_inbox_text(body="operator original", priority=None, stop=False)
    write_inbox_item(tmp_path, name, composed)
    archive_ackd_inbox_items(tmp_path, [bare_key])

    payload = ack_context_payload_for_worktree(
        _State(sends=0),
        _Target(id="wt", repo_root=tmp_path),
        keys=[bare_key],
    )

    assert payload["acks"][0]["key"] == bare_key
    assert payload["acks"][0]["found"] is True
    assert payload["acks"][0]["text"] == "operator original"


def test_ack_context_payload_does_not_quote_assistant_ack_when_inbox_missing(
    monkeypatch, tmp_path
):
    _init_repo(tmp_path)
    key = "20260104T000000000005Z"
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text(
        json.dumps(
            {
                "timestamp": "2026-01-04T00:00:01.000000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "output_text",
                            "text": f"ACK {key}: assistant-only acknowledgment",
                        }
                    ],
                },
            },
            separators=(",", ":"),
        )
        + "\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(
        payloads, "resolve_thread_id_for_target", lambda _state, _target: "thread-a"
    )

    payload = ack_context_payload_for_worktree(
        _State(sends=0),
        _Target(id="wt", repo_root=tmp_path),
        keys=[key],
    )

    assert payload["acks"] == [{"key": key, "found": False}]


def test_task_filter_inventory_reports_open_assignable_tasks(monkeypatch):
    monkeypatch.setattr(
        tw,
        "export",
        lambda _args: [
            {"project": "serve.ui"},
            {"project": "serve.ui"},
            {"project": "task.review"},
            {"project": "agent.abc123.task"},
            {"project": "oops"},
            {"project": "oops", "start": "2026-06-16T23:00:00Z"},
        ],
    )
    inventory = task_filter_inventory()
    filters = {item["name"]: item["openTaskCount"] for item in inventory["filters"]}
    stems = {item["name"]: item["openTaskCount"] for item in inventory["primaryStems"]}
    assert inventory["openTaskCount"] == 3
    assert filters["serve.ui"] == 2
    assert filters["task.review"] == 1
    assert "agent.abc123.task" not in filters
    assert "oops" not in filters
    assert "serve.example" in inventory["catalog"]["filterExamples"]
    assert stems["serve"] == 2
    assert stems["task"] == 1
    assert stems["agent"] == 1
    assert stems["oops"] == 1
