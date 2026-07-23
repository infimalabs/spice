"""Lane metrics: sparkline buckets, uptime, and counter assembly."""

from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
import json
from pathlib import Path
import subprocess
from types import SimpleNamespace

import pytest

from spice.agent import watchdog
from spice.serve.messages import AssistantMessage
from spice.mail.feedback import supervisor_feedback_line
from spice.serve import messages as message_reader
from spice.serve.payload import lane
from spice.serve.payload.lane import (
    agent_uptime_seconds,
    lane_metrics_payload,
    task_filter_inventory,
)
from spice.serve.team.store import ServeTeamStore
from spice.tasks import tw

IMAGE_DATA_URL = "data:image/png;base64,aW1hZ2UtYnl0ZXM="

FIVE_MINUTES_SECONDS = 300


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
    index: int = 0,
    source_kind: str = "",
):
    return AssistantMessage(
        key=f"{timestamp}#{index}",
        index=index,
        timestamp=timestamp,
        text="hello",
        display_text="hello",
        display_html="<p>hello</p>",
        ack_count=ack_count,
        ack_keys=[],
        ack_utterances=[],
        kind=kind,
        preview=preview,
        source_kind=source_kind,
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

    def targets_discovery_errors(self) -> list[str]:
        return []


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
        "pendingInboxVersion": 100 + count,
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
        driver=driver,
        state_path=repo / ".git" / ".spice" / "agents" / "state.json",
    )


def test_uptime_measures_started_at_to_latest_message():
    started = datetime(2026, 6, 10, 12, 0, tzinfo=UTC)
    latest = started + timedelta(minutes=5)
    status = _Status(running=True, started_at=_stamp(started))
    uptime = agent_uptime_seconds(status, [_message(_stamp(latest))])
    assert uptime == FIVE_MINUTES_SECONDS


def test_uptime_reads_zero_while_agent_is_off():
    status = _Status(running=False, started_at="2026-06-10T12:00:00.000000Z")
    assert agent_uptime_seconds(status, []) == 0


def test_status_line_pairs_activity_preview_with_activity_timestamp(
    tmp_path, monkeypatch
):
    latest = _stamp(datetime(2026, 6, 10, 12, 0, tzinfo=UTC))
    target = _Target(id="wt", repo_root=tmp_path)
    items = [_message(latest, kind="presence:reasoning", preview="thinking")]
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _Status(running=True, started_at="", process_status="running"),
    )
    monkeypatch.setattr(
        lane,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )

    line = lane.status_line_payload(_State(), target, items=items, error=None)

    assert line["lastAssistantAt"] == latest
    assert line["preview"] == "thinking"
    assert line["latestActivityPreview"] == "thinking"
    assert line["latestMessagePreview"] == ""


