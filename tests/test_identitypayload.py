"""Lane metrics: sparkline buckets, uptime, and counter assembly."""

from dataclasses import dataclass
from datetime import datetime
import json
from pathlib import Path
import subprocess
from types import SimpleNamespace

import pytest

from spice.errors import SpiceError
from spice.serve.messages import AssistantMessage
from spice.serve import messages as message_reader
from spice.serve import (
    identitypayload,
    lanepayload,
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


def test_target_identity_payload_rejects_blank_bound_thread_id():
    with pytest.raises(SpiceError, match="thread id must be non-empty"):
        identitypayload.target_identity_payload(
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

    identity = identitypayload.target_identity_payload(
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
    serve_identity = identitypayload.serve_agent_identity_payload(
        target,
        "",
        binding_status="unbound",
    )
    rows = {
        row["key"]: row["value"]
        for row in lanepayload._lane_info_payload(target, serve_identity)["summaryRows"]
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
        identitypayload,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "gpt-5.5", "effort": "xhigh"},
    )
    monkeypatch.setattr(
        identitypayload,
        "agent_status",
        lambda _repo: _identity_status(repo),
    )

    identity = identitypayload.serve_agent_identity_payload(target)

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
        identitypayload,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "desired-model", "effort": "high"},
    )
    monkeypatch.setattr(
        identitypayload,
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

    identity = identitypayload.serve_agent_identity_payload(
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


def test_serve_agent_identity_reports_explicit_actor_renewal(tmp_path, monkeypatch):
    repo = tmp_path / "repo"
    repo.mkdir()
    _init_repo(repo)
    store = ServeTeamStore(tmp_path / "teams.sqlite")
    store.create_team(members=["thread:thread-a", "thread:thread-b"])
    _record_identity(store, "thread:thread-a", thread_id="thread-a")
    renewal = store.record_pending_renewal(
        agent_id="thread:thread-a", ancestor_thread_id="thread-a"
    )
    target = _Target(id="wt", repo_root=repo, name="repo", branch="main")
    monkeypatch.setattr(
        identitypayload,
        "effective_agent_config",
        lambda _repo: {"driver": "codex", "model": "gpt-5.5", "effort": "xhigh"},
    )
    monkeypatch.setattr(
        identitypayload,
        "agent_status",
        lambda _repo: _identity_status(repo, thread_id="thread-a"),
    )

    identity = identitypayload.serve_agent_identity_payload(
        target,
        actor_id="thread:thread-a",
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
    assert store.current_team_for_agent("thread:thread-a") is not None
    assert (
        identitypayload.serve_agent_identity_payload(target, actor_id="target:wt")[
            "actorId"
        ]
        == "target:wt"
    )


def test_team_identity_payload_rejects_missing_member_revisions():
    with pytest.raises(SpiceError, match="team revision is required"):
        identitypayload.team_identity_payload({"teamId": "team-1"})
