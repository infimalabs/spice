"""Lane metrics: sparkline buckets, uptime, and counter assembly."""

from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
import json
from pathlib import Path

from spice.mail.acks import archive_ackd_inbox_items
from spice.mail.attachments import prepare_inbox_attachments
from spice.mail.inbox import compose_inbox_text, inbox_item_key, write_inbox_item
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
        say_count=0,
        say_utterances=[],
        kind=kind,
        preview=preview,
    )


@dataclass(frozen=True)
class _Status:
    running: bool
    started_at: str
    process_status: str = "idle"
    thread_id: str = ""


@dataclass(frozen=True)
class _Target:
    id: str
    repo_root: Path | None = None
    name: str = "repo"
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


def _stamp(when: datetime) -> str:
    return when.isoformat(timespec="microseconds").replace("+00:00", "Z")


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
    monkeypatch.setattr(payloads, "pending_inbox_count", lambda _repo: 0)

    line = payloads.status_line_payload(_State(), target, items=items, error=None)

    assert line["lastAssistantAt"] == latest
    assert line["preview"] == "thinking"
    assert line["latestActivityPreview"] == "thinking"
    assert line["latestMessagePreview"] == ""


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
    monkeypatch.setattr(payloads, "pending_inbox_count", lambda _repo: 0)

    items = message_reader.read_assistant_messages(transcript, limit=5)
    line = payloads.status_line_payload(_State(), target, items=items, error=None)

    assert items[0].kind == "presence:function_call"
    assert line["lastAssistantAt"] == latest
    assert line["preview"] == "Bash: ls"
    assert line["latestActivityPreview"] == "Bash: ls"
    assert line["latestMessagePreview"] == "older answer"


def test_lane_metrics_payload_reads_durable_agent_metrics(tmp_path):
    latest = datetime(2026, 6, 10, 12, 0, tzinfo=UTC)
    store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    store.record_agent_metric_delta(
        "agent-a",
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


def test_sent_steering_payload_includes_image_attachments(tmp_path):
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
    assert payload["attachments"][0]["path"].startswith(".spice/inbox/")
    assert payload["attachments"][0]["url"].startswith(
        "/api/work/trees/wt/files/image?path="
    )


def test_ack_context_payload_round_trips_inbox_attachments(tmp_path):
    name = "20260104T000000000004Z.txt"
    composed = compose_inbox_text(body="look here", priority=None, stop=False)
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
    assert attachment["name"] == "upload.png"
    assert attachment["contentType"] == "image/png"
    assert attachment["path"].startswith(".spice/inbox/")
    assert attachment["url"].startswith("/api/work/trees/wt/files/image?path=")


def test_messages_payload_reports_inbox_status_without_streaming_requests(
    monkeypatch, tmp_path
):
    repo = tmp_path / "repo"
    repo.mkdir()
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
        "targetAgentName",
        "targetThreadId",
        "taskFilters",
        "laneFilterVersion",
        "teamId",
        "teamRevision",
        "configRevision",
        "lifetime",
        "taskFilterInventory",
        "laneMetrics",
        "laneInfo",
        "agentProcessStatus",
        "error",
        "pendingInboxCount",
        "agentEnsure",
        "statusLine",
    }
    assert payload["messages"] == []
    assert payload["pendingInboxCount"] == 1
    assert payload["statusLine"]["pendingInboxCount"] == 1
    assert payload["statusLine"]["pendingInboxLabel"] == "1"


def test_ack_context_payload_finds_archived_inbox_item_by_dropped_z_alias(tmp_path):
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
    monkeypatch.setattr(
        message_reader, "transcript_path_for_thread", lambda _thread_id: transcript
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
        ],
    )
    inventory = task_filter_inventory()
    filters = {item["name"]: item["openTaskCount"] for item in inventory["filters"]}
    stems = {item["name"]: item["openTaskCount"] for item in inventory["primaryStems"]}
    assert inventory["openTaskCount"] == 3
    assert filters["serve.ui"] == 2
    assert filters["task.review"] == 1
    assert "agent.abc123.task" not in filters
    assert stems["serve"] == 2
    assert stems["task"] == 1
    assert stems["agent"] == 1
