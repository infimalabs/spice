from __future__ import annotations

import subprocess
from dataclasses import dataclass
from http import HTTPStatus
from io import BytesIO
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from spice.agent.driver import CODEX_DRIVER
from spice.serve import agentapi, app, workroutes
from spice.serve.worktree import inventory
from spice.serve.payload import identity, lane, message
from spice.serve.app import ServeState
from spice.serve.team.store import ServeTeamStore, TeamCommandService
from spice.serve.worktree.target import WorktreeTarget

IMAGE_DATA_URL = "data:image/png;base64,aW1hZ2UtYnl0ZXM="
THREAD_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
THREAD_B = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
ACTOR_A = f"thread:{THREAD_A}"
ACTOR_B = f"thread:{THREAD_B}"
TEAM_HISTORICAL_TEST_BUCKET_COUNT = 13


@dataclass(frozen=True)
class _BusTarget:
    id: str


class _Connection:
    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []

    def send_json(self, payload: dict[str, Any]) -> None:
        self.sent.append(payload)


class _StaticHandler:
    def __init__(self) -> None:
        self.status: HTTPStatus | None = None
        self.headers: dict[str, str] = {}
        self.body = BytesIO()
        self.wfile = self.body

    def send_error(self, status: HTTPStatus) -> None:
        self.status = status

    def send_response(self, status: HTTPStatus) -> None:
        self.status = status

    def send_header(self, name: str, value: str) -> None:
        self.headers[name] = value

    def end_headers(self) -> None:
        pass


class _ImageHandler(_StaticHandler):
    def __init__(self, state: ServeState) -> None:
        super().__init__()
        self.server = SimpleNamespace(spice_state=state)

    @property
    def state(self) -> ServeState:
        return self.server.spice_state

    def send_error(self, status: HTTPStatus, *_args: object) -> None:
        self.status = status

    def _send_bytes(self, data: bytes, content_type: str) -> None:
        app._ServeHandler._send_bytes(self, data, content_type)

    def _send_json(
        self, payload: dict[str, Any], status: HTTPStatus = HTTPStatus.OK
    ) -> None:
        app._ServeHandler._send_json(self, payload, status)


def _repo(tmp_path: Path) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir(exist_ok=True)
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    return repo


def _target(repo: Path) -> WorktreeTarget:
    return WorktreeTarget(id="target-1", repo_root=repo, name=repo.name, branch="main")


def _transcript_resolution(thread_id: str, path: Path) -> app.TranscriptResolution:
    return app.TranscriptResolution(
        thread_id=thread_id,
        path=path,
        owner_driver=CODEX_DRIVER,
    )


def _serve_state(tmp_path: Path, target: WorktreeTarget) -> ServeState:
    state = ServeState(anchor_root=tmp_path)
    state.cached_targets = [target]
    state.team_store = ServeTeamStore(path=tmp_path / "teams.sqlite3")
    state.team_commands = TeamCommandService(state.team_store)
    return state


def test_task_filter_inventory_carries_task_event_revision(tmp_path, monkeypatch):
    from spice.tasks import tw

    event_path = tmp_path / "events"
    event_path.write_text("123456789 unit\n", encoding="utf-8")
    calls: list[str] = []

    def ensure_event_file() -> Path:
        calls.append("revision")
        return event_path

    def export_tasks(_args):
        calls.append("export")
        return []

    monkeypatch.setattr(lane.task_config, "ensure_task_event_file", ensure_event_file)
    monkeypatch.setattr(tw, "export", export_tasks)

    inventory_payload = lane.task_filter_inventory()

    assert inventory_payload["revision"] == "123456789"
    assert calls == ["revision", "export"]


def _record_identity(
    state: ServeState,
    target: WorktreeTarget,
    actor_id: str,
    thread_id: str,
    *,
    desired_model: str = "gpt-next",
    desired_effort: str = "high",
) -> None:
    state.team_store.record_agent_identity(
        actor_id=actor_id,
        target_id=target.id,
        thread_id=thread_id,
        actual_driver="codex",
        actual_model="gpt-test",
        actual_effort="low",
        actual_service_tier="fast",
        desired_driver="codex",
        desired_model=desired_model,
        desired_effort=desired_effort,
        transcript_owner="codex",
    )


def _patch_agent_status(monkeypatch, *, thread_id: str, running: bool) -> None:
    status = SimpleNamespace(
        running=running,
        thread_id=thread_id,
        process_status="running" if running else "idle",
        pid=123 if running else 0,
        process_group_id=123 if running else 0,
        model="gpt-test",
        reasoning_effort="low",
        service_tier="fast",
        started_at="",
        log_path=None,
        prompt_skill_path=None,
    )
    monkeypatch.setattr(agentapi, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(identity, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(lane, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(message, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(workroutes, "agent_status", lambda *_args, **_kwargs: status)
    monkeypatch.setattr(inventory, "agent_status", lambda *_args, **_kwargs: status)