def test_status_line_derives_visual_status_from_structural_activity_kind(
    tmp_path, monkeypatch
):
    timestamp = _stamp(datetime(2026, 6, 10, 12, 0, tzinfo=UTC))
    target = _Target(id="wt", repo_root=tmp_path)
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id="thread",
        ),
    )
    monkeypatch.setattr(
        lane,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    cases = (
        [_message(timestamp, kind="final", preview="Confirmed fixed.")],
        [_message(timestamp, kind="assistant", preview="Working")],
        [_message(timestamp, kind="presence:function_call", preview="Bash: test")],
        [],
    )

    observed = []
    for items in cases:
        line = lane.status_line_payload(_State(), target, items=items, error=None)
        observed.append(
            (
                line["latestActivityKind"],
                line["agentProcessStatus"],
                line["agentVisualStatus"],
                line["pendingInboxCount"],
            )
        )

    assert observed == [
        ("final", "running", "idle", 0),
        ("assistant", "running", "running", 0),
        ("presence:function_call", "running", "running", 0),
        ("", "running", "running", 0),
    ]


def test_status_line_renders_claimed_task_handle_and_title(tmp_path, monkeypatch):
    target = _Target(id="wt", repo_root=tmp_path)
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id="019f6edd-ab8c-7ab2-870a-f6b81dfc5b7f",
        ),
    )
    monkeypatch.setattr(
        lane,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    monkeypatch.setattr(
        lane.tw,
        "export",
        lambda filters=None, **_k: (
            [
                {
                    "claim_by": "019f6eddab8c7ab2870af6b81dfc5b7f",
                    "claim_at": "2026-06-10T00:00:00Z",
                    "description": "Show  claimed task\nwithout breaking the card",
                    "incepted": "1kF5xdSM",
                    "phase": "todo",
                    "project": "serve.ui",
                }
            ]
            if list(filters or []) == ["+ACTIVE"]
            else []
        ),
    )

    line = lane.status_line_payload(_State(), target, items=[], error=None)

    assert line["claimedTask"] == {
        "handle": "UI-1kF5xdSM",
        "phase": "todo",
        "title": "Show claimed task without breaking the card",
    }


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
                "  "
                + supervisor_feedback_line("ack.archived", keys=["1k4Yh5gN"])
                + "\n"
                "Supervisor Feedback\n"
                "  "
                + supervisor_feedback_line(
                    "task.created",
                    handles=[
                        "FILTERS-1k4Yh5gP",
                        "UI-1k4Yh5gQ",
                    ],
                )
                + "\n"
                "next task:\n"
            ),
        },
    )
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _Status(running=True, started_at="", process_status="running"),
    )
    monkeypatch.setattr(
        lane,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)
    line = lane.status_line_payload(
        _State(), _Target(id="wt", repo_root=tmp_path), items=items, error=None
    )

    assert len(items) == 1
    item = items[0]
    assert item.kind == "presence:function_call_output"
    assert item.preview == (
        "Acknowledged: 1k4Yh5gN Tasks captured: FILTERS-1k4Yh5gP, UI-1k4Yh5gQ"
    )
    assert line["preview"] == item.preview
    assert line["latestActivityPreview"] == item.preview
    assert line["latestMessagePreview"] == ""


