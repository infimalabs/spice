"""Lane metrics: sparkline buckets, uptime, and counter assembly."""

from dataclasses import dataclass
from datetime import UTC, datetime
import json
from pathlib import Path
import subprocess
from types import SimpleNamespace


from spice.serve.messages import AssistantMessage
from spice.serve import messages as message_reader
from spice.serve import (
    identitypayload,
    messagepayload,
    worktreepayload,
)
from spice.serve.team.store import ServeTeamStore

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
        state_path=repo / ".git" / "spice" / "agents" / driver / "state.json",
    )


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

    monkeypatch.setattr(worktreepayload, "task_filter_inventory", lambda: {})
    monkeypatch.setattr(
        worktreepayload,
        "pending_inbox_identity_payload",
        lambda _repo: _pending_identity(),
    )
    monkeypatch.setattr(
        worktreepayload,
        "ensure_agent_for_pending_inbox",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(
        worktreepayload,
        "resolve_thread_id_for_target",
        lambda _state, _target: "agent-a",
    )
    monkeypatch.setattr(
        worktreepayload,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id="agent-a",
        ),
    )
    monkeypatch.setattr(
        identitypayload,
        "agent_status",
        lambda _repo: _Status(
            running=True,
            started_at="",
            process_status="running",
            thread_id="agent-a",
        ),
    )
    monkeypatch.setattr(
        worktreepayload, "agent_binding_error", lambda _repo, _status: ""
    )
    monkeypatch.setattr(identitypayload, "configured_say_voice", lambda _repo: "")
    monkeypatch.setattr(
        messagepayload.message_reader,
        "assistant_messages_for_thread_id",
        fake_assistant_messages_for_thread_id,
    )

    payload = worktreepayload.work_trees_payload(_InventoryState(target))

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
