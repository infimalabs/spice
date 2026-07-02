"""Lane metrics: sparkline buckets, uptime, and counter assembly."""

from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
import json
from pathlib import Path
import subprocess
from types import SimpleNamespace


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
        state_path=repo / ".git" / "spice" / "agents" / "state.json",
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
                + supervisor_feedback_line(
                    "ack.archived", keys=["20260610T120000000000Z"]
                )
                + "\n"
                "Supervisor Feedback\n"
                "  "
                + supervisor_feedback_line(
                    "task.created",
                    handles=[
                        "FILTERS-20260610T120000000001Z",
                        "UI-20260610T120000000002Z",
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
        "Acknowledged: 20260610T120000000000Z "
        "Tasks captured: FILTERS-20260610T120000000001Z, UI-20260610T120000000002Z"
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
    key = "20260610T120000000000Z"
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
                    "incepted": "20260102T000000000001Z",
                    "project": "task.review",
                    "description": "Fix reviewed issue",
                    "review_author": "agent-a",
                    "review_by": "agent-b",
                    "review_finding": "changes",
                    "review_at": "2026-01-02T00:00:00Z",
                },
                {
                    "uuid": "clean-uuid",
                    "incepted": "20260102T000000000002Z",
                    "project": "task.review",
                    "description": "Clean review",
                    "review_author": "agent-a",
                    "review_by": "agent-c",
                    "review_finding": "clean",
                    "review_at": "2026-01-03T00:00:00Z",
                },
                {
                    "uuid": "other-uuid",
                    "incepted": "20260102T000000000003Z",
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
            "reviewedTask": "REVIEW-20260102T000000000001Z",
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
            "changes on REVIEW-20260102T000000000001Z "
            "by agent-b via task-review; 2 follow-ups"
        ),
        "span": True,
    }


def test_task_filter_inventory_reports_open_assignable_tasks(monkeypatch):
    seen: dict[str, list[str]] = {}

    def fake_export(args: list[str]) -> list[dict[str, object]]:
        seen["args"] = args
        return [
            {"project": "serve.ui"},
            {"project": "serve.ui"},
            {"project": "task.review"},
            {"project": "agent.abc123.task"},
            {"project": ".oops"},
            {"project": ".oops", "start": "2026-06-16T23:00:00Z"},
            {"project": "serve.ui", "status": "waiting", "wait": "2099-01-01"},
            {
                "project": ".oops",
                "status": "waiting",
                "tags": ["oops"],
                "wait": "2099-01-01",
            },
            {
                "project": "serve.ui",
                "status": "waiting",
                "tags": "hidden",
                "project_hidden": "1",
                "wait": "2099-01-01",
            },
        ]

    monkeypatch.setattr(
        tw,
        "export",
        fake_export,
    )
    inventory = task_filter_inventory()
    filters = {item["name"]: item["openTaskCount"] for item in inventory["filters"]}
    stems = {item["name"]: item["openTaskCount"] for item in inventory["primaryStems"]}
    assert seen["args"] == ["(", "status:pending", "or", "status:waiting", ")"]
    assert inventory["openTaskCount"] == 3
    assert filters["serve.ui"] == 2
    assert filters["task.review"] == 1
    assert "waiting" not in filters
    assert "agent.abc123.task" not in filters
    assert "oops" not in filters
    assert "serve.example" in inventory["catalog"]["filterExamples"]
    assert inventory["catalog"]["hiddenStems"] == ["oops"]
    assert inventory["catalog"]["hiddenProjectPrefix"] == "."
    assert stems["serve"] == 2
    assert stems["task"] == 1
    assert stems["agent"] == 1
    assert stems["oops"] == 4
    assert stems["waiting"] == 1