def test_tool_output_preview_uses_matching_call_context(tmp_path, monkeypatch):
    call_time = _stamp(datetime(2026, 6, 10, 12, 0, tzinfo=UTC))
    output_time = _stamp(datetime(2026, 6, 10, 12, 0, 1, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text(
        "\n".join(
            json.dumps(
                {"timestamp": timestamp, "type": "response_item", "payload": payload},
                separators=(",", ":"),
            )
            for timestamp, payload in (
                (
                    call_time,
                    {
                        "type": "function_call",
                        "call_id": "call-status",
                        "name": "exec_command",
                        "arguments": json.dumps({"cmd": "git status --short"}),
                    },
                ),
                (
                    output_time,
                    {
                        "type": "function_call_output",
                        "call_id": "call-status",
                        "output": "ok\n",
                    },
                ),
            )
        )
        + "\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _Status(running=True, started_at="", process_status="running"),
    )
    monkeypatch.setattr(
        lane,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)
    line = lane.status_line_payload(
        _State(), _Target(id="wt", repo_root=tmp_path), items=items, error=None
    )

    assert len(items) == 1
    assert items[0].kind == "presence:function_call_output"
    assert items[0].preview == "exec command: git status --short -> ok"
    assert line["preview"] == items[0].preview
    assert line["latestActivityPreview"] == items[0].preview


def test_tool_output_preview_uses_output_text_without_call_context(tmp_path):
    timestamp = _stamp(datetime(2026, 6, 10, 12, 0, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        timestamp,
        {
            "type": "function_call_output",
            "call_id": "call-missing",
            "output": "build passed\n",
        },
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)

    assert len(items) == 1
    assert items[0].kind == "presence:function_call_output"
    assert items[0].preview == "Tool output: build passed"


def test_ack_feedback_distinguishes_first_and_duplicate_attempts(tmp_path, monkeypatch):
    first = _stamp(datetime(2026, 6, 10, 12, 0, tzinfo=UTC))
    duplicate = _stamp(datetime(2026, 6, 10, 12, 1, tzinfo=UTC))
    key = "1k4Yh5gN"
    transcript = tmp_path / "rollout.jsonl"
    transcript.write_text(
        "\n".join(
            json.dumps(
                {"timestamp": timestamp, "type": "response_item", "payload": payload},
                separators=(",", ":"),
            )
            for timestamp, payload in (
                (
                    first,
                    {
                        "type": "function_call_output",
                        "call_id": "call-ack-first",
                        "output": (
                            "Output:\n"
                            "Supervisor Feedback\n"
                            f"  {supervisor_feedback_line('ack.archived', keys=[key])}\n"
                        ),
                    },
                ),
                (
                    duplicate,
                    {
                        "type": "function_call_output",
                        "call_id": "call-ack-duplicate",
                        "output": (
                            "Output:\n"
                            "Supervisor Feedback\n"
                            f"  {supervisor_feedback_line('ack.already-acked', keys=[key])}\n"
                        ),
                    },
                ),
            )
        )
        + "\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(
        lane,
        "agent_status",
        lambda _repo: _Status(running=True, started_at="", process_status="running"),
    )
    monkeypatch.setattr(
        lane,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)
    item_payloads = [item.to_payload() for item in items]
    line = lane.status_line_payload(
        _State(), _Target(id="wt", repo_root=tmp_path), items=items, error=None
    )

    assert [item["preview"] for item in item_payloads] == [
        f"Already acknowledged: {key}",
        f"Acknowledged: {key}",
    ]
    assert [item.preview for item in reversed(items)] == [
        f"Acknowledged: {key}",
        f"Already acknowledged: {key}",
    ]
    assert [item.kind for item in items] == [
        "presence:function_call_output",
        "presence:function_call_output",
    ]
    assert items[0].preview == f"Already acknowledged: {key}"
    assert line["preview"] == f"Already acknowledged: {key}"
    assert line["latestActivityPreview"] == f"Already acknowledged: {key}"
    assert line["latestMessagePreview"] == ""


def test_ack_noop_feedback_updates_presence_preview(tmp_path):
    timestamp = _stamp(datetime(2026, 6, 10, 12, 2, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        timestamp,
        {
            "type": "function_call_output",
            "call_id": "call-ack-noop",
            "output": (
                "Output:\n"
                "Supervisor Feedback\n"
                "  "
                + supervisor_feedback_line(
                    "ack.noop",
                    message=watchdog.ACK_NOOP_MESSAGE,
                )
                + "\n"
            ),
        },
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)

    assert len(items) == 1
    assert items[0].kind == "presence:function_call_output"
    assert items[0].preview == (
        'ACK ignored: Run spice task add --project <stem.child> --title "..." '
        '--acceptance "..." to capture non-inbox work; ACK…'
    )


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
                "  "
                + supervisor_feedback_line(
                    "task.error",
                    error="batch add rejected: line 2 project depth",
                )
                + "\n"
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


def test_ack_archival_supervisor_error_updates_presence_preview(tmp_path):
    latest = _stamp(datetime(2026, 6, 10, 12, 2, tzinfo=UTC))
    transcript = tmp_path / "rollout.jsonl"
    _write_response_item(
        transcript,
        latest,
        {
            "type": "function_call_output",
            "call_id": "call-ack-archival-error",
            "output": (
                "Output:\n"
                "Supervisor Feedback\n"
                "  "
                + supervisor_feedback_line(
                    "ack.error",
                    keys=["1kG83Rg9", "1kG8LMXw"],
                    error="database is locked",
                )
                + "\n"
            ),
        },
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)

    assert len(items) == 1
    item = items[0]
    assert item.kind == "presence:function_call_output"
    assert item.preview == (
        "Acknowledgment failed: 1kG83Rg9, 1kG8LMXw: database is locked"
    )


def test_ack_error_without_keys_still_renders_the_failure():
    output = (
        "Output:\n"
        "Supervisor Feedback\n"
        "  " + supervisor_feedback_line("ack.error", error="database is locked") + "\n"
    )

    items = message_reader._supervisor_feedback_items(output)

    assert items == [
        {
            "kind": "ack.error",
            "label": "Acknowledgment failed",
            "detail": "database is locked",
            "keys": [],
        }
    ]


def test_ack_error_presence_is_retained_behind_later_tool_output():
    failure = _message(
        _stamp(datetime(2026, 6, 10, 12, 3, tzinfo=UTC)),
        kind="presence:function_call_output",
        source_kind="function_call_output",
        preview="Acknowledgment failed: 1kG83Rg9: database is locked",
    )
    later = _message(
        _stamp(datetime(2026, 6, 10, 12, 4, tzinfo=UTC)),
        index=1,
        kind="presence:function_call_output",
        source_kind="function_call_output",
        preview="Output: ok",
    )

    kept = message_reader._trim_chronological([failure, later], limit=5)

    assert [message.preview for message in kept] == [failure.preview, later.preview]


def test_status_line_prefers_latest_claude_presence_over_visible_message(
    tmp_path, monkeypatch
):
    claude_home = tmp_path / "claude"
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(claude_home))
    latest_at = datetime.now(UTC)
    older = _stamp(latest_at - timedelta(minutes=1))
    latest = _stamp(latest_at)
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
        lane,
        "agent_status",
        lambda _repo: _Status(running=True, started_at="", process_status="running"),
    )
    monkeypatch.setattr(
        lane,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )

    items = message_reader.read_assistant_messages(transcript, limit=5)
    line = lane.status_line_payload(_State(), target, items=items, error=None)

    assert items[0].kind == "presence:function_call"
    assert line["lastAssistantAt"] == latest
    assert line["activityStatus"] == "active"
    assert line["preview"] == "Bash: ls"
    assert line["latestActivityPreview"] == "Bash: ls"
    assert line["latestMessagePreview"] == "older answer"


def test_lane_metrics_payload_reads_durable_agent_metrics(tmp_path):
    latest = datetime.now(UTC)
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.create_team(members=["thread:agent-a"])
    for index in range(3):
        store.record_directive_sent(
            f"d{index}", agent_id="thread:agent-a", team_id="thread:agent-a"
        )
    store.mark_directive_acked("d0")
    store.mark_directive_acked("d1")
    store.record_agent_metric_delta(
        "thread:agent-a",
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


def test_lane_info_payload_reports_review_pressure(monkeypatch):
    seen: list[list[str]] = []

    def fake_export(args: list[str]) -> list[dict[str, object]]:
        seen.append(args)
        if args == ["status:completed"]:
            return [
                {
                    "uuid": "reviewed-uuid",
                    "incepted": "1jNJvRyn",
                    "project": "task.review",
                    "description": "Fix reviewed issue",
                    "review_author": "agent-a",
                    "review_by": "agent-b",
                    "review_finding": "changes",
                    "review_at": "2026-01-02T00:00:00Z",
                },
                {
                    "uuid": "clean-uuid",
                    "incepted": "1jNJvRyp",
                    "project": "task.review",
                    "description": "Clean review",
                    "review_author": "agent-a",
                    "review_by": "agent-c",
                    "review_finding": "clean",
                    "review_at": "2026-01-03T00:00:00Z",
                },
                {
                    "uuid": "other-uuid",
                    "incepted": "1jNJvRyq",
                    "project": "task.review",
                    "description": "Other actor review",
                    "review_author": "agent-z",
                    "review_by": "agent-b",
                    "review_finding": "changes",
                    "review_at": "2026-01-04T00:00:00Z",
                },
            ]
        if args == ["(", "status:pending", "or", "status:waiting", ")"]:
            return [
                {"uuid": "followup-a", "depends": ["reviewed-uuid"]},
                {"uuid": "followup-b", "depends": "reviewed-uuid"},
                {"uuid": "unrelated", "depends": ["other-uuid"]},
            ]
        raise AssertionError(f"unexpected export args: {args}")

    monkeypatch.setattr(tw, "export", fake_export)
    serve_identity = {
        "actorId": "thread:agent-a",
        "thread": {"threadId": "agent-a"},
        "driver": {},
        "launch": {"desired": {}, "actual": {}},
    }

    payload = lane._lane_info_payload(_Target(id="wt"), serve_identity)
    pressure = payload["reviewPressure"]
    rows = {row["key"]: row for row in payload["summaryRows"]}

    assert seen == [
        ["status:completed"],
        ["(", "status:pending", "or", "status:waiting", ")"],
    ]
    assert pressure["count"] == 1
    assert pressure["openFollowupCount"] == 2
    assert pressure["items"] == [
        {
            "reviewedTask": "REVIEW-1jNJvRyn",
            "finding": "changes",
            "findingSeverity": "changes",
            "reviewer": "agent-b",
            "source": "task-review",
            "followupCount": 2,
            "reviewedAt": "2026-01-02T00:00:00Z",
        }
    ]
    assert rows["review pressure"] == {
        "key": "review pressure",
        "value": (
            "changes on REVIEW-1jNJvRyn by agent-b via task-review; 2 follow-ups"
        ),
        "span": True,
    }


@pytest.fixture(autouse=True)
def _reset_task_filter_inventory_cache():
    """Clear the revision-keyed inventory memo around every test.

    ``task_filter_inventory`` memoizes on ``task_filter_inventory_revision()``.
    These tests fake distinct boards under the same real task event revision, so
    a warm cache from one test would otherwise serve another test's board (and
    hide its export). Resetting the module cache before and after each test keeps
    every faked board computed fresh.
    """
    lane._task_filter_inventory_cache = None
    yield
    lane._task_filter_inventory_cache = None


def test_task_filter_inventory_reports_open_assignable_tasks(monkeypatch):
    seen: list[list[str]] = []

    def fake_export(args: list[str]) -> list[dict[str, object]]:
        seen.append(args)
        assert args == ["(", "status:pending", "or", "status:waiting", ")"]
        return [
            {"uuid": "ready-serve-a", "project": "serve.ui"},
            {"uuid": "ready-serve-b", "project": "serve.ui"},
            {
                "uuid": "in-flight-serve",
                "project": "serve.ui",
                "claim_by": "agent-a",
                "start": "20260616T230000Z",
            },
            {
                "uuid": "blocked-serve",
                "project": "serve.ui",
                "depends": ["ready-serve-a"],
            },
            {"uuid": "ready-task", "project": "task.review"},
            {"uuid": "private", "project": "agent.abc123.task"},
            {"uuid": "oops", "project": ".oops"},
            {
                "uuid": "active-oops",
                "project": ".oops",
                "start": "20260616T230000Z",
            },
            {
                "uuid": "deferred-serve",
                "project": "serve.ui",
                "status": "pending",
                "wait": "20990101T000000Z",
            },
            {
                "uuid": "deferred-oops",
                "project": ".oops.correctness",
                "status": "pending",
                "wait": "20990101T000000Z",
            },
            {
                "uuid": "deferred-maxim",
                "project": ".maxim_proposal",
                "status": "pending",
                "wait": "20990101T000000Z",
            },
        ]

    monkeypatch.setattr(
        tw,
        "export",
        fake_export,
    )
    inventory = task_filter_inventory()
    filters = {item["name"]: item for item in inventory["filters"]}
    stems = {item["name"]: item for item in inventory["primaryStems"]}
    assert seen == [["(", "status:pending", "or", "status:waiting", ")"]]
    assert inventory["openTaskCount"] == 6
    assert filters["serve.ui"] == {
        "name": "serve.ui",
        "primaryStem": "serve",
        "openTaskCount": 5,
        "readyTaskCount": 2,
        "inFlightTaskCount": 1,
        "blockedTaskCount": 1,
        "deferredTaskCount": 1,
    }
    assert filters["task.review"] == {
        "name": "task.review",
        "primaryStem": "task",
        "openTaskCount": 1,
        "readyTaskCount": 1,
        "inFlightTaskCount": 0,
        "blockedTaskCount": 0,
        "deferredTaskCount": 0,
    }
    assert "waiting" not in filters
    assert "agent.abc123.task" not in filters
    assert "oops" not in filters
    assert "serve.example" in inventory["catalog"]["filterExamples"]
    assert inventory["catalog"]["hiddenStems"] == ["oops", "maxim_proposal"]
    assert inventory["catalog"]["hiddenProjectPrefix"] == "."
    assert stems["serve"]["openTaskCount"] == 5
    assert stems["serve"]["readyTaskCount"] == 2
    assert stems["serve"]["inFlightTaskCount"] == 1
    assert stems["serve"]["blockedTaskCount"] == 1
    assert stems["serve"]["deferredTaskCount"] == 1
    assert stems["task"]["readyTaskCount"] == 1
    assert stems["agent"]["readyTaskCount"] == 1
    assert stems["oops"] == {
        "name": "oops",
        "openTaskCount": 3,
        "readyTaskCount": 0,
        "inFlightTaskCount": 0,
        "blockedTaskCount": 0,
        "deferredTaskCount": 3,
        "filters": [],
        "oopsTaskCount": 3,
    }
    assert stems["maxim_proposal"] == {
        "name": "maxim_proposal",
        "openTaskCount": 1,
        "readyTaskCount": 0,
        "inFlightTaskCount": 0,
        "blockedTaskCount": 0,
        "deferredTaskCount": 1,
        "filters": [],
    }
    assert stems["waiting"]["openTaskCount"] == 1
    assert stems["waiting"]["deferredTaskCount"] == 1


def test_task_filter_inventory_resolves_config_once_independent_of_row_count(
    monkeypatch,
):
    rows: list[dict[str, object]] = []
    resolver_calls = 0
    real_resolver = lane.task_config._tasks_config_table

    def counted_resolver(*args, **kwargs):
        nonlocal resolver_calls
        resolver_calls += 1
        return real_resolver(*args, **kwargs)

    monkeypatch.setattr(lane.task_config, "_tasks_config_table", counted_resolver)
    monkeypatch.setattr(tw, "export", lambda _args: list(rows))

    for row_count in (1, 64):
        rows[:] = [
            {"uuid": f"ready-{index}", "project": "serve.latency"}
            for index in range(row_count)
        ]
        # An invalid project remains ignored without triggering another config
        # resolution or disturbing valid inventory counts.
        rows.append({"uuid": f"malformed-{row_count}", "project": ".bad-stem"})
        # Each row count is a distinct board, so give it its own revision: the
        # inventory memoizes on the revision, and reusing one token would serve
        # the first build's payload instead of re-resolving against these rows.
        monkeypatch.setattr(
            lane, "task_filter_inventory_revision", lambda rc=row_count: f"rows-{rc}"
        )
        calls_before = resolver_calls

        inventory = task_filter_inventory()

        assert resolver_calls == calls_before + 1
        assert inventory["openTaskCount"] == row_count
        assert inventory["filters"] == [
            {
                "name": "serve.latency",
                "primaryStem": "serve",
                "openTaskCount": row_count,
                "readyTaskCount": row_count,
                "inFlightTaskCount": 0,
                "blockedTaskCount": 0,
                "deferredTaskCount": 0,
            }
        ]


def test_task_filter_inventory_emits_configured_hidden_stem_rows(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(
        ["git", "init", "-b", "main"],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    )
    monkeypatch.chdir(repo)
    (repo / "pyproject.toml").write_text(
        '[tool.spice.tasks]\nhidden_stems = ["sandbox"]\n',
        encoding="utf-8",
    )

    def fake_export(args: list[str]) -> list[dict[str, object]]:
        assert args == ["(", "status:pending", "or", "status:waiting", ")"]
        return [
            {"uuid": "ready-serve", "project": "serve.ui"},
            {"uuid": "oops-a", "project": ".oops"},
            {"uuid": "oops-b", "project": ".oops.correctness"},
            {"uuid": "sandbox-a", "project": ".sandbox"},
            {"uuid": "sandbox-b", "project": ".sandbox.triage"},
            {"uuid": "sandbox-c", "project": ".sandbox"},
        ]

    monkeypatch.setattr(tw, "export", fake_export)
    inventory = task_filter_inventory()
    stems = {item["name"]: item for item in inventory["primaryStems"]}

    # The project-configured stem merges onto the built-in hidden stems, so the
    # catalog and pills carry it alongside oops and maxim_proposal.
    assert inventory["catalog"]["hiddenStems"] == ["oops", "maxim_proposal", "sandbox"]
    # Built-in oops keeps its dedicated oopsTaskCount signal for its own tasks...
    assert stems["oops"] == {
        "name": "oops",
        "openTaskCount": 2,
        "readyTaskCount": 0,
        "inFlightTaskCount": 0,
        "blockedTaskCount": 0,
        "deferredTaskCount": 2,
        "filters": [],
        "oopsTaskCount": 2,
    }
    # ...while the project-configured sandbox stem carries its own exact open
    # count on a distinct pill and never borrows the oops signal.
    assert stems["sandbox"] == {
        "name": "sandbox",
        "openTaskCount": 3,
        "readyTaskCount": 0,
        "inFlightTaskCount": 0,
        "blockedTaskCount": 0,
        "deferredTaskCount": 3,
        "filters": [],
    }


def test_task_filter_inventory_preserves_all_deferred_project_as_zero_ready(
    monkeypatch,
):
    def fake_export(args: list[str]) -> list[dict[str, object]]:
        assert args == ["(", "status:pending", "or", "status:waiting", ")"]
        return [
            {"uuid": "deferred-a", "project": "serve.ui", "wait": "20990101T000000Z"},
            {"uuid": "deferred-b", "project": "serve.ui", "wait": "20990101T000000Z"},
        ]

    monkeypatch.setattr(tw, "export", fake_export)
    inventory = task_filter_inventory()
    stems = {item["name"]: item for item in inventory["primaryStems"]}

    assert stems["serve"] == {
        "name": "serve",
        "openTaskCount": 2,
        "readyTaskCount": 0,
        "inFlightTaskCount": 0,
        "blockedTaskCount": 0,
        "deferredTaskCount": 2,
        "filters": ["serve.ui"],
    }
    assert stems["waiting"]["openTaskCount"] == 2


def test_task_filter_inventory_empty_board_has_empty_counts(monkeypatch):
    monkeypatch.setattr(tw, "export", lambda _args: [])

    inventory = task_filter_inventory()

    assert inventory["filters"] == []
    assert inventory["primaryStems"] == []
    assert inventory["openTaskCount"] == 0


def test_task_filter_inventory_memoizes_until_a_task_backend_change(
    tmp_path, monkeypatch
):
    """One export serves every same-revision build; a backend change re-exports.

    The pending/waiting export is the dominant repeated cost on the messages and
    work-trees builds, so unchanged-board builds must reuse a single export.
    Driving the real task event file (rather than stubbing the revision) proves
    the memo keys on the very token ``mark_task_backend_changed`` advances, so a
    genuine board change is never served stale.
    """
    monkeypatch.setenv(lane.task_config.TASK_BACKEND_ENV, str(tmp_path))
    board = [{"uuid": "ready", "project": "serve.latency"}]
    exports: list[list[str]] = []

    def counting_export(args: list[str]) -> list[dict[str, object]]:
        exports.append(args)
        return [dict(row) for row in board]

    monkeypatch.setattr(tw, "export", counting_export)

    bootstrap_revision = lane.task_config.task_event_revision()
    first = task_filter_inventory()
    second = task_filter_inventory()
    third = task_filter_inventory()

    # Three same-revision builds resolve to one underlying export and one payload.
    assert len(exports) == 1
    assert first == second == third
    assert first["revision"] == bootstrap_revision
    assert first["openTaskCount"] == 1

    lane.task_config.mark_task_backend_changed()
    advanced_revision = lane.task_config.task_event_revision()
    assert advanced_revision != bootstrap_revision

    fourth = task_filter_inventory()

    # The revision advanced, so the next build recomputes against the live board.
    assert len(exports) == 2
    assert fourth["revision"] == advanced_revision
    # Same board => the recompute reproduces the memoized values exactly, so the
    # cache changes nothing but the revision token it is keyed on.
    assert {key: value for key, value in fourth.items() if key != "revision"} == {
        key: value for key, value in first.items() if key != "revision"
    }
